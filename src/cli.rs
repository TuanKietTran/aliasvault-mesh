//! The `avmesh` command line.
//!
//! Commands: `status`, `snapshot create|verify|list`, `promote`,
//! `demote`. The module is written against the `PeerSource` trait and
//! an output sink so tests can assert the exact CLI output with a fake
//! peer source and temp directories — no tailnet, no real data.
//!
//! `status` keeps the tailnet failure causes of tailnet.md section 6
//! distinct: `tailnet_down` is reported separately from any peer
//! state, and each peer carries one cause: `ok`, `offline`, `acl`,
//! `endpoint` (online but no dial target: a Serve/endpoint
//! configuration problem, not a down peer) or `identity` (a tagged
//! peer whose key is not allowlisted: a new, unapproved node).

use crate::discovery::{
    parse_tailscale_status, DiscoveryError, PeerRecord, PeerSource, PeerState, TailscaleCliSource,
};
use crate::manifest::{BucketState, SnapshotManifest, VaultState};
use crate::snapshot::{
    create_snapshot_with_node_local, fingerprint, verify_snapshot, SnapshotSources, SnapshotStore,
};
use crate::transport::init_token;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 1;
pub const EXIT_UNAVAILABLE: i32 = 2;

struct Flags {
    values: Vec<(String, String)>,
    switches: BTreeSet<String>,
}

impl Flags {
    fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn get_all(&self, name: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }
    fn has(&self, name: &str) -> bool {
        self.switches.contains(name)
    }
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        values: Vec::new(),
        switches: BTreeSet::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let name = arg
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument: {arg}"))?;
        if matches!(name, "confirm-primary-down") {
            flags.switches.insert(name.to_string());
            i += 1;
        } else {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("flag --{name} needs a value"))?;
            flags.values.push((name.to_string(), value.clone()));
            i += 2;
        }
    }
    Ok(flags)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn peer_cause(peer: &PeerRecord) -> &'static str {
    match peer.state {
        PeerState::Offline => "offline",
        PeerState::AclBlocked => "acl",
        PeerState::Online => {
            if peer.dns_name.is_none() && peer.tailscale_ips.is_empty() {
                "endpoint"
            } else {
                "ok"
            }
        }
    }
}

fn print_peer(out: &mut dyn Write, peer: &PeerRecord) {
    let dns = peer.dns_name.as_deref().unwrap_or("-");
    let mut line = format!(
        "peer {} {} cause={}",
        peer.public_key,
        dns,
        peer_cause(peer)
    );
    if let Some(seen) = &peer.last_seen {
        line.push_str(&format!(" last_seen={seen}"));
    }
    let _ = writeln!(out, "{line}");
}

fn cmd_status(flags: &Flags, injected: Option<&dyn PeerSource>, out: &mut dyn Write) -> i32 {
    let _ = writeln!(out, "avmesh status");
    if let Some(path) = flags.get("status-json") {
        let json = match std::fs::read_to_string(path) {
            Ok(j) => j,
            Err(e) => {
                let _ = writeln!(out, "tailnet: down cause=tailnet_down detail={e}");
                return EXIT_UNAVAILABLE;
            }
        };
        let allowlist: BTreeSet<String> = flags
            .get_all("allow")
            .iter()
            .map(|s| s.to_string())
            .collect();
        return match parse_tailscale_status(&json, &allowlist, flags.get("tailnet")) {
            Ok(parsed) => {
                let _ = writeln!(out, "tailnet: running");
                for peer in &parsed.peers {
                    print_peer(out, peer);
                }
                for key in &parsed.unapproved_keys {
                    let _ = writeln!(out, "peer {key} - cause=identity");
                }
                let _ = writeln!(
                    out,
                    "peers: {} unapproved: {}",
                    parsed.peers.len(),
                    parsed.unapproved_keys.len()
                );
                EXIT_OK
            }
            Err(DiscoveryError::NotRunning(state)) => {
                let _ = writeln!(
                    out,
                    "tailnet: down cause=tailnet_down detail=BackendState={state}"
                );
                EXIT_UNAVAILABLE
            }
            Err(e) => {
                let _ = writeln!(out, "tailnet: down cause=tailnet_down detail={e}");
                EXIT_UNAVAILABLE
            }
        };
    }
    let source: Box<dyn PeerSource> = match injected {
        Some(s) => Box::new(SourceRef(s)),
        None => Box::new(TailscaleCliSource {
            allowlist: flags
                .get_all("allow")
                .iter()
                .map(|s| s.to_string())
                .collect(),
            expected_tailnet: flags.get("tailnet").map(|s| s.to_string()),
        }),
    };
    match source.discover() {
        Ok(peers) => {
            let _ = writeln!(out, "tailnet: running");
            for peer in &peers {
                print_peer(out, peer);
            }
            let _ = writeln!(out, "peers: {}", peers.len());
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "tailnet: down cause=tailnet_down detail={e}");
            EXIT_UNAVAILABLE
        }
    }
}

struct SourceRef<'a>(&'a dyn PeerSource);

impl PeerSource for SourceRef<'_> {
    fn discover(&self) -> crate::discovery::Result<Vec<PeerRecord>> {
        self.0.discover()
    }
}

fn require_data_dir(flags: &Flags, out: &mut dyn Write) -> Option<PathBuf> {
    match flags.get("data-dir") {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            let _ = writeln!(out, "error: --data-dir is required");
            None
        }
    }
}

fn cmd_snapshot_create(flags: &Flags, out: &mut dyn Write) -> i32 {
    let Some(data_dir) = require_data_dir(flags, out) else {
        return EXIT_USAGE;
    };
    let (Some(db), Some(secrets), Some(env)) = (
        flags.get("db-export"),
        flags.get("secrets-dir"),
        flags.get("env-file"),
    ) else {
        let _ = writeln!(
            out,
            "error: snapshot create needs --db-export, --secrets-dir and --env-file"
        );
        return EXIT_USAGE;
    };
    let at: u64 = match flags.get("at") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => {
                let _ = writeln!(out, "error: --at must be unix seconds");
                return EXIT_USAGE;
            }
        },
        None => now_unix(),
    };
    let revision: u64 = match flags.get("revision").unwrap_or("0").parse() {
        Ok(n) => n,
        Err(_) => {
            let _ = writeln!(out, "error: --revision must be a number");
            return EXIT_USAGE;
        }
    };
    let mut buckets = Vec::new();
    for spec in flags.get_all("bucket") {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() != 3 {
            let _ = writeln!(out, "error: --bucket must be ID:REVISION:HASH");
            return EXIT_USAGE;
        }
        let Ok(bucket_revision) = parts[1].parse::<u64>() else {
            let _ = writeln!(out, "error: --bucket revision must be a number");
            return EXIT_USAGE;
        };
        buckets.push(BucketState {
            bucket_id: parts[0].into(),
            revision: bucket_revision,
            ciphertext_hash: parts[2].into(),
        });
    }
    // Node-local secrets (ADR decision C): postgres_password is always
    // excluded; each --node-local-secret NAME adds one more plain file
    // name. A path here would be a footgun, so reject it as usage.
    let node_local: Vec<&str> = flags.get_all("node-local-secret");
    for name in &node_local {
        if name.is_empty()
            || *name == "."
            || *name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            let _ = writeln!(
                out,
                "error: --node-local-secret must be a plain file name, not a path: {name}"
            );
            return EXIT_USAGE;
        }
    }
    // The ciphertext hash is bound to the payload: without an explicit
    // --ciphertext-hash, compute it as the fingerprint of the actual db
    // export being packaged, never an empty or invented string.
    let ciphertext_hash = match flags.get("ciphertext-hash") {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => match fs::read(db) {
            Ok(bytes) => fingerprint(&bytes),
            Err(e) => {
                let _ = writeln!(out, "error: cannot read db export {db}: {e}");
                return EXIT_UNAVAILABLE;
            }
        },
    };
    let manifest = SnapshotManifest::new(
        flags.get("node-id").unwrap_or("node-a-key"),
        at,
        vec![VaultState {
            manifest_id: flags.get("vault").unwrap_or("v1").into(),
            revision,
            ciphertext_hash,
            buckets,
        }],
    );
    let dest = data_dir.join("snapshots").join(format!("snap-{at}"));
    let sources = SnapshotSources {
        database_export: PathBuf::from(db),
        secrets_dir: PathBuf::from(secrets),
        environment_file: PathBuf::from(env),
    };
    let mut lines: Vec<String> = Vec::new();
    let mut log = |line: &str| lines.push(line.to_string());
    match create_snapshot_with_node_local(&sources, &manifest, &dest, &mut log, &node_local) {
        Ok(()) => {
            for line in &lines {
                let _ = writeln!(out, "{line}");
            }
            let _ = writeln!(out, "snapshot created at {}", dest.display());
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn cmd_snapshot_verify(flags: &Flags, out: &mut dyn Write) -> i32 {
    let Some(dir) = flags.get("snapshot") else {
        let _ = writeln!(out, "error: snapshot verify needs --snapshot DIR");
        return EXIT_USAGE;
    };
    let mut lines: Vec<String> = Vec::new();
    let mut log = |line: &str| lines.push(line.to_string());
    match verify_snapshot(Path::new(dir), &mut log) {
        Ok(manifest) => {
            for line in &lines {
                let _ = writeln!(out, "{line}");
            }
            for vault in &manifest.vaults {
                let _ = writeln!(
                    out,
                    "vault {} revision {}",
                    vault.manifest_id, vault.revision
                );
            }
            let _ = writeln!(out, "snapshot ok");
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn describe_snapshot(label: &str, dir: &Path, out: &mut dyn Write) {
    if !dir.is_dir() {
        let _ = writeln!(out, "{label}: none");
        return;
    }
    let mut log = |_: &str| {};
    match verify_snapshot(dir, &mut log) {
        Ok(manifest) => {
            let summary = manifest
                .vaults
                .iter()
                .map(|v| format!("{}@r{}", v.manifest_id, v.revision))
                .collect::<Vec<_>>()
                .join(",");
            let _ = writeln!(out, "{label}: {} vaults={}", dir.display(), summary);
        }
        Err(e) => {
            let _ = writeln!(out, "{label}: {} invalid ({e})", dir.display());
        }
    }
}

fn cmd_snapshot_list(flags: &Flags, out: &mut dyn Write) -> i32 {
    let Some(data_dir) = require_data_dir(flags, out) else {
        return EXIT_USAGE;
    };
    let store = SnapshotStore::new(data_dir.join("store"));
    describe_snapshot("current", &store.current_dir(), out);
    describe_snapshot("previous", &store.previous_dir(), out);
    describe_snapshot("staging", &store.staging_dir(), out);
    let snapshots = data_dir.join("snapshots");
    if snapshots.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&snapshots)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                describe_snapshot("snapshot", &entry, out);
            }
        }
    }
    EXIT_OK
}

fn cmd_promote(flags: &Flags, out: &mut dyn Write) -> i32 {
    let Some(data_dir) = require_data_dir(flags, out) else {
        return EXIT_USAGE;
    };
    if !flags.has("confirm-primary-down") {
        let _ = writeln!(
            out,
            "error: promote requires --confirm-primary-down: failover is manual and the operator must confirm the primary is really down, not merely partitioned (ADR decisions B/F)"
        );
        return EXIT_USAGE;
    }
    let store = SnapshotStore::new(data_dir.join("store"));
    let mut log = |_: &str| {};
    // Fork guard: a staged snapshot forked against the current one
    // halts promotion (ADR decision B: a fork is a stop condition).
    if store.current_dir().is_dir() && store.staging_dir().is_dir() {
        if let (Ok(current), Ok(staged)) = (
            verify_snapshot(&store.current_dir(), &mut log),
            verify_snapshot(&store.staging_dir(), &mut log),
        ) {
            if crate::manifest::compare_manifests(&current, &staged).is_forked() {
                let _ = writeln!(out, "error: staged snapshot is forked against current; promotion halted (ADR decision B)");
                return EXIT_UNAVAILABLE;
            }
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut log2 = |line: &str| lines.push(line.to_string());
    match store.promote(&mut log2) {
        Ok(manifest) => {
            for line in &lines {
                let _ = writeln!(out, "{line}");
            }
            let summary = manifest
                .vaults
                .iter()
                .map(|v| format!("{}@r{}", v.manifest_id, v.revision))
                .collect::<Vec<_>>()
                .join(",");
            let _ = writeln!(out, "promoted: vaults={summary}");
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn cmd_demote(flags: &Flags, out: &mut dyn Write) -> i32 {
    let Some(data_dir) = require_data_dir(flags, out) else {
        return EXIT_USAGE;
    };
    let store = SnapshotStore::new(data_dir.join("store"));
    let current = store.current_dir();
    let previous = store.previous_dir();
    if !current.is_dir() || !previous.is_dir() {
        let _ = writeln!(
            out,
            "error: nothing to demote to: no previous snapshot is retained"
        );
        return EXIT_UNAVAILABLE;
    }
    // Swap current and previous by rename; nothing is deleted, so a
    // demote is itself reversible (ADR decision F).
    let tmp = data_dir.join("store").join("demote-tmp");
    let result = (|| -> std::io::Result<()> {
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::rename(&current, &tmp)?;
        std::fs::rename(&previous, &current)?;
        std::fs::rename(&tmp, &previous)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = writeln!(out, "demoted: previous snapshot is current again");
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn cmd_token_init(flags: &Flags, out: &mut dyn Write) -> i32 {
    let Some(data_dir) = require_data_dir(flags, out) else {
        return EXIT_USAGE;
    };
    match init_token(&data_dir) {
        Ok(path) => {
            let _ = writeln!(
                out,
                "token written to {} (contents not shown; copy it to the other nodes out of band)",
                path.display()
            );
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

const USAGE: &str = "usage: avmesh <status|snapshot|promote|demote|token> [flags]\n  status [--status-json FILE] [--allow KEY]... [--tailnet NAME]\n  snapshot create --data-dir D --db-export F --secrets-dir D --env-file F [--node-id KEY] [--vault ID] [--revision N] [--ciphertext-hash H] [--bucket ID:REV:HASH]... [--node-local-secret NAME]... [--at UNIX]\n  snapshot verify --snapshot DIR\n  snapshot list --data-dir D\n  promote --data-dir D --confirm-primary-down\n  demote --data-dir D\n  token init --data-dir D";

/// Run the CLI with the production peer source. Returns an exit code.
pub fn run(args: &[String], out: &mut dyn Write) -> i32 {
    run_with_source(args, None, out)
}

/// Run the CLI with an injected peer source (tests).
pub fn run_with_source(
    args: &[String],
    source: Option<&dyn PeerSource>,
    out: &mut dyn Write,
) -> i32 {
    let Some(command) = args.first() else {
        let _ = writeln!(out, "{USAGE}");
        return EXIT_USAGE;
    };
    match command.as_str() {
        "--version" | "-V" | "version" => {
            let _ = writeln!(out, "avmesh {}", env!("CARGO_PKG_VERSION"));
            EXIT_OK
        }
        "status" => match parse_flags(&args[1..]) {
            Ok(flags) => cmd_status(&flags, source, out),
            Err(e) => {
                let _ = writeln!(out, "error: {e}");
                EXIT_USAGE
            }
        },
        "snapshot" => {
            let Some(sub) = args.get(1) else {
                let _ = writeln!(out, "{USAGE}");
                return EXIT_USAGE;
            };
            let flags = match parse_flags(&args[2..]) {
                Ok(f) => f,
                Err(e) => {
                    let _ = writeln!(out, "error: {e}");
                    return EXIT_USAGE;
                }
            };
            match sub.as_str() {
                "create" => cmd_snapshot_create(&flags, out),
                "verify" => cmd_snapshot_verify(&flags, out),
                "list" => cmd_snapshot_list(&flags, out),
                _ => {
                    let _ = writeln!(out, "{USAGE}");
                    EXIT_USAGE
                }
            }
        }
        "promote" => match parse_flags(&args[1..]) {
            Ok(flags) => cmd_promote(&flags, out),
            Err(e) => {
                let _ = writeln!(out, "error: {e}");
                EXIT_USAGE
            }
        },
        "demote" => match parse_flags(&args[1..]) {
            Ok(flags) => cmd_demote(&flags, out),
            Err(e) => {
                let _ = writeln!(out, "error: {e}");
                EXIT_USAGE
            }
        },
        "token" => {
            let Some(sub) = args.get(1) else {
                let _ = writeln!(out, "{USAGE}");
                return EXIT_USAGE;
            };
            let flags = match parse_flags(&args[2..]) {
                Ok(f) => f,
                Err(e) => {
                    let _ = writeln!(out, "error: {e}");
                    return EXIT_USAGE;
                }
            };
            match sub.as_str() {
                "init" => cmd_token_init(&flags, out),
                _ => {
                    let _ = writeln!(out, "{USAGE}");
                    EXIT_USAGE
                }
            }
        }
        _ => {
            let _ = writeln!(out, "{USAGE}");
            EXIT_USAGE
        }
    }
}
