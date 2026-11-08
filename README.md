# aliasvault-mesh

Unofficial community project, not affiliated with or endorsed by AliasVault or XIVISOFT.

## What it is

aliasvault-mesh keeps one AliasVault instance replicated across several machines on a single Tailscale tailnet. One machine is the primary and runs the writable AliasVault instance. The other machines are standbys: each holds a verified, recent whole-instance snapshot of the primary and can be promoted manually if the primary is lost or taken offline.

The mesh agent is a small, cleanly implemented program that runs beside an unmodified upstream AliasVault instance. It takes snapshots of the primary (a database export plus the instance's `secrets/` and configuration), transfers them to the standbys over the tailnet only, verifies them, and keeps the previous verified snapshot until a new one has been verified and has passed a standby start test.

Design: `docs/adr/0001-sync-design.md` (accepted, with amendments). Architecture summary: `docs/architecture.md`. Operating procedures: `docs/runbooks/`.

## The honest tradeoff

This is single-writer replication with manual failover. It is NOT live multi-writer sync.

- Any node can be the primary, but only one node is the primary at a time. Standbys run no writable AliasVault instance.
- Devices point at the primary's tailnet address. If the primary fails, an operator promotes a standby by hand, following `docs/runbooks/promote.md`.
- The recovery point is the last verified snapshot, not zero-loss. Changes made on the primary after that snapshot exist only on the primary until the next snapshot completes.
- Why not multi-writer: the agent handles ciphertext only (see the security model below), and a ciphertext-only agent cannot merge vault rows. Upstream merging is per-row and happens inside the user's own unlocked client. Multi-writer sync with the user's client doing the merge is documented in the design as future work, not as part of this project.

If two nodes ever accept writes for the same vault at the same time, that is treated as a revision fork: replication halts, both branches are kept, and the fork is resolved with the upstream client following `docs/runbooks/fork-resolution.md`. Nothing is silently overwritten.

## Security model

- End-to-end encryption is preserved. The agent handles ciphertext, database exports of ciphertext, and revision and hash metadata only.
- The agent never receives, derives, stores, or transmits a master password or any key derived from it — not in transit, not at rest, not in logs, and not in process arguments. Vault decryption happens only inside the user's own upstream client and server flows, exactly as upstream designed.
- Instance secrets that failover must move (for example the contents of `secrets/`) are transferred only over the tailnet endpoint, stored only inside the receiving instance's own data directory with owner-only permissions, and are never logged, printed, or placed in command-line arguments.
- Snapshot archives and staging areas use owner-only permissions (0600 for files, 0700 for directories), live only under the agent's own data directory, and are transmitted only over the tailnet. The snapshot manifest contains manifest ids, revisions, hashes, and byte counts — never ciphertext bodies and never secret material.
- Logs carry manifest ids, revisions, hashes, snapshot ids, and byte counts only.
- Peers are identified by their Tailscale node key, not by name or address. A reachable port is never proof of identity; the sync layer also authenticates peers at the application level. See `docs/design/tailnet.md`.

## Quick start

Placeholder values only — replace every placeholder with your own before use.

1. Install an unmodified upstream AliasVault instance on `node-a` and on `node-b`, each with its own data directory. Follow `docs/runbooks/setup.md`.
2. Join both machines to the same tailnet, for example `example.ts.net`, and note their tailnet addresses, for example `100.64.0.1` for `node-a`.
3. Install the mesh agent beside AliasVault on both machines, configure `node-a` as the primary and `node-b` as a standby, and allowlist each peer by its node key.
4. Take a manual whole-instance backup before any real use: `docs/runbooks/backup-before-real-use.md`.
5. Start the agent on both machines and confirm that a snapshot taken on `node-a` arrives on `node-b`, verifies, and passes the standby start test.

Until the procedures in the runbooks are marked verified, treat every command in them as a draft: the runbooks say so per command.

## Platform support

The agent supports Linux and macOS. On Windows, run it inside a WSL2 distribution (the AliasVault containers run there anyway; see ADR 0001, section E). A native Windows build compiles and passes its unit tests, but it cannot generate the mesh token (provision `mesh.token` out of band) and it does not enforce owner-only file permissions, so do not run it natively on a machine that holds a real instance.

## Status

Design accepted (ADR 0001, with amendments). MVP in progress: two nodes, the all-in-one upstream deployment shape, one primary and one standby, snapshot replication, fork detection, and manual promotion and demotion. Automatic failover, multi-writer sync, more than two nodes, and other upstream deployment shapes are out of scope.

Experiments that the design depends on are run first, on throwaway instances with fake data only, before the agent relies on them.

## License

The project's own code is dual-licensed under MIT OR Apache-2.0, at your option: see `LICENSE-MIT` and `LICENSE-APACHE`. It contains no upstream AliasVault code. Upstream AliasVault is a separate project under its own license (AGPL-3.0-only) and trademark policy; see `docs/THIRD_PARTY.md`.
