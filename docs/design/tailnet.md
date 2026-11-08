# Tailnet design: discovery, identity and transport

Status: design. This document describes how the nodes of one
`aliasvault-mesh` deployment find each other and authenticate over
Tailscale. It makes no claim about AliasVault internals; the sync
protocol itself is decided in `docs/adr/0001-sync-design.md`.

All names and addresses in examples are placeholders: `node-a`,
`node-b`, `100.64.0.1`, `100.64.0.2`, `example.ts.net`. Never put a
real tailnet name, hostname, address or token in this repository.

## Principles

1. The tailnet is the network security layer, not the only one.
   Membership in the tailnet plus an ACL grant decides who can open a
   connection; the sync protocol still authenticates the peer itself
   (see the sync ADR). A reachable port is never proof of identity.
2. The sync layer only ever carries ciphertext. Nothing in this
   document changes that: Tailscale encryption protects data in
   transit between machines, it is not a substitute for the
   end-to-end encryption of the vault data itself.
3. Discovery is read-only. The sync agent reads local Tailscale
   state; it never changes tailnet configuration. Commands that
   change state (`tailscale up`, `tailscale set`,
   `tailscale serve`, `tailscale funnel`) are run once, by the
   operator, during setup — never automatically by the agent.
4. Fail closed. If a peer cannot be identified, or the transport
   cannot be verified, the agent does not sync with it and does not
   fall back to a plaintext or public path.

## 1. Peer discovery

### 1.1 Interface boundary

Discovery sits behind one interface: a function (or trait) that
returns the current peer list. There are two implementations:

- a fake implementation returning a fixed peer list, used in tests
  and in multi-instance simulations on a single machine;
- a real implementation that runs `tailscale status --json`
  locally and parses its output, used in production.

No other part of the system shells out to the `tailscale` CLI or
parses its output. The sync logic consumes only the peer records
defined in section 1.3.

Reference: Tailscale CLI — https://tailscale.com/kb/1080/cli

### 1.2 `tailscale status --json`: fields used

Only the fields below are read. The parser must ignore unknown
fields and treat a missing field as `unknown`, never as a default
that implies trust or presence: the field set varies between
Tailscale versions.

Top level:

| Field | Meaning | Used for |
|---|---|---|
| `BackendState` | Local client state, e.g. `Running`, `NeedsLogin`, `Stopped` | Gate: discovery only proceeds when `Running` |
| `Self` | Peer object for this machine | Excluding self; learning our own identity |
| `Peer` | Map of peer objects, keyed by the peer's public node key string | Candidate peer list |
| `MagicDNSSuffix` | Tailnet DNS suffix, e.g. `example.ts.net` | Validating that a `DNSName` belongs to this tailnet |
| `CurrentTailnet` | Tailnet the client is logged into | Detecting a machine joined to the wrong tailnet |
| `Health` | Local client health warnings | Surfacing local problems in logs/status |

Peer object (`Self` and each value of `Peer`):

| Field | Meaning | Used for |
|---|---|---|
| `PublicKey` | Node public key, e.g. `nodekey:...` | **Peer identity** (section 2) |
| `ID` | Stable node identifier assigned by the control plane | Log correlation only; never an allowlist key on its own |
| `HostName` | Operator-chosen machine name | Display only; user-controlled, not unique, not trusted |
| `DNSName` | MagicDNS name, e.g. `node-a.example.ts.net.` | Dial target for the HTTPS endpoint (section 4) |
| `TailscaleIPs` | Tailnet addresses (100.64.0.0/10 IPv4, plus IPv6) | Fallback dial target if MagicDNS fails (section 6) |
| `Tags` | ACL tags on the device, e.g. `tag:aliasvault-mesh` | Candidate filter (section 3) |
| `Online` | Control plane's view: is the device connected | Liveness hint, combined with a probe |
| `LastSeen` | When an offline device was last seen | Separating `just left` from `long gone` in status output |
| `Active` | Whether a connection to the peer is currently active | Diagnostics only |
| `CurAddr` | Current direct connection address, if any | Diagnostics: direct vs relayed (section 6) |
| `Relay` | DERP region used when no direct path exists | Diagnostics: direct vs relayed (section 6) |
| `InMagicSock`, `InEngine` | Internal client bookkeeping | Ignored except for debugging |

### 1.3 Discovery procedure

1. Run `tailscale status --json`. If the command fails, Tailscale
   is not installed or not running: report `tailnet unavailable`,
   sync nothing, retry with backoff.
2. Require `BackendState` = `Running`. `NeedsLogin` means the local
   key has expired or was never authenticated (section 6): report
   it, do not attempt any peer connection.
3. Take every value of `Peer` except `Self`.
4. Keep only peers whose `Tags` contain the mesh tag (section 3).
   A peer without the tag is not a mesh node, however reachable.
5. Verify each remaining peer's `PublicKey` against the locally
   configured peer allowlist (section 2). An unknown key is a new
   node that needs explicit operator approval, not a sync partner.
6. For approved peers, record `DNSName`, `TailscaleIPs`, `Online`
   and `LastSeen`. A peer that is approved but `Online` = false is
   `known but offline`: keep it in the peer list, attempt no
   connection, recheck on the next discovery round.
7. Repeat discovery periodically and before each sync round. Peer
   state is a cache with an age, not a fact: never act on a peer
   record older than the discovery interval without renewing it.

There is no broadcast, scanning or probing of the 100.64.0.0/10
range. Peers come from the control plane's view as reported by the
local client, nowhere else.

## 2. Peer identity

Three different things are easily confused. They are not
interchangeable:

| Identifier | What it is | Trust it for |
|---|---|---|
| Node public key (`PublicKey`) | The WireGuard public key of the node. The control plane distributes it; traffic to that key is encrypted and authenticated by WireGuard, so only the holder of the matching private key can answer. | **Identity.** The allowlist key. |
| Tailscale IP (`TailscaleIPs`) | A routing address in 100.64.0.0/10 assigned to the node. | A locator. Dial it, never authenticate by it: an address says where to send packets, and trust comes from the key the packets are sealed to, not the address. |
| MagicDNS name (`DNSName`) | A DNS name under the tailnet suffix that resolves to the node's Tailscale IP. | A human-readable, stable locator. Convenient dial target, still not a credential: DNS resolution happens inside the tailnet, and the connection that follows is authenticated by node key. |

Rules:

- The peer allowlist stores node public keys, with the current
  `DNSName` and addresses kept alongside as attributes.
- Trust follows the key, not the name or address. If a known
  `DNSName` or address reappears with a different `PublicKey`, it
  is a different node: stop, require operator approval. This is
  what protects the mesh when a machine is reinstalled or a name is
  reused.
- If a known key moves to a new address or name (for example after
  a rename), update the attributes and continue; note the change
  in the log.
- `HostName` is never used for a trust decision. Anyone who controls
  a machine can set it to anything, including another node's name.
- Re-enrolment creates a new node key. A reinstalled machine is a
  new peer until the operator approves its new key and removes the
  old one from the allowlist.
- Key expiry and re-authentication are operator actions (section 6).
  The agent must never store a Tailscale auth key or attempt to
  re-authenticate a node itself.

UNVERIFIED: whether the numeric `ID` field survives a node's key
expiry and re-authentication unchanged. This design does not depend
on the answer: `ID` is used for log correlation only.

References:

- MagicDNS — https://tailscale.com/kb/1081/magicdns
- Connection types (direct vs DERP relay) — https://tailscale.com/kb/1257/connection-types

## 3. ACLs and tags

Mesh nodes run under a dedicated tag, proposed as
`tag:aliasvault-mesh`. A tagged device acts as a service rather
than as its owner's personal device, which is exactly the model a
sync agent needs.

Example policy fragment (placeholder names; the operator adapts and
applies it in the Tailscale admin console):

```json
{
  "tagOwners": {
    "tag:aliasvault-mesh": ["autogroup:admin"]
  },
  "acls": [
    {
      "action": "accept",
      "src": ["tag:aliasvault-mesh"],
      "dst": ["tag:aliasvault-mesh:443"]
    }
  ]
}
```

Rules:

- The only grant mesh nodes get is mesh-to-mesh on the sync HTTPS
  port (443 in the example, matching section 4). No grant to the
  AliasVault web UI, its database, SSH, or any other port follows
  from membership of the mesh.
- Personal devices (phones, laptops without the tag) get no access
  to the sync port unless the operator adds a separate, explicit
  grant. Sync is machine-to-machine.
- Only tailnet admins are tag owners: only an admin can apply the
  tag to a device, so only an admin can admit a machine to the mesh.
- Tagging a device disables key expiry for it (per the Tailscale
  tags documentation), which removes the most common cause of a
  node silently dropping out of the mesh (section 6). The trade-off
  is that a tagged node stays admitted until an admin removes the
  tag or the device — revocation is an admin action, and the peer
  allowlist (section 2) is the second line of defence.
- The policy file is kept under version control by the operator.
  An ACL change is a deployment: review it like code, because a
  wrong grant silently widens who can reach a vault sync endpoint
  and a wrong removal silently stops sync (section 6).
- Removing a node from the mesh is two steps, both required: remove
  the tag / device in the admin console, and remove its public key
  from every remaining node's peer allowlist.

References:

- ACLs — https://tailscale.com/kb/1018/acls
- Tags — https://tailscale.com/kb/1068/tags
- Key expiry — https://tailscale.com/kb/1028/key-expiry

## 4. HTTPS endpoint with `tailscale serve`

Each mesh node exposes its sync endpoint over HTTPS inside the
tailnet using Tailscale Serve:

- Prerequisites, set once by the operator in the admin console:
  MagicDNS enabled, and HTTPS certificates enabled for the tailnet.
- The sync agent process itself binds only to `127.0.0.1` on a
  loopback port. It never binds to a tailnet or LAN address
  directly, so Serve (plus the ACL) is the only way in.
- The operator publishes it once, for example:

  ```text
  tailscale serve --bg --https=443 http://127.0.0.1:18080
  ```

  (18080 is an example loopback port; the deployment picks one and
  uses it consistently.)
- Serve terminates TLS in the Tailscale client with a real,
  automatically issued and renewed certificate for the node's
  MagicDNS name (for example `node-a.example.ts.net`), and forwards
  plain HTTP over loopback to the agent. Clients get normal TLS
  verification against a public CA: no self-signed certificates, no
  certificate pinning hacks, no warnings to click through.
- The certificate private key stays inside the Tailscale client.
  The sync agent never reads, generates or stores a TLS key.
- Peers dial `https://<DNSName>` (for example
  `https://node-b.example.ts.net`). TLS authenticates the name,
  WireGuard authenticates the node key, the ACL authorises the
  connection, and the sync protocol authenticates the peer on top
  (section: Principles, rule 1).
- Serve is tailnet-only: it does not publish anything to the public
  internet. That is the property that makes it acceptable here, and
  the property Funnel does not have (section 5 of this document:
  Funnel, below).
- The agent checks at startup that its loopback port answers; it
  cannot check its own Serve configuration (that state belongs to
  the Tailscale client), so setup documentation includes a manual
  verification step: from a second mesh node,
  `curl https://<DNSName>/` must reach the agent, and
  `tailscale serve status` on the serving node must show the
  mapping.

References:

- Tailscale Serve — https://tailscale.com/kb/1312/serve
- HTTPS certificates — https://tailscale.com/kb/1153/enabling-https

## 5. What must never be exposed with Funnel

Tailscale Funnel is Serve's public sibling: it relays a local
service through Tailscale's servers to anyone on the public
internet who knows the name, on a small set of allowed ports
(443, 8443, 10000). Tailnet ACLs do not protect a Funnel endpoint:
the whole point of Funnel is reachability without a tailnet.

Therefore, on any machine that holds vault data, Funnel must never
be enabled for:

- the sync endpoint (this design);
- the AliasVault web UI or API;
- any database, admin, metrics, debug or health endpoint of either
  AliasVault or the sync agent;
- file or directory sharing of a data directory, backup or export.

No exception is made for `the sync protocol authenticates anyway`:
Funnel removes the network layer of defence, advertises the
endpoint's existence to the internet, and exposes the agent to
scanning, credential-stuffing against any auth bug, and denial of
service — for zero benefit, because every legitimate sync peer is
by definition inside the tailnet.

Operator check: `tailscale serve status` shows Funnel mappings.
A mesh node's status must show Serve mappings only; any Funnel
mapping on a vault machine is a misconfiguration to fix
immediately, whatever it points at.

Reference: Tailscale Funnel — https://tailscale.com/kb/1223/funnel

## 6. Failure modes the tailnet itself introduces

Sync must treat every one of these as `pause and retry`, never as
`peer data is gone` and never as a reason to weaken verification.

| Failure mode | How it presents | Required behaviour |
|---|---|---|
| Device offline, asleep or shut down | `Online` = false, `LastSeen` ages; dials time out | Keep the peer and its state; skip it this round; exponential backoff; surface `offline since <LastSeen>` in status |
| Local Tailscale down or not logged in | `tailscale status` fails, or `BackendState` != `Running` | Sync nothing; report `tailnet unavailable` distinctly from `all peers offline` |
| Node key expiry (untagged devices) | Peer disappears from `Peer` or local client shows `NeedsLogin` | Operator re-authenticates; agent never holds an auth key. Prevented for mesh nodes by the tag (section 3) |
| Key rotation / reinstall | Same name or address, different `PublicKey` | Treat as a new, unapproved node (section 2); never silently adopt the new key |
| ACL change removes a grant | Peer still discovered and `Online`, but connections to the sync port are dropped or time out | Report as a probable ACL/policy problem, not as `peer down`; fail closed; resume automatically when the grant returns |
| ACL change widens a grant | Nothing visible locally | Mitigated by process, not code: policy under version control and review (section 3); app-level peer authentication limits the blast radius |
| Tag removed from a node | Peer loses `Tags`, is filtered out of discovery (section 1.3 step 4) | Treat as `removed from mesh`: stop syncing with it, keep local data, alert in status; do not delete anything |
| MagicDNS failure or disabled | `DNSName` does not resolve, while the peer is `Online` | Fall back to dialling a `TailscaleIPs` address over HTTPS only if certificate verification can still succeed; if it cannot (name mismatch), fail closed and report a DNS problem. Never fall back to plaintext |
| Serve not configured, removed, or certificate provisioning fails | Peer reachable at the network layer, HTTPS dial fails (connection refused, TLS or HTTP error) | Report an endpoint/configuration error for that peer, distinct from `offline`; never fall back to the loopback port of a remote node or to plain HTTP over the tailnet |
| Control plane unreachable | Existing WireGuard paths may keep working, but `status` data goes stale and new peer/key changes do not arrive | Keep using the last discovery result only within its freshness window; make no new trust decisions (no new peers, no key changes) until the control plane is reachable again |
| DERP-only path (no direct connection) | `CurAddr` empty, `Relay` set; sync works but with relay latency and throughput limits | Sync anyway — the path is still WireGuard-encrypted end to end — but expect slow large transfers; prefer scheduling bulk transfer and report `relayed` in status |
| DERP relay unavailable, and no direct path | Peer `Online` per control plane, but no traffic passes | Indistinguishable from a dead peer at first: treat as offline with backoff; diagnostics may note the last known `Relay` |
| Wrong tailnet | `CurrentTailnet` / `MagicDNSSuffix` differ from the configured values | Refuse to discover or sync at all; a machine joined to a different tailnet is not a degraded mesh node, it is not in the mesh |
| Two nodes with the same `HostName` | Two peer records, same display name, different keys | Harmless if identity rules are followed (keys, not names); status output must disambiguate peers by key fingerprint, never by name alone |

Two general rules follow from the table:

- Absence is not deletion. A peer being offline, expired or
  de-tagged never deletes, rolls back or `resolves` any local data.
- Errors must keep their cause. `Offline`, `ACL denied`,
  `endpoint misconfigured`, `identity changed` and `tailnet down`
  are different operator problems with different fixes; collapsing
  them into `sync failed` makes the mesh undebuggable.

Reference: DERP servers — https://tailscale.com/kb/1232/derp-servers

## 7. Open questions

- UNVERIFIED: exact `tailscale status --json` field stability across
  the Tailscale versions in use on macOS and Windows. Mitigation is
  already in the design (ignore unknown fields, missing = unknown),
  but the parser needs a captured sample from each platform before
  it is written.
- UNVERIFIED: whether Serve on Windows behaves identically for a
  service running without an interactive login session. To be
  checked on the Windows test machine during the rollout described
  in the sync ADR.
- Whether discovery should also confirm liveness with an application
  level health check before every sync round, or only when
  `Online` and the last successful sync disagree. Left to the sync
  ADR / implementation, as long as `Online` alone is never treated
  as proof a peer is ready to sync.

## References

- Tailscale CLI — https://tailscale.com/kb/1080/cli
- MagicDNS — https://tailscale.com/kb/1081/magicdns
- ACLs — https://tailscale.com/kb/1018/acls
- Tags — https://tailscale.com/kb/1068/tags
- Key expiry — https://tailscale.com/kb/1028/key-expiry
- Tailscale Serve — https://tailscale.com/kb/1312/serve
- HTTPS certificates — https://tailscale.com/kb/1153/enabling-https
- Tailscale Funnel — https://tailscale.com/kb/1223/funnel
- Connection types — https://tailscale.com/kb/1257/connection-types
- DERP servers — https://tailscale.com/kb/1232/derp-servers
