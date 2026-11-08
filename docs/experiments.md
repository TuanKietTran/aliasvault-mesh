# Experiments E1-E9 — UNVERIFIED items from ADR 0001 §H

Date: 2026-11-08. Worker: w-harness (H2; E9 added in H3).
Method: throwaway all-in-one instances only (`mx-test-e7a`
127.0.0.1:18020, `mx-test-e7b` :18030, `mx-test-e7c` :18040;
E9: `mx-test-e9b-i` :18060, `mx-test-e9b-ii` :18070), fake
accounts and fake data only, torn down at the end. The
protected existing instance was never touched. Upstream
version observed: 0.31.2. Secret values were never read or
printed; file names only.

## Summary

| E | §H item | Depended on? | Verdict |
|---|---|---|---|
| E1 | Legal boundary of API-level coupling (S1) | Yes (decision G) | Not resolvable by experiment — see below |
| E2 | Health route and payload (S2) | Yes | RESOLVED: `GET /api` → 200 `OK` |
| E3 | Upstream Kubernetes/Helm shape (S2) | No | Not run, not a dependency |
| E4 | Full table list, bucket categories (S3) | No | Not depended on; incidental evidence recorded |
| E5 | JWT key filename in `secrets/` (S5) | No | Incidentally resolved: `jwt_key` (names only) |
| E6 | Login/validate handshake fields (S6) | No | Not run, not a dependency |
| E7 | `db-export` blob coverage (S7) | Yes | RESOLVED: a DB dump covers blobs completely |
| E8 | Windows guide / versions / ports (S8) | Rollout gate only | Not run — operator checklist, gates rollout step 4 |
| E9 | Snapshot `secrets/` apply rule (decision C amendment) | Yes (decision C) | RESOLVED: both variants pass all four checks |

## E1 — legal boundary (no experiment)

No throwaway experiment can resolve a legal question. Per ADR
§H.1 the resolution is a human legal review before any public
release, and the design is kept inside the conservative shape of
decision G (no upstream code, unmodified upstream) so the
dependency cannot silently widen. Nothing in H2 changes this.

## E2 — health route: RESOLVED

Probed on fresh throwaways (e7a, re-verified on e7c after the E7
restore cycle):

- `GET /api` → 200, `text/plain; charset=utf-8`, body `OK`
- `GET /api/` → 200, same payload
- `GET /api/health`, `GET /api/v1/health`, `GET /api/Health`,
  `GET /api/status` → 404
- `GET /health` and `GET /` → 200 `text/html` (the SPA, not an
  API health signal)
- `GET /api/v2/Vault` → 401, `GET /api/Auth/status` → 400
  (API is up; auth required / bad request, as expected)

There is no dedicated health route. The agent's liveness check
is defined as: `GET /api` expecting status 200 and body `OK`.

## E3 — Kubernetes/Helm shape: not run

Not depended on (§H.3): the MVP supports the two verified
Docker shapes only, so an unverified third shape cannot become
a dependency. No experiment was run.

## E4 — tables and bucket categories: incidental evidence

Not depended on (§H.4): replication is whole-database and fork
detection uses manifest/bucket revision + hash metadata. For
the record, observed on the throwaways: 39 tables in schema
`public`, including `VaultManifests`, `VaultDataBuckets` (+
`VaultDataBucketsHistory`), `VaultBlobObjects`,
`VaultBlobReferences`, `VaultManifestsHistory`,
`AliasVaultUsers`. Bucket categories observed in
`VaultDataBuckets.Category`: `Settings` and `Stats` — exactly
one of each per vault (e7a with 5 vaults: Settings 5, Stats 5;
1-vault instances: 1 and 1, 2 buckets total).

## E5 — secrets filenames: incidentally resolved (names only)

Not depended on (§H.5, decision C: the snapshot replicates
`secrets/` as a whole). Observed filenames in `/secrets`:
`data_protection_cert_pass`, `jwt_key`, `postgres_password`.
The JWT key file is therefore `jwt_key`. Values were never read.

## E6 — login/validate handshake: not run

Not depended on (§H.6, decision A): the MVP agent never
authenticates as a vault user; merging is left to the user's
own upstream client. The field sequence stays UNVERIFIED and
must be resolved before any future multi-writer design (O4).

## E7 — db-export blob coverage: RESOLVED

Question (§H.7): does `install.sh db-export` — in the
all-in-one shape, a `pg_dump` of the instance database (no
`install.sh` exists inside the container) — cover blob storage
completely?

Storage fact: blobs live in the database. `VaultBlobObjects`
holds `EncryptedData bytea` (PK `ManifestId, Hash`, plus
`EncryptedBlobKey`, `Category`, `SizeBytes`), referenced by
`VaultBlobReferences (ManifestId, RevisionNumber, BlobHash)`;
`VaultManifests.ManifestBlob` is also `bytea`. No filesystem
blob store exists.

UI finding: the 0.31.2 web UI exposes no attachment creation
path — the item create, detail and edit pages have no file
input and no attachment/logo control, and saving an item with a
website URL on a fresh instance produced 0 blobs (no favicon/
logo auto-blob). The fake attachment was therefore seeded
directly at the storage layer the export operates on.

Run (source e7b → target e7c):

- Seeded blob on e7b: 1 object, `Category=attachment`, 29 bytes,
  `Hash = dd7a988cc924fc714a4fd417ddda419300941af659f63de35eec5c2e65300588`
  (= SHA-256 of the payload; `sha256(EncryptedData)` matches),
  1 reference with the same hash; source totals manifests=1,
  users=1, buckets=2.
- Export: `pg_dump` exit 0, 99,909-byte dump containing `COPY`
  blocks for both `VaultBlobObjects` and `VaultBlobReferences`;
  the blob hash appears twice in the dump (object + reference).
- Restore: restored into e7c's scratch database `e7restored`
  and compared row for row: blobs=1 with identical hash and
  identical `sha256(EncryptedData)`, refs=1 with identical
  reference hash, manifests=1, users=1, buckets=2 — the blob
  survived the dump/restore bit-identically.
- Target function: e7c healthy per E2 (`/api` 200 `OK`) and a
  full fake-account cycle passed on it (`ok items=1 readback=1`).

Caveats: (1) restoring over e7c's live database was blocked —
the running app reconnected and held the database, so the
restore was verified into a scratch database on the same
instance and the restored vault was not opened through the app;
the app-level vault open was verified on the target's live
database with a fresh fake account. (2) Client export/import
formats were not tested: the design never uses them (§H.7).

Conclusion: a database dump covers blob storage completely,
because blobs are `bytea` rows in the same database. The
snapshot format needs no additional blob store.

## E8 — Windows: not run (operator checklist)

Not an automated experiment: the harness never touches other
machines. E8 is an operator checklist on the Windows test
machine (Tailscale Serve without an interactive session,
Docker Desktop/WSL2 versions, ports 25/587), recorded in the
deployment setup notes, and it gates rollout step 4. ARM
Windows is out of scope (§H.8).

## E9 — snapshot `secrets/` apply rule: RESOLVED (both variants pass)

Question (decision C amendment brief): decision C as written
replicates `secrets/` as a whole. Hypothesized risk:
`postgres_password` belongs to a node's own Postgres cluster;
a logical `pg_dump` restore does not change the cluster
password, so overwriting it with the primary's value could
lock the app out of its own database. A second question was
whether `data_protection_cert_pass` is meaningful only
together with a certificate file, and where that certificate
lives.

Certificate location (recorded before the variant runs):
there is no data-protection certificate file — no `.pfx` or
`.p12` anywhere on the instance. `data_protection_cert_pass`
lives in `secrets/`; the only certificate pair on the
instance is `certificates/ssl/cert.pem` + `key.pem` (the TLS
pair); the data-protection keys themselves persist in the
database. There is therefore no separate DP certificate to
replicate together with the pass file.

Method: one source snapshot from a throwaway A (a `pg_dump`
`dump.sql`, 99,453 bytes, plus `secrets/` holding exactly
`data_protection_cert_pass`, `jwt_key`, `postgres_password`
— names only). For each variant: fresh B via
`harness/instance.sh`, B stopped, a standalone helper
Postgres on B's data dir dropped/recreated B's database and
restored A's dump (`HELPER_RESTORE_ERRORS=0` in both runs),
secrets applied per variant, B started again. Four checks
each: (1) B starts (container running, not restarting,
restart count 0), (2) `GET /api` → 200 `OK` (E2 liveness),
(3) app connects to its DB (0 `password authentication
failed` lines and schema up-to-date line present in the app
log), (4) fake credential readback (the snapshot's fake
account verified login-only: `ok items=1 readback=1`).

- Variant (i) — secrets applied whole (`mx-test-e9b-i`,
  127.0.0.1:18060): starts OK, api OK, db OK, readback OK.
- Variant (ii) — secrets applied except `postgres_password`,
  B keeps its own (`mx-test-e9b-ii`, 127.0.0.1:18070):
  starts OK, api OK, db OK, readback OK.

The hypothesized lockout did not occur in the all-in-one
shape tested: overwriting `postgres_password` with the
source value left the app able to connect (0 auth-failure
lines). Excluding it is equally safe: variant (ii) passes
all four checks with B's own password untouched. Both
instances were torn down at the end of their runs.

Caveat: the helper's post-restore probe (`SELECT count(*)`
from `"Users"`) errored in both variants — the table is
`AliasVaultUsers` (see E4). The restore log itself contained
0 `ERROR` lines, and the app-level DB check and credential
readback on the restored data both passed, so this is a
probe-name error only, not a restore failure.

Conclusion: in the verified all-in-one shape, a snapshot
apply works with `secrets/` whole and with `postgres_password`
excluded; the file-level rule for decision C can rest on
either behaviour, with variant (ii) as the proof that
keeping `postgres_password` node-local does not break a
restore.

## Harness notes

- Reused instances hit the upstream registration rate limit
  (429 after 5 fake accounts on e7a); a fresh throwaway clears
  it. Experiments needing a clean account state used fresh
  instances (e7b, e7c).
- `harness/fake-account.mjs` needs a Playwright whose browser
  revision matches the local cache; the run above used a local
  Playwright install (Playwright 1.65.0, chromium revision 1254).
