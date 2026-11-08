//! Peer discovery behind a trait (tailnet.md section 1, ADR decision D).
//!
//! Discovery is read-only: nothing here changes tailnet configuration,
//! and no part of the system outside this module parses `tailscale
//! status --json` output. The parser reads only the fields listed in
//! tailnet.md section 1.2, ignores unknown fields, and reports missing
//! fields instead of defaulting them into trust or presence. Identity
//! is the node public key (section 2); names and addresses are
//! locators only. Only tagged, allowlisted peers become sync partners
//! (sections 1.3 and 3).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const MESH_TAG: &str = "tag:aliasvault-mesh";

/// Reachability of a peer as discovery reports it. `AclBlocked` is not
/// derivable from status JSON alone; it is produced by the fake source
/// and by the status layer when the ACL/endpoint checks fail, so the
/// three causes stay distinct end to end (ADR MVP scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerState {
    Online,
    Offline,
    AclBlocked,
}

/// One peer record: the only shape the sync logic consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// Node public key: the identity, and the allowlist key.
    pub public_key: String,
    /// Control-plane node id: log correlation only, never trusted.
    pub id: Option<String>,
    /// Operator-chosen machine name: display only, never trusted.
    pub host_name: Option<String>,
    /// MagicDNS name: dial target for the sync HTTPS endpoint.
    pub dns_name: Option<String>,
    /// Tailnet addresses: fallback dial targets.
    pub tailscale_ips: Vec<String>,
    pub tags: Vec<String>,
    pub state: PeerState,
    /// RFC3339 timestamp as reported by the local client, if present.
    pub last_seen: Option<String>,
}

#[derive(Debug)]
pub enum DiscoveryError {
    /// The `tailscale` command could not be run or exited non-zero:
    /// report `tailnet unavailable`, sync nothing (section 1.3 step 1).
    CommandFailed(String),
    /// `BackendState` was not `Running` (for example `NeedsLogin` after
    /// key expiry): report it, attempt no peer connection (step 2).
    NotRunning(String),
    /// The local client is joined to a different tailnet than expected.
    WrongTailnet {
        expected: String,
        actual: String,
    },
    /// A field the design depends on was absent (section 1.2: a missing
    /// field is `unknown`, never a default that implies trust).
    MissingField(String),
    Parse(String),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::CommandFailed(m) => write!(f, "tailnet unavailable: {m}"),
            DiscoveryError::NotRunning(s) => write!(f, "tailnet not running: BackendState={s}"),
            DiscoveryError::WrongTailnet { expected, actual } => {
                write!(
                    f,
                    "wrong tailnet: expected {expected}, client is joined to {actual}"
                )
            }
            DiscoveryError::MissingField(name) => {
                write!(f, "tailscale status is missing required field: {name}")
            }
            DiscoveryError::Parse(m) => write!(f, "could not parse tailscale status: {m}"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

pub type Result<T> = std::result::Result<T, DiscoveryError>;

/// The discovery interface (section 1.1). Implementations: a fake for
/// tests and simulations, and sources backed by `tailscale status
/// --json` for production.
pub trait PeerSource {
    fn discover(&self) -> Result<Vec<PeerRecord>>;
}

/// Fake implementation returning a fixed peer list, including states a
/// status dump cannot express (for example `AclBlocked`).
#[derive(Debug, Clone, Default)]
pub struct FakePeerSource {
    pub peers: Vec<PeerRecord>,
}

impl FakePeerSource {
    pub fn new(peers: Vec<PeerRecord>) -> Self {
        FakePeerSource { peers }
    }
}

impl PeerSource for FakePeerSource {
    fn discover(&self) -> Result<Vec<PeerRecord>> {
        Ok(self.peers.clone())
    }
}

/// The result of parsing one `tailscale status --json` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedStatus {
    pub backend_state: String,
    pub tailnet_name: Option<String>,
    pub magic_dns_suffix: Option<String>,
    pub health: Vec<String>,
    pub self_record: Option<PeerRecord>,
    /// Tagged + allowlisted peers only: the sync partners.
    pub peers: Vec<PeerRecord>,
    /// Tagged peers whose public key is not on the allowlist: new nodes
    /// needing explicit operator approval, never sync partners.
    pub unapproved_keys: Vec<String>,
    /// Optional fields that were absent, reported per section 1.2.
    pub missing_fields: Vec<String>,
}

fn str_field(v: &serde_json::Value, name: &str) -> Option<String> {
    v.get(name).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn id_field(v: &serde_json::Value) -> Option<String> {
    match v.get("ID") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn string_list(v: &serde_json::Value, name: &str) -> Vec<String> {
    v.get(name)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn peer_from_value(key_hint: &str, v: &serde_json::Value, missing: &mut Vec<String>) -> PeerRecord {
    let public_key = str_field(v, "PublicKey").unwrap_or_else(|| {
        missing.push(format!("Peer[{key_hint}].PublicKey"));
        key_hint.to_string()
    });
    if v.get("DNSName").is_none() {
        missing.push(format!("Peer[{key_hint}].DNSName"));
    }
    if v.get("Online").is_none() {
        missing.push(format!("Peer[{key_hint}].Online"));
    }
    let online = v.get("Online").and_then(|x| x.as_bool()).unwrap_or(false);
    PeerRecord {
        public_key,
        id: id_field(v),
        host_name: str_field(v, "HostName"),
        dns_name: str_field(v, "DNSName"),
        tailscale_ips: string_list(v, "TailscaleIPs"),
        tags: string_list(v, "Tags"),
        state: if online {
            PeerState::Online
        } else {
            PeerState::Offline
        },
        last_seen: str_field(v, "LastSeen"),
    }
}

/// Parse `tailscale status --json` output following section 1.3:
/// gate on `BackendState`, exclude self, keep only mesh-tagged peers,
/// and split those into allowlisted partners and unapproved keys.
pub fn parse_tailscale_status(
    json: &str,
    allowlist: &BTreeSet<String>,
    expected_tailnet: Option<&str>,
) -> Result<ParsedStatus> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| DiscoveryError::Parse(e.to_string()))?;
    let backend_state = str_field(&root, "BackendState")
        .ok_or_else(|| DiscoveryError::MissingField("BackendState".into()))?;
    if backend_state != "Running" {
        return Err(DiscoveryError::NotRunning(backend_state));
    }
    let tailnet_name = root
        .get("CurrentTailnet")
        .and_then(|t| str_field(t, "Name"))
        .or_else(|| str_field(&root, "CurrentTailnet"));
    if let (Some(expected), Some(actual)) = (expected_tailnet, tailnet_name.as_deref()) {
        if expected != actual {
            return Err(DiscoveryError::WrongTailnet {
                expected: expected.into(),
                actual: actual.into(),
            });
        }
    }
    let magic_dns_suffix = str_field(&root, "MagicDNSSuffix").or_else(|| {
        root.get("CurrentTailnet")
            .and_then(|t| str_field(t, "MagicDNSSuffix"))
    });
    let health = string_list(&root, "Health");
    let mut missing = Vec::new();
    let self_record = root.get("Self").map(|s| {
        let key = str_field(s, "PublicKey").unwrap_or_default();
        peer_from_value(&key, s, &mut missing)
    });
    let self_key = self_record.as_ref().map(|r| r.public_key.clone());
    let peer_map = root
        .get("Peer")
        .and_then(|p| p.as_object())
        .ok_or_else(|| DiscoveryError::MissingField("Peer".into()))?;
    let mut peers = Vec::new();
    let mut unapproved_keys = Vec::new();
    for (key, value) in peer_map {
        if self_key.as_deref() == Some(key.as_str()) {
            continue;
        }
        let record = peer_from_value(key, value, &mut missing);
        if !record.tags.iter().any(|t| t == MESH_TAG) {
            continue;
        }
        if allowlist.contains(&record.public_key) {
            peers.push(record);
        } else {
            unapproved_keys.push(record.public_key);
        }
    }
    peers.sort_by(|a, b| a.public_key.cmp(&b.public_key));
    unapproved_keys.sort();
    Ok(ParsedStatus {
        backend_state,
        tailnet_name,
        magic_dns_suffix,
        health,
        self_record,
        peers,
        unapproved_keys,
        missing_fields: missing,
    })
}

/// A source backed by a fixed status document (tests, simulations,
/// and the CLI's `--status-json` style checks).
#[derive(Debug, Clone)]
pub struct StaticStatusSource {
    pub json: String,
    pub allowlist: BTreeSet<String>,
    pub expected_tailnet: Option<String>,
}

impl PeerSource for StaticStatusSource {
    fn discover(&self) -> Result<Vec<PeerRecord>> {
        Ok(parse_tailscale_status(
            &self.json,
            &self.allowlist,
            self.expected_tailnet.as_deref(),
        )?
        .peers)
    }
}

/// The production source: runs `tailscale status --json` locally and
/// parses its output. Nothing else in the system shells out to the
/// `tailscale` CLI (section 1.1). Tests never call this.
#[derive(Debug, Clone, Default)]
pub struct TailscaleCliSource {
    pub allowlist: BTreeSet<String>,
    pub expected_tailnet: Option<String>,
}

impl PeerSource for TailscaleCliSource {
    fn discover(&self) -> Result<Vec<PeerRecord>> {
        let output = std::process::Command::new("tailscale")
            .args(["status", "--json"])
            .output()
            .map_err(|e| DiscoveryError::CommandFailed(e.to_string()))?;
        if !output.status.success() {
            return Err(DiscoveryError::CommandFailed(format!(
                "tailscale exited with {}",
                output.status
            )));
        }
        let json = String::from_utf8_lossy(&output.stdout);
        Ok(parse_tailscale_status(&json, &self.allowlist, self.expected_tailnet.as_deref())?.peers)
    }
}
