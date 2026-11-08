//! avmesh: whole-instance snapshot replication for AliasVault over a
//! tailnet, implementing ADR 0001 option O3 (single-writer, manual
//! failover). The agent handles ciphertext and metadata only; it never
//! sees a master password or any vault key (ADR decision A).

pub mod cli;
pub mod discovery;
pub mod manifest;
pub mod snapshot;
pub mod transport;

pub use manifest::{
    compare_manifests, BucketState, Comparison, ComparisonReport, SnapshotManifest, VaultState,
    MANIFEST_SCHEMA_VERSION,
};
