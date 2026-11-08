# Architecture

A condensed picture of the design in `docs/adr/0001-sync-design.md` (ADR 0001, accepted with amendments). The ADR is authoritative; this page is a summary.

## Shape

One logical AliasVault instance, replicated as a whole. Exactly one node is the primary at a time; every other node is a standby holding the latest verified snapshot and running no writable AliasVault.

```text
                         tailnet (one Tailscale network)
  +-------------------------------+   +-------------------------------+
  | node-a  (PRIMARY)             |   | node-b  (STANDBY)              |
  |                               |   |                               |
  |  AliasVault (unmodified,      |   |  AliasVault (stopped; started  |
  |  writable)                    |   |  only for a start test or on   |
  |                               |   |  manual promotion)             |
  |  mesh agent                   |   |  mesh agent                    |
  |    snapshot: db export +      |   |    staging -> verify checksums |
  |    secrets/ + config +        |   |    + manifest -> replace data  |
  |    manifest (ids, revisions,  |   |    dirs; previous verified     |
  |    hashes, byte counts)       |   |    snapshot retained           |
  +---------------+---------------+   +---------------+---------------+
                  |        snapshot over tailnet        |
                  +---------- Serve endpoint ---------->+
                  |   (loopback bind, published with    |
                  |    tailscale serve; never Funnel)   |
                  +-------------------------------------+

  Clients (browser / apps) point at the primary's tailnet address only.
```

## Components

- **Upstream AliasVault, unmodified.** Runs and integrates only through its published image, its operator surface (container lifecycle, database export), and its API. The project never patches, forks, or redistributes it.
- **Mesh agent.** A single static binary (Rust) installed beside AliasVault on each node. It discovers peers, takes and transfers snapshots, verifies them, detects forks, and reports status. Discovery and the upstream operator surface sit behind interfaces with fake implementations, so tests run with no tailnet and no real vault data.
- **Peer discovery.** Read-only. The real implementation parses `tailscale status --json`; the agent never changes tailnet configuration.
- **Snapshot.** A database export plus the instance's `secrets/` directory and environment configuration, with a manifest listing manifest ids, revisions, ciphertext hashes, and byte counts. The manifest contains no ciphertext and no secrets.
- **Fork detector.** Compares snapshot manifests between nodes: equal revision and equal hash means converged; a lower revision means behind; a higher revision means ahead; the same revision with a different hash means forked, and replication halts.

## Snapshot flow

1. On the primary, the agent asks the upstream operator surface for a database export and assembles the snapshot in a staging area with owner-only permissions (0600 files, 0700 directories), under the agent's own data directory only.
2. The snapshot is transferred to each standby over the tailnet Serve endpoint only. Nothing is written to logs or command-line arguments.
3. On the standby, the snapshot lands in staging, is verified (checksums and manifest), and only then replaces the standby's data directories.
4. The previous verified snapshot is retained until the new one has both verified and survived a standby start test. Discarding a bad or partial snapshot is always safe: the primary still holds the source data.

## Identity and access

- Identity is the Tailscale node key. Names, addresses, and MagicDNS names are locators only; a changed key is a new, unapproved peer.
- Mesh nodes carry the tag described in `docs/design/tailnet.md`; the only network grant is mesh-to-mesh on the sync HTTPS port.
- Each agent binds to loopback only and is published with `tailscale serve`. Funnel is not used for the sync endpoint, the AliasVault UI or API, or any data or admin surface.
- The sync layer also authenticates peers at the application level, bound to the allowlisted node key. A reachable port is never proof of identity.

## Failure modes

| Failure | Detection | Response | Data loss |
|---|---|---|---|
| Network partition between nodes | Peer unreachable; sync pauses | Replication pauses and resumes when the partition heals; the primary keeps serving | None; the standby falls behind and catches up from the next snapshot |
| Split brain (two writable primaries) | Fork detector at the next manifest comparison | Treated as a revision fork (below); replication halts | No silent loss; resolution may invoke upstream per-row merging (see fork row) |
| Node loss (primary destroyed) | Peer permanently absent; the operator declares the loss | Promote the standby from its latest verified snapshot (`docs/runbooks/promote.md`) | Changes made on the primary after the last completed snapshot exist only on the lost node. The recovery point is the last verified snapshot, stated honestly — not zero-loss |
| Clock skew between nodes | Snapshot manifests carry revision numbers (authoritative) plus wall-clock times (advisory only); skew shows as inconsistent advisory times at equal revisions | Revisions and hashes, never timestamps, decide fork detection. Before any client-side merge resolution, the operator checks both nodes' clocks | None at the mesh layer. Residual risk lives inside upstream merging, which compares client update timestamps, so a skewed clock can make stale rows look newer |
| Restore from backup | A restored node rejoins with an older revision set than the current primary | The restored instance is quarantined on rejoin: its manifest is compared before it may serve or replicate, and it is re-seeded from the primary, never the other way round, unless the operator explicitly promotes it after a primary loss | None if the quarantine step is followed. Restoring over a live data directory without the comparison is a prohibited operator action |
| Revision fork: two servers accepted a different next revision for the same vault | Same manifest id, same revision number, different ciphertext hash on two nodes. Equal revision and equal hash is convergence, not a fork | 1) Freeze: neither node replicates over the other; both branches are snapshotted and kept. 2) The operator picks one node as the resolution base. 3) On that node, the user opens their normal upstream client (the only key holder) and points it at the other branch's server; the upstream client pull-and-merge merges the other branch in. 4) The merged result is pushed, snapshotted, and re-replicated; only then are fork snapshots released. Full procedure: `docs/runbooks/fork-resolution.md` | No branch is ever discarded by the agent, so nothing is silently lost by sync. Inside upstream merging, two edits to the same row resolve last-writer-wins on the row's update timestamp: the older edit to that row is superseded |

## Rollback

The mesh layer is additive. Stopping the agent on every node is a complete rollback of the sync feature; upstream instances are untouched and keep working standalone. Failover rollback (demoting a promoted standby) is covered in `docs/runbooks/demote.md`. No rollback step deletes the only copy of anything: every destructive action requires a verified second copy to exist first.

## Out of scope

For the MVP, and each needing its own later decision:

- Automatic failover (promotion is manual, always).
- Live multi-writer sync (see the tradeoff in `README.md`).
- More than two nodes.
- Upstream deployment shapes other than the all-in-one shape, including multi-container and Kubernetes shapes.
- Inbound-email handling beyond whole-instance state.
- Any decryption step: vault decryption happens only inside the user's own upstream client and server flows.
