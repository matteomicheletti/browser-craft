//! Browser-side plugin logic in WebAssembly, interpreted by `wasmi` (pure
//! Rust, no JIT, no native code). Behind the `wasm` cargo feature.
//!
//! Why wasmi and not wasmtime: plugin logic is small glue (reacting to a
//! message, a few string operations), so an interpreter is fast enough, adds
//! a fraction of wasmtime's build time and binary size, has no executable
//! memory (no W^X exceptions) and a much smaller attack surface. Rust plugins
//! are compiled to `wasm32-unknown-unknown` and run here, never loaded as
//! native libraries.
//!
//! ABI ("bc" import module, all numbers i32, strings are UTF-8 in the
//! module's exported `memory`):
//!
//! - `log(ptr, len)`: append a line to the plugin log.
//! - `input_len() -> len`, `input_read(ptr, cap) -> copied`: the JSON of the
//!   `bc.sendMessage` argument.
//! - `output(ptr, len)`: the JSON (or text) answer; last call wins.
//! - `storage_get(kptr, klen, optr, ocap) -> len`: value length, -1 when
//!   absent; copies at most `ocap` bytes. Needs the `storage` permission.
//! - `storage_set(kptr, klen, vptr, vlen) -> 0 | -1`. Needs `storage`.
//!
//! The module exports `run()`, called once per message. Limits per call:
//! a fuel budget (about one unit per instruction), 16 MiB of memory, 64 KB
//! input/output, 64 log lines. A module that runs out of fuel or memory is
//! stopped and the call fails; the browser is unaffected.

use serde_json::Value;
#[cfg(feature = "wasm")]
use serde_json::Map;

pub const MAX_IO: usize = 64 * 1024;

#[cfg(not(feature = "wasm"))]
pub fn handle_message(reg: &mut super::registry::Registry, id: &str, _data: &Value) -> Result<Value, String> {
    let has_wasm = reg.enabled().any(|p| p.manifest.id == id && p.wasm.is_some());
    Err(if has_wasm {
        "this build has no WASM support (cargo feature \"wasm\")".into()
    } else {
        "plugin has no browser-side handler".into()
    })
}

#[cfg(feature = "wasm")]
pub use real::{handle_message, run, Limits, Run};

#[cfg(feature = "wasm")]
mod real {
    use super::*;
    use wasmi::{Caller, Config, Engine, Extern, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

    pub struct Limits {
        pub fuel: u64,
        pub memory_bytes: usize,
    }

    impl Default for Limits {
        fn default() -> Self {
            Self { fuel: 5_000_000, memory_bytes: 16 << 20 }
        }
    }

    pub struct Run {
        pub output: Vec<u8>,
        pub logs: Vec<String>,
        pub storage: Map<String, Value>,
        pub fuel_used: u64,
    }

    struct Host {
        input: Vec<u8>,
        output: Vec<u8>,
        logs: Vec<String>,
        storage: Map<String, Value>,
        limits: StoreLimits,
    }

    fn mem_read(c: &Caller<'_, Host>, ptr: i32, len: i32) -> Option<Vec<u8>> {
        let (ptr, len) = (usize::try_from(ptr).ok()?, usize::try_from(len).ok()?);
        if len > MAX_IO {
            return None;
        }
        let mem = c.get_export("memory").and_then(Extern::into_memory)?;
        let mut buf = vec![0u8; len];
        mem.read(c, ptr, &mut buf).ok()?;
        Some(buf)
    }

    fn mem_write(c: &mut Caller<'_, Host>, ptr: i32, bytes: &[u8]) -> Option<()> {
        let ptr = usize::try_from(ptr).ok()?;
        let mem = c.get_export("memory").and_then(Extern::into_memory)?;
        mem.write(c, ptr, bytes).ok()
    }

    /// Runs `run()` of `wasm` once. `storage` is the plugin's private
    /// key/value store (string values); changes come back in `Run::storage`.
    pub fn run(wasm: &[u8], input: &[u8], storage: Map<String, Value>, allow_storage: bool, lim: &Limits) -> Result<Run, String> {
        // Own thread with a known stack: the interpreter's frames are large in
        // debug builds and the caller (the UI thread) should not depend on it.
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(16 << 20)
                .spawn_scoped(s, || run_inner(wasm, input, storage, allow_storage, lim))
                .map_err(|e| e.to_string())?
                .join()
                .unwrap_or_else(|_| Err("module crashed the interpreter".into()))
        })
    }

    fn run_inner(wasm: &[u8], input: &[u8], storage: Map<String, Value>, allow_storage: bool, lim: &Limits) -> Result<Run, String> {
        if input.len() > MAX_IO {
            return Err("input too large".into());
        }
        let mut cfg = Config::default();
        cfg.consume_fuel(true);
        let engine = Engine::new(&cfg);
        let module = Module::new(&engine, wasm).map_err(|e| format!("invalid WASM module: {e}"))?;
        let host = Host {
            input: input.to_vec(),
            output: Vec::new(),
            logs: Vec::new(),
            storage,
            limits: StoreLimitsBuilder::new().memory_size(lim.memory_bytes).instances(1).memories(1).tables(2).trap_on_grow_failure(false).build(),
        };
        let mut store = Store::new(&engine, host);
        store.limiter(|h| &mut h.limits);
        store.set_fuel(lim.fuel).map_err(|e| e.to_string())?;

        let mut l = <Linker<Host>>::new(&engine);
        let e = |e: wasmi::errors::LinkerError| e.to_string();
        l.func_wrap("bc", "log", |mut c: Caller<'_, Host>, ptr: i32, len: i32| {
            if let Some(b) = mem_read(&c, ptr, len)
                && c.data().logs.len() < 64
            {
                c.data_mut().logs.push(String::from_utf8_lossy(&b).chars().take(500).collect());
            }
        })
        .map_err(e)?;
        l.func_wrap("bc", "input_len", |c: Caller<'_, Host>| c.data().input.len() as i32).map_err(e)?;
        l.func_wrap("bc", "input_read", |mut c: Caller<'_, Host>, ptr: i32, cap: i32| -> i32 {
            let n = c.data().input.len().min(usize::try_from(cap).unwrap_or(0));
            let bytes = c.data().input[..n].to_vec();
            if mem_write(&mut c, ptr, &bytes).is_some() { n as i32 } else { -1 }
        })
        .map_err(e)?;
        l.func_wrap("bc", "output", |mut c: Caller<'_, Host>, ptr: i32, len: i32| {
            if let Some(b) = mem_read(&c, ptr, len) {
                c.data_mut().output = b;
            }
        })
        .map_err(e)?;
        if allow_storage {
            l.func_wrap("bc", "storage_get", |mut c: Caller<'_, Host>, kp: i32, kl: i32, op: i32, oc: i32| -> i32 {
                let Some(key) = mem_read(&c, kp, kl).and_then(|k| String::from_utf8(k).ok()) else { return -1 };
                let Some(v) = c.data().storage.get(&key).and_then(|v| v.as_str()).map(|s| s.as_bytes().to_vec()) else { return -1 };
                let n = v.len().min(usize::try_from(oc).unwrap_or(0));
                if mem_write(&mut c, op, &v[..n]).is_none() {
                    return -1;
                }
                v.len() as i32
            })
            .map_err(e)?;
            l.func_wrap("bc", "storage_set", |mut c: Caller<'_, Host>, kp: i32, kl: i32, vp: i32, vl: i32| -> i32 {
                let key = mem_read(&c, kp, kl).and_then(|k| String::from_utf8(k).ok());
                let val = mem_read(&c, vp, vl).and_then(|v| String::from_utf8(v).ok());
                match (key, val) {
                    (Some(k), Some(v)) if !k.is_empty() => {
                        c.data_mut().storage.insert(k, Value::String(v));
                        0
                    }
                    _ => -1,
                }
            })
            .map_err(e)?;
        }
        let instance = l
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| format!("cannot start module (imports outside the documented ABI or missing permission?): {e}"))?;
        let entry = instance.get_typed_func::<(), ()>(&store, "run").map_err(|_| "module must export run()".to_string())?;
        entry.call(&mut store, ()).map_err(|e| format!("module stopped: {e}"))?;
        let left = store.get_fuel().unwrap_or(0);
        let h = store.into_data();
        if h.output.len() > MAX_IO {
            return Err("output too large".into());
        }
        Ok(Run { output: h.output, logs: h.logs, storage: h.storage, fuel_used: lim.fuel.saturating_sub(left) })
    }

    /// Bridge entry: runs the plugin's module for one `bc.sendMessage`.
    pub fn handle_message(reg: &mut super::super::registry::Registry, id: &str, data: &Value) -> Result<Value, String> {
        let (wasm, storage_ok) = reg
            .enabled()
            .find(|p| p.manifest.id == id)
            .and_then(|p| p.wasm.clone().map(|w| (w, p.manifest.has(super::super::permissions::Permission::Storage))))
            .ok_or("plugin has no browser-side handler")?;
        let out = run(&wasm, data.to_string().as_bytes(), reg.storage_all(id), storage_ok, &Limits::default())?;
        reg.storage_replace(id, out.storage)?;
        Ok(serde_json::from_slice(&out.output).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&out.output).into_owned())))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn module(body: &str) -> Vec<u8> {
            wat::parse_str(body).unwrap()
        }

        const ECHO: &str = r#"(module
          (import "bc" "log" (func $log (param i32 i32)))
          (import "bc" "input_len" (func $ilen (result i32)))
          (import "bc" "input_read" (func $iread (param i32 i32) (result i32)))
          (import "bc" "output" (func $out (param i32 i32)))
          (import "bc" "storage_set" (func $sset (param i32 i32 i32 i32) (result i32)))
          (import "bc" "storage_get" (func $sget (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "hello")
          (data (i32.const 16) "k")
          (func (export "run")
            (call $log (i32.const 0) (i32.const 5))
            (drop (call $iread (i32.const 100) (call $ilen)))
            (drop (call $sset (i32.const 16) (i32.const 1) (i32.const 100) (call $ilen)))
            (call $out (i32.const 100) (call $ilen))))"#;

        #[test]
        fn host_api_log_input_output_storage() {
            let r = run(&module(ECHO), br#"{"a":1}"#, Map::new(), true, &Limits::default()).unwrap();
            assert_eq!(r.logs, ["hello"]);
            assert_eq!(r.output, br#"{"a":1}"#);
            assert_eq!(r.storage["k"], Value::String(r#"{"a":1}"#.into()));
            assert!(r.fuel_used > 0 && r.fuel_used < 1000);
        }

        #[test]
        fn a_plugin_module_answers_bc_send_message() {
            use crate::plugins::{bridge, registry::Registry};
            let d = tempfile::tempdir().unwrap();
            let dev = d.path().join("dev");
            std::fs::create_dir_all(&dev).unwrap();
            std::fs::write(
                dev.join("plugin.toml"),
                "id = \"com.t.wasm\"\nname = \"W\"\nversion = \"1.0.0\"\napi_version = \"1\"\npermissions = [\"storage\"]\nwasm = \"p.wasm\"\n",
            )
            .unwrap();
            std::fs::write(dev.join("p.wasm"), module(ECHO)).unwrap();
            let mut r = Registry::open(d.path().join("cfg"));
            r.add_dev_folder(&dev).unwrap();
            let body = format!("bc-plugin:{}", serde_json::json!({"n": r.nonce(), "p": "com.t.wasm", "i": 1, "m": "message", "a": {"x": 2}}));
            let out = bridge::dispatch(&mut r, bridge::parse_page_message(&body).unwrap(), None);
            assert!(out.reply.unwrap().contains("__bcDeliver(\"com.t.wasm\",1,true"));
            assert_eq!(r.storage_get("com.t.wasm", "k"), Some(&Value::String(r#"{"x":2}"#.into())));
        }

        #[test]
        fn without_storage_permission_the_module_does_not_link() {
            let e = run(&module(ECHO), b"1", Map::new(), false, &Limits::default()).err().unwrap();
            assert!(e.contains("cannot start module"), "{e}");
        }

        #[test]
        fn fuel_stops_endless_loops() {
            let m = module(r#"(module (memory (export "memory") 1) (func (export "run") (loop $l (br $l))))"#);
            let e = run(&m, b"", Map::new(), false, &Limits { fuel: 10_000, ..Limits::default() }).err().unwrap();
            assert!(e.contains("module stopped"), "{e}");
        }

        #[test]
        fn memory_is_capped() {
            // Declares 2 pages (128 KiB) minimum against a 64 KiB cap: refused at instantiation.
            let m = module(r#"(module (memory (export "memory") 2) (func (export "run")))"#);
            let small = Limits { memory_bytes: 65536, ..Limits::default() };
            assert!(run(&m, b"", Map::new(), false, &small).is_err());
            // Growing past the cap fails (-1) instead of allocating.
            let g = module(r#"(module (import "bc" "output" (func $out (param i32 i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "ok")
                (func (export "run") (if (i32.eq (memory.grow (i32.const 100)) (i32.const -1)) (then (call $out (i32.const 0) (i32.const 2))))))"#);
            let r = run(&g, b"", Map::new(), false, &Limits { memory_bytes: 1 << 20, ..Limits::default() }).unwrap();
            assert_eq!(r.output, b"ok");
        }

        #[test]
        fn bad_modules_and_bad_pointers_fail_cleanly() {
            assert!(run(b"not wasm", b"", Map::new(), false, &Limits::default()).is_err());
            let no_run = module(r#"(module (memory (export "memory") 1))"#);
            assert!(run(&no_run, b"", Map::new(), false, &Limits::default()).err().unwrap().contains("run()"));
            // Out-of-bounds pointer: the host call is refused, the browser survives.
            let oob = module(r#"(module (import "bc" "log" (func $log (param i32 i32)))
                (memory (export "memory") 1)
                (func (export "run") (call $log (i32.const 70000) (i32.const 10)) (call $log (i32.const -1) (i32.const 10))))"#);
            let r = run(&oob, b"", Map::new(), false, &Limits::default()).unwrap();
            assert!(r.logs.is_empty());
        }
    }
}
