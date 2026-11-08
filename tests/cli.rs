//! CLI tests: exact output and exit codes via an injected fake
//! peer source and temp directories only — no tailnet, no real data,
//! no real `tailscale` CLI. Fixture values are synthetic placeholders
//! (node-a/node-b, 100.64.0.x, example.ts.net).

use avmesh::cli::{run, run_with_source, serve_loop, EXIT_OK, EXIT_UNAVAILABLE, EXIT_USAGE};
use avmesh::discovery::{FakePeerSource, PeerRecord, PeerSource, PeerState, MESH_TAG};
use avmesh::snapshot::SnapshotStore;
use avmesh::transport::ServeConfig;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MARKER_SECRET: &str = "MARKER-SECRET-VALUE-7f3a9";
const FULL: &str = include_str!("fixtures/status_full.json");
const NEEDS_LOGIN: &str = include_str!("fixtures/status_needs_login.json");

fn a(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn cli(args: &[String], source: Option<&dyn PeerSource>) -> (i32, String) {
    let mut out: Vec<u8> = Vec::new();
    let code = run_with_source(args, source, &mut out);
    (code, String::from_utf8(out).unwrap())
}

fn flag(args: &mut Vec<String>, name: &str, value: &Path) {
    args.push(name.to_string());
    args.push(value.display().to_string());
}

fn flag_str(args: &mut Vec<String>, name: &str, value: &str) {
    args.push(name.to_string());
    args.push(value.to_string());
}

fn record(key: &str, state: PeerState, dns: Option<&str>, ips: &[&str]) -> PeerRecord {
    PeerRecord {
        public_key: key.into(),
        id: None,
        host_name: None,
        dns_name: dns.map(|d| d.to_string()),
        tailscale_ips: ips.iter().map(|s| s.to_string()).collect(),
        tags: vec![MESH_TAG.into()],
        state,
        last_seen: None,
    }
}

#[cfg(unix)]
#[test]
fn token_init_creates_private_token_file_and_never_prints_it() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("agent-data");
    let args = a(&["token", "init", "--data-dir", data_dir.to_str().unwrap()]);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK, "output: {out}");
    let token_file = data_dir.join("mesh.token");
    let contents = fs::read_to_string(&token_file).unwrap();
    let token = contents.trim();
    assert_eq!(token.len(), 64, "token must be 32 bytes hex");
    assert!(token
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    assert!(!out.contains(token), "cli output leaked the token: {out}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&token_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file mode: {mode:o}");
    }
    let (code2, _) = cli(&args, None);
    assert_eq!(
        code2, EXIT_UNAVAILABLE,
        "second init must refuse to overwrite"
    );
    assert_eq!(fs::read_to_string(&token_file).unwrap(), contents);
}

#[cfg(windows)]
#[test]
fn token_init_refuses_clearly_where_there_is_no_randomness_source() {
    // No OS randomness source is wired up on this platform: the command must fail loudly, say how to
    // provision the token instead, and leave no token file behind.
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("agent-data");
    let args = a(&["token", "init", "--data-dir", data_dir.to_str().unwrap()]);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE, "output: {out}");
    assert!(out.contains("out of band"), "output: {out}");
    assert!(!data_dir.join("mesh.token").exists());
}

#[test]
fn no_args_shows_usage() {
    let mut out: Vec<u8> = Vec::new();
    let code = run(&[], &mut out);
    assert_eq!(code, EXIT_USAGE);
    assert!(String::from_utf8(out).unwrap().contains("usage: avmesh"));
}

#[test]
fn unknown_command_is_usage_error() {
    let (code, out) = cli(&a(&["frobnicate"]), None);
    assert_eq!(code, EXIT_USAGE);
    assert!(out.contains("usage: avmesh"));
}

#[test]
fn version_prints_crate_version() {
    let (code, out) = cli(&a(&["version"]), None);
    assert_eq!(code, EXIT_OK);
    assert!(out.starts_with("avmesh "));
}

#[test]
fn status_reports_each_peer_cause() {
    let source = FakePeerSource::new(vec![
        record(
            "nodekey:bbbb-peer",
            PeerState::Online,
            Some("node-b.example.ts.net."),
            &["100.64.0.2"],
        ),
        record(
            "nodekey:cccc-peer",
            PeerState::Offline,
            Some("node-c.example.ts.net."),
            &["100.64.0.3"],
        ),
        record("nodekey:dddd-peer", PeerState::AclBlocked, None, &[]),
        record("nodekey:eeee-peer", PeerState::Online, None, &[]),
    ]);
    let (code, out) = cli(&a(&["status"]), Some(&source));
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("tailnet: running"));
    assert!(out.contains("cause=ok"));
    assert!(out.contains("cause=offline"));
    assert!(out.contains("cause=acl"));
    assert!(out.contains("cause=endpoint"));
    assert!(out.contains("peers: 4"));
}

#[test]
fn status_json_parses_fixture_and_flags_unapproved() {
    let tmp = tempfile::tempdir().unwrap();
    let status_file = tmp.path().join("status.json");
    fs::write(&status_file, FULL).unwrap();
    let mut args = a(&["status"]);
    flag(&mut args, "--status-json", &status_file);
    flag_str(&mut args, "--allow", "nodekey:bbbb-peer");
    flag_str(&mut args, "--allow", "nodekey:cccc-peer");
    flag_str(&mut args, "--tailnet", "example.ts.net");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("nodekey:bbbb-peer"));
    assert!(out.contains("nodekey:eeee-stranger - cause=identity"));
    assert!(out.contains("peers: 2 unapproved: 1"));
}

#[test]
fn status_json_missing_file_is_tailnet_down() {
    let tmp = tempfile::tempdir().unwrap();
    let mut args = a(&["status"]);
    flag(&mut args, "--status-json", &tmp.path().join("absent.json"));
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE);
    assert!(out.contains("tailnet: down cause=tailnet_down"));
}

#[test]
fn status_json_backend_not_running_is_tailnet_down() {
    let tmp = tempfile::tempdir().unwrap();
    let status_file = tmp.path().join("status.json");
    fs::write(&status_file, NEEDS_LOGIN).unwrap();
    let mut args = a(&["status"]);
    flag(&mut args, "--status-json", &status_file);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE);
    assert!(out.contains("cause=tailnet_down"));
    assert!(out.contains("NeedsLogin"));
}

struct Sources {
    db: PathBuf,
    secrets: PathBuf,
    env_file: PathBuf,
}

fn write_sources(base: &Path) -> Sources {
    let db = base.join("db-export.sql");
    fs::write(&db, "fake database export").unwrap();
    let secrets = base.join("secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(secrets.join("jwt-key.txt"), MARKER_SECRET).unwrap();
    let env_file = base.join("instance.env");
    fs::write(&env_file, "PRIVATE_EMAIL_DOMAINS=example.ts.net\n").unwrap();
    Sources {
        db,
        secrets,
        env_file,
    }
}

fn cli_create(data_dir: &Path, src: &Sources, at: u64, revision: u64) -> PathBuf {
    let mut args = a(&["snapshot", "create"]);
    flag(&mut args, "--data-dir", data_dir);
    flag(&mut args, "--db-export", &src.db);
    flag(&mut args, "--secrets-dir", &src.secrets);
    flag(&mut args, "--env-file", &src.env_file);
    flag_str(&mut args, "--at", &at.to_string());
    flag_str(&mut args, "--revision", &revision.to_string());
    // The manifest hash is bound to the payload: it is the fingerprint
    // of the actual db export, never an invented string.
    let hash = avmesh::snapshot::fingerprint(&fs::read(&src.db).unwrap());
    flag_str(&mut args, "--ciphertext-hash", &hash);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK, "create failed: {out}");
    assert!(out.contains("snapshot created at"));
    assert!(!out.contains(MARKER_SECRET));
    data_dir.join("snapshots").join(format!("snap-{at}"))
}

fn cli_verify(dir: &Path) -> (i32, String) {
    let mut args = a(&["snapshot", "verify"]);
    flag(&mut args, "--snapshot", dir);
    cli(&args, None)
}

fn cli_list(data_dir: &Path) -> (i32, String) {
    let mut args = a(&["snapshot", "list"]);
    flag(&mut args, "--data-dir", data_dir);
    cli(&args, None)
}

fn stage(data_dir: &Path, snapshot_dir: &Path) {
    let store = SnapshotStore::new(data_dir.join("store"));
    let mut log = |_: &str| {};
    store.stage_from(snapshot_dir, &mut log).unwrap();
}

fn cli_promote(data_dir: &Path, confirm: bool) -> (i32, String) {
    let mut args = a(&["promote"]);
    flag(&mut args, "--data-dir", data_dir);
    if confirm {
        args.push("--confirm-primary-down".to_string());
    }
    cli(&args, None)
}

fn current_line(data_dir: &Path) -> String {
    let (code, out) = cli_list(data_dir);
    assert_eq!(code, EXIT_OK);
    out.lines()
        .find(|l| l.starts_with("current:"))
        .unwrap()
        .to_string()
}

#[test]
fn snapshot_create_verify_list_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    let dest = cli_create(&data_dir, &src, 1_700_000_000, 3);
    assert!(dest.is_dir());
    let (code, out) = cli_verify(&dest);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("vault v1 revision 3"));
    assert!(out.contains("snapshot ok"));
    let (code, out) = cli_list(&data_dir);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("current: none"));
    assert!(out.contains("snapshot:"));
    assert!(out.contains("v1@r3"));
}

#[test]
fn snapshot_create_without_ciphertext_hash_computes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    let mut args = a(&["snapshot", "create"]);
    flag(&mut args, "--data-dir", &data_dir);
    flag(&mut args, "--db-export", &src.db);
    flag(&mut args, "--secrets-dir", &src.secrets);
    flag(&mut args, "--env-file", &src.env_file);
    flag_str(&mut args, "--at", "1700000000");
    flag_str(&mut args, "--revision", "3");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK, "create failed: {out}");
    let dest = data_dir.join("snapshots").join("snap-1700000000");
    let manifest_text = fs::read_to_string(dest.join("manifest.json")).unwrap();
    let expected = avmesh::snapshot::fingerprint(b"fake database export");
    assert!(
        manifest_text.contains(&expected),
        "manifest does not carry the computed hash: {manifest_text}"
    );
    assert!(!manifest_text.contains("\"ciphertext_hash\": \"\""));
    let (code, out) = cli_verify(&dest);
    assert_eq!(code, EXIT_OK, "verify failed: {out}");
}

#[test]
fn snapshot_create_requires_source_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let mut args = a(&["snapshot", "create"]);
    flag(&mut args, "--data-dir", &tmp.path().join("data"));
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_USAGE);
    assert!(out.contains("--db-export"));
}

#[test]
fn snapshot_verify_rejects_empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = cli_verify(tmp.path());
    assert_eq!(code, EXIT_UNAVAILABLE);
    assert!(out.contains("error:"));
}

#[test]
fn promote_requires_confirmation_then_promotes_staged() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    let snap = cli_create(&data_dir, &src, 1_700_000_000, 3);
    stage(&data_dir, &snap);
    let (code, out) = cli_promote(&data_dir, false);
    assert_eq!(code, EXIT_USAGE);
    assert!(out.contains("--confirm-primary-down"));
    let (code, out) = cli_promote(&data_dir, true);
    assert_eq!(code, EXIT_OK, "promote failed: {out}");
    assert!(out.contains("promoted: vaults=v1@r3"));
    assert!(current_line(&data_dir).contains("v1@r3"));
}

#[test]
fn promote_without_staging_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = cli_promote(&tmp.path().join("data"), true);
    assert_eq!(code, EXIT_UNAVAILABLE);
    assert!(out.contains("error:"));
}

#[test]
fn demote_swaps_current_and_previous_and_fails_without_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    let mut args = a(&["demote"]);
    flag(&mut args, "--data-dir", &data_dir);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE);
    assert!(out.contains("nothing to demote"));
    let snap3 = cli_create(&data_dir, &src, 1_700_000_000, 3);
    stage(&data_dir, &snap3);
    assert_eq!(cli_promote(&data_dir, true).0, EXIT_OK);
    let snap4 = cli_create(&data_dir, &src, 1_700_000_100, 4);
    stage(&data_dir, &snap4);
    assert_eq!(cli_promote(&data_dir, true).0, EXIT_OK);
    assert!(current_line(&data_dir).contains("v1@r4"));
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("demoted:"));
    assert!(current_line(&data_dir).contains("v1@r3"));
    let (code, _) = cli(&args, None);
    assert_eq!(code, EXIT_OK);
    assert!(current_line(&data_dir).contains("v1@r4"));
}

#[test]
fn promote_forked_staged_snapshot_halts() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    let snap_a = cli_create(&data_dir, &src, 1_700_000_000, 3);
    stage(&data_dir, &snap_a);
    assert_eq!(cli_promote(&data_dir, true).0, EXIT_OK);
    // Same revision, different vault ciphertext: a genuinely different
    // db export, so its computed ciphertext_hash differs (a fork).
    let base_b = tmp.path().join("src-b");
    fs::create_dir_all(&base_b).unwrap();
    let src_b = write_sources(&base_b);
    fs::write(&src_b.db, "fake database export (forked)").unwrap();
    let snap_b = cli_create(&data_dir, &src_b, 1_700_000_100, 3);
    stage(&data_dir, &snap_b);
    let (code, out) = cli_promote(&data_dir, true);
    assert_eq!(code, EXIT_UNAVAILABLE);
    assert!(out.contains("forked"));
    assert!(current_line(&data_dir).contains("v1@r3"));
}

#[test]
fn snapshot_create_excludes_node_local_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    fs::write(src.secrets.join("postgres_password"), "node-local-pg-pw").unwrap();
    fs::write(src.secrets.join("jwt_key"), "jwt-signing-material").unwrap();
    fs::write(src.secrets.join("local_only.txt"), "stay-here").unwrap();
    let data_dir = tmp.path().join("data");
    // Default: postgres_password is node-local (ADR decision C) and is
    // never replicated; jwt_key is replicated.
    let dest = cli_create(&data_dir, &src, 1_700_000_000, 3);
    assert!(dest.join("payload/secrets/jwt_key").is_file());
    assert!(!dest.join("payload/secrets/postgres_password").exists());
    // Configured extra: --node-local-secret adds to the default set.
    let mut args = a(&["snapshot", "create"]);
    flag(&mut args, "--data-dir", &data_dir);
    flag(&mut args, "--db-export", &src.db);
    flag(&mut args, "--secrets-dir", &src.secrets);
    flag(&mut args, "--env-file", &src.env_file);
    flag_str(&mut args, "--at", "1700000500");
    flag_str(&mut args, "--revision", "3");
    let hash = avmesh::snapshot::fingerprint(&fs::read(&src.db).unwrap());
    flag_str(&mut args, "--ciphertext-hash", &hash);
    flag_str(&mut args, "--node-local-secret", "local_only.txt");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK, "create failed: {out}");
    let dest2 = data_dir.join("snapshots").join("snap-1700000500");
    assert!(!dest2.join("payload/secrets/local_only.txt").exists());
    assert!(!dest2.join("payload/secrets/postgres_password").exists());
    assert!(dest2.join("payload/secrets/jwt_key").is_file());
}

#[test]
fn snapshot_create_rejects_node_local_secret_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    let mut args = a(&["snapshot", "create"]);
    flag(&mut args, "--data-dir", &data_dir);
    flag(&mut args, "--db-export", &src.db);
    flag(&mut args, "--secrets-dir", &src.secrets);
    flag(&mut args, "--env-file", &src.env_file);
    flag_str(&mut args, "--node-local-secret", "../evil");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_USAGE);
    assert!(out.contains("--node-local-secret"), "output: {out}");
}

fn serve_current(data_dir: &Path, src: &Sources) {
    let snap = cli_create(data_dir, src, 1_700_000_000, 3);
    let store = SnapshotStore::new(data_dir);
    let mut log = |_: &str| {};
    store.stage_from(&snap, &mut log).unwrap();
    store.promote(&mut log).unwrap();
}

/// A fixed test token written straight to the data directory, so these tests do not depend on the
/// platform's randomness source (`token init` is unix-only; see README, Platform support).
fn serve_token(data_dir: &Path) -> String {
    let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
    fs::create_dir_all(data_dir).unwrap();
    fs::write(data_dir.join("mesh.token"), format!("{token}\n")).unwrap();
    token
}

#[test]
fn serve_rejects_non_loopback_listen() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let mut args = a(&["serve"]);
    flag(&mut args, "--data-dir", &data_dir);
    flag_str(&mut args, "--listen", "0.0.0.0:18080");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_USAGE, "output: {out}");
    assert!(out.contains("--listen"), "output: {out}");
}

#[test]
fn serve_missing_token_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    serve_current(&data_dir, &src);
    let mut args = a(&["serve"]);
    flag(&mut args, "--data-dir", &data_dir);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE, "output: {out}");
    assert!(out.contains("error:"), "output: {out}");
}

#[test]
fn serve_without_current_snapshot_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let token = serve_token(&data_dir);
    let mut args = a(&["serve"]);
    flag(&mut args, "--data-dir", &data_dir);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE, "output: {out}");
    assert!(out.contains("error:"), "output: {out}");
    assert!(!out.contains(&token), "output leaked the token: {out}");
}

#[test]
fn serve_allow_mismatch_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let src = write_sources(tmp.path());
    let data_dir = tmp.path().join("data");
    serve_current(&data_dir, &src);
    let token = serve_token(&data_dir);
    let mut args = a(&["serve"]);
    flag(&mut args, "--data-dir", &data_dir);
    flag_str(&mut args, "--allow", "nodekey:other-node");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_UNAVAILABLE, "output: {out}");
    assert!(out.contains("error:"), "output: {out}");
    assert!(!out.contains(&token), "output leaked the token: {out}");
}

#[test]
fn serve_loop_stops_when_flag_is_set() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = ServeConfig::new("test-token", "127.0.0.1");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_in_thread = Arc::clone(&stop);
    let dir = tmp.path().to_path_buf();
    let handle = std::thread::spawn(move || {
        serve_loop(listener, &dir, &config, &stop_in_thread, &mut |_| {});
    });
    std::thread::sleep(Duration::from_millis(50));
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap();
}

#[test]
fn pull_fetches_a_served_snapshot_and_refuses_a_wrong_token() {
    let tmp = tempfile::tempdir().unwrap();
    let a_home = tmp.path().join("a");
    let src = write_sources(tmp.path());
    let snap = cli_create(&a_home, &src, 1_700_000_000, 3);
    stage(&a_home, &snap);
    let (code, out) = cli_promote(&a_home, true);
    assert_eq!(code, EXIT_OK, "{out}");
    let a_store = SnapshotStore::new(a_home.join("store"));
    let node_id = a_store.current_manifest(&mut |_: &str| {}).unwrap().node_id;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let current = a_store.current_dir();
    let config = ServeConfig::new("pull-test-token", "127.0.0.1");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_in_thread = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        serve_loop(listener, &current, &config, &stop_in_thread, &mut |_| {});
    });

    let from = format!("http://127.0.0.1:{port}");
    let good = tmp.path().join("good.token");
    fs::write(&good, "pull-test-token\n").unwrap();
    let b_store = tmp.path().join("b");
    let mut args = a(&["pull"]);
    flag(&mut args, "--data-dir", &b_store);
    flag_str(&mut args, "--from", &from);
    flag_str(&mut args, "--allow", &node_id);
    flag(&mut args, "--token-file", &good);
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_OK, "pull failed: {out}");
    assert!(out.contains("pulled snapshot from node"), "{out}");
    assert!(
        !out.contains("pull-test-token"),
        "output leaked the token: {out}"
    );
    let (code, out) = cli_verify(&b_store.join("staging"));
    assert_eq!(code, EXIT_OK, "staged snapshot does not verify: {out}");

    let bad = tmp.path().join("bad.token");
    fs::write(&bad, "not-the-token\n").unwrap();
    let b_wrong = tmp.path().join("b-wrong");
    let mut args = a(&["pull"]);
    flag(&mut args, "--data-dir", &b_wrong);
    flag_str(&mut args, "--from", &from);
    flag_str(&mut args, "--allow", &node_id);
    flag(&mut args, "--token-file", &bad);
    let (code, out) = cli(&args, None);
    assert_eq!(
        code, EXIT_UNAVAILABLE,
        "a wrong token must be refused: {out}"
    );
    assert!(out.contains("401"), "{out}");
    assert!(!b_wrong.join("staging").exists());

    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
}

#[test]
fn pull_needs_from_and_exactly_one_allow() {
    let tmp = tempfile::tempdir().unwrap();
    let mut args = a(&["pull"]);
    flag(&mut args, "--data-dir", tmp.path());
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_USAGE, "{out}");
    flag_str(&mut args, "--from", "http://127.0.0.1:1");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_USAGE, "{out}");
    assert!(out.contains("--allow"), "{out}");
    // A non-loopback source is refused before any token is read or sent.
    let mut args = a(&["pull"]);
    flag(&mut args, "--data-dir", tmp.path());
    flag_str(&mut args, "--from", "http://192.168.1.10:18080");
    flag_str(&mut args, "--allow", "node-a");
    let (code, out) = cli(&args, None);
    assert_eq!(code, EXIT_USAGE, "{out}");
    assert!(out.contains("not loopback"), "{out}");
}
