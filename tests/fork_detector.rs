//! Integration tests for the fork detector, exercising the public API
//! exactly as the snapshot/transport layers will use it.

use avmesh::{compare_manifests, BucketState, Comparison, SnapshotManifest, VaultState};

fn vault(id: &str, revision: u64, hash: &str) -> VaultState {
    VaultState {
        manifest_id: id.into(),
        revision,
        ciphertext_hash: hash.into(),
        buckets: vec![],
    }
}

#[test]
fn equal_revision_different_hash_is_forked() {
    let local = SnapshotManifest::new("node-a-key", 10, vec![vault("v1", 7, "aaa")]);
    let remote = SnapshotManifest::new("node-b-key", 10, vec![vault("v1", 7, "bbb")]);
    assert_eq!(
        compare_manifests(&local, &remote).overall,
        Comparison::Forked
    );
}

#[test]
fn clock_skew_never_creates_a_fork() {
    let local = SnapshotManifest::new("node-a-key", 1, vec![vault("v1", 7, "aaa")]);
    let remote = SnapshotManifest::new("node-b-key", 4_000_000_000, vec![vault("v1", 7, "aaa")]);
    assert_eq!(
        compare_manifests(&local, &remote).overall,
        Comparison::Converged
    );
}

#[test]
fn bucket_level_fork_is_detected() {
    let mut local = SnapshotManifest::new("node-a-key", 1, vec![vault("v1", 7, "aaa")]);
    local.vaults[0].buckets.push(BucketState {
        bucket_id: "vault".into(),
        revision: 7,
        ciphertext_hash: "x".into(),
    });
    let mut remote = SnapshotManifest::new("node-b-key", 1, vec![vault("v1", 7, "aaa")]);
    remote.vaults[0].buckets.push(BucketState {
        bucket_id: "vault".into(),
        revision: 7,
        ciphertext_hash: "y".into(),
    });
    let report = compare_manifests(&local, &remote);
    assert_eq!(report.per_vault["v1"], Comparison::Forked);
    assert!(report.is_forked());
}
