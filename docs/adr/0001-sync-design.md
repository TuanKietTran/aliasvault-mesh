# ADR 0001: Sync design for aliasvault-mesh

Status: accepted with amendments
Date: 2026-11-08

Related: `docs/research.md` (upstream facts, cited below as
"research S<n>"), `docs/design/tailnet.md` (discovery, identity,
transport), `docs/THIRD_PARTY.md` (upstream touchpoints).

All names and addresses in examples are placeholders: `node-a`,
`node-b`, `100.64.0.1`, `example.ts.net`.

## Context

One user runs AliasVault on several machines of one tailnet and
wants the vaults kept in sync. Verified upstream facts that bound
the design (research S4, contract rule 10):

- Sync upstream is client-merge, not server-merge. A `POST v2/Vault`
  with a stale `CurrentRevision` is rejected whole as
  `VaultWriteStatus.Outdated`; the server never merges
  (`apps/server/AliasVault.Api/Controllers/V2/VaultController.cs`
  ~L380-406).
- On an accepted write the server only increments the revision and
  archives the previous revision, keeping the last 3
  (`apps/server/AliasVault.Api/Helpers/VaultRevisionWriter.cs:29/41,67,89`).
- The merge itself is per-row last-writer-wins on `UpdatedAt`,
  performed by the client on decrypted rows: the incoming row wins
  only if strictly newer, ties keep the server snapshot, and item
  deletes are resolved as a unit
  (`core/rust/src/vault_merge/mod.rs:73,121,217`,
  `core/rust/src/vault_merge/item_deletes.rs`). If merge fails, the
  client falls back to taking the server vault wholesale
  (`core/rust/src/vault_sync/merge.rs:33,53-56`).
- The server stores one opaque ciphertext per vault part; it cannot
  read items, fields or folders (research S3). There is no upstream
  HA story, and the protocol assumes a single server-side revision
  counter per vault (research S7).

Consequence: a sync layer that only ever handles ciphertext
(decision A) cannot perform upstream's merge, because that merge
operates on decrypted rows. Any honest design must therefore
prevent revision forks by construction, detect them if they
happen anyway, and leave merging to the one component that
legitimately holds the key: the user's own unlocked upstream
client.

## Options considered

### O1. One central AliasVault server; every machine is just a client

No per-node server, so no fork is possible and upstream sync works
exactly as designed. Rejected as the whole answer: it gives a node
no local vault when the central machine is off or unreachable,
which is the situation this project exists for (a local instance
already runs on the Windows test machine). Retained as the
degenerate case of the recommendation: at any moment there is
exactly one authoritative server.

### O2. Database-level multi-primary replication

Rejected. Upstream assumes one revision counter per vault
(research S4/S7); two writable Postgres primaries would fork that
counter by design, and server state outside the database
(decision C) has no replication story in Postgres at all.

### O3. Per-node instances, single-writer, whole-instance replication (recommended)

Each node runs an unmodified AliasVault (all-in-one shape,
research S2). Exactly one node is the primary: its AliasVault is
running and writable. Every other node is a standby: its
AliasVault is stopped, and a mesh agent replicates whole-instance
snapshots (database export plus the state directories of decision
C) from the primary to the standby over the tailnet. Promotion of
a standby is a manual, operator-run failover. Forks are prevented
by construction and detected by metadata comparison if the
single-writer rule is ever broken (decision B).

### O4. Multi-writer mesh agent merging vaults at the sync layer

Rejected for the MVP, deferred as future work. Per-row merge needs
decrypted rows, which a ciphertext-only agent must never have
(decision A). A ciphertext-only agent can detect a fork (same
manifest, same revision, different ciphertext hash) but cannot
merge one. A future multi-writer design would have to pair the
agent with the user's unlocked upstream client to do the merge;
that depends on unverified protocol details (decision H, items 6
and 7) and is out of MVP scope.

### O5. Reuse upstream's Rust sync/merge crates inside the agent

Rejected on two independent grounds: the agent would need the
user's key material to merge, violating decision A and the project
rule that the sync layer never sees a master password or derived
key; and copying or linking `core/rust/vault_sync` /
`core/rust/vault_merge` makes the agent AGPL-3.0 (decision G).

## Decision

Adopt O3.

Summary (amendment A2): O3 is single-writer replication with
manual failover: any node can be the primary, standbys hold a
verified recent copy, and devices point at the primary's tailnet
address. It is NOT live multi-writer sync: a ciphertext-only
agent cannot merge rows. O4 (multi-writer with the user's
unlocked client doing the merge) stays documented as future
work with its open questions (H items 6 and 7). This tradeoff
must also be stated in the README.

- One primary per deployment at a time. Standbys run no writable
  AliasVault; their data directories hold the latest verified
  replicated snapshot.
- The mesh agent is a clean implementation that talks to the
  unmodified upstream only through its operator surface (container
  lifecycle, `install.sh db-export`, the state directories) and
  reads its own node's local metadata. It contains no upstream
  code (decision G) and never authenticates as a vault user.
- Replication unit = one snapshot: database export, `secrets/`
  (per the file-level rule in decision C), `certificates/`
  (subject to the per-node rule in decision C),
  and the environment configuration, plus a snapshot manifest
  listing, per vault manifest and bucket, the revision number and
  ciphertext hash (metadata shape per research S3/S4). The
  manifest is what fork detection compares; it contains no
  ciphertext and no secrets.
- Transport and identity follow `docs/design/tailnet.md` exactly
  (decision D). Snapshots move over the Serve HTTPS endpoint,
  checksummed, into a staging area on the standby; a snapshot is
  promoted to the standby's live data directories only after its
  checksums and manifest verify. The previous verified snapshot is
  kept until the new one is verified (decision F).
- Promotion (failover) is manual: stop replication, confirm the
  primary is really down (not merely partitioned — decision B),
  apply the latest verified snapshot if not already applied, run
  the fork check, then start AliasVault on the standby and record
  the promotion in the snapshot log.

## A. End-to-end encryption is preserved

- The agent handles ciphertext, database exports of ciphertext,
  and revision/hash metadata only. It never receives, derives,
  stores or transmits a master password or any key derived from
  it, in transit, at rest, in logs, or in process arguments.
- Nothing in the design adds a decryption step anywhere: vault
  decryption happens only inside the user's own upstream client
  and server flows, exactly as upstream designed (research S3/S6).
- Server-side secrets the agent must move for failover (decision C)
  are instance secrets, not vault keys: they are transferred only
  over the tailnet Serve endpoint, stored only inside the
  standby's own `secrets/` data directory with the same
  permissions, and are never logged, printed, or placed in argv
  (pass file paths, not contents, to tooling).
- Logs carry manifest ids, revisions, hashes, snapshot ids and
  byte counts — never ciphertext bodies, never secret material.

## B. Failure modes

Grounded in the verified facts above: stale writes are rejected
whole as `Outdated`, history is keep-3 per server, and merging is
client-side per-row LWW on `UpdatedAt`.

| Failure mode | Detection | Resolution | Data-loss behaviour |
|---|---|---|---|
| Tailnet partition (primary alive, unreachable) | Discovery marks the peer offline / tailnet failure modes in tailnet.md S6 | Sync pauses and retries with backoff; no promotion while partition is possible — promotion requires operator confirmation the primary is down | None: nothing is overwritten during a partition |
| Concurrent edits on two nodes | Prevented by construction (standby AliasVault is stopped). If the rule is broken, each server accepts writes independently | Treat as a revision fork (below) once detected | No silent loss at the mesh layer; resolution may invoke upstream LWW (see fork row) |
| Node loss (primary destroyed) | Peer permanently absent; operator declares loss | Promote the standby from its latest verified snapshot | Edits made on the primary after the last completed snapshot exist only on the lost node: the recovery point is the last verified snapshot, stated honestly, not zero-loss. Shorten the window with frequent snapshots; do not claim otherwise |
| Clock skew between nodes | Snapshot manifests carry revision numbers (authoritative) plus wall-clock times (advisory only); skew shows as inconsistent advisory times at equal revisions | Revisions and hashes, never timestamps, decide fork detection at the mesh layer. Before any client-side merge resolution, the operator checks both nodes' clocks: upstream LWW compares client `UpdatedAt` values, so a skewed clock can make stale rows look newer | Mesh layer: none. Residual risk lives inside upstream LWW during fork resolution and is listed below |
| Restore from backup | A restored node rejoins with an older revision set than the current primary | Restored instance is quarantined on rejoin: its snapshot manifest is compared before it may serve or replicate; it is re-seeded from the primary, never the other way round, unless the operator explicitly promotes it after a primary loss | None if the quarantine step is followed; restoring over a live data directory without the comparison is a prohibited operator action |
| Revision fork: two servers accepted a different next revision for the same vault | Snapshot/status comparison: same manifest id, same `RevisionNumber`, different `ManifestCiphertextHash` on two nodes (hash and revision are stored per manifest, research S3). Equal revision + equal hash is convergence, not a fork | 1) Freeze: neither node replicates over the other; both fork branches are snapshotted and kept outside the keep-3 history rotation. 2) The operator picks one node as the resolution base. 3) On that node, the user opens their normal upstream client (the only key holder), and points it at the other branch's server: upstream's client pull-and-merge (status check, pull-and-merge, push; `Outdated` resync bounded at 3 attempts, research S4) merges the other branch in per-row LWW. 4) The merged result is pushed, snapshotted, and re-replicated; only then are fork snapshots released | No branch is ever discarded by the agent, so nothing is silently lost by sync. Residual risk, stated honestly: inside upstream's merge, two edits to the *same row* resolve LWW on `UpdatedAt` — the older edit to that row is superseded, and if the client merge fails upstream falls back to taking the server vault wholesale (research S4). The retained fork snapshots are the mitigation: both pre-merge branches survive until the operator confirms the merged result and deletes them manually |

Two standing rules follow:

- Absence is not deletion (also tailnet.md S6): an offline, lost or
  de-tagged peer never causes local data to be deleted or rolled
  back.
- A fork is a stop condition, not an error to retry through:
  automated replication halts for the affected vault until the
  operator runs the resolution procedure.

## C. Server-side state outside the database

Research S5: not everything that makes an instance itself lives in
the vault tables. The replication unit is therefore the whole
instance (research S7), not the database alone.

Must move with the database, as one consistent snapshot:

- `secrets/` is replicated per file, not as a whole (amended
  2026-11-08 on experiment E9 evidence, `docs/experiments.md`):
  - `jwt_key` — replicated. This is the exact JWT key filename,
    verified by E9 (decision H item 5 resolved). It holds the JWT
    signing material the API loads from the secrets directory or
    environment
    (`apps/server/AliasVault.Api/Controllers/V1/AuthController.cs:716-719,737-738`,
    research S5). If it diverges from the replicated database,
    sessions and tokens issued by one node are invalid on the
    other.
  - `data_protection_cert_pass` — replicated only together with
    its data-protection certificate, or not at all. E9 found no
    such certificate file in the verified all-in-one shape: no
    `.pfx`/`.p12` exists, the only certificate pair is the TLS
    pair `certificates/ssl/cert.pem` + `key.pem`, and the
    data-protection keys themselves persist in the database. In
    that shape the pass file therefore replicates on its own;
    where a certificate file does exist, pass and certificate
    move together or neither moves.
  - Any other file in `secrets/` is UNVERIFIED: snapshot apply
    must not silently include or exclude it; it is replicated
    only when explicitly classified in the deployment's setup
    notes before the first failover.
- The environment configuration (private/hidden email domains,
  SMTP TLS toggles; `.env.example:70-80,87-109`, research S5):
  a standby promoted without it behaves as a different email
  instance.
- Database-internal server state travels inside the database
  export automatically: SRP salts/verifiers and unlock methods,
  per-user signing keys (`UserSigningKeys`, research S5/S6), and
  inbound-email state including `EmailDecryptionKey` rows (wrapped
  keys, research S5). None of it is handled separately.

Must never diverge between the primary and a promoted standby:

- The JWT signing material (above): one logical instance, one key,
  replicated — never independently regenerated on a standby while
  its database is a replica. Regenerating it is a deliberate,
  operator-visible act that invalidates all sessions, never a
  side effect of failover.
- SRP verifiers and user signing keys: these live in the database,
  so they cannot diverge unless the databases do — which is
  exactly what fork detection (decision B) guards.

Node-local, explicitly not overwritten by replication:

- `secrets/postgres_password`: the node's own Postgres cluster
  password. Snapshot apply never overwrites it, even though E9
  variant (i) (whole `secrets/`, including this file) and
  variant (ii) (all except this file) both passed all four
  checks (starts OK, `GET /api` 200 OK, DB connect OK,
  credential readback OK) — a logical `pg_dump` restore does not
  change a cluster's password, so the file-level rule keeps it
  node-local regardless of the all-in-one result. A configurable
  node-local list in the agent is a follow-up, not this rule.
- `certificates/`: a standby serving under its own MagicDNS name
  (tailnet.md S4) uses its own certificates. Replication of
  `certificates/` applies only if a promotion preserves the
  original node's serving name; the deployment's setup notes must
  record which rule applies before the first failover.
  `UNVERIFIED:` which certificates upstream places in
  `/certificates` for the all-in-one shape beyond the mount itself
  (research S2/S5) — the setup-notes step, not the agent, owns
  this choice.

## D. Tailnet identity and ACL model

Incorporates `docs/design/tailnet.md` by reference; the ADR adds
no second model.

- Identity is the Tailscale node key: names, addresses and
  MagicDNS names are locators only; a changed key is a new,
  unapproved peer (tailnet.md S2).
- Mesh nodes carry `tag:aliasvault-mesh`; the only ACL grant is
  mesh-to-mesh on the sync HTTPS port; only tailnet admins are tag
  owners; removing a node means removing the tag/device *and* its
  key from every peer allowlist (tailnet.md S3).
- Each node binds its agent to loopback only and publishes it with
  `tailscale serve` (real CA certificate, key stays in the
  Tailscale client); Funnel is banned for the sync endpoint, the
  AliasVault UI/API, and every data or admin surface on a vault
  machine (tailnet.md S4/S5).
- Discovery is read-only behind the peer-list interface (fake
  implementation for tests, real one parsing
  `tailscale status --json`); the agent never changes tailnet
  configuration (tailnet.md S1, project rules).
- The sync layer still authenticates peers at the application
  level (snapshot manifest exchange is bound to the allowlisted
  node key); a reachable port is never proof of identity
  (tailnet.md Principles).

## E. Windows node (WSL2 / Docker Desktop)

Research S8: there is no native Windows server install; upstream
targets 64-bit Linux with Docker. The Windows test machine
therefore runs the same Linux images inside a Linux layer —
Docker Desktop in Linux-container mode (WSL2 backend), a WSL2
distro, or a Linux VM; the upstream installer is a bash script
whose architecture and Docker checks see the WSL environment, not
Windows (`install.sh:624-640,647-677`).

- All four state directories (`database/`, `secrets/`,
  `certificates/`, `logs/`) live inside the Linux filesystem with
  Linux ownership, never on a Windows drive with translated
  permissions. The documented Postgres fix applies on first start
  under WSL: `sudo chown -R 999:999 ./database/postgres && sudo chmod -R 700 ./database/postgres`
  (research S8).
- Data-level interchangeability follows: a WSL2 instance and a
  Linux instance holding copies of the same snapshot are the same
  instance as far as replication is concerned.
- Rollout checks specific to Windows (Serve behaviour without an
  interactive session, Docker Desktop/WSL2 versions, firewall/NAT
  for ports 25/587) are decision H item 8: they gate the Windows
  rollout and are run by the operator of that machine, not by the
  sync agent.

## F. Rollback plan (cannot lose data)

- The agent never writes to the primary's data. Rolling back the
  agent itself is: stop the agent on every node. Upstream
  AliasVault instances are untouched and keep working standalone —
  the mesh layer is additive only.
- Snapshots are append-only on the standby. A new snapshot lands in
  staging, is verified (checksums + manifest), and only then
  replaces the standby's previous data directories; the previous
  verified snapshot is retained until the new one has both
  verified and survived a standby start test. Discarding a bad or
  partial snapshot is always safe: the primary still holds the
  source data.
- Failover rollback (demoting a promoted standby): stop its
  AliasVault, mark it standby again, re-seed it from the primary.
  If it accepted writes while promoted, demotion is a fork
  (decision B) and follows the fork procedure — it is never
  resolved by copying the primary over it.
- Upstream upgrades are out of the agent's control and follow
  upstream's own guidance (database export first, research S7).
  The mesh layer pins the upstream image tag per deployment so a
  rollback is a tag change plus the pre-upgrade snapshot.
- No rollback step in this plan deletes the only copy of anything:
  every destructive action requires a verified second copy to
  exist first.

## G. License and trademark — explicit decision

Facts: upstream is AGPL-3.0-only (`LICENSE.md`, research S1), with
a separate trademark policy (`TRADEMARKS.md`: the AliasVault name
may be used truthfully and non-misleadingly; nothing may suggest
official status or endorsement; logos and brand assets are not
licensed by the AGPL).

Decision:

1. Clean implementation, permissive license. The mesh agent is
   written from the verified protocol facts only and contains no
   upstream source: no copying, linking or vendoring of
   `core/rust/vault_sync`, `core/rust/vault_merge`, or any other
   upstream code, in whole or in part. On that basis the project's
   own code is licensed MIT (chosen over Apache-2.0 for brevity;
   either stays available if a contributor policy later requires
   a patent grant). Consequence, stated plainly: this forfeits
   reuse of upstream's merge engine, which is why multi-writer
   merge is deferred (option O4/O5) — if a future design chooses
   to reuse upstream code, it must first re-decide this ADR, and
   the agent becomes AGPL-3.0 as a whole, with source-offer duties
   for anyone offered it over a network.
   `UNVERIFIED:` the exact legal boundary of API-level coupling
   (decision H item 1) — this decision rests on the research S1
   reading, not on legal advice; the conservative shape chosen
   here (no upstream code, unmodified upstream, API/operator
   surface only) is the one that stays defensible under any
   reasonable reading.
2. Unmodified upstream only. The project runs and integrates with
   the published upstream image/binary. It never patches, forks or
   redistributes AliasVault itself; deployment pins an upstream
   release tag and pulls it from upstream's registry.
3. Naming and trademark duties. The name `aliasvault-mesh` is used
   only as a truthful, independent-integration name. The repo
   README opens with: "Unofficial community project, not
   affiliated with or endorsed by AliasVault / XIVISOFT". The repo
   contains no AliasVault logos, icons, or screenshots of the
   branded UI, and nothing in docs or release text suggests
   official status, sponsorship or endorsement.
4. `docs/THIRD_PARTY.md` is a required deliverable of this design:
   it lists every upstream artifact the project touches and how.
   A first draft ships with this ADR; keeping it current is part
   of the definition of done for any change that touches a new
   upstream surface.

## MVP scope

In:

- Two nodes, all-in-one deployment shape, one user account with
  fake data until the rollout plan says otherwise.
- Peer discovery interface + fake implementation; real
  implementation parsing `tailscale status --json` (tailnet.md S1).
- Snapshot replication primary -> standby over the Serve endpoint:
  database export, `secrets/` (file-level rule, decision C),
  environment configuration,
  snapshot manifest (revisions + ciphertext hashes), staging +
  checksum verification + previous-snapshot retention.
- Fork detector: manifest comparison with the decision B outcomes
  (converged / behind / ahead / forked), halting replication on
  fork.
- Manual promotion and demotion runbooks implementing decisions B
  and F, including the clock check before any client-side merge.
- Status output that keeps tailnet failure causes distinct
  (offline / ACL / endpoint / identity / tailnet down, tailnet.md
  S6).

Out (post-MVP, each needs its own decision):

- Automatic failover, multi-writer sync, more than two nodes,
  the multi-container and any Kubernetes deployment shape,
  inbound-email-specific handling beyond whole-instance state.

## Test plan

No test in this plan touches real data. Every test that needs an
AliasVault uses a fresh throwaway instance: its own compose
project name, its own 127.0.0.1 ports in 18000-18999, a fake
account and fake data only, its own data directory under
`notes/instances/<name>`, torn down at the end
(`docker compose down -v`, data directory removed). Mesh behaviour
is simulated with Docker networks on one machine and the fake
discovery implementation; no test changes tailnet state.

Unit tests:

- Discovery: parser fixtures for `tailscale status --json`
  (present/missing/unknown fields, wrong tailnet, tag filtering).
- Fork detector: manifest pairs covering converged, behind, ahead,
  forked (same revision, different hash), and history-window cases.
- Snapshot manifest: checksum verification, staging promotion,
  previous-snapshot retention.

Integration tests (throwaway instances):

- Replicate: seed fake vault on the primary (items, an attachment
  blob, an email alias), snapshot, apply on the standby, open the
  standby vault with the fake account, diff item counts and blob
  hashes.
- Partition drill: cut the Docker network mid-replication; expect
  a paused, resumable sync and an unmodified primary.
- Fork drill: run two throwaways writable, edit the same fake vault
  on both, confirm the detector reports fork, replication halts,
  both branch snapshots are retained, and the decision B resolution
  procedure merges them with the upstream client.
- Node-loss drill: destroy the primary instance, promote the
  standby, confirm the vault at the last verified snapshot.
- Rollback drill: corrupt a staged snapshot (checksum failure) and
  confirm the previous verified snapshot still starts.

Experiments (resolve depended-on UNVERIFIED items; listed here,
not run in this phase). Amendment A5: experiments E1-E8 for the
UNVERIFIED items the design depends on are run FIRST by the
test harness on throwaway instances and their results written
to `docs/experiments.md` before the agent relies on them:

- E2 (item 2, health route): on a fresh throwaway all-in-one
  instance, probe the API root controller and record the exact
  health route and response payload; the agent's liveness check is
  written against the recorded payload only.
- E7 (item 7, db-export blob coverage): on a fresh throwaway
  instance with a fake attachment, run `install.sh db-export`,
  restore the export into a second fresh throwaway instance, and
  compare blob count and ciphertext hashes plus a fake-account
  vault open. If blobs are not fully covered, the snapshot format
  must add the missing blob store explicitly before MVP ships.
- E8 (item 8, Windows): not an automated experiment — the test harness does not touch
  other machines. It is an operator checklist on the Windows test
  machine (Serve without an interactive session, Docker
  Desktop/WSL2 versions, ports 25/587) whose results gate the
  Windows rollout step below.

## Rollout plan

1. Single node: agent installed beside a throwaway instance;
   snapshot/restore drills green.
2. Two throwaway instances on one machine (Docker networks, fake
   discovery): full test plan green, including fork and node-loss
   drills.
3. Two real machines, fake account and fake vault only: real
   discovery, Serve endpoint, ACL per tailnet.md S3; partition and
   promotion drills repeated on the tailnet. Amendment A1: the
   second machine runs a NEW, separate throwaway AliasVault
   compose project (distinct project name, distinct ports,
   distinct data directory) BESIDE any existing instance; the
   existing instance on that machine is never stopped, restarted,
   reconfigured, snapshotted or mounted by the agent or by any
   test, and may only be observed read-only (container list) by
   the operator. Check free ports and disk before
   starting.
4. Windows node joins (gated on E8): same fake-data drills from
   the Windows node as standby, then as primary. Amendment A1
   applies here too: a NEW, separate throwaway compose project
   beside any existing instance, which is never stopped,
   restarted, reconfigured, snapshotted or mounted; free ports
   and disk are checked before starting. Real vaults are touched
   only at step 5, by the operator, after a manual backup.
5. Real use: the operator points the runbooks at their real
   instances only after step 4, takes a manual whole-instance
   backup first (research S7), and keeps snapshots at the default
   retention until two clean promotion drills exist on real
   hardware.

## Rollback plan

See decision F. Summary: the mesh layer is additive; stopping the
agent is always a complete rollback of the sync feature, snapshots
are append-only and verified before promotion, and no step deletes
the only copy of any data.

## H. UNVERIFIED items from research.md

One row per `UNVERIFIED:` item in `docs/research.md` S1-S8 (the
S-preamble occurrence defines the marker and is not an item).

| # | Item | Section | Design depends on it? | Resolution / why not |
|---|---|---|---|---|
| 1 | Exact legal boundary of API-level coupling | S1 | Yes — the license decision (G) rests on the reading that running + API/operator-surface integration is a separate work | No throwaway experiment can resolve a legal question. Resolution is a human legal review before any public release beyond the conservative shape in G (no upstream code, unmodified upstream). The design is deliberately kept inside that shape so the dependency cannot silently widen |
| 2 | Exact health route and response payload | S2 | Yes — the agent's liveness check before replication and promotion needs a defined health signal | Experiment E2 (test plan): probe a fresh throwaway all-in-one instance and record route + payload |
| 3 | Any upstream Kubernetes or Helm deployment shape | S2 | No | MVP supports the two verified Docker shapes only; an unverified third shape cannot be a dependency |
| 4 | Full canonical table list and bucket category enum | S3 | No | Replication is whole-database plus whole directories, and fork detection uses the verified manifest/bucket revision + hash metadata (S3/S4); neither needs the enumerated tables or categories |
| 5 | Exact filename of the JWT key inside `secrets/` | S5 | No | Resolved by experiment E9 (2026-11-08): the filename is `jwt_key`; `secrets/` in the verified shape holds exactly `jwt_key`, `postgres_password` and `data_protection_cert_pass`, and decision C now replicates per file |
| 6 | Login/validate handshake field sequence | S6 | No | The MVP agent never authenticates as a vault user (decision A); merging is left to the user's own upstream client. A future multi-writer design (O4) would depend on it and must resolve it first |
| 7 | Client export/import formats; whether `db-export` covers blob storage completely | S7 | Yes — the MVP snapshot is built on `install.sh db-export` | Experiment E7 (test plan): db-export on a fresh throwaway with a fake attachment, restore into a second throwaway, compare blob count/hashes. Client export/import formats are not depended on: the design never uses them |
| 8 | Windows production guide; Docker Desktop/WSL2 version floor; firewall/NAT for ports 25/587; ARM Windows | S8 | Yes for the Windows rollout gate (versions, Serve behaviour, ports); No for ARM Windows and for the missing production guide as such | Operator checklist E8 on the Windows test machine (automated tests never touch other machines), results recorded in the deployment setup notes before rollout step 4. ARM Windows is out of scope: the design targets the verified x86_64/aarch64 Linux layer only, and the test machines are not ARM Windows hosts |

## Amendments (accepted 2026-11-08)

ADR 0001 is APPROVED as option O3 (single-writer, whole-instance
snapshot replication, manual failover) WITH these amendments:

- A1 SAFETY OF EXISTING INSTANCES. Rollout steps 3 and 4: the second machine
  runs a NEW, separate throwaway AliasVault compose project
  (distinct project name, distinct ports, distinct data
  directory) BESIDE any existing instance. The existing instance
  on that machine is never stopped, restarted, reconfigured,
  snapshotted or mounted by the agent or by any test; it may only
  be observed read-only (container list) by the operator.
  Check free ports and disk before starting. Real vaults are
  touched only at step 5, by the operator, after a manual backup.
- A2 STATE THE TRADEOFF IN THE README AND THE ADR SUMMARY. O3 is
  single-writer replication with manual failover: any node can be
  the primary, standbys hold a verified recent copy, devices
  point at the primary's tailnet address. It is NOT live
  multi-writer sync: a ciphertext-only agent cannot merge rows.
  O4 (multi-writer with the user's unlocked client doing the
  merge) stays documented as future work with its open questions
  (H items 6 and 7).
- A3 SNAPSHOT HANDLING. Snapshot archives and staging areas are
  created with owner-only permissions (0600/0700, the Windows ACL
  equivalent), live only under the agent's own data directory,
  are never written to logs or argv, are transmitted only over
  the tailnet endpoint, and the snapshot manifest never contains
  ciphertext or secrets. Contents of `secrets/` are treated as
  secrets everywhere.
- A4 IMPLEMENTATION POLICY. Language: Rust (single static binary
  for macOS, Linux and Windows; our own code, no upstream code).
  Few, well-known dependencies, each listed with its license in
  `docs/DEPENDENCIES.md`; no telemetry, no network access except
  the tailnet peer and the local `tailscale status --json` CLI;
  the discovery and the upstream operator surface (container
  lifecycle, db-export) sit behind traits with fake
  implementations so every test runs without a tailnet and
  without real vault data. Licence of our code: MIT OR
  Apache-2.0 (no upstream code inside).
- A5 TEST PLAN. Experiments E1-E8 for the UNVERIFIED items the
  design depends on are run FIRST by the test harness on
  throwaway instances and their results written to
  `docs/experiments.md` before the agent relies on them.
