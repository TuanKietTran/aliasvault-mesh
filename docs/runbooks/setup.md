# Runbook: setup

Set up a two-node deployment: `node-a` as primary, `node-b` as standby. All values below are placeholders — replace them with your own.

> Status of commands: upstream and operating-system commands are real. Every `avmesh` command is marked `UNVERIFIED` — the exact agent CLI is verified against the merged agent source once it exists, and no flag in this runbook should be trusted before then.

## Safety rules (read first)

- If a machine already runs an AliasVault instance you care about, the mesh test instance is a NEW, separate compose project beside it: distinct project name, distinct ports, distinct data directory. The existing instance is never stopped, restarted, reconfigured, snapshotted, or mounted by the agent or by any test. It may only be observed read-only (for example, a container list).
- Check free ports and disk space on both machines before starting anything.
- Real vaults are touched only at the final rollout step, by the operator, after a manual backup (see `backup-before-real-use.md`). Until then: fake accounts and fake data only.
- Throwaway test instances use their own compose project name, their own loopback ports in the 18000–18999 range, and their own data directory, and are torn down at the end of the test.

## Prerequisites

- Both machines joined to the same tailnet, for example `example.ts.net`; mesh nodes carry the tag and ACL grant described in `docs/design/tailnet.md`.
- Docker on both machines (on Windows: Docker Desktop in Linux-container mode with the WSL2 backend, or a WSL2 distro — see the Windows notes in ADR 0001 section E; all state directories live inside the Linux filesystem, never on a Windows drive).
- An unmodified upstream AliasVault release, pinned by tag, pulled from upstream's registry, on both machines.

## Steps

1. Verify the tailnet on each machine:

   ```sh
   tailscale status --json
   ```

   Confirm both nodes are present, on the expected tailnet, and record each node's key. A changed key is a new, unapproved peer.

2. Note the primary's tailnet address, for example `100.64.0.1` for `node-a`.

3. Install AliasVault on `node-a` and on `node-b`, each with its own data directory (`database/`, `secrets/`, `certificates/`, `logs/`). Do not start a writable instance on the standby.

4. Install the mesh agent beside AliasVault on both machines. Configure `node-a` as primary and `node-b` as standby, and allowlist each peer by node key:

   ```sh
   avmesh peer add <peer-node-key>   # UNVERIFIED
   avmesh role set primary           # UNVERIFIED — on node-a only
   avmesh role set standby           # UNVERIFIED — on node-b only
   ```

5. Publish the agent on each machine with Serve (the agent itself binds to loopback only; Funnel is never used):

   ```sh
   tailscale serve status
   ```

   Confirm the sync endpoint is published as configured in `docs/design/tailnet.md`.

6. Start the agent on both machines and check status; the causes of a tailnet failure (offline / ACL / endpoint / identity / tailnet down) are reported distinctly:

   ```sh
   avmesh status   # UNVERIFIED
   ```

7. Trigger a first snapshot on the primary and confirm it arrives on the standby, verifies, and passes the standby start test:

   ```sh
   avmesh snapshot now   # UNVERIFIED
   avmesh status         # UNVERIFIED — run on node-b
   ```

## Verification

- `avmesh status` (UNVERIFIED) on the standby reports the primary's latest snapshot id, manifest revisions, and hashes, and the fork state `converged`.
- The standby's previous verified snapshot is retained alongside the new one.
- The vault on the primary opens normally with the (fake) account; the standby holds data directories only.

## Rollback

Stop the agent on both machines. The mesh layer is additive: both AliasVault instances keep working standalone. See ADR 0001 section F.
