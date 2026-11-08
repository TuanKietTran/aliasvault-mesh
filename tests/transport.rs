//! Transport tests: local-directory and loopback HTTP only.
//! No test calls tailscale or any other machine; the HTTP server
//! binds 127.0.0.1:0 in a thread and serves a fixed request count.

use avmesh::discovery::{PeerRecord, PeerState};
use avmesh::manifest::{SnapshotManifest, VaultState};
use avmesh::snapshot::{create_snapshot, SnapshotSources, SnapshotStore};
use avmesh::transport::{
    constant_time_eq, pull_snapshot, serve_once, HttpTransport, LocalDirTransport, ServeConfig,
    Transport, TransportError,
};
#[cfg(unix)]
use avmesh::transport::{init_token, token_path};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn make_source_snapshot(base: &std::path::Path, revision: u64) -> PathBuf {
    let db = base.join("db-export.sql");
    fs::write(&db, format!("fake database export revision {revision}")).unwrap();
    let secrets = base.join("secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(secrets.join("jwt-key.txt"), "fake-secret-value").unwrap();
    let env_file = base.join("instance.env");
    fs::write(&env_file, "PRIVATE_EMAIL_DOMAINS=example.ts.net\n").unwrap();
    let hash = avmesh::snapshot::fingerprint(
        format!("fake database export revision {revision}").as_bytes(),
    );
    let manifest = SnapshotManifest::new(
        "node-a-key",
        1_700_000_000,
        vec![VaultState {
            manifest_id: "v1".into(),
            revision,
            ciphertext_hash: hash,
            buckets: vec![],
        }],
    );
    let dest = base.join("snap");
    let mut log = |_: &str| {};
    create_snapshot(
        &SnapshotSources {
            database_export: db,
            secrets_dir: secrets,
            environment_file: env_file,
        },
        &manifest,
        &dest,
        &mut log,
    )
    .unwrap();
    dest
}

fn peer(public_key: &str) -> PeerRecord {
    PeerRecord {
        public_key: public_key.to_string(),
        id: None,
        host_name: Some("node-a".into()),
        dns_name: Some("node-a.example.ts.net".into()),
        tailscale_ips: vec!["100.64.0.1".into()],
        tags: vec!["tag:aliasvault-mesh".into()],
        state: PeerState::Online,
        last_seen: None,
    }
}

const TEST_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SERVE_HOST: &str = "node-a.example.ts.net";
const SECRET_MARKER: &str = "fake-secret-value";

fn test_config() -> ServeConfig {
    ServeConfig::new(TEST_TOKEN, SERVE_HOST)
}

fn spawn_server(snapshot_dir: PathBuf, request_count: usize) -> String {
    let (port, _, _) = spawn_logged_server(snapshot_dir, request_count, test_config());
    format!("http://127.0.0.1:{port}")
}

fn spawn_logged_server(
    snapshot_dir: PathBuf,
    request_count: usize,
    config: ServeConfig,
) -> (u16, std::thread::JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let thread_logs = Arc::clone(&logs);
    let handle = std::thread::spawn(move || {
        for _ in 0..request_count {
            let mut log = |line: &str| thread_logs.lock().unwrap().push(line.to_string());
            let _ = serve_once(&listener, &snapshot_dir, &config, &mut log);
        }
    });
    (port, handle, logs)
}

fn request_bytes(method: &str, path: &str, host: Option<&str>, auth: Option<&str>) -> Vec<u8> {
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    if let Some(host) = host {
        req.push_str(&format!("Host: {host}\r\n"));
    }
    if let Some(auth) = auth {
        req.push_str(&format!("Authorization: Bearer {auth}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.into_bytes()
}

fn raw_exchange(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}

fn status_of(response: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(response).into_owned();
    text.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[test]
fn local_dir_pull_stages_and_promotes() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 3);
    let store = SnapshotStore::new(tmp.path().join("store"));
    let transport = LocalDirTransport::new(&snap);
    let mut log = |_: &str| {};
    let staged = pull_snapshot(&transport, &peer("node-a-key"), &store, &mut log).unwrap();
    assert_eq!(staged.vaults[0].revision, 3);
    let promoted = store.promote(&mut log).unwrap();
    assert_eq!(promoted.vaults[0].revision, 3);
    assert!(store.current_dir().is_dir());
}

#[test]
fn failed_verify_removes_incoming() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 3);
    // Corrupt a payload file after creation: the checksums no longer
    // match, so verification during staging must fail.
    fs::write(snap.join("payload/secrets/jwt-key.txt"), "tampered").unwrap();
    let store = SnapshotStore::new(tmp.path().join("store"));
    let transport = LocalDirTransport::new(&snap);
    let mut log = |_: &str| {};
    let err = pull_snapshot(&transport, &peer("node-a-key"), &store, &mut log).unwrap_err();
    assert!(
        matches!(err, TransportError::Snapshot(_)),
        "unexpected error: {err:?}"
    );
    let incoming = store.staging_dir().with_file_name("incoming");
    assert!(!incoming.exists(), "failed verify must remove incoming/");
}

#[cfg(unix)]
#[test]
fn successful_pull_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 3);
    let store = SnapshotStore::new(tmp.path().join("store"));
    let transport = LocalDirTransport::new(&snap);
    let mut log = |_: &str| {};
    pull_snapshot(&transport, &peer("node-a-key"), &store, &mut log).unwrap();
    // incoming/ itself is removed after a successful pull; the staged
    // tree it produced must be owner-only throughout (0700 dirs,
    // 0600 files).
    let incoming = store.staging_dir().with_file_name("incoming");
    assert!(!incoming.exists());
    fn check(path: &std::path::Path) {
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        if path.is_dir() {
            assert_eq!(mode, 0o700, "dir {} mode {mode:o}", path.display());
            for entry in fs::read_dir(path).unwrap() {
                check(&entry.unwrap().path());
            }
        } else {
            assert_eq!(mode, 0o600, "file {} mode {mode:o}", path.display());
        }
    }
    check(&store.staging_dir());
}

#[test]
fn pull_rejects_identity_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 3);
    let store = SnapshotStore::new(tmp.path().join("store"));
    let transport = LocalDirTransport::new(&snap);
    let mut log = |_: &str| {};
    let err = pull_snapshot(&transport, &peer("node-b-key"), &store, &mut log).unwrap_err();
    assert!(matches!(err, TransportError::IdentityMismatch { .. }));
    assert!(!store.current_dir().exists());
    assert!(!store.staging_dir().exists());
}

#[test]
fn fetch_rejects_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let transport = LocalDirTransport::new(&snap);
    for bad in [
        "../secret",
        "payload/../secret",
        "/etc/passwd",
        "payload//x",
    ] {
        assert!(
            matches!(
                transport.fetch_bytes(bad),
                Err(TransportError::UnsafePath(_))
            ),
            "path not rejected: {bad}"
        );
    }
}

#[test]
fn http_pull_stages_and_promotes() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 5);
    // manifest + checksums + 3 payload files.
    let base = spawn_server(snap, 5);
    let transport = HttpTransport::from_base_url(&base)
        .unwrap()
        .with_auth(TEST_TOKEN, SERVE_HOST);
    let store = SnapshotStore::new(tmp.path().join("store"));
    let mut log = |_: &str| {};
    let staged = pull_snapshot(&transport, &peer("node-a-key"), &store, &mut log).unwrap();
    assert_eq!(staged.vaults[0].revision, 5);
    store.promote(&mut log).unwrap();
    assert!(store.current_dir().is_dir());
}

#[test]
fn http_missing_file_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let base = spawn_server(snap, 1);
    let transport = HttpTransport::from_base_url(&base)
        .unwrap()
        .with_auth(TEST_TOKEN, SERVE_HOST);
    assert!(matches!(
        transport.fetch_bytes("payload/missing.bin"),
        Err(TransportError::NotFound(_))
    ));
}

#[test]
fn http_server_rejects_traversal_request() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = snap.clone();
    let config = test_config();
    let server = std::thread::spawn(move || {
        let mut log = |_: &str| {};
        let _ = serve_once(&listener, &dir, &config, &mut log);
    });
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!(
                "GET /payload/../../Cargo.toml HTTP/1.1\r\nHost: {SERVE_HOST}\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    server.join().unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "response: {response}");
}

#[test]
fn http_no_token_is_401_and_leaks_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let (port, server, _) = spawn_logged_server(snap, 1, test_config());
    let response = raw_exchange(
        port,
        &request_bytes(
            "GET",
            "/payload/secrets/jwt-key.txt",
            Some(SERVE_HOST),
            None,
        ),
    );
    server.join().unwrap();
    assert_eq!(
        status_of(&response),
        401,
        "response: {}",
        String::from_utf8_lossy(&response)
    );
    let text = String::from_utf8_lossy(&response).into_owned();
    assert!(
        !text.contains(SECRET_MARKER),
        "401 leaked file content: {text}"
    );
    assert!(!text.contains(TEST_TOKEN), "401 leaked token: {text}");
}

#[test]
fn http_wrong_token_is_401_same_and_different_length() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let (port, server, _) = spawn_logged_server(snap, 2, test_config());
    let same_len = format!("f{}", &TEST_TOKEN[1..]);
    for wrong in [same_len.as_str(), "short"] {
        let response = raw_exchange(
            port,
            &request_bytes(
                "GET",
                "/payload/secrets/jwt-key.txt",
                Some(SERVE_HOST),
                Some(wrong),
            ),
        );
        assert_eq!(
            status_of(&response),
            401,
            "wrong token {wrong:?} not rejected"
        );
        assert!(!String::from_utf8_lossy(&response).contains(SECRET_MARKER));
    }
    server.join().unwrap();
}

#[test]
fn http_right_token_serves_exact_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let file = snap.join("payload/secrets/jwt-key.txt");
    let expected = fs::read(&file).unwrap();
    let (port, server, _) = spawn_logged_server(snap, 1, test_config());
    let response = raw_exchange(
        port,
        &request_bytes(
            "GET",
            "/payload/secrets/jwt-key.txt",
            Some(SERVE_HOST),
            Some(TEST_TOKEN),
        ),
    );
    server.join().unwrap();
    assert_eq!(status_of(&response), 200);
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    assert_eq!(&response[split + 4..], expected.as_slice());
}

#[test]
fn http_wrong_host_is_421() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let (port, server, _) = spawn_logged_server(snap, 1, test_config());
    let response = raw_exchange(
        port,
        &request_bytes(
            "GET",
            "/manifest.json",
            Some("evil.example"),
            Some(TEST_TOKEN),
        ),
    );
    server.join().unwrap();
    assert_eq!(
        status_of(&response),
        421,
        "response: {}",
        String::from_utf8_lossy(&response)
    );
}

#[test]
fn http_post_is_405() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let (port, server, _) = spawn_logged_server(snap, 1, test_config());
    let response = raw_exchange(
        port,
        &request_bytes("POST", "/manifest.json", Some(SERVE_HOST), Some(TEST_TOKEN)),
    );
    server.join().unwrap();
    assert_eq!(
        status_of(&response),
        405,
        "response: {}",
        String::from_utf8_lossy(&response)
    );
}

#[test]
fn http_stalled_client_is_closed_after_read_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let config =
        ServeConfig::new(TEST_TOKEN, SERVE_HOST).with_read_timeout(Duration::from_millis(150));
    let (port, server, _) = spawn_logged_server(snap, 1, config);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let started = Instant::now();
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(n, 0, "stalled connection was not closed");
    assert!(
        elapsed < Duration::from_secs(3),
        "close took too long: {elapsed:?}"
    );
    server.join().unwrap();
}

#[test]
fn http_drip_client_is_closed_at_overall_request_deadline() {
    // T6c: per-read timeout is 2s and the drip gap is 50ms, so the
    // per-read clock never fires; only the 600ms overall request
    // deadline can close this connection.
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let config = ServeConfig::new(TEST_TOKEN, SERVE_HOST)
        .with_read_timeout(Duration::from_secs(2))
        .with_request_timeout(Duration::from_millis(600));
    let (port, server, logs) = spawn_logged_server(snap, 1, config);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) && !server.is_finished() {
        if stream.write_all(b"x").is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let elapsed = started.elapsed();
    assert!(
        server.is_finished(),
        "drip connection was not closed by the overall deadline after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "close took too long (per-read reset?): {elapsed:?}"
    );
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "drip connection was not closed");
    server.join().unwrap();
    let lines = logs.lock().unwrap();
    assert!(
        lines.iter().any(|l| l.contains("timed out")),
        "logs: {lines:?}"
    );
}

#[test]
fn token_never_appears_in_server_log_output() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let (port, server, logs) = spawn_logged_server(snap, 2, test_config());
    let _ = raw_exchange(
        port,
        &request_bytes("GET", "/manifest.json", Some(SERVE_HOST), Some(TEST_TOKEN)),
    );
    let _ = raw_exchange(
        port,
        &request_bytes("GET", "/manifest.json", Some(SERVE_HOST), None),
    );
    server.join().unwrap();
    let lines = logs.lock().unwrap();
    assert!(!lines.is_empty(), "server produced no log lines to inspect");
    for line in lines.iter() {
        assert!(!line.contains(TEST_TOKEN), "log leaked token: {line}");
    }
}

#[test]
fn constant_time_eq_compares_without_length_leak_semantics() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
    assert!(!constant_time_eq(b"", b"a"));
    assert!(constant_time_eq(b"", b""));
}

// Token generation needs an OS randomness source that is wired up on unix only (see README, Platform support).
#[cfg(unix)]
#[test]
fn token_init_writes_0600_hex_token_and_refuses_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("agent-data");
    let path = init_token(&data_dir).unwrap();
    assert_eq!(path, token_path(&data_dir));
    let contents = fs::read_to_string(&path).unwrap();
    let token = contents.trim();
    assert_eq!(token.len(), 64, "token must be 32 bytes hex: {token}");
    assert!(token
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file mode: {mode:o}");
    }
    assert!(
        init_token(&data_dir).is_err(),
        "second init must refuse to overwrite"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        contents,
        "token changed on refused init"
    );
}

#[test]
fn http_base_url_must_be_plain_http() {
    assert!(HttpTransport::from_base_url("https://node-a.example.ts.net").is_err());
    assert!(HttpTransport::from_base_url("http://127.0.0.1:18080").is_ok());
}

#[test]
fn http_base_url_must_be_loopback() {
    // Non-loopback hosts are rejected: with_auth would otherwise send
    // the per-mesh bearer token in cleartext off-machine.
    for base in [
        "http://example.com:80",
        "http://100.64.0.2:18080",
        "http://192.168.1.10:18080",
        "http://10.0.0.5:18080",
        "http://node-a.example.ts.net:18080",
        "http://0.0.0.0:18080",
        "http://127.0.0.2:18080",
        "http://[2001:db8::1]:18080",
    ] {
        assert!(
            HttpTransport::from_base_url(base).is_err(),
            "non-loopback base url must be rejected: {base}"
        );
    }
    for base in [
        "http://127.0.0.1:18080",
        "http://localhost:18080",
        "http://[::1]:18080",
    ] {
        assert!(
            HttpTransport::from_base_url(base).is_ok(),
            "loopback base url must be accepted: {base}"
        );
    }
}

fn spawn_raw_server<F>(handler: F) -> (String, std::thread::JoinHandle<()>)
where
    F: FnOnce(TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handler(stream);
    });
    (format!("http://127.0.0.1:{port}"), server)
}

#[test]
fn http_fetch_stalled_server_times_out() {
    // T6b (1): a server that accepts and sends nothing must fail the
    // fetch within the test-configured short timeouts, not hang.
    let (base, server) = spawn_raw_server(|_stream| {
        std::thread::sleep(Duration::from_millis(1200));
    });
    let transport = HttpTransport::from_base_url(&base).unwrap().with_timeouts(
        Duration::from_millis(200),
        Duration::from_millis(200),
        Duration::from_secs(2),
    );
    let started = Instant::now();
    let result = transport.fetch_bytes("manifest.json");
    let elapsed = started.elapsed();
    assert!(result.is_err(), "stalled fetch must fail: {result:?}");
    assert!(
        elapsed < Duration::from_millis(900),
        "stalled fetch took too long: {elapsed:?}"
    );
    server.join().unwrap();
}

#[test]
fn http_fetch_rejects_oversized_content_length_before_body() {
    // T6b (2): a Content-Length above the cap is rejected right after
    // the headers, without waiting for or consuming the body.
    let (base, server) = spawn_raw_server(|mut stream| {
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n")
            .unwrap();
        std::thread::sleep(Duration::from_millis(1200));
    });
    let transport = HttpTransport::from_base_url(&base)
        .unwrap()
        .with_timeouts(
            Duration::from_millis(500),
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .with_max_response_bytes(1024);
    let started = Instant::now();
    let result = transport.fetch_bytes("manifest.json");
    let elapsed = started.elapsed();
    assert!(result.is_err(), "oversized fetch must fail: {result:?}");
    assert!(
        elapsed < Duration::from_millis(900),
        "oversized Content-Length not rejected before body: {elapsed:?}"
    );
    server.join().unwrap();
}

#[test]
fn http_fetch_rejects_body_exceeding_cap_without_content_length() {
    // T6b: regardless of headers, the read itself is capped — a body
    // larger than the cap with no Content-Length still fails.
    let (base, server) = spawn_raw_server(|mut stream| {
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
        let _ = stream.write_all(&vec![b'x'; 4096]);
    });
    let transport = HttpTransport::from_base_url(&base)
        .unwrap()
        .with_max_response_bytes(1024);
    let result = transport.fetch_bytes("manifest.json");
    assert!(result.is_err(), "over-cap body must fail: {result:?}");
    server.join().unwrap();
}

#[test]
fn http_fetch_small_response_is_byte_identical() {
    // T6b (3): a normal small 200 response still fetches exactly.
    let (base, server) = spawn_raw_server(|mut stream| {
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
            .unwrap();
    });
    let transport = HttpTransport::from_base_url(&base).unwrap();
    let body = transport.fetch_bytes("manifest.json").unwrap();
    assert_eq!(body, b"hello");
    server.join().unwrap();
}

#[test]
fn a_late_request_is_read_even_though_the_listener_is_nonblocking() {
    // `serve_loop` polls a nonblocking listener; on macOS an accepted connection inherits that mode
    // unless it is reset, and a read that finds no data yet then looks like a timeout.
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServeConfig::new(TEST_TOKEN, SERVE_HOST);
    let server = std::thread::spawn(move || loop {
        match serve_once(&listener, &snap, &config, &mut |_| {}) {
            Err(TransportError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => break,
        }
    });
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let request = format!(
        "GET /manifest.json HTTP/1.1\r\nHost: {SERVE_HOST}\r\nAuthorization: Bearer wrong\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    server.join().unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401"),
        "a late request must still be read and answered, got: {response:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_snapshot_is_never_served() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = make_source_snapshot(tmp.path(), 1);
    let outside = tmp.path().join("outside-secret.txt");
    fs::write(&outside, "outside marker").unwrap();
    std::os::unix::fs::symlink(&outside, snap.join("payload").join("link.txt")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServeConfig::new(TEST_TOKEN, SERVE_HOST);
    let server = std::thread::spawn(move || serve_once(&listener, &snap, &config, &mut |_| {}));
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let request = format!(
        "GET /payload/link.txt HTTP/1.1\r\nHost: {SERVE_HOST}\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    server.join().unwrap().unwrap();
    assert!(response.starts_with("HTTP/1.1 404"), "got: {response:?}");
    assert!(
        !response.contains("outside marker"),
        "a linked file was served"
    );
}
