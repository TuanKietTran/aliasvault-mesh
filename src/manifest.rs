//! Snapshot manifest type and the fork detector (ADR 0001 decision B).
//!
//! A manifest lists, per vault manifest and per bucket, the revision
//! number and ciphertext hash reported by the upstream status surface
//! (research S3/S4). It contains no ciphertext and no secrets
//! (amendment A3). The wall-clock field is advisory only: fork
//! detection compares revisions and hashes, never timestamps, so clock
//! skew between nodes cannot create or hide a fork at this layer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// One bucket of a vault: a category of vault data with its own
/// revision counter and ciphertext hash on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketState {
    pub bucket_id: String,
    pub revision: u64,
    pub ciphertext_hash: String,
}

/// One vault as seen by one node: its manifest revision/hash plus the
/// state of each of its buckets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultState {
    pub manifest_id: String,
    pub revision: u64,
    pub ciphertext_hash: String,
    #[serde(default)]
    pub buckets: Vec<BucketState>,
}

impl VaultState {
    pub fn bucket(&self, bucket_id: &str) -> Option<&BucketState> {
        self.buckets.iter().find(|b| b.bucket_id == bucket_id)
    }
}

/// The snapshot manifest exchanged between peers and stored with every
/// snapshot. `node_id` is the producing node's public key (tailnet.md
/// section 2: the key is the identity; names and addresses are only
/// locators). `created_at_unix` is advisory metadata for operators and
/// is never an input to fork detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub node_id: String,
    pub created_at_unix: u64,
    #[serde(default)]
    pub vaults: Vec<VaultState>,
}

impl SnapshotManifest {
    pub fn new(node_id: impl Into<String>, created_at_unix: u64, vaults: Vec<VaultState>) -> Self {
        SnapshotManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            node_id: node_id.into(),
            created_at_unix,
            vaults,
        }
    }

    pub fn vault(&self, manifest_id: &str) -> Option<&VaultState> {
        self.vaults.iter().find(|v| v.manifest_id == manifest_id)
    }
}

/// Outcome of comparing two states of the same data, from the local
/// node's point of view. Used per entry, per vault, and overall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// Same revisions and same hashes: the two states agree.
    Converged,
    /// The remote state is strictly newer (or holds vaults/buckets the
    /// local state lacks) and nothing local is newer: safe to replicate
    /// remote -> local.
    Behind,
    /// The local state is strictly newer (or holds vaults/buckets the
    /// remote state lacks) and nothing remote is newer.
    Ahead,
    /// The states diverged: equal revision with different hash (a
    /// revision fork, ADR decision B), or some entries newer on each
    /// side. A fork is a stop condition, never something to retry
    /// through or auto-merge at this layer.
    Forked,
}

/// Full result of comparing a local manifest against a remote one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub overall: Comparison,
    pub per_vault: BTreeMap<String, Comparison>,
}

impl ComparisonReport {
    pub fn is_forked(&self) -> bool {
        self.overall == Comparison::Forked
    }
}

fn combine(a: Comparison, b: Comparison) -> Comparison {
    use Comparison::{Ahead, Behind, Converged, Forked};
    match (a, b) {
        (Forked, _) | (_, Forked) => Forked,
        (Converged, x) => x,
        (x, Converged) => x,
        (Behind, Behind) => Behind,
        (Ahead, Ahead) => Ahead,
        (Behind, Ahead) | (Ahead, Behind) => Forked,
    }
}

/// Compare one (revision, hash) entry. `None` means the entry is absent
/// on that side, which reads as older than any present entry.
fn compare_entry(local: Option<(u64, &str)>, remote: Option<(u64, &str)>) -> Comparison {
    use Comparison::{Ahead, Behind, Converged, Forked};
    match (local, remote) {
        (None, None) => Converged,
        (Some(_), None) => Ahead,
        (None, Some(_)) => Behind,
        (Some((lr, lh)), Some((rr, rh))) => {
            if lr == rr {
                if lh == rh {
                    Converged
                } else {
                    Forked
                }
            } else if lr < rr {
                Behind
            } else {
                Ahead
            }
        }
    }
}

fn compare_vault(local: &VaultState, remote: &VaultState) -> Comparison {
    let mut acc = compare_entry(
        Some((local.revision, local.ciphertext_hash.as_str())),
        Some((remote.revision, remote.ciphertext_hash.as_str())),
    );
    let mut bucket_ids: Vec<&str> = local.buckets.iter().map(|b| b.bucket_id.as_str()).collect();
    for b in &remote.buckets {
        if !bucket_ids.contains(&b.bucket_id.as_str()) {
            bucket_ids.push(b.bucket_id.as_str());
        }
    }
    for id in bucket_ids {
        let l = local
            .bucket(id)
            .map(|b| (b.revision, b.ciphertext_hash.as_str()));
        let r = remote
            .bucket(id)
            .map(|b| (b.revision, b.ciphertext_hash.as_str()));
        acc = combine(acc, compare_entry(l, r));
    }
    acc
}

/// Compare a local manifest with a remote peer's manifest.
///
/// Decision B, exactly: same manifest id, same revision, different
/// ciphertext hash is a revision fork. Revisions and hashes decide;
/// `created_at_unix` on either side is ignored, so a skewed clock can
/// neither create nor mask a fork.
pub fn compare_manifests(local: &SnapshotManifest, remote: &SnapshotManifest) -> ComparisonReport {
    let mut per_vault: BTreeMap<String, Comparison> = BTreeMap::new();
    let mut ids: Vec<&str> = local
        .vaults
        .iter()
        .map(|v| v.manifest_id.as_str())
        .collect();
    for v in &remote.vaults {
        if !ids.contains(&v.manifest_id.as_str()) {
            ids.push(v.manifest_id.as_str());
        }
    }
    let mut overall = Comparison::Converged;
    for id in ids {
        let outcome = match (local.vault(id), remote.vault(id)) {
            (Some(l), Some(r)) => compare_vault(l, r),
            (Some(_), None) => Comparison::Ahead,
            (None, Some(_)) => Comparison::Behind,
            (None, None) => Comparison::Converged,
        };
        overall = combine(overall, outcome);
        per_vault.insert(id.to_string(), outcome);
    }
    ComparisonReport { overall, per_vault }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(id: &str, revision: u64, hash: &str) -> BucketState {
        BucketState {
            bucket_id: id.into(),
            revision,
            ciphertext_hash: hash.into(),
        }
    }
    fn vault(id: &str, revision: u64, hash: &str, buckets: Vec<BucketState>) -> VaultState {
        VaultState {
            manifest_id: id.into(),
            revision,
            ciphertext_hash: hash.into(),
            buckets,
        }
    }
    fn manifest(node: &str, at: u64, vaults: Vec<VaultState>) -> SnapshotManifest {
        SnapshotManifest::new(node, at, vaults)
    }

    #[test]
    fn manifest_json_roundtrip() {
        let m = manifest(
            "node-a-key",
            100,
            vec![vault("v1", 3, "h3", vec![bucket("vault", 3, "bh3")])],
        );
        let json = serde_json::to_string(&m).unwrap();
        let back: SnapshotManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.schema_version, MANIFEST_SCHEMA_VERSION);
    }

    #[test]
    fn fork_detector_table() {
        let v = |rev: u64, hash: &str| vault("v1", rev, hash, vec![]);
        let cases: Vec<(&str, SnapshotManifest, SnapshotManifest, Comparison)> = vec![
            (
                "identical",
                manifest("a", 100, vec![v(3, "h3")]),
                manifest("b", 100, vec![v(3, "h3")]),
                Comparison::Converged,
            ),
            (
                "clock skew only",
                manifest("a", 100, vec![v(3, "h3")]),
                manifest("b", 999_999, vec![v(3, "h3")]),
                Comparison::Converged,
            ),
            (
                "both empty",
                manifest("a", 1, vec![]),
                manifest("b", 2, vec![]),
                Comparison::Converged,
            ),
            (
                "remote newer",
                manifest("a", 1, vec![v(3, "h3")]),
                manifest("b", 1, vec![v(4, "h4")]),
                Comparison::Behind,
            ),
            (
                "local newer",
                manifest("a", 1, vec![v(5, "h5")]),
                manifest("b", 1, vec![v(4, "h4")]),
                Comparison::Ahead,
            ),
            (
                "same revision different hash is a fork",
                manifest("a", 1, vec![v(3, "h3")]),
                manifest("b", 1, vec![v(3, "hX")]),
                Comparison::Forked,
            ),
            (
                "vault missing locally",
                manifest("a", 1, vec![]),
                manifest("b", 1, vec![v(1, "h1")]),
                Comparison::Behind,
            ),
            (
                "vault missing remotely",
                manifest("a", 1, vec![v(1, "h1")]),
                manifest("b", 1, vec![]),
                Comparison::Ahead,
            ),
            (
                "bucket behind",
                manifest(
                    "a",
                    1,
                    vec![vault("v1", 3, "h3", vec![bucket("vault", 3, "b3")])],
                ),
                manifest(
                    "b",
                    1,
                    vec![vault("v1", 3, "h3", vec![bucket("vault", 4, "b4")])],
                ),
                Comparison::Behind,
            ),
            (
                "bucket fork at equal revision",
                manifest(
                    "a",
                    1,
                    vec![vault("v1", 3, "h3", vec![bucket("vault", 3, "b3")])],
                ),
                manifest(
                    "b",
                    1,
                    vec![vault("v1", 3, "h3", vec![bucket("vault", 3, "bX")])],
                ),
                Comparison::Forked,
            ),
            (
                "bucket missing locally",
                manifest("a", 1, vec![vault("v1", 3, "h3", vec![])]),
                manifest(
                    "b",
                    1,
                    vec![vault("v1", 3, "h3", vec![bucket("vault", 1, "b1")])],
                ),
                Comparison::Behind,
            ),
            (
                "multi-vault one behind rest converged",
                manifest("a", 1, vec![v(3, "h3"), vault("v2", 1, "g1", vec![])]),
                manifest("b", 1, vec![v(4, "h4"), vault("v2", 1, "g1", vec![])]),
                Comparison::Behind,
            ),
            (
                "diverged across vaults is forked",
                manifest("a", 1, vec![v(5, "h5"), vault("v2", 1, "g1", vec![])]),
                manifest("b", 1, vec![v(4, "h4"), vault("v2", 2, "g2", vec![])]),
                Comparison::Forked,
            ),
            (
                "one vault forked poisons overall",
                manifest("a", 1, vec![v(3, "h3"), vault("v2", 1, "g1", vec![])]),
                manifest("b", 1, vec![v(3, "hX"), vault("v2", 2, "g2", vec![])]),
                Comparison::Forked,
            ),
        ];
        for (name, local, remote, expected) in &cases {
            let report = compare_manifests(local, remote);
            assert_eq!(report.overall, *expected, "case: {name}");
            assert_eq!(
                report.is_forked(),
                *expected == Comparison::Forked,
                "case: {name}"
            );
        }
    }

    #[test]
    fn per_vault_outcomes_are_reported() {
        let local = manifest(
            "a",
            1,
            vec![vault("v1", 5, "h5", vec![]), vault("v2", 1, "g1", vec![])],
        );
        let remote = manifest(
            "b",
            1,
            vec![vault("v1", 4, "h4", vec![]), vault("v2", 1, "g1", vec![])],
        );
        let report = compare_manifests(&local, &remote);
        assert_eq!(report.per_vault["v1"], Comparison::Ahead);
        assert_eq!(report.per_vault["v2"], Comparison::Converged);
        assert_eq!(report.overall, Comparison::Ahead);
    }
}
