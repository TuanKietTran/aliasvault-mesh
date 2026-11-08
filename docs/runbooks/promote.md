# Runbook: promote a standby (manual failover)

Promote `node-b` to primary when `node-a` is lost, destroyed, or deliberately taken offline. Promotion is always manual. The recovery point is the standby's last verified snapshot — changes made on the old primary after that snapshot are not in it, and this runbook does not pretend otherwise.

> Status of commands: upstream and operating-system commands are real. Every `avmesh` command is marked `UNVERIFIED` — the exact agent CLI is verified against the merged agent source once it exists.

## Before you start

1. Declare the situation out loud, in writing, in your operations notes: the old primary is lost, or is offline and will stay offline. If the old primary might still be writable, stop — two writable primaries create a revision fork (see `fork-resolution.md`).
2. **Clock check.** Check the wall clock on the standby and, if it is reachable at all, on the old primary:

   ```sh
   date -u
   ```

   Upstream merging compares client update timestamps, so a skewed clock can make stale rows look newer during any later merge. Record both readings. Do not proceed with a merge resolution until skew is understood; promotion itself is decided by revisions and hashes, never timestamps.
3. Confirm the standby's state:

   ```sh
   avmesh status   # UNVERIFIED
   ```

   It must report a verified snapshot and fork state `converged` (or `behind` relative to a primary that is now gone). If it reports `forked`, stop and follow `fork-resolution.md` first.

## Steps

1. Stop the mesh agent on the standby so no replication touches the data directories during promotion:

   ```sh
   avmesh stop   # UNVERIFIED
   ```

2. Mark the standby as primary:

   ```sh
   avmesh role set primary   # UNVERIFIED
   ```

3. Start the standby's AliasVault instance from its latest verified snapshot data directories, using the same pinned upstream release tag as before.

4. Open the vault with the normal upstream client and the real account. Confirm item counts and a spot check of recent items against what you expect from the last verified snapshot.

5. Point clients at the promoted node's tailnet address. Restart the mesh agent on the promoted node:

   ```sh
   avmesh start   # UNVERIFIED
   ```

6. Record the promotion in your operations notes: time, the snapshot id promoted from, the clock readings from step 2, and the reason.

## After promotion

- The old primary, if it ever returns, rejoins quarantined: it is re-seeded from the new primary (see `demote.md`), never the other way round, unless it holds writes the new primary lacks — that is a fork, and `fork-resolution.md` applies.
- Re-establish a standby as soon as practical: a single primary has no recovery point beyond its own snapshots.

## Rollback

If the promotion itself fails (the instance does not start, or the vault does not open): stop the instance, keep its data directories untouched, and return to the previous verified snapshot, which was retained. The old primary's data, if the machine still exists, is unchanged by anything in this runbook.
