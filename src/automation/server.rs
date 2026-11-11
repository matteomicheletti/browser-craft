//! The control server: a Unix socket only the user can reach (directory
//! 0700, socket 0600, and every peer's uid checked with SO_PEERCRED), one
//! thread per connection (at most `MAX_CONNECTIONS`), JSON-RPC 2.0 with one
//! request per line of at most `MAX_REQUEST_BYTES`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use super::protocol::{self, MAX_REQUEST_BYTES, Method, RpcError, RpcRequest, RpcResponse, VersionResult};
use super::{Request, Settings};

/// Connections served at once; more are answered "busy" and closed.
pub const MAX_CONNECTIONS: usize = 16;
/// An idle connection is closed after this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

type Callback = Arc<dyn Fn(Request) + Send + Sync>;

/// The running server. Dropping it stops listening and removes the socket.
pub struct Handle {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    settings: Arc<RwLock<Settings>>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Applies new permissions to the requests that follow.
    pub fn set_settings(&self, settings: Settings) {
        if let Ok(mut s) = self.settings.write() {
            *s = settings;
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = UnixStream::connect(&self.path);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Creates `dir` (0700) if needed and checks it is a real directory owned
/// by the user that nobody else can enter.
fn secure_dir(dir: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if meta.uid() != own_uid() {
        return Err(format!("{} belongs to another user", dir.display()));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// The uid of the process at the other end of `stream`.
pub fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::fd::AsRawFd;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred = libc::ucred { pid: 0, uid: u32::MAX, gid: u32::MAX };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: cred and len are valid for writes of the sizes given.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid) = (u32::MAX, u32::MAX);
        // SAFETY: uid and gid are valid for writes.
        let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// Starts listening on `path`. Fails if another server answers there, or
/// the path is something other than a stale socket.
pub fn start_at(
    path: &Path,
    settings: Settings,
    callback: impl Fn(Request) + Send + Sync + 'static,
) -> Result<Handle, String> {
    let dir = path.parent().ok_or("the socket path has no directory")?;
    secure_dir(dir)?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(format!("{} exists and is not a socket", path.display()));
        }
        if UnixStream::connect(path).is_ok() {
            return Err(format!("another BrowseCraft is already listening on {}", path.display()));
        }
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The directory is private, so nobody could connect before this.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let stop = Arc::new(AtomicBool::new(false));
    let settings = Arc::new(RwLock::new(settings));
    let callback: Callback = Arc::new(callback);
    let thread = {
        let (stop, settings) = (stop.clone(), settings.clone());
        std::thread::Builder::new()
            .name("automation".into())
            .spawn(move || accept_loop(listener, stop, settings, callback))
            .map_err(|e| e.to_string())?
    };
    Ok(Handle { path: path.to_path_buf(), stop, settings, thread: Some(thread) })
}

fn accept_loop(listener: UnixListener, stop: Arc<AtomicBool>, settings: Arc<RwLock<Settings>>, callback: Callback) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut stream) = stream else { continue };
        match peer_uid(&stream) {
            Ok(uid) if uid == own_uid() => {}
            // Another user (or an unknown one): close without a word.
            _ => continue,
        }
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let busy = RpcResponse::new(Value::Null, Err(RpcError::new(RpcError::BUSY, "too many connections")));
            let _ = writeln!(stream, "{}", busy.to_line());
            continue;
        }
        let (settings, callback, active) = (settings.clone(), callback.clone(), active.clone());
        let spawned = std::thread::Builder::new().name("automation-conn".into()).spawn(move || {
            serve(stream, &settings, &callback);
            active.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            eprintln!("browsecraft: automation: cannot start a connection thread");
        }
    }
}

/// Reads one line of at most `max` bytes (without the newline). None at the
/// end of the stream; an error for a longer line.
pub fn read_line_limited<R: BufRead>(reader: &mut R, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut buf = Vec::new();
    let n = reader.by_ref().take(max as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    } else if buf.len() > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long"));
    }
    Ok(Some(buf))
}

fn serve(stream: UnixStream, settings: &RwLock<Settings>, callback: &Callback) {
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let Ok(mut writer) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    loop {
        let line = match read_line_limited(&mut reader, MAX_REQUEST_BYTES) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    let err = RpcError::new(
                        RpcError::INVALID_REQUEST,
                        format!("request larger than {MAX_REQUEST_BYTES} bytes"),
                    );
                    let _ = writeln!(writer, "{}", RpcResponse::new(Value::Null, Err(err)).to_line());
                }
                return;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let current = settings.read().map(|s| s.clone()).unwrap_or_default();
        if let Some(response) = handle_line(&line, &current, callback)
            && writeln!(writer, "{response}").and_then(|_| writer.flush()).is_err()
        {
            return;
        }
    }
}

/// Answers one request line; None for notifications.
pub fn handle_line(line: &[u8], settings: &Settings, callback: &Callback) -> Option<String> {
    let value: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(e) => {
            let err = RpcError::new(RpcError::PARSE_ERROR, format!("invalid JSON: {e}"));
            return Some(RpcResponse::new(Value::Null, Err(err)).to_line());
        }
    };
    if value.is_array() {
        let err = RpcError::new(RpcError::INVALID_REQUEST, "batch requests are not supported");
        return Some(RpcResponse::new(Value::Null, Err(err)).to_line());
    }
    let id_of = |v: &Value| v.get("id").cloned().unwrap_or(Value::Null);
    let request: RpcRequest = match serde_json::from_value(value.clone()) {
        Ok(r) => r,
        Err(e) => {
            let err = RpcError::new(RpcError::INVALID_REQUEST, e.to_string());
            return Some(RpcResponse::new(id_of(&value), Err(err)).to_line());
        }
    };
    let id = request.id.clone();
    let outcome = dispatch(request, settings, callback);
    id.map(|id| RpcResponse::new(id, outcome).to_line())
}

fn dispatch(request: RpcRequest, settings: &Settings, callback: &Callback) -> Result<Value, RpcError> {
    if request.jsonrpc != "2.0" {
        return Err(RpcError::new(RpcError::INVALID_REQUEST, "jsonrpc must be \"2.0\""));
    }
    let method = Method::from_name(&request.method)
        .ok_or_else(|| RpcError::new(RpcError::METHOD_NOT_FOUND, format!("unknown method {}", request.method)))?;
    let params = request.params.unwrap_or(Value::Null);
    if !(params.is_null() || params.is_object()) {
        return Err(RpcError::invalid_params("params must be an object"));
    }
    method.validate(&params)?;
    settings.check(method, &params)?;
    if method == Method::BrowserVersion {
        return serde_json::to_value(VersionResult::current()).map_err(|e| RpcError::new(RpcError::INTERNAL, e.to_string()));
    }
    let wait = reply_timeout(method, &params);
    let (reply, answer) = mpsc::channel();
    callback(Request { method, params, reply });
    match answer.recv_timeout(wait) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(RpcError::timeout(format!("the browser did not answer {method} within {} s", wait.as_secs())))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(RpcError::browser(format!("the browser dropped {method}"))),
    }
}

/// How long the server waits for the browser's answer.
pub fn reply_timeout(method: Method, params: &Value) -> Duration {
    let asked = || protocol::timeout_ms(params.get("timeout_ms").and_then(Value::as_u64));
    Duration::from_millis(match method {
        Method::PageEval
        | Method::PageWait
        | Method::PageNavigate
        | Method::PageBack
        | Method::PageForward
        | Method::PageReload
        | Method::TabsOpen => asked() + 5_000,
        Method::PageScreenshot | Method::PagePdf => 60_000,
        _ => 30_000,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn on() -> Settings {
        Settings { enabled: true, ..Settings::default() }
    }

    /// A fake browser: answers tabs.list, echoes params, never answers
    /// page.reload (to test timeouts it is dropped instead).
    pub(crate) fn fake_browser(req: Request) {
        match req.method {
            Method::TabsList => req.respond(Ok(json!({"tabs": [{"id": 1, "index": 0, "url": "https://a.test/", "title": "A", "active": true, "loading": false}]}))),
            Method::PageReload => drop(req),
            Method::PageClick => req.respond(Err(RpcError::new(RpcError::NO_ELEMENT, "no element"))),
            _ => req.respond(Ok(json!({"method": req.method.name(), "params": req.params}))),
        }
    }

    fn call(line: &str, settings: &Settings) -> Value {
        let cb: Callback = Arc::new(fake_browser);
        serde_json::from_str(&handle_line(line.as_bytes(), settings, &cb).unwrap()).unwrap()
    }

    #[test]
    fn automation_server_dispatch() {
        let s = on();
        let r = call(r#"{"jsonrpc":"2.0","id":1,"method":"tabs.list"}"#, &s);
        assert_eq!(r["result"]["tabs"][0]["url"], "https://a.test/");
        let r = call(r#"{"jsonrpc":"2.0","id":2,"method":"page.text","params":{"selector":"main"}}"#, &s);
        assert_eq!(r["result"]["params"]["selector"], "main");
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#, &s)["error"]["code"], RpcError::METHOD_NOT_FOUND);
        assert_eq!(call("{oops", &s)["error"]["code"], RpcError::PARSE_ERROR);
        assert_eq!(call("[]", &s)["error"]["code"], RpcError::INVALID_REQUEST);
        assert_eq!(call(r#"{"jsonrpc":"1.0","id":4,"method":"tabs.list"}"#, &s)["error"]["code"], RpcError::INVALID_REQUEST);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":5,"method":"tabs.list","params":[1]}"#, &s)["error"]["code"], RpcError::INVALID_PARAMS);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":6,"method":"page.navigate","params":{}}"#, &s)["error"]["code"], RpcError::INVALID_PARAMS);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":7,"method":"page.eval","params":{"script":"1"}}"#, &s)["error"]["code"], RpcError::PERMISSION_DENIED);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":8,"method":"page.click","params":{"ref":"e1"}}"#, &s)["error"]["code"], RpcError::NO_ELEMENT);
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":9,"method":"page.reload"}"#, &s)["error"]["code"], RpcError::BROWSER);
        let v = call(r#"{"jsonrpc":"2.0","id":"v","method":"browser.version"}"#, &Settings::default());
        assert_eq!((v["id"].as_str(), v["result"]["api_version"].as_u64()), (Some("v"), Some(1)));
        let cb: Callback = Arc::new(fake_browser);
        assert!(handle_line(br#"{"jsonrpc":"2.0","method":"tabs.list"}"#, &s, &cb).is_none(), "notification");
    }

    #[test]
    fn automation_server_line_limits() {
        let mut r = Cursor::new(b"abc\r\ndef\nxyz".to_vec());
        assert_eq!(read_line_limited(&mut r, 8).unwrap().unwrap(), b"abc");
        assert_eq!(read_line_limited(&mut r, 8).unwrap().unwrap(), b"def");
        assert_eq!(read_line_limited(&mut r, 8).unwrap().unwrap(), b"xyz");
        assert!(read_line_limited(&mut r, 8).unwrap().is_none());
        let mut r = Cursor::new(b"12345678\n".to_vec());
        assert_eq!(read_line_limited(&mut r, 8).unwrap().unwrap(), b"12345678");
        let mut r = Cursor::new(b"123456789\n".to_vec());
        assert!(read_line_limited(&mut r, 8).is_err());
        let mut r = Cursor::new(vec![b'a'; 100]);
        assert!(read_line_limited(&mut r, 8).is_err());
    }

    #[test]
    fn automation_reply_timeouts() {
        assert_eq!(reply_timeout(Method::PageEval, &json!({"timeout_ms": 1000})), Duration::from_millis(6000));
        assert_eq!(reply_timeout(Method::PageWait, &json!({"timeout_ms": 99_999_999})), Duration::from_millis(125_000));
        assert_eq!(reply_timeout(Method::PageScreenshot, &json!({})), Duration::from_secs(60));
        assert_eq!(reply_timeout(Method::TabsList, &json!({})), Duration::from_secs(30));
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().mode() & 0o777
    }

    #[test]
    fn automation_socket_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("browsecraft");
        let path = dir.join("control.sock");
        let handle = start_at(&path, on(), fake_browser).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_socket());

        // The peer check sees us.
        let stream = UnixStream::connect(&path).unwrap();
        assert_eq!(peer_uid(&stream).unwrap(), own_uid());
        drop(stream);

        // A second server on the same socket is refused.
        let err = start_at(&path, on(), fake_browser).err().unwrap();
        assert!(err.contains("already listening"), "{err}");
        drop(handle);
        assert!(!path.exists(), "socket removed on drop");

        // A stale socket is replaced; a regular file is not.
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        let handle = start_at(&path, on(), fake_browser).unwrap();
        drop(handle);
        std::fs::write(&path, b"not a socket").unwrap();
        assert!(start_at(&path, on(), fake_browser).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
    }

    #[test]
    fn automation_socket_dir_is_tightened() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("open");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let handle = start_at(&dir.join("c.sock"), on(), fake_browser).unwrap();
        assert_eq!(mode(&dir), 0o700);
        drop(handle);
        // A directory of another user (root's /) is refused.
        if own_uid() != 0 {
            assert!(secure_dir(Path::new("/")).unwrap_err().contains("another user"));
        }
    }

    #[test]
    fn automation_server_over_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s").join("control.sock");
        let handle = start_at(&path, on(), fake_browser).unwrap();
        let stream = UnixStream::connect(&path).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        writeln!(writer, r#"{{"jsonrpc":"2.0","id":1,"method":"tabs.list"}}"#).unwrap();
        writeln!(writer).unwrap();
        writeln!(writer, r#"{{"jsonrpc":"2.0","id":2,"method":"page.html"}}"#).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains(r#""id":1"#) && line.contains("a.test"), "{line}");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains(r#""id":2"#) && line.contains("page.html"), "{line}");

        // Settings changes apply to the next request.
        handle.set_settings(Settings::default());
        writeln!(writer, r#"{{"jsonrpc":"2.0","id":3,"method":"tabs.list"}}"#).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("-32001"), "{line}");
        handle.set_settings(on());

        // An oversized request is answered with an error and the
        // connection closed.
        let big = format!(r#"{{"jsonrpc":"2.0","id":4,"method":"page.type","params":{{"selector":"a","text":"{}"}}}}"#, "x".repeat(MAX_REQUEST_BYTES));
        let _ = writer.write_all(big.as_bytes());
        let _ = writer.write_all(b"\n");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("-32600") && line.contains("larger"), "{line}");
        line.clear();
        assert_eq!(reader.read_line(&mut line).unwrap_or(0), 0, "closed");
    }

    #[test]
    fn automation_server_limits_connections() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("c").join("control.sock");
        let _handle = start_at(&path, on(), fake_browser).unwrap();
        let held: Vec<_> = (0..MAX_CONNECTIONS).map(|_| UnixStream::connect(&path).unwrap()).collect();
        // Make sure the server accepted them all before the extra one.
        for s in &held {
            let mut w = s.try_clone().unwrap();
            writeln!(w, r#"{{"jsonrpc":"2.0","id":0,"method":"browser.version"}}"#).unwrap();
            let mut line = String::new();
            BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
        }
        let extra = UnixStream::connect(&path).unwrap();
        let mut line = String::new();
        BufReader::new(extra).read_line(&mut line).unwrap();
        assert!(line.contains("-32004"), "{line}");
    }
}
