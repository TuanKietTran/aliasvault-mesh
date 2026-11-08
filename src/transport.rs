//! Snapshot transport behind a trait (ADR 0001 decision D,
//! tailnet.md sections 1 and 4).
//!
//! Two implementations move the same snapshot files (`manifest.json`,
//! `checksums.sha256`, `payload/**`): a local-directory transport
//! (tests, operator seeding) and a plain-HTTP transport built on
//! `std::net` only — no new dependencies. Per decision D the agent
//! binds loopback only and `tailscale serve` terminates TLS with the
//! node's real certificate; the agent never reads, generates or
//! stores a TLS key, so the HTTP transport speaks plain HTTP/1.1 to
//! the loopback port Serve forwards to. Identity and ACL come from
//! the tailnet (node key, `tag:aliasvault-mesh` grant); on top of
//! that, `pull_snapshot` binds the transfer to the allowlisted peer:
//! the fetched manifest's `node_id` must equal the peer's public
//! key, because a reachable port is never proof of identity. Files
//! land in a scratch directory first and are staged only when every
//! file listed in `checksums.sha256` has arrived; staging verifies
//! checksums and manifest, so a partial or corrupted transfer
//! is never promoted. Log lines carry paths and byte counts only,
//! never file contents.

use crate::discovery::PeerRecord;
use crate::manifest::SnapshotManifest;
use crate::snapshot::{SnapshotError, SnapshotStore, CHECKSUMS_FILE, MANIFEST_FILE, PAYLOAD_DIR};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    Snapshot(SnapshotError),
    NotFound(String),
    UnsafePath(String),
    IdentityMismatch { expected: String, actual: String },
    BadData(String),
    Http(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "io error: {e}"),
            TransportError::Snapshot(e) => write!(f, "snapshot error: {e}"),
            TransportError::NotFound(p) => write!(f, "snapshot file not found: {p}"),
            TransportError::UnsafePath(p) => write!(f, "unsafe snapshot path: {p}"),
            TransportError::IdentityMismatch { expected, actual } => write!(
                f,
                "manifest node_id {actual} does not match allowlisted peer key {expected}"
            ),
            TransportError::BadData(m) => write!(f, "bad snapshot data: {m}"),
            TransportError::Http(m) => write!(f, "http transport error: {m}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<SnapshotError> for TransportError {
    fn from(e: SnapshotError) -> Self {
        TransportError::Snapshot(e)
    }
}

pub type Result<T> = std::result::Result<T, TransportError>;

/// Name of the per-mesh bearer-token file inside the agent data
/// directory. The token is provisioned once per mesh by
/// `avmesh token init`, copied to the other nodes out of band, and
/// is never logged, never placed in argv and never stored in a
/// snapshot manifest.
pub const TOKEN_FILE: &str = "mesh.token";

/// Default read timeout for an accepted serve connection: a client
/// that stalls mid-request cannot hold the listener.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Default overall request deadline for `serve_once`:
/// bounds the whole request read from accept, so a client
/// dripping bytes below the per-read timeout cannot hold the
/// connection indefinitely.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Hard cap on request-header bytes read by `serve_once`.
pub const MAX_HEADER_BYTES: usize = 16384;

/// Default connect timeout for `HttpTransport::fetch_bytes`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default per-read timeout for `HttpTransport::fetch_bytes`.
pub const DEFAULT_FETCH_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Default overall deadline for one `HttpTransport::fetch_bytes`
/// call: per-read timeouts alone let a server that drips
/// bytes reset the clock forever, so the whole fetch is bounded.
pub const DEFAULT_FETCH_OVERALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Default response-body size cap for `HttpTransport::fetch_bytes`
///: 512 MiB.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;

/// Path of the token file inside an agent data directory.
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE)
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> Result<()> {
    // Owner-only ACLs are the operator's concern on non-Unix, the
    // same convention snapshot.rs follows for A3 permissions.
    Ok(())
}

#[cfg(unix)]
fn random_bytes(buf: &mut [u8]) -> Result<()> {
    use std::io::Read as _;
    let mut source = fs::File::open("/dev/urandom")?;
    source.read_exact(buf)?;
    Ok(())
}

#[cfg(not(unix))]
fn random_bytes(_buf: &mut [u8]) -> Result<()> {
    Err(TransportError::Http(
        "token generation needs an OS randomness source (/dev/urandom); provision the token file out of band on this platform".into(),
    ))
}

/// Create the per-mesh bearer token in `data_dir`: 32 random bytes,
/// hex-encoded (64 chars), written create-new with mode 0600 so an
/// existing token is never silently overwritten. Returns the token
/// file path; the token itself is never returned to a log or argv.
pub fn init_token(data_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(data_dir)?;
    let path = token_path(data_dir);
    let mut raw = [0u8; 32];
    random_bytes(&mut raw)?;
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            TransportError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "token already exists at {}; refusing to overwrite",
                    path.display()
                ),
            ))
        } else {
            TransportError::Io(e)
        }
    })?;
    file.write_all(hex.as_bytes())?;
    file.write_all(b"\n")?;
    set_file_mode(&path, 0o600)?;
    Ok(path)
}

/// Load the bearer token provisioned by `init_token`. The returned
/// value is a secret: callers must not log it or put it in argv.
pub fn load_token(data_dir: &Path) -> Result<String> {
    let text = fs::read_to_string(token_path(data_dir))?;
    let token = text.trim().to_string();
    if token.is_empty() {
        return Err(TransportError::BadData("token file is empty".into()));
    }
    Ok(token)
}

/// Constant-time byte comparison for bearer tokens: xor-folds every
/// byte position up to the longer input plus the length difference,
/// with no early return, so the result leaks neither the matching
/// prefix nor (beyond the fold) the expected length.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut acc: usize = a.len() ^ b.len();
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        acc |= usize::from(x ^ y);
    }
    acc == 0
}

/// Serving policy for `serve_once`: the bearer token a request
/// must present, the exact Host header it must carry (the configured
/// serve name; an optional `:port` suffix on the wire is ignored),
/// the per-read timeout applied to the accepted stream, and the
/// overall request deadline bounding the whole request read.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    token: String,
    expected_host: String,
    read_timeout: Duration,
    request_timeout: Duration,
}

impl ServeConfig {
    pub fn new(token: impl Into<String>, expected_host: impl Into<String>) -> Self {
        ServeConfig {
            token: token.into(),
            expected_host: expected_host.into(),
            read_timeout: DEFAULT_READ_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    pub fn with_read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }
}

/// Validate a snapshot-relative path (`manifest.json`,
/// `checksums.sha256`, `payload/...`) and turn it into a relative
/// `PathBuf`. Empty, absolute and parent-traversing paths are
/// rejected on both the fetching and the serving side.
fn safe_relative(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.contains('\\') {
        return Err(TransportError::UnsafePath(rel.to_string()));
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        match part {
            "" | "." | ".." => return Err(TransportError::UnsafePath(rel.to_string())),
            p => out.push(p),
        }
    }
    Ok(out)
}

/// A source of snapshot files. Implementations only fetch bytes;
/// verification, identity binding and staging live in
/// `pull_snapshot` so every transport gets them.
pub trait Transport {
    fn fetch_bytes(&self, rel: &str) -> Result<Vec<u8>>;

    fn fetch_manifest(&self) -> Result<SnapshotManifest> {
        let bytes = self.fetch_bytes(MANIFEST_FILE)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| TransportError::BadData(format!("invalid manifest json: {e}")))
    }
}

/// Fetch a whole snapshot from `transport` for the allowlisted
/// `peer` and stage it in `store` (verified, not yet promoted).
/// Fails before staging if the manifest's `node_id` is not the
/// peer's public key.
pub fn pull_snapshot<T: Transport>(
    transport: &T,
    peer: &PeerRecord,
    store: &SnapshotStore,
    log: &mut dyn FnMut(&str),
) -> Result<SnapshotManifest> {
    let manifest_bytes = transport.fetch_bytes(MANIFEST_FILE)?;
    let manifest: SnapshotManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| TransportError::BadData(format!("invalid manifest json: {e}")))?;
    if manifest.node_id != peer.public_key {
        return Err(TransportError::IdentityMismatch {
            expected: peer.public_key.clone(),
            actual: manifest.node_id.clone(),
        });
    }
    let checksums_bytes = transport.fetch_bytes(CHECKSUMS_FILE)?;
    let checksums_text = String::from_utf8(checksums_bytes.clone())
        .map_err(|e| TransportError::BadData(format!("checksums not utf-8: {e}")))?;
    let incoming = store.staging_dir().with_file_name("incoming");
    if incoming.exists() {
        fs::remove_dir_all(&incoming)?;
    }
    // Received files land owner-only in `incoming/` and the directory
    // is removed on every failure path: a failed verify must
    // not leave the received payload on disk at default modes.
    let staged = stage_incoming(
        transport,
        &incoming,
        &manifest_bytes,
        &checksums_bytes,
        &checksums_text,
        store,
        log,
    );
    match staged {
        Ok(manifest) => {
            fs::remove_dir_all(&incoming)?;
            Ok(manifest)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&incoming);
            Err(e)
        }
    }
}

/// Write the fetched snapshot files into `incoming` (dirs 0700,
/// files 0600 on unix) and stage them from there. Split out of
/// `pull_snapshot` so the caller can remove `incoming` on any error,
/// verification failures included.
fn stage_incoming<T: Transport>(
    transport: &T,
    incoming: &Path,
    manifest_bytes: &[u8],
    checksums_bytes: &[u8],
    checksums_text: &str,
    store: &SnapshotStore,
    log: &mut dyn FnMut(&str),
) -> Result<SnapshotManifest> {
    create_dirs_owner_only(incoming, incoming)?;
    let write_one = |rel: &str, bytes: &[u8]| -> Result<()> {
        let path = incoming.join(safe_relative(rel)?);
        if let Some(parent) = path.parent() {
            create_dirs_owner_only(incoming, parent)?;
        }
        fs::write(&path, bytes)?;
        set_file_mode(&path, 0o600)?;
        Ok(())
    };
    write_one(MANIFEST_FILE, manifest_bytes)?;
    write_one(CHECKSUMS_FILE, checksums_bytes)?;
    for line in checksums_text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let rel = line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| TransportError::BadData(format!("bad checksums line: {line}")))?;
        if !rel.starts_with(&format!("{PAYLOAD_DIR}/")) {
            return Err(TransportError::BadData(format!(
                "checksums entry outside payload: {rel}"
            )));
        }
        let bytes = transport.fetch_bytes(rel)?;
        log(&format!("transport: fetched {rel} ({} bytes)", bytes.len()));
        write_one(rel, &bytes)?;
    }
    let staged = store.stage_from(incoming, log)?;
    Ok(staged)
}

/// Create `dir` (and any missing parents), then force every
/// directory from `base` down to `dir` to owner-only 0700 on unix.
fn create_dirs_owner_only(base: &Path, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let mut cur: Option<&Path> = Some(dir);
    while let Some(d) = cur {
        set_file_mode(d, 0o700)?;
        if d == base {
            break;
        }
        cur = d.parent();
    }
    Ok(())
}

/// Local-directory transport: the snapshot lives in a directory on
/// this machine (tests, and an operator seeding a standby by hand).
#[derive(Debug, Clone)]
pub struct LocalDirTransport {
    root: PathBuf,
}

impl LocalDirTransport {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalDirTransport { root: root.into() }
    }
}

impl Transport for LocalDirTransport {
    fn fetch_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        let path = self.root.join(safe_relative(rel)?);
        fs::read(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                TransportError::NotFound(rel.to_string())
            } else {
                TransportError::Io(e)
            }
        })
    }
}

/// Plain-HTTP transport to a loopback base URL such as
/// `http://127.0.0.1:18080` — the port `tailscale serve` forwards to
/// on the serving node (tailnet.md section 4). An `https://` base is
/// rejected: TLS is Serve's layer, never the agent's. A non-loopback
/// host is rejected too: the bearer token must never be sent
/// off-machine.
#[derive(Debug, Clone)]
pub struct HttpTransport {
    host: String,
    port: u16,
    token: Option<String>,
    serve_host: Option<String>,
    connect_timeout: Duration,
    read_timeout: Duration,
    overall_timeout: Duration,
    max_response_bytes: usize,
}

impl HttpTransport {
    pub fn from_base_url(base: &str) -> Result<Self> {
        let rest = base.strip_prefix("http://").ok_or_else(|| {
            TransportError::Http(format!(
                "base url must be plain http on loopback (TLS is terminated by tailscale serve): {base}"
            ))
        })?;
        let hostport = rest.split('/').next().unwrap_or("");
        let (host, port) = hostport
            .rsplit_once(':')
            .ok_or_else(|| TransportError::Http(format!("base url has no port: {base}")))?;
        if host.is_empty() {
            return Err(TransportError::Http(format!(
                "base url has no host: {base}"
            )));
        }
        let port: u16 = port
            .parse()
            .map_err(|_| TransportError::Http(format!("base url has bad port: {base}")))?;
        // Loopback only. The bracketed form `[::1]` unwraps to
        // `::1`; lookalikes (127.0.0.2, localhost.evil, tailnet or LAN
        // addresses) never match the exact allowlist.
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        let loopback =
            host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost");
        if !loopback {
            return Err(TransportError::Http(format!(
                "base url host is not loopback (only 127.0.0.1, ::1 or localhost are allowed; refusing to send the mesh token off-machine): {base}"
            )));
        }
        Ok(HttpTransport {
            host: host.to_string(),
            port,
            token: None,
            serve_host: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_FETCH_READ_TIMEOUT,
            overall_timeout: DEFAULT_FETCH_OVERALL_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        })
    }

    /// Attach the per-mesh bearer token: every fetch sends
    /// `Authorization: Bearer <token>` and uses `serve_host` — the
    /// configured serve name the server requires — as the Host
    /// header instead of the loopback dial target.
    pub fn with_auth(mut self, token: impl Into<String>, serve_host: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self.serve_host = Some(serve_host.into());
        self
    }

    /// Override the fetch connect timeout, per-read timeout and
    /// overall fetch deadline (defaults are
    /// `DEFAULT_CONNECT_TIMEOUT`, `DEFAULT_FETCH_READ_TIMEOUT` and
    /// `DEFAULT_FETCH_OVERALL_TIMEOUT`).
    pub fn with_timeouts(
        mut self,
        connect_timeout: Duration,
        read_timeout: Duration,
        overall_timeout: Duration,
    ) -> Self {
        self.connect_timeout = connect_timeout;
        self.read_timeout = read_timeout;
        self.overall_timeout = overall_timeout;
        self
    }

    /// Override the response-body size cap (default
    /// `DEFAULT_MAX_RESPONSE_BYTES`).
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }
}

impl Transport for HttpTransport {
    fn fetch_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        safe_relative(rel)?;
        // T6b: every phase of the fetch is time-bounded. The connect
        // and each read have their own timeouts AND the whole fetch
        // has an overall deadline, so a hung or dripping server can
        // neither stall a pull nor reset the clock forever. The
        // response body is capped twice: a Content-Length above the
        // cap is rejected before the body is read, and the read
        // itself fails once the body passes the cap.
        let deadline = Instant::now() + self.overall_timeout;
        let remaining = |deadline: Instant| {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
        };
        let addr: SocketAddr = match self.host.parse::<std::net::IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, self.port),
            Err(_) => (self.host.as_str(), self.port)
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| {
                    TransportError::Http(format!("cannot resolve base url host: {}", self.host))
                })?,
        };
        let connect_budget = self
            .connect_timeout
            .min(remaining(deadline).ok_or_else(|| {
                TransportError::Http(format!(
                    "fetch for {rel} exceeded overall deadline {:?}",
                    self.overall_timeout
                ))
            })?)
            .max(Duration::from_millis(1));
        let mut stream = TcpStream::connect_timeout(&addr, connect_budget)?;
        let host_header = match &self.serve_host {
            Some(name) => name.clone(),
            None if self.host.contains(':') => format!("[{}]:{}", self.host, self.port),
            None => format!("{}:{}", self.host, self.port),
        };
        let mut request = format!("GET /{rel} HTTP/1.1\r\nHost: {host_header}\r\n");
        if let Some(token) = &self.token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.set_write_timeout(Some(self.read_timeout.max(Duration::from_millis(1))))?;
        stream.write_all(request.as_bytes())?;
        let mut response: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut split: Option<usize> = None;
        let mut status: u32 = 0;
        loop {
            let left = remaining(deadline).ok_or_else(|| {
                TransportError::Http(format!(
                    "fetch for {rel} exceeded overall deadline {:?}",
                    self.overall_timeout
                ))
            })?;
            stream.set_read_timeout(Some(
                self.read_timeout.min(left).max(Duration::from_millis(1)),
            ))?;
            let n = match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    if Instant::now() >= deadline {
                        return Err(TransportError::Http(format!(
                            "fetch for {rel} exceeded overall deadline {:?}",
                            self.overall_timeout
                        )));
                    }
                    return Err(TransportError::Io(e));
                }
                Err(e) => return Err(TransportError::Io(e)),
            };
            response.extend_from_slice(&chunk[..n]);
            if split.is_none() {
                match response.windows(4).position(|w| w == b"\r\n\r\n") {
                    Some(pos) => {
                        let head = String::from_utf8_lossy(&response[..pos]).into_owned();
                        status = head
                            .split_whitespace()
                            .nth(1)
                            .and_then(|s| s.parse().ok())
                            .ok_or_else(|| {
                                TransportError::Http(format!("malformed status line for {rel}"))
                            })?;
                        if let Some(cl) = header_value(&head, "Content-Length")
                            .and_then(|v| v.parse::<usize>().ok())
                        {
                            if cl > self.max_response_bytes {
                                return Err(TransportError::Http(format!(
                                    "response for {rel} exceeds size cap: content-length {cl} > {} bytes",
                                    self.max_response_bytes
                                )));
                            }
                        }
                        split = Some(pos);
                    }
                    None if response.len() > MAX_HEADER_BYTES => {
                        return Err(TransportError::Http(format!(
                            "response headers for {rel} exceed {MAX_HEADER_BYTES} bytes"
                        )));
                    }
                    None => {}
                }
            }
            if let Some(pos) = split {
                if response.len() - (pos + 4) > self.max_response_bytes {
                    return Err(TransportError::Http(format!(
                        "response for {rel} exceeds size cap of {} bytes",
                        self.max_response_bytes
                    )));
                }
            }
        }
        let split =
            split.ok_or_else(|| TransportError::Http(format!("malformed response for {rel}")))?;
        match status {
            200 => Ok(response[split + 4..].to_vec()),
            404 => Err(TransportError::NotFound(rel.to_string())),
            other => Err(TransportError::Http(format!(
                "unexpected status {other} for {rel}"
            ))),
        }
    }
}

fn header_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn host_matches(header: &str, expected: &str) -> bool {
    if header == expected {
        return true;
    }
    match header.rsplit_once(':') {
        Some((host, port)) => {
            host == expected && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Serve one HTTP request for a snapshot file from `snapshot_dir` on
/// an already-bound loopback listener, under the T5b policy in
/// `config`: GET only (405 otherwise), the Host header must equal
/// the configured serve name (421 otherwise, blocking DNS
/// rebinding), and the request must carry the per-mesh bearer token
/// compared in constant time (401 otherwise, with a body that holds
/// no file content and no hint about the token). The accepted stream
/// carries the configured read timeout, every read is bounded by
/// the overall request deadline, and headers are capped at
/// 16 KiB, so a stalled or dripping client cannot hold the listener. Only
/// `manifest.json`, `checksums.sha256` and `payload/**` are served;
/// anything else is a 400, a missing file a 404. Log lines carry the
/// outcome and byte counts only — never the token or any header
/// value. The caller owns the accept loop, so tests drive this on
/// `127.0.0.1:0` and no test leaves this machine.
pub fn serve_once(
    listener: &TcpListener,
    snapshot_dir: &Path,
    config: &ServeConfig,
    log: &mut dyn FnMut(&str),
) -> Result<()> {
    let (mut stream, _) = listener.accept()?;
    // A listener polled in nonblocking mode (`serve_loop`) hands out connections that inherit that
    // mode on BSD and macOS but not on Linux. Reads and writes here rely on timeouts, not on polling.
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + config.request_timeout;
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut timed_out = false;
    loop {
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => left,
            _ => {
                timed_out = true;
                break;
            }
        };
        let _ = stream.set_read_timeout(Some(
            config
                .read_timeout
                .min(remaining)
                .max(Duration::from_millis(1)),
        ));
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                request.extend_from_slice(&chunk[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") || request.len() > MAX_HEADER_BYTES
                {
                    break;
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                timed_out = true;
                break;
            }
            Err(e) => return Err(TransportError::Io(e)),
        }
    }
    if timed_out {
        log("transport: request timed out; connection closed");
        return Ok(());
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let mut request_line = text.lines().next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("");
    let raw_path = request_line.next().unwrap_or("");
    if method != "GET" {
        log("transport: rejected request (405)");
        return respond(
            &mut stream,
            405,
            "Method Not Allowed",
            b"method not allowed\n",
        );
    }
    let host_ok = header_value(&text, "host")
        .map(|h| host_matches(h, &config.expected_host))
        .unwrap_or(false);
    if !host_ok {
        log("transport: rejected request (421)");
        return respond(
            &mut stream,
            421,
            "Misdirected Request",
            b"misdirected request\n",
        );
    }
    let authorized = header_value(&text, "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|presented| constant_time_eq(presented.as_bytes(), config.token.as_bytes()))
        .unwrap_or(false);
    if !authorized {
        log("transport: rejected request (401)");
        return respond(&mut stream, 401, "Unauthorized", b"unauthorized\n");
    }
    let rel = raw_path.trim_start_matches('/');
    let allowed = rel == MANIFEST_FILE
        || rel == CHECKSUMS_FILE
        || rel.starts_with(&format!("{PAYLOAD_DIR}/"));
    if !allowed {
        log("transport: rejected request (400)");
        return respond(&mut stream, 400, "Bad Request", b"bad request\n");
    }
    let rel_path = match safe_relative(rel) {
        Ok(p) => p,
        Err(_) => {
            log("transport: rejected request (400)");
            return respond(&mut stream, 400, "Bad Request", b"bad request\n");
        }
    };
    match fs::read(snapshot_dir.join(rel_path)) {
        Ok(bytes) => {
            log(&format!("transport: served {rel} ({} bytes)", bytes.len()));
            respond(&mut stream, 200, "OK", &bytes)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            respond(&mut stream, 404, "Not Found", b"not found\n")
        }
        Err(e) => Err(TransportError::Io(e)),
    }
}

fn respond(stream: &mut TcpStream, status: u32, text: &str, body: &[u8]) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {text}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
