# Runbook: resolve a revision fork

A fork means two servers accepted a different next revision for the same vault: the same manifest id at the same revision number with a different ciphertext hash on each node. Equal revision and equal hash is convergence, not a fork.

> Status of commands: upstream and operating-system commands are real. Every `avmesh` command is marked `UNVERIFIED` — the exact agent CLI is verified against the merged agent source once it exists.

## Principles

- No branch is ever discarded by the agent, and nothing in this runbook deletes a branch. The loss risk is inside upstream merging only: two edits to the same row resolve last-writer-wins on the row's update timestamp, so the older edit to that row is superseded.
- Merging is done by the user's own upstream client — the only component that holds the keys. The agent never merges and never decrypts.

## Steps

1. **Freeze.** Confirm replication has halted (the fork detector halts it on detection) and stop the agent on both nodes so nothing replicates over either branch:

   ```sh
   avmesh status   # UNVERIFIED — both nodes: expect fork state `forked`
   avmesh stop     # UNVERIFIED — both nodes
   ```

2. **Snapshot both branches.** Take a snapshot of each node's current state and keep both outside any retention rotation until this runbook is finished.

   ```sh
   avmesh snapshot now   # UNVERIFIED — both nodes
   ```

3. **Clock check.** Record the wall clock on both nodes:

   ```sh
   date -u
   ```

   Upstream merging compares client update timestamps. If one clock is skewed, note it in your operations record before merging: rows edited on the skewed node may win or lose against rows they should not.

4. **Pick a resolution base.** Choose one node as the base — normally the node with more or more important recent edits. Write the choice and the reason in your operations notes.

5. **Merge with the upstream client.** On the base node, open the normal upstream client (signed in as the vault owner), and point it at the *other* branch's server. The client's own sync does the work: status check, pull-and-merge, push. The merge is per-row, last-writer-wins on each row's update timestamp; item deletes are resolved as a unit against the other side's latest activity.

   If the client reports it cannot merge and falls back to taking the server vault as a whole, stop: that outcome discards the base branch's divergent rows. Restore the base from its step-2 snapshot and reconsider which node should be the base, or accept the outcome explicitly and record it.

6. **Verify the merged result.** Open the merged vault on the base node. Check item counts against both branches' counts, and spot-check the specific items known to have diverged on each side.

7. **Re-replicate.** Snapshot the merged state, restart the agent on the base node, mark the other node standby, and re-seed it from the merged snapshot (see `demote.md`):

   ```sh
   avmesh snapshot now   # UNVERIFIED — base node
   avmesh start          # UNVERIFIED — both nodes, after roles are set
   avmesh status         # UNVERIFIED — both nodes: expect `converged`
   ```

8. **Release the fork snapshots** from step 2 only after step 7 reports `converged` and step 6 has been signed off in your operations notes.

## Afterwards

Record in your operations notes: when the fork was detected, both branches' revisions and hashes, the clock readings, the chosen base and why, any rows known to have been superseded in the merge, and the final merged revision.
