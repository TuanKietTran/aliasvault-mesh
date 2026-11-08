//! Filesystem tests for snapshot packaging (T2): temp dirs only, fake
//! data only, no tailnet, no real vault data.

use avmesh::manifest::{SnapshotManifest, VaultState};
use avmesh::snapshot::{
    create_snapshot, create_snapshot_with_node_local, fingerprint, verify_snapshot, SnapshotError,
    SnapshotSources, SnapshotStore, DEFAULT_NODE_LOCAL_SECRETS,
};
use std::fs;
use std::path::PathBuf;

const MARKER_SECRET: &str = "MARKER-SECRET-VALUE-7f3a9";

struct Fixture {
    _tmp: tempfile::TempDir,
    sources: SnapshotSources,
    manifest: SnapshotManifest,
}

fn fixture(revision: u64) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let db = base.join("db-export.sql");
    fs::write(&db, format!("fake database export revision {revision}")).unwrap();
    let secrets = base.join("secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(secrets.join("jwt-key.txt"), MARKER_SECRET).unwrap();
    let env_file = base.join("instance.env");
    fs::write(&env_file, "PRIVATE_EMAIL_DOMAINS=example.ts.net\n").unwrap();
    let hash = fingerprint(format!("fake database export revision {revision}").as_bytes());
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
    Fixture {
        _tmp: tmp,
        sources: SnapshotSources {
            database_export: db,
            secrets_dir: secrets,
            environment_file: env_file,
        },
        manifest,
    }
}

fn make_snapshot(fx: &Fixture, dest: PathBuf) -> Vec<String> {
    let lines = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let sink = lines.clone();
    let mut log = move |line: &str| sink.borrow_mut().push(line.to_string());
    create_snapshot(&fx.sources, &fx.manifest, &dest, &mut log).unwrap();
    let collected = lines.borrow().clone();
    collected
}

#[test]
fn create_and_verify_roundtrip() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    let mut log = |_: &str| {};
    let manifest = verify_snapshot(&dest, &mut log).unwrap();
    assert_eq!(manifest, fx.manifest);
}

#[test]
fn corrupted_file_fails_verify() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    let target = dest.join("payload/database/db-export.sql");
    let mut data = fs::read(&target).unwrap();
    data[0] ^= 0xff;
    fs::write(&target, data).unwrap();
    let mut log = |_: &str| {};
    assert!(matches!(
        verify_snapshot(&dest, &mut log),
        Err(SnapshotError::ChecksumMismatch { .. })
    ));
}

#[test]
fn tampered_db_with_repaired_checksums_fails_ciphertext_verify() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    // Tamper with the packaged db export WITHOUT touching the manifest,
    // and repair checksums.sha256 so the checksum layer passes: only the
    // manifest ciphertext_hash binding can catch this.
    let target = dest.join("payload/database/db-export.sql");
    let tampered: &[u8] = b"fake database export revision 999";
    fs::write(&target, tampered).unwrap();
    let checksums_path = dest.join("checksums.sha256");
    let text = fs::read_to_string(&checksums_path).unwrap();
    let mut repaired = String::new();
    for line in text.lines() {
        if line.ends_with("payload/database/db-export.sql") {
            repaired.push_str(&format!(
                "{}  payload/database/db-export.sql\n",
                fingerprint(tampered)
            ));
        } else {
            repaired.push_str(line);
            repaired.push('\n');
        }
    }
    fs::write(&checksums_path, repaired).unwrap();
    let mut log = |_: &str| {};
    let err = verify_snapshot(&dest, &mut log).unwrap_err();
    assert!(
        format!("{err}").contains("ciphertext"),
        "unexpected error: {err}"
    );
}

#[test]
fn empty_ciphertext_hash_fails_verify() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    // Checksums do not cover manifest.json, so blanking the hash there
    // leaves the checksum layer intact: verify itself must reject it.
    let manifest_path = dest.join("manifest.json");
    let text = fs::read_to_string(&manifest_path).unwrap();
    let hash = fx.manifest.vaults[0].ciphertext_hash.clone();
    assert!(!hash.is_empty());
    fs::write(&manifest_path, text.replace(&hash, "")).unwrap();
    let mut log = |_: &str| {};
    let err = verify_snapshot(&dest, &mut log).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("ciphertext"), "unexpected error: {err}");
    assert!(msg.contains("empty"), "unexpected error: {err}");
}

#[test]
fn truncated_file_fails_verify() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    let target = dest.join("payload/environment/instance.env");
    let data = fs::read(&target).unwrap();
    fs::write(&target, &data[..3]).unwrap();
    let mut log = |_: &str| {};
    assert!(verify_snapshot(&dest, &mut log).is_err());
}

#[test]
fn missing_manifest_fails_verify() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    fs::remove_file(dest.join("manifest.json")).unwrap();
    let mut log = |_: &str| {};
    assert!(matches!(
        verify_snapshot(&dest, &mut log),
        Err(SnapshotError::MissingManifest(_))
    ));
}

#[test]
fn partial_staging_is_never_promoted() {
    let fx = fixture(3);
    let snap = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, snap.clone());
    let store = SnapshotStore::new(fx._tmp.path().join("store"));
    // Simulate an interrupted transfer: only part of the snapshot
    // reaches staging.
    fs::create_dir_all(store.staging_dir().join("payload")).unwrap();
    fs::copy(
        snap.join("manifest.json"),
        store.staging_dir().join("manifest.json"),
    )
    .unwrap();
    let mut log = |_: &str| {};
    assert!(store.promote(&mut log).is_err());
    assert!(!store.current_dir().exists());
    // A complete stage afterwards still works.
    let mut log2 = |_: &str| {};
    store.stage_from(&snap, &mut log2).unwrap();
    store.promote(&mut log2).unwrap();
    assert!(store.current_dir().is_dir());
}

#[test]
fn promotion_keeps_previous_snapshot() {
    let fx1 = fixture(1);
    let fx2 = fixture(2);
    let store = SnapshotStore::new(fx1._tmp.path().join("store"));
    let snap1 = fx1._tmp.path().join("snap1");
    let _ = make_snapshot(&fx1, snap1.clone());
    let mut log = |_: &str| {};
    store.stage_from(&snap1, &mut log).unwrap();
    store.promote(&mut log).unwrap();
    let snap2 = fx2._tmp.path().join("snap2");
    let _ = make_snapshot(&fx2, snap2.clone());
    store.stage_from(&snap2, &mut log).unwrap();
    let promoted = store.promote(&mut log).unwrap();
    assert_eq!(promoted.vaults[0].revision, 2);
    let previous = verify_snapshot(&store.previous_dir(), &mut log).unwrap();
    assert_eq!(previous.vaults[0].revision, 1);
}

#[test]
fn permissions_are_owner_only() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        let secret_mode = fs::metadata(dest.join("payload/secrets/jwt-key.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(secret_mode, 0o600);
        let manifest_mode = fs::metadata(dest.join("manifest.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(manifest_mode, 0o600);
    }
}

#[test]
fn secret_value_never_appears_in_logs() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let lines = make_snapshot(&fx, dest.clone());
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(
            !line.contains(MARKER_SECRET),
            "log line leaked a secret value: {line}"
        );
    }
    let store = SnapshotStore::new(fx._tmp.path().join("store"));
    let lines2 = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let sink = lines2.clone();
    let mut log = move |line: &str| sink.borrow_mut().push(line.to_string());
    store.stage_from(&dest, &mut log).unwrap();
    store.promote(&mut log).unwrap();
    for line in lines2.borrow().iter() {
        assert!(
            !line.contains(MARKER_SECRET),
            "log line leaked a secret value: {line}"
        );
    }
}

#[test]
fn node_local_postgres_password_is_never_replicated() {
    assert_eq!(DEFAULT_NODE_LOCAL_SECRETS, ["postgres_password"]);
    let fx = fixture(3);
    fs::write(
        fx.sources.secrets_dir.join("postgres_password"),
        "node-local-pg-pw",
    )
    .unwrap();
    fs::write(
        fx.sources.secrets_dir.join("jwt_key"),
        "jwt-signing-material",
    )
    .unwrap();
    fs::write(
        fx.sources.secrets_dir.join("data_protection_cert_pass"),
        "dp-pass",
    )
    .unwrap();
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    assert!(
        dest.join("payload/secrets/jwt_key").is_file(),
        "jwt_key must be replicated (ADR decision C)"
    );
    assert!(dest
        .join("payload/secrets/data_protection_cert_pass")
        .is_file());
    assert!(
        !dest.join("payload/secrets/postgres_password").exists(),
        "postgres_password is node-local and must not be replicated"
    );
    let checksums = fs::read_to_string(dest.join("checksums.sha256")).unwrap();
    assert!(!checksums.contains("postgres_password"));
    let mut log = |_: &str| {};
    verify_snapshot(&dest, &mut log).unwrap();
}

#[test]
fn configured_extra_node_local_secret_is_never_replicated() {
    let fx = fixture(3);
    fs::write(
        fx.sources.secrets_dir.join("postgres_password"),
        "node-local-pg-pw",
    )
    .unwrap();
    fs::write(fx.sources.secrets_dir.join("local_only.txt"), "stay-here").unwrap();
    let dest = fx._tmp.path().join("snap");
    let mut log = |_: &str| {};
    create_snapshot_with_node_local(
        &fx.sources,
        &fx.manifest,
        &dest,
        &mut log,
        &["local_only.txt"],
    )
    .unwrap();
    assert!(!dest.join("payload/secrets/local_only.txt").exists());
    assert!(
        !dest.join("payload/secrets/postgres_password").exists(),
        "the default node-local set must still apply when extras are configured"
    );
    assert!(dest.join("payload/secrets/jwt-key.txt").is_file());
    let mut log = |_: &str| {};
    verify_snapshot(&dest, &mut log).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_payload_file_fails_verify_and_stage() {
    use std::os::unix::fs::symlink;
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    // Control: the regular snapshot stages fine.
    let store = SnapshotStore::new(fx._tmp.path().join("store"));
    let mut log = |_: &str| {};
    store.stage_from(&dest, &mut log).unwrap();
    // Replace a payload file with a symlink to a file outside the snapshot.
    let outside = fx._tmp.path().join("outside-secret.txt");
    fs::write(&outside, "outside-content").unwrap();
    let link = dest.join("payload/secrets/jwt-key.txt");
    fs::remove_file(&link).unwrap();
    symlink(&outside, &link).unwrap();
    let mut log = |_: &str| {};
    let err = verify_snapshot(&dest, &mut log).unwrap_err();
    assert!(
        matches!(err, SnapshotError::Symlink(ref p) if p == "payload/secrets/jwt-key.txt"),
        "unexpected error: {err}"
    );
    let store2 = SnapshotStore::new(fx._tmp.path().join("store2"));
    let mut log = |_: &str| {};
    let err = store2.stage_from(&dest, &mut log).unwrap_err();
    assert!(
        matches!(err, SnapshotError::Symlink(ref p) if p == "payload/secrets/jwt-key.txt"),
        "unexpected error: {err}"
    );
    // Nothing was copied: outside content is never materialised.
    assert!(!store2.staging_dir().exists());
}

#[cfg(unix)]
#[test]
fn symlinked_payload_dir_fails_verify() {
    use std::os::unix::fs::symlink;
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    let outside_dir = fx._tmp.path().join("outside-secrets");
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("jwt-key.txt"), "outside-content").unwrap();
    let link = dest.join("payload/secrets");
    fs::remove_dir_all(&link).unwrap();
    symlink(&outside_dir, &link).unwrap();
    let mut log = |_: &str| {};
    let err = verify_snapshot(&dest, &mut log).unwrap_err();
    assert!(
        matches!(err, SnapshotError::Symlink(ref p) if p == "payload/secrets"),
        "unexpected error: {err}"
    );
}

#[test]
fn checksums_traversal_entry_fails_verify_even_with_matching_hash() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    let outside = fx._tmp.path().join("outside.txt");
    fs::write(&outside, "outside-content").unwrap();
    let hash = fingerprint(b"outside-content");
    let checksums = dest.join("checksums.sha256");
    let mut text = fs::read_to_string(&checksums).unwrap();
    text.push_str(&format!("{hash}  ../outside.txt\n"));
    fs::write(&checksums, text).unwrap();
    let mut log = |_: &str| {};
    let err = verify_snapshot(&dest, &mut log).unwrap_err();
    assert!(
        matches!(err, SnapshotError::InvalidChecksums(ref m) if m.contains("../outside.txt")),
        "unexpected error: {err}"
    );
}

#[test]
fn checksums_absolute_entry_fails_verify_even_with_matching_hash() {
    let fx = fixture(3);
    let dest = fx._tmp.path().join("snap");
    let _ = make_snapshot(&fx, dest.clone());
    let outside = fx._tmp.path().join("outside.txt");
    fs::write(&outside, "outside-content").unwrap();
    let abs = outside.to_string_lossy().into_owned();
    let hash = fingerprint(b"outside-content");
    let checksums = dest.join("checksums.sha256");
    let mut text = fs::read_to_string(&checksums).unwrap();
    text.push_str(&format!("{hash}  {abs}\n"));
    fs::write(&checksums, text).unwrap();
    let mut log = |_: &str| {};
    let err = verify_snapshot(&dest, &mut log).unwrap_err();
    assert!(
        matches!(err, SnapshotError::InvalidChecksums(ref m) if m.contains(&abs)),
        "unexpected error: {err}"
    );
}

#[test]
fn checksums_unsafe_entry_variants_fail_verify() {
    for bad in [
        "payload//database/db-export.sql",
        "payload/./database/db-export.sql",
        "payload\\database\\db-export.sql",
        "payload/database/db-export.sql/",
        "payload/ba\u{0}d.txt",
        "..",
        ".",
    ] {
        let fx = fixture(3);
        let dest = fx._tmp.path().join("snap");
        let _ = make_snapshot(&fx, dest.clone());
        let checksums = dest.join("checksums.sha256");
        let mut text = fs::read_to_string(&checksums).unwrap();
        text.push_str(&format!("{}  {bad}\n", "0".repeat(64)));
        fs::write(&checksums, text).unwrap();
        let mut log = |_: &str| {};
        assert!(
            matches!(
                verify_snapshot(&dest, &mut log),
                Err(SnapshotError::InvalidChecksums(_))
            ),
            "entry {bad:?} must be rejected"
        );
    }
}
