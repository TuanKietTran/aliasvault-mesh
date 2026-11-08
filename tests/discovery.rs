//! Discovery tests (T3): synthetic fixtures only — placeholders
//! node-a/node-b, 100.64.0.x, example.ts.net. No test runs the real
//! `tailscale` CLI or touches a real tailnet.

use avmesh::discovery::{
    parse_tailscale_status, DiscoveryError, FakePeerSource, PeerRecord, PeerSource, PeerState,
    StaticStatusSource, MESH_TAG,
};
use std::collections::BTreeSet;

const FULL: &str = include_str!("fixtures/status_full.json");
const NEEDS_LOGIN: &str = include_str!("fixtures/status_needs_login.json");
const MISSING: &str = include_str!("fixtures/status_missing_fields.json");

fn allowlist() -> BTreeSet<String> {
    [
        "nodekey:bbbb-peer".to_string(),
        "nodekey:cccc-peer".to_string(),
    ]
    .into_iter()
    .collect()
}

fn record(key: &str, state: PeerState) -> PeerRecord {
    PeerRecord {
        public_key: key.into(),
        id: None,
        host_name: None,
        dns_name: None,
        tailscale_ips: vec![],
        tags: vec![MESH_TAG.into()],
        state,
        last_seen: None,
    }
}

#[test]
fn fake_source_returns_configured_states() {
    let source = FakePeerSource::new(vec![
        record("nodekey:bbbb-peer", PeerState::Online),
        record("nodekey:cccc-peer", PeerState::Offline),
        record("nodekey:ffff-peer", PeerState::AclBlocked),
    ]);
    let peers = source.discover().unwrap();
    assert_eq!(peers.len(), 3);
    assert_eq!(peers[2].state, PeerState::AclBlocked);
}

#[test]
fn parser_keeps_only_tagged_allowlisted_peers_and_excludes_self() {
    let parsed = parse_tailscale_status(FULL, &allowlist(), Some("example.ts.net")).unwrap();
    let keys: Vec<&str> = parsed.peers.iter().map(|p| p.public_key.as_str()).collect();
    assert_eq!(keys, vec!["nodekey:bbbb-peer", "nodekey:cccc-peer"]);
    assert_eq!(parsed.self_record.unwrap().public_key, "nodekey:aaaa-self");
    assert_eq!(
        parsed.unapproved_keys,
        vec!["nodekey:eeee-stranger".to_string()]
    );
    assert_eq!(parsed.magic_dns_suffix.as_deref(), Some("example.ts.net"));
}

#[test]
fn parser_maps_online_flag_to_state_and_reads_locators() {
    let parsed = parse_tailscale_status(FULL, &allowlist(), None).unwrap();
    let online = &parsed.peers[0];
    assert_eq!(online.state, PeerState::Online);
    assert_eq!(online.dns_name.as_deref(), Some("node-b.example.ts.net."));
    assert_eq!(online.tailscale_ips, vec!["100.64.0.2".to_string()]);
    assert_eq!(online.id.as_deref(), Some("1002"));
    let offline = &parsed.peers[1];
    assert_eq!(offline.state, PeerState::Offline);
    assert_eq!(offline.id.as_deref(), Some("1003"));
}

#[test]
fn parser_ignores_unknown_fields() {
    // status_full.json carries Version, OS, SomeFutureField and
    // UnknownPeerField; parsing succeeds and reads only listed fields.
    let parsed = parse_tailscale_status(FULL, &allowlist(), None).unwrap();
    assert_eq!(parsed.backend_state, "Running");
    assert!(parsed.missing_fields.is_empty());
}

#[test]
fn parser_reports_missing_fields_without_defaulting_trust() {
    let parsed = parse_tailscale_status(MISSING, &allowlist(), None).unwrap();
    assert!(parsed.missing_fields.iter().any(|f| f.contains("DNSName")));
    assert!(parsed.missing_fields.iter().any(|f| f.contains("Online")));
    // The peer is still listed (it is tagged + allowlisted) but with
    // unknown locators and Offline-by-unknown, never assumed present.
    assert_eq!(parsed.peers.len(), 1);
    assert_eq!(parsed.peers[0].dns_name, None);
    assert_eq!(parsed.peers[0].state, PeerState::Offline);
    assert_eq!(parsed.health, vec!["Tailscale is starting".to_string()]);
}

#[test]
fn missing_backend_state_is_an_error() {
    let err = parse_tailscale_status("{\"Peer\": {}}", &allowlist(), None).unwrap_err();
    assert!(matches!(err, DiscoveryError::MissingField(ref f) if f == "BackendState"));
}

#[test]
fn backend_not_running_is_an_error() {
    let err = parse_tailscale_status(NEEDS_LOGIN, &allowlist(), None).unwrap_err();
    assert!(matches!(err, DiscoveryError::NotRunning(ref s) if s == "NeedsLogin"));
}

#[test]
fn wrong_tailnet_is_detected() {
    let err = parse_tailscale_status(FULL, &allowlist(), Some("other.ts.net")).unwrap_err();
    assert!(matches!(err, DiscoveryError::WrongTailnet { .. }));
}

#[test]
fn static_source_implements_peer_source() {
    let source = StaticStatusSource {
        json: FULL.to_string(),
        allowlist: allowlist(),
        expected_tailnet: None,
    };
    let peers = source.discover().unwrap();
    assert_eq!(peers.len(), 2);
}
