//! `browsecraft ctl <method> [params-json]`: one call to the control
//! socket, the result printed as JSON. Also the socket client `mcp` uses.
//!
//! Exit codes: 0 success, 1 the browser answered an error, 2 bad usage,
//! 3 no browser is listening.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use super::protocol::{MAX_RESPONSE_BYTES, Method, RpcError, RpcResponse};

/// Why a call failed.
#[derive(Debug)]
pub enum CallError {
    /// Nobody listens on the socket (the browser is not running with
    /// automation on).
    Connect(String),
    /// The connection broke or the answer was not JSON-RPC.
    Io(String),
    /// The browser answered with an error.
    Rpc(RpcError),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Connect(e) => write!(f, "cannot reach the browser: {e}"),
            CallError::Io(e) => write!(f, "{e}"),
            CallError::Rpc(e) => write!(f, "{e}"),
        }
    }
}

/// Longest the client waits for an answer (the server gives up earlier).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Calls `method` on the browser listening at `path`.
#[cfg(unix)]
pub fn call_at(path: &Path, method: &str, params: Value) -> Result<Value, CallError> {
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(path).map_err(|e| CallError::Connect(format!("{}: {e}", path.display())))?;
    let _ = stream.set_read_timeout(Some(CALL_TIMEOUT));
    let mut writer = stream.try_clone().map_err(|e| CallError::Io(e.to_string()))?;
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    writeln!(writer, "{request}").map_err(|e| CallError::Io(e.to_string()))?;
    let mut reader = BufReader::new(stream);
    let line = super::server::read_line_limited(&mut reader, MAX_RESPONSE_BYTES)
        .map_err(|e| CallError::Io(format!("reading the answer: {e}")))?
        .ok_or_else(|| CallError::Io("the browser closed the connection".into()))?;
    let response: RpcResponse =
        serde_json::from_slice(&line).map_err(|e| CallError::Io(format!("invalid answer: {e}")))?;
    match (response.result, response.error) {
        (_, Some(e)) => Err(CallError::Rpc(e)),
        (Some(v), None) => Ok(v),
        (None, None) => Ok(Value::Null),
    }
}

#[cfg(not(unix))]
pub fn call_at(_path: &Path, _method: &str, _params: Value) -> Result<Value, CallError> {
    Err(CallError::Connect("the control socket is only available on Linux and macOS for now".into()))
}

/// Calls `method` on the running browser.
pub fn call(method: &str, params: Value) -> Result<Value, CallError> {
    let path = super::socket_path().map_err(CallError::Connect)?;
    call_at(&path, method, params)
}

fn usage() -> String {
    let mut s = String::from(
        "usage: browsecraft ctl <method> [params-json] [--out FILE] [--socket PATH]\n\n\
         Calls the browser started with --automation (or automation enabled in\n\
         settings) and prints the result as JSON. --out writes a screenshot or\n\
         PDF result to FILE instead of printing its base64.\n\nmethods:\n",
    );
    for m in Method::ALL {
        s.push_str(&format!("  {:<16} {}\n", m.name(), m.doc()));
    }
    s.push_str("\nexample: browsecraft ctl page.navigate '{\"url\":\"https://example.com\"}'\n");
    s
}

/// The `ctl` command; returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let mut positional = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut socket: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                return 0;
            }
            "--out" | "-o" => out = it.next().map(PathBuf::from),
            "--socket" => socket = it.next().map(PathBuf::from),
            _ => positional.push(arg.as_str()),
        }
    }
    let (Some(method), true) = (positional.first(), positional.len() <= 2) else {
        eprint!("{}", usage());
        return 2;
    };
    let params = match positional.get(1) {
        None => json!({}),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(v) if v.is_object() => v,
            _ => {
                eprintln!("browsecraft ctl: params must be a JSON object, such as '{{\"url\":\"https://example.com\"}}'");
                return 2;
            }
        },
    };
    if Method::from_name(method).is_none() {
        eprintln!("browsecraft ctl: unknown method {method} (see browsecraft ctl --help)");
        return 2;
    }
    let path = match socket.map(Ok).unwrap_or_else(super::socket_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("browsecraft ctl: {e}");
            return 3;
        }
    };
    match call_at(&path, method, params) {
        Ok(mut result) => {
            if let Some(out) = &out
                && let Err(e) = save_binary(&mut result, out)
            {
                eprintln!("browsecraft ctl: {e}");
                return 1;
            }
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            0
        }
        Err(CallError::Rpc(e)) => {
            println!("{}", json!({ "error": e }));
            eprintln!("browsecraft ctl: {e}");
            1
        }
        Err(e @ CallError::Connect(_)) => {
            eprintln!("browsecraft ctl: {e}\n(start the browser with `browsecraft --automation`)");
            3
        }
        Err(e) => {
            eprintln!("browsecraft ctl: {e}");
            1
        }
    }
}

/// Writes the `png_base64` / `pdf_base64` of a result to `out` and puts
/// `path` in its place.
fn save_binary(result: &mut Value, out: &Path) -> Result<(), String> {
    let Some(obj) = result.as_object_mut() else {
        return Err("--out: the result has no file data".into());
    };
    let key = ["png_base64", "pdf_base64"].into_iter().find(|k| obj.contains_key(*k)).ok_or("--out: the result has no file data")?;
    let data = obj.remove(key).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("--out: {e}"))?;
    std::fs::write(out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    obj.insert("path".into(), json!(out.display().to_string()));
    obj.insert("bytes".into(), json!(bytes.len()));
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::automation::server::{start_at, tests::fake_browser};
    use crate::automation::Settings;

    #[test]
    fn automation_ctl_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("r").join("control.sock");
        let _h = start_at(&path, Settings { enabled: true, ..Settings::default() }, fake_browser).unwrap();
        let tabs = call_at(&path, "tabs.list", json!({})).unwrap();
        assert_eq!(tabs["tabs"][0]["title"], "A");
        let echoed = call_at(&path, "page.snapshot", json!({"max_elements": 5})).unwrap();
        assert_eq!(echoed["params"]["max_elements"], 5);
        match call_at(&path, "page.click", json!({"selector": "#x"})) {
            Err(CallError::Rpc(e)) => assert_eq!(e.code, RpcError::NO_ELEMENT),
            other => panic!("{other:?}"),
        }
        assert!(matches!(call_at(&tmp.path().join("none.sock"), "tabs.list", json!({})), Err(CallError::Connect(_))));
        // The CLI: exit codes.
        let s = path.display().to_string();
        let args = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(main(&args(&["tabs.list", "--socket", &s])), 0);
        assert_eq!(main(&args(&["page.click", "{\"ref\":\"e1\"}", "--socket", &s])), 1);
        assert_eq!(main(&args(&["page.click", "[1]", "--socket", &s])), 2);
        assert_eq!(main(&args(&["no.such", "--socket", &s])), 2);
        assert_eq!(main(&args(&["tabs.list", "--socket", "/nonexistent/x.sock"])), 3);
        assert_eq!(main(&[]), 2);
    }

    #[test]
    fn automation_ctl_saves_binary_results() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("a.png");
        let mut result = json!({"png_base64": "iVBORw==", "width": 1, "height": 1});
        save_binary(&mut result, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), [0x89, b'P', b'N', b'G']);
        assert_eq!(result["bytes"], 4);
        assert!(result.get("png_base64").is_none());
        assert!(save_binary(&mut json!({"a": 1}), &out).is_err());
    }
}
