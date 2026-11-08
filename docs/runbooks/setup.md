# Runbook: setup

Set up a two-node deployment: `node-a` as primary, `node-b` as standby. All values below are placeholders — replace them with your own.

> Status of commands: upstream and operating-system commands are real. `avmesh` commands marked `VERIFIED` are proven by the two-instance drill. Commands marked `UNVERIFIED` have not been exercised yet, and no flag in them should be trusted before then.

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

4. Install the mesh agent beside AliasVault on both machines. There is no peer or role configuration command: the primary is the node that serves, a standby is a node that pulls, and the serving node allowlists the pulling node's key on each `serve`. On the primary, create the mesh token in the agent store directory:

   ```sh
   avmesh token init --data-dir ./node-a/store   # VERIFIED
   ```

   Copy `./node-a/store/mesh.token` to the standby out of band (never over the mesh itself); on the standby it sits at `./node-b/mesh.token`. The standby presents this token when it pulls: by default `pull` reads it from the token file in its data directory, and a different file can be named with `--token-file <file>` (the drill's wrong-token control uses exactly that form). A pull presenting a wrong token is refused (401) — VERIFIED.

5. Publish the agent on each machine with Serve (the agent itself binds to loopback only; Funnel is never used):

   ```sh
   tailscale serve status
   ```

   Confirm the sync endpoint is published as configured in `docs/design/tailnet.md`.

6. Check status on both machines; the causes of a tailnet failure (offline / ACL / endpoint / identity / tailnet down) are reported distinctly (status reporting is not part of the verified flow):

   ```sh
   avmesh status   # UNVERIFIED
   ```

7. Take a first snapshot on the primary, promote it, and serve it. On `node-a`:

   ```sh
   avmesh snapshot create --data-dir ./node-a --db-export ./db-export.sql --secrets-dir ./secrets --env-file ./instance.env --node-id node-a-key --vault v1 --revision 1   # VERIFIED
   cp -R ./node-a/snapshots/snap-<timestamp> ./node-a/store/staging
   avmesh promote --data-dir ./node-a --confirm-primary-down   # VERIFIED
   avmesh snapshot verify --snapshot ./node-a/store/current    # VERIFIED
   avmesh serve --data-dir ./node-a/store --listen 127.0.0.1:18100 --allow node-a-key   # VERIFIED — foreground; use its own shell
   ```

   Then pull the snapshot on the standby and verify it. On `node-b`:

   ```sh
   avmesh pull --data-dir ./node-b --from http://127.0.0.1:18100 --allow node-a-key   # VERIFIED
   avmesh snapshot verify --snapshot ./node-b/staging   # VERIFIED
   ```

   The loopback `--from` form above is the drill-proven form. When the primary is reached over the tailnet through Tailscale Serve (step 5), the pull also takes `--serve-host <primary-serve-name>`; that form is UNVERIFIED. Starting the standby's AliasVault on the pulled snapshot (the standby start test) is not part of the verified flow yet: UNVERIFIED.

## Verification

- `avmesh snapshot verify --snapshot ./node-b/staging` (VERIFIED) succeeds on the standby, and the pulled payload matches the primary's current snapshot (the drill compares payload bytes between the two nodes).
- `avmesh status` (UNVERIFIED) on the standby reports the primary's latest snapshot id, manifest revisions, and hashes, and the fork state `converged`.
- The standby's previous verified snapshot is retained alongside the new one.
- The vault on the primary opens normally with the (fake) account; the standby holds data directories only.

## Rollback

Stop the agent on both machines. The mesh layer is additive: both AliasVault instances keep working standalone. See ADR 0001 section F.
