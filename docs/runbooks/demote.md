# Runbook: demote a promoted standby

Return a promoted node to standby duty and re-seed it from the current primary. Typical case: the original primary (`node-a`) is healthy again and resumes its role, and the node that was promoted during the incident (`node-b`) goes back to holding snapshots.

> Status of commands: upstream and operating-system commands are real. Every `avmesh` command is marked `UNVERIFIED` — the exact agent CLI is verified against the merged agent source once it exists.

## The one rule

Demotion is not an overwrite. If the node being demoted accepted any writes while it was primary, demoting it is a fork: stop and follow `fork-resolution.md`. Re-seeding from the primary is allowed only when the demoted node holds no writes the primary lacks.

## Steps

1. Confirm which node is the primary now, and confirm the demotion candidate's fork state:

   ```sh
   avmesh status   # UNVERIFIED — run on both nodes
   ```

   The candidate must report `converged`, or `behind`/`ahead` only by snapshots it received as a standby. Any `forked` state ends this runbook.

2. Stop the candidate's writable AliasVault instance. From this moment it serves nothing.

3. Stop its mesh agent, then mark it standby:

   ```sh
   avmesh stop            # UNVERIFIED
   avmesh role set standby   # UNVERIFIED
   ```

4. Re-seed it from the current primary: take a fresh snapshot on the primary and let the standby receive, verify, and stage it like any other snapshot:

   ```sh
   avmesh snapshot now   # UNVERIFIED — on the primary
   ```

   The re-seeded data directories replace the candidate's previous ones only after the new snapshot verifies and passes the standby start test; the previous verified snapshot is retained until then.

5. Restart the agent on the demoted node and confirm status:

   ```sh
   avmesh start    # UNVERIFIED
   avmesh status   # UNVERIFIED
   ```

   Expected: role `standby`, fork state `converged`, latest snapshot equal to the primary's.

## Verification

- Clients still point at the primary's tailnet address; nothing about demotion changes client configuration.
- A subsequent snapshot on the primary replicates to the demoted node normally.

## Rollback

Until step 4 completes, the demoted node's previous data directories are untouched: restart its instance and it is a primary again (and the other node must then be treated as the one to demote, under the same one rule above).
