# Runbook: backup before real use

Do this before the first rollout step that touches a real vault, and before promoting real data onto any node. The rule for the whole project: no step deletes or overwrites the only copy of anything, and real vaults are touched only by the operator, only after this backup exists and has been verified.

> Status of commands: upstream and operating-system commands are real. Every `avmesh` command is marked `UNVERIFIED` — the exact agent CLI is verified against the merged agent source once it exists.

## What a backup is here

A manual, whole-instance backup, stored outside every node's live data directories and outside the mesh agent's snapshot retention:

- A database export taken with upstream's own operator surface (`install.sh db-export`).
  `TODO: verified in experiment E7` — whether the database export covers blob storage completely is an open verification item; until E7 passes, also copy the instance's blob/data directories as part of step 2, so the backup does not depend on that answer.
- A copy of the instance's whole data directory set: `database/`, `secrets/`, `certificates/`, and the environment configuration. The contents of `secrets/` are secrets: store the copy with owner-only permissions (0600 files, 0700 directories), never in a shared or synced folder, never in logs or command-line arguments.

## Steps

1. Announce a quiet window: no client edits the vault during the backup.

2. Take the database export and copy the data directories to the backup location. Record, in a note stored with the backup: the date and time, the node it came from, the upstream release tag, and the snapshot manifest (manifest ids, revisions, hashes) if a mesh snapshot exists:

   ```sh
   avmesh status   # UNVERIFIED — record the manifest it reports
   ```

3. Verify the backup copy: file counts and sizes match the source, and checksums of the copied database export match the original. A backup that has not been compared is a hope, not a backup.

4. Prove restorability on a throwaway instance (fake account check only, on loopback ports in the 18000–18999 range, its own data directory): restore the export into a fresh throwaway instance, start it, and confirm the instance comes up healthy and the vault opens. Tear the throwaway down afterwards. Never restore over a live data directory to "test" a backup.

5. Only now proceed with the rollout or promotion step this backup protects. Keep the backup until that step has completed and two clean promotion drills exist on the real hardware; keep snapshot retention at its default until then as well.

## Notes

- The mesh agent's own snapshots are not a substitute for this backup: they live on the mesh nodes, under the agent's control, with their own retention. This backup is manual, separate, and operator-held.
- If any step above cannot be completed, stop. The rollout step that needed this backup does not happen.
