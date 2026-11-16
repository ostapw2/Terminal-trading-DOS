//! Unix-socket server inside the TUI.
//!
//! Connection threads only authenticate (token → role) and queue an
//! [`AgentCall`]; the **main loop** runs the tool, because it alone owns the
//! app state.  A call waits at most [`REPLY_TIMEOUT`] for that answer.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

#[cfg_attr(not(unix), allow(unused_imports))]
use super::{Role, WireResponse};

/// How long a connection waits for the main loop to answer.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(45);
/// Longest request line accepted (bytes).
pub const MAX_LINE: u64 = 1 << 20;
/// Longest socket path we bind (Unix `sun_path` is ~104 bytes on macOS, 108 on
/// Linux); longer paths fail in the OS with a confusing error.
pub const MAX_SOCKET_PATH: usize = 100;
/// A connection that sends nothing for this long is closed (otherwise 16 idle
/// connections, opened before any authentication, would lock everyone out).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Concurrent client connections.
pub const MAX_CONNECTIONS: usize = 16;

/// A request that passed authentication, waiting for the main loop.
pub struct AgentCall {
    pub role: Role,
    pub tool: String,
    pub args: Value,
    pub reply: mpsc::Sender<Result<Value, String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoleStats {
    pub calls: u64,
    pub denied: u64,
    pub last_call_ms: i64,
}

pub struct AgentServer {
    /// Calls waiting for the main loop (drain with `try_iter`).
    pub rx: Receiver<AgentCall>,
    pub path: PathBuf,
    stats: Arc<Mutex<HashMap<Role, RoleStats>>>,
    /// Requests that carried an unknown token (never reach the main loop).
    rejected: Arc<AtomicU64>,
    /// Set on drop: the accept thread ends and the socket file goes away,
    /// so another window can take over serving agents.
    #[cfg_attr(not(unix), allow(dead_code))]
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentServer {
    /// Count a handled call (`ok = false` means it was refused).
    pub fn record(&self, role: Role, ok: bool, now_ms: i64) {
        if let Ok(mut m) = self.stats.lock() {
            let s = m.entry(role).or_default();
            s.calls += 1;
            if !ok {
                s.denied += 1;
            }
            s.last_call_ms = now_ms;
        }
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn stats(&self) -> HashMap<Role, RoleStats> {
        self.stats.lock().map(|m| m.clone()).unwrap_or_default()
    }
}

#[cfg(unix)]
pub use imp::{start, start_with_idle};

#[cfg(not(unix))]
pub fn start(_dir: &std::path::Path) -> io::Result<AgentServer> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the agent connector needs Unix sockets",
    ))
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::agent::auth::Tokens;
    use crate::agent::{WireRequest, SOCKET_FILE};

    /// Start listening in `dir` (created if missing).  Fails if another
    /// instance already answers on the socket.
    pub fn start(dir: &Path) -> io::Result<AgentServer> {
        start_with_idle(dir, IDLE_TIMEOUT)
    }

    pub fn start_with_idle(dir: &Path, idle: Duration) -> io::Result<AgentServer> {
        let path = dir.join(SOCKET_FILE);
        let len = path.as_os_str().len();
        if len > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "socket path is {len} bytes (limit {MAX_SOCKET_PATH}); \
                     set DOS_AGENT_DIR to a shorter directory"
                ),
            ));
        }
        crate::agent::auth::secure_dir(dir)?;
        let tokens = Arc::new(Tokens::load_or_create(dir)?);
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another dos instance already serves agents on this socket",
                ));
            }
            std::fs::remove_file(&path)?; // stale file of a dead instance
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

        let (tx, rx) = mpsc::channel::<AgentCall>();
        let stats = Arc::new(Mutex::new(HashMap::new()));
        let rejected = Arc::new(AtomicU64::new(0));
        let rej = Arc::clone(&rejected);
        let active = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let mut s = stream;
                    let _ = writeln!(
                        s,
                        "{}",
                        serde_json::to_string(&WireResponse::err(0, "too many connections"))
                            .unwrap_or_default()
                    );
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let (tokens, tx, active) = (Arc::clone(&tokens), tx.clone(), Arc::clone(&active));
                let rej = Arc::clone(&rej);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(idle));
                    serve(stream, &tokens, &tx, &rej);
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Ok(AgentServer {
            rx,
            path,
            stats,
            rejected,
            stop,
        })
    }

    impl Drop for AgentServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wake the accept loop, then remove the socket file.
            let _ = UnixStream::connect(&self.path);
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn serve(
        stream: UnixStream,
        tokens: &Tokens,
        tx: &mpsc::Sender<AgentCall>,
        rejected: &AtomicU64,
    ) {
        let Ok(write_half) = stream.try_clone() else {
            return;
        };
        let mut writer = write_half;
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            let n = match reader.by_ref().take(MAX_LINE).read_line(&mut line) {
                Ok(n) => n,
                Err(_) => return,
            };
            if n == 0 {
                return; // closed
            }
            let resp = if !line.ends_with('\n') && n as u64 >= MAX_LINE {
                WireResponse::err(0, "request too large")
            } else {
                answer(&line, tokens, tx, rejected)
            };
            let mut text = serde_json::to_string(&resp).unwrap_or_default();
            text.push('\n');
            if writer.write_all(text.as_bytes()).is_err() {
                return;
            }
            if !line.ends_with('\n') {
                return; // oversized line: the stream is out of sync, drop it
            }
        }
    }

    fn answer(
        line: &str,
        tokens: &Tokens,
        tx: &mpsc::Sender<AgentCall>,
        rejected: &AtomicU64,
    ) -> WireResponse {
        let req: WireRequest = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(_) => return WireResponse::err(0, "bad request"),
        };
        let Some(role) = tokens.role_for(&req.token) else {
            rejected.fetch_add(1, Ordering::Relaxed);
            return WireResponse::err(req.id, "bad token");
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        let call = AgentCall {
            role,
            // The name is shown in the audit log and error texts: clean it.
            tool: crate::agent::clean_text(&req.tool, false)
                .chars()
                .take(64)
                .collect(),
            args: req.args,
            reply: reply_tx,
        };
        if tx.send(call).is_err() {
            return WireResponse::err(req.id, "terminal is shutting down");
        }
        match reply_rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(Ok(v)) => WireResponse::ok(req.id, v),
            Ok(Err(e)) => WireResponse::err(req.id, e),
            Err(_) => WireResponse::err(req.id, "terminal busy, try again"),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agent::auth::Tokens;
    use crate::agent::mcp::call_socket;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    fn temp_dir(tag: &str) -> PathBuf {
        // Unix socket paths are short (~100 bytes): keep the name tiny.
        let d = std::env::temp_dir().join(format!("dosa-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Stand-in for the main loop: answers `whoami` with the caller's role.
    fn fake_main_loop(server: AgentServer) -> (PathBuf, std::thread::JoinHandle<()>) {
        let path = server.path.clone();
        let h = std::thread::spawn(move || {
            while let Ok(call) = server.rx.recv_timeout(Duration::from_secs(5)) {
                let res = crate::agent::authorize(call.role, &call.tool)
                    .map(|_| serde_json::json!({"role": call.role.name()}));
                server.record(call.role, res.is_ok(), 1);
                let _ = call.reply.send(res);
            }
        });
        (path, h)
    }

    #[test]
    fn round_trip_tells_roles_apart_and_refuses_bad_tokens() {
        let dir = temp_dir("rt");
        let server = start(&dir).unwrap();
        let tokens = Tokens::load_or_create(&dir).unwrap();
        let rejected = server.rejected.clone();
        let (path, _h) = fake_main_loop(server);
        let rejected_of = |_: &PathBuf| rejected.load(std::sync::atomic::Ordering::Relaxed);

        for role in Role::ALL {
            let v = call_socket(
                &path,
                tokens.token_for(role).unwrap(),
                1,
                "whoami",
                &Value::Null,
            )
            .unwrap();
            assert_eq!(v["role"], role.name(), "role comes from the token");
        }
        let e = call_socket(&path, "wrong", 2, "whoami", &Value::Null).unwrap_err();
        assert!(e.contains("bad token"), "{e}");
        assert_eq!(rejected_of(&dir), 1, "bad tokens are counted");
        let t = tokens.token_for(Role::Analyst).unwrap();
        let e = call_socket(&path, t, 3, "no_such_tool", &Value::Null).unwrap_err();
        assert!(e.contains("unknown tool"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_and_oversized_input_do_not_break_the_server() {
        let dir = temp_dir("bad");
        let server = start(&dir).unwrap();
        let tokens = Tokens::load_or_create(&dir).unwrap();
        let (path, _h) = fake_main_loop(server);

        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"this is not json\n").unwrap();
        let mut buf = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&s), &mut buf).unwrap();
        assert!(buf.contains("bad request"), "{buf}");

        let mut s = UnixStream::connect(&path).unwrap();
        let big = vec![b'x'; MAX_LINE as usize + 10];
        let _ = s.write_all(&big); // the server may close before we finish
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.contains("too large") || out.is_empty());

        // Still serving afterwards.
        let v = call_socket(
            &path,
            tokens.token_for(Role::Risk).unwrap(),
            9,
            "whoami",
            &Value::Null,
        )
        .unwrap();
        assert_eq!(v["role"], "risk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn socket_is_owner_only_and_a_stale_file_is_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perm");
        let server = start(&dir).unwrap();
        let mode = std::fs::metadata(&server.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // A second instance on a live socket is refused.
        assert_eq!(
            start(&dir).err().map(|e| e.kind()),
            Some(io::ErrorKind::AddrInUse)
        );
        // A dead instance leaves a plain file behind: it must be replaced.
        let dir2 = temp_dir("stale");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join(crate::agent::SOCKET_FILE), b"junk").unwrap();
        assert!(start(&dir2).is_ok(), "stale non-socket file is removed");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn dropping_the_server_frees_the_socket_for_another_window() {
        let dir = temp_dir("handover");
        let first = start(&dir).unwrap();
        let path = first.path.clone();
        drop(first);
        assert!(!path.exists(), "socket file removed on drop");
        // A second window can now serve agents (the old accept thread is gone).
        let second = start(&dir).expect("the socket is free after a drop");
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_too_long_socket_path_is_refused_with_advice() {
        let dir = std::env::temp_dir().join("d".repeat(MAX_SOCKET_PATH));
        let _ = std::fs::remove_dir_all(&dir);
        let e = start(&dir).err().expect("must fail");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("DOS_AGENT_DIR"), "{e}");
        assert!(!dir.exists(), "nothing is created for a path we cannot use");
    }

    #[test]
    fn an_idle_connection_is_closed_so_it_cannot_hold_a_slot_forever() {
        let dir = temp_dir("idle");
        let server = imp_start_idle(&dir, Duration::from_millis(300));
        let mut s = UnixStream::connect(&server.path).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // Send nothing; the server must hang up on its own.
        let t0 = std::time::Instant::now();
        let mut buf = [0u8; 8];
        let n = s.read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0, "closed by the server");
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn imp_start_idle(dir: &std::path::Path, idle: Duration) -> AgentServer {
        super::imp::start_with_idle(dir, idle).unwrap()
    }

    #[test]
    fn stats_count_calls_and_denials() {
        let dir = temp_dir("st");
        let server = start(&dir).unwrap();
        server.record(Role::Scalper, true, 10);
        server.record(Role::Scalper, false, 20);
        let s = server.stats();
        let sc = s.get(&Role::Scalper).unwrap();
        assert_eq!((sc.calls, sc.denied, sc.last_call_ms), (2, 1, 20));
        assert!(!s.contains_key(&Role::Analyst));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
