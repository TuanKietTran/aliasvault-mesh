# AliasVault upstream research

Verified against a depth-1 clone of https://github.com/aliasvault/aliasvault
(kept outside this repo at `notes/upstream`) and https://docs.aliasvault.com.
Citations are upstream file paths with line numbers, or URLs. Anything not
confirmed in the source is marked `UNVERIFIED:`.

## 1. License

- Upstream is licensed under the GNU Affero General Public License v3:
  `LICENSE.md:1` is headed "GNU AFFERO GENERAL PUBLIC LICENSE, Version 3,
  19 November 2007" (659 lines total; closing reference to
  https://www.gnu.org/licenses/ at `LICENSE.md:660`). SPDX identifier:
  `AGPL-3.0-only` (the file is the verbatim AGPL-3 text).
- What AGPL-3 adds over GPL-3 is the network clause (AGPL section 13):
  offering a *modified* version for remote network interaction obliges the
  operator to offer those users the modified source.
- A separate public repo that only *runs* an unmodified AliasVault and
  *talks to it over its API* is a separate work: it may pick its own
  license for its own code, and no copyleft reaches that code merely from
  running or calling the AGPL program. Picking a permissive license
  (MIT/Apache-2.0) or a copyleft one for the new repo's own code is
  compatible with that use.
- Obligations do attach if the new repo copies or modifies upstream code,
  or distributes a combined work containing it: the upstream parts (and,
  for a combined work, the whole) must then meet AGPL-3 terms, including
  source offer.
- `UNVERIFIED:` the exact legal boundary of API-level coupling (no legal
  review done; this is a reading of the license text, not advice).

## 2. Deployment shapes

- Two Docker shapes are shipped. The multi-container shape is the root
  `docker-compose.yml`, with seven services: Postgres
  (`docker-compose.yml:2-3`, image `ghcr.io/aliasvault/postgres:latest`),
  client (`docker-compose.yml:21-22`), api (`docker-compose.yml:29-30`),
  admin (`docker-compose.yml:44-45`), reverse-proxy
  (`docker-compose.yml:59-60`), smtp (`docker-compose.yml:77-78`) and
  task-runner (`docker-compose.yml:94-95`). Postgres carries a compose
  healthcheck (`docker-compose.yml:14`).
- The all-in-one shape is a single service using image
  `ghcr.io/aliasvault/aliasvault:latest` with `restart: unless-stopped`
  (`dockerfiles/docker-compose.all-in-one.yml:8,10`). It publishes
  80/443 (web) and 25/587 (SMTP) (`dockerfiles/docker-compose.all-in-one.yml:13-16`)
  and bind-mounts four state directories: `./database:/database`,
  `./logs:/logs`, `./secrets:/secrets`, `./certificates:/certificates`
  (`dockerfiles/docker-compose.all-in-one.yml:19-22`).
- Inside the all-in-one image the processes are supervised by s6-overlay:
  the Dockerfile downloads s6-overlay as its own stage
  (`dockerfiles/all-in-one/Dockerfile:88,96-97`), and per-service run
  scripts live in `dockerfiles/all-in-one/s6-scripts/` for admin, api,
  client, init, nginx, notification, postgres, smtp and taskrunner.
- Health surface: the API has a root controller whose doc comment says it
  "contains health check endpoints"
  (`apps/server/AliasVault.Api/Controllers/RootController.cs:15`).
  `UNVERIFIED:` the exact health route and response payload (not re-read
  in this pass).
- `install.sh` is the operator entry point: it checks for a working
  `docker compose` (`install.sh:712`) and resolves the latest release tag
  from the GitHub releases API (`install.sh:1046`).
- `UNVERIFIED:` any upstream Kubernetes or Helm deployment shape (none
  was verified in the upstream tree in this pass).

## 3. Vault data model

- On a device, an unlocked vault is a SQLite database held in memory by
  the Rust core; on the server the same vault is a set of encrypted
  documents. Two client-side functions convert between the two:
  *canonicalize* (local tables to storage format) and *materialize*
  (storage format back to local tables), implemented in
  `core/rust/src/vault_codec/` (`docs/docs/security-architecture/vault-format.mdx:13-18`).
- Only the canonical model is stored, so the local schema is a client
  choice; a client that meets tables or columns it does not know keeps
  them in a local overflow row and writes them back unchanged, so an
  older client never removes data a newer one added
  (`docs/docs/security-architecture/vault-format.mdx:37`).
- Before encryption, every manifest and data bucket is written as
  canonical JSON (object keys sorted, ids lowercase) inside an envelope,
  gzipped, then encrypted as one whole ciphertext with AES-256-GCM,
  with the manifest id as associated data
  (`docs/docs/security-architecture/vault-format.mdx:44,46,60,66`).
  The server therefore stores one opaque document per vault part and
  cannot read the items, fields, URLs or folders inside it.
- A vault splits into separately encrypted parts: a *manifest* (one per
  vault, personal or shared: items, fields, folders, tags, passkeys and
  other tables, encrypted under that vault's VEK), data buckets, and
  *blobs* (one attachment or logo each, under its own random blob key
  which is itself encrypted with the VEK)
  (`docs/docs/security-architecture/vault-format.mdx:52-58`).
- Server-side, a manifest row (`VaultManifest`, keyed by `ManifestId`,
  owned by a group via `OwnerGroupId`:
  `apps/server/Databases/AliasServerDb/VaultManifest.cs:24,29`) carries
  the ciphertext and its metadata on `VaultManifestBase`:
  `ManifestBlob` (byte[]), `ManifestCiphertextHash`, `StorageFormat`,
  `Version`, `KeyVersion`, `RevisionNumber`, `Salt`, `Verifier`,
  `CredentialsCount`, `EmailClaimsCount`, `EncryptionType`,
  `EncryptionSettings`
  (`apps/server/Databases/AliasServerDb/VaultManifestBase.cs:43-120`).
- A bucket row carries `EncryptedData` (required byte[]), `KeyVersion`,
  `RevisionNumber` and `CiphertextHash`
  (`apps/server/Databases/AliasServerDb/VaultDataBucketBase.cs:21-37`)
  and links back to its manifest with a `Category`
  (`apps/server/Databases/AliasServerDb/VaultDataBucket.cs:20,31`).
- The server DB context exposes manifests, buckets, blob objects and
  blob references, plus history tables for both manifests and buckets
  (`apps/server/Databases/AliasServerDb/AliasServerDbContext.cs:76,172,178,183,188`).
- `UNVERIFIED:` the full canonical table list and the bucket category
  enum values (not enumerated in this pass).

## 4. Sync protocol (v2)

- Sync is client-merge, not server-merge. The server never merges: on a
  write it either accepts the whole vault or rejects it whole. A POST to
  `v2/Vault` whose `CurrentRevision` is stale (server `RevisionNumber`
  >= client `CurrentRevision + 1`) is rejected with
  `VaultWriteStatus.Outdated`, returning the current manifest and bucket
  revisions so the client can resync (`apps/server/AliasVault.Api/Controllers/V2/VaultController.cs`,
  POST handler ~L380-406; routes on the same controller: GET/POST `` at
  L48/L265, `manifest/{id}` L149, `storage` L214, `blobs` L603,
  `blobs/missing` L653, `blob-keys` L689, `blobs/download` L718,
  `email-claims/transfer` L749).
- On an accepted write the server only increments the revision number
  and archives the previous revision to the history tables, keeping the
  last 3 (`apps/server/AliasVault.Api/Helpers/VaultRevisionWriter.cs:67,89`,
  keep-3 at L29/41).
- Change detection is revision-based, not diff-based. `GET v2/Status`
  returns a `StatusResponse` with `clientVersionSupported`,
  `serverVersion`, per-manifest and per-bucket revision maps and the
  `srpSalt` (`core/rust/src/vault_sync/types.rs:477-514`; the server
  builds the manifest revisions in `VaultStatusHelper.cs`). `GET v2/Vault`
  returns the full manifests (ciphertext, ciphertext hash, revision,
  blob references, key type) and buckets (revision).
- The write shape is `VaultWriteRequest`: username, `manifests[]`
  (manifest id, ciphertext, ciphertext hash, current revision,
  credentials count, blob references) and `buckets[]` (manifest id,
  category, ciphertext, ciphertext hash, current revision), plus
  optional email-routing and migration blocks
  (`core/rust/src/vault_sync/types.rs:618-650`).
- The client engine runs: status check, then pull-and-merge, then push;
  on repeated `Outdated` rejections it retries the resync at most
  `MAX_OUTDATED_RESYNCS = 3` times and then gives up
  (`core/rust/src/vault_sync/engine.rs:1,19,390-433`).
- The merge itself (`merge_canonical`,
  `core/rust/src/vault_merge/mod.rs:73,121,217`) is per-row
  last-writer-wins on `UpdatedAt`: the incoming row wins only if it is
  strictly newer; ties keep the base (server snapshot) row. The winning
  row is overlaid with the base row's identity columns and any columns
  this schema does not know. Item deletes are resolved as a unit against
  the other side's latest item-plus-child-row activity
  (`core/rust/src/vault_merge/item_deletes.rs`).
- If the merge fails, or the local vault predates the manifest format
  (pre-v1), the client falls back to taking the server vault wholesale
  (`core/rust/src/vault_sync/merge.rs:33,53-56`).
- Consequence for a mesh design: two nodes must never write the same
  vault to one server concurrently and expect the server to reconcile
  them; reconciliation exists only in the client merge, and a mesh
  layer has to reuse it or serialize writers per vault.

## 5. External state and secrets (outside the vault ciphertext)

- A running instance keeps much more than vault ciphertext. The vault
  rows sit in Postgres alongside user accounts, SRP credentials,
  refresh tokens, ASP.NET Data Protection keys, groups, email and
  server settings (`apps/server/Databases/AliasServerDb/AliasServerDbContext.cs:44,49,65,87-107,147`).
  Syncing or backing up the vault tables alone does not move an
  instance.
- File-based secrets live in one directory, `/secrets` by default and
  relocatable via the `SECRETS_PATH` environment variable
  (`dockerfiles/all-in-one/s6-scripts/init/script:9-10`,
  `dockerfiles/all-in-one/scripts/reset-admin-password.sh:13-14`,
  `.env.example:40`).
- The Postgres password is a file in that directory,
  `${SECRETS_PATH}/postgres_password`, read at startup by every service
  run script (api, admin, smtp, client, taskrunner:
  `dockerfiles/all-in-one/s6-scripts/api/run:9` and siblings). One
  shared secret therefore gates the whole database.
- The admin account is also file-based: its password hash lives at
  `${SECRETS_PATH}/admin_password_hash` (checked by
  `dockerfiles/all-in-one/s6-scripts/notification/run:36`) and is reset
  by `dockerfiles/all-in-one/scripts/reset-admin-password.sh`, not via
  the database.
- API auth tokens are JWTs (`apps/server/AliasVault.Api/Models/IssuedTokens.cs:13`)
  signed with a symmetric key the API reads "from the container secrets
  or environment variables"
  (`apps/server/AliasVault.Api/Controllers/V1/AuthController.cs:716-719,737-738`).
  Losing or rotating that key invalidates sessions, but not vault data.
- Per-user signing keys used by the v2 API are database state, fetched
  from `UserSigningKeys` (`apps/server/AliasVault.Api/Controllers/V2/VaultController.cs:779`,
  `apps/server/AliasVault.Api/Controllers/V2/VaultKeyController.cs:70`).
- Inbound email is server state, not vault content: messages, parts,
  attachments and claims are their own tables, and each email's
  symmetric decryption key is stored encrypted in an
  `EmailDecryptionKey` row, wrapped against a vault manifest delivery
  key (`apps/server/Databases/AliasServerDb/EmailDecryptionKey.cs:20,31,42`).
- Operator configuration is environment-driven: private email domains,
  hidden domains and SMTP TLS toggles are all env vars
  (`.env.example:70-80,87-109`), and TLS certificates live in the
  separate `/certificates` mount (section 2). None of this travels with
  a vault export.
- `UNVERIFIED:` the exact filename the JWT key takes inside the secrets
  directory (the code path accepts secrets or env; the file layout was
  not enumerated in this pass).

## 6. Authentication (SRP) and unlock methods

- Login uses SRP-6a (Secure Remote Password): the client proves
  knowledge of the password without sending it, and the server checks
  the proof against a stored verifier. The client side lives in the
  Rust core and the server side in the .NET API, with wire formats kept
  compatible with the `SecureRemotePassword` .NET package
  (`core/rust/src/crypto/srp.rs:1-5`). The group is the 2048-bit SRP
  group (`core/rust/src/crypto/srp.rs`, `srp::groups::G_2048`; padded
  values are 256 bytes).
- Client SRP operations are: generate salt, derive the private key from
  (salt, identity, password hash), derive the verifier, generate the
  ephemeral pair, derive the session (proof + shared key), and verify
  the server's proof (`core/rust/src/crypto/srp.rs:92,98,112,122,132,166`).
- What the server stores per unlock method is a `UserSrpCredentials`
  record: `Salt`, `Verifier`, `EncryptionType` and
  `EncryptionSettings`, plus the unlock method id
  (`apps/server/Databases/AliasServerDb/UserSrpCredentials.cs:15-20`).
  `EncryptionType` names how the client derives the SRP input from the
  password: `Argon2IdHkdf`, or legacy `Argon2Id`; `EncryptionSettings`
  holds that KDF's parameters. For legacy users the same salt/verifier
  pair still lives on the vault manifest itself
  (`VaultManifestBase.Salt/.Verifier`, section 3).
- A password is only one *unlock method*. Each `UserUnlockMethod` row
  has a type, a vault key algorithm, and the account key encrypted
  under that method, with a key version
  (`apps/server/Databases/AliasServerDb/UserUnlockMethod.cs:41,47,59,64`),
  so additional methods (for example a recovery or passkey-style
  method) wrap the same account key instead of re-encrypting vaults.
- The v2 auth API is split into steps: `login`, then `validate`, with
  separate `validate-2fa` and `validate-recovery-code` steps, plus
  `refresh`, `revoke` / `revoke-token` and `register`
  (`apps/server/AliasVault.Api/Controllers/V2/AuthController.cs:65,123,159,205,255,322,365,400`).
  Successful validation issues the JWT from section 5; refresh tokens
  are stored server-side and revocable.
- Password change and KDF upgrades are explicit two-step flows
  (`change-password/initiate` + `change-password`,
  `key-derivation-upgrade/initiate` + `key-derivation-upgrade`:
  `apps/server/AliasVault.Api/Controllers/V2/AuthController.cs:554,581,678,700`),
  and a change submits a `new_verifier` to the server
  (`core/rust/src/vault_sync/types.rs:57`). The SRP salt is also exposed
  via the v2 Status response (section 4).
- Consequence for a mesh design: nodes authenticate *users* against
  one server's SRP verifier; there is no node-to-node credential in
  this protocol, so mesh peer trust has to come from elsewhere (for
  example the tailnet, see the architecture notes).
- `UNVERIFIED:` the exact request/response field sequence of the
  login/validate handshake (routes verified; message bodies not traced
  end to end in this pass).

## 7. Backup, restore and HA

- The supported backup unit is the whole instance, not the vault: the
  database, the `/secrets` directory, the `/certificates` directory and
  the environment configuration (sections 2 and 5). The installer's
  own update flow tells the operator to dump the database first:
  `./install.sh db-export > backup.sql.gz` (`install.sh:2551`), and it
  backs up `docker-compose.yml` before modifying it
  (`install.sh:1581-1582`) and warns to back up the generated
  credentials (`install.sh:1721`). Database operations are documented
  in `docs/docs/installation/docker-compose/advanced/database.mdx`.
- Restore is therefore: reinstall or restart the containers, restore
  the Postgres dump, and restore the secrets and certificates
  alongside it. A vault-only copy is not a restore: users, SRP
  verifiers, email and the JWT key all live outside the vault tables
  (section 5).
- Short-term recovery inside the database comes from the history
  tables: every accepted vault write archives the previous manifest
  and bucket revisions, keeping the last 3 (section 4,
  `VaultRevisionWriter.cs:29/41`). Retention is configurable and
  actively enforced: unlock-method history is pruned to a configured
  number of days (`apps/server/Databases/AliasServerDb/Retention/UnlockMethodHistoryPolicy.cs:22-26,54`),
  a blob retention policy exists alongside it
  (`apps/server/Databases/AliasServerDb/Retention/VaultBlobRetentionPolicy.cs`),
  and a daily Maintenance job runs under the task runner
  (`apps/server/AliasVault.TaskRunner/Workers/TaskRunnerWorker.cs:62,70`).
- HA: there is no upstream high-availability story. A search of the
  docs, README and ARCHITECTURE for replication, clustering or load
  balancing returned nothing; both deployment shapes run exactly one
  Postgres and one of each service, and the sync protocol assumes a
  single server-side revision counter per vault (section 4). Running
  two writable servers for one vault would fork that counter.
- Consequence for a mesh design: availability has to come from the
  client side (each node keeps its local vault and merges on reconnect,
  section 4) or from one-server failover of the whole instance, not
  from multi-writer servers.
- `UNVERIFIED:` client-side vault export/import formats and whether
  `db-export` covers the blob storage completely (docs page exists;
  its full text was not re-read in this pass).

## 8. Windows, Docker Desktop and WSL2

- There is no native Windows server install. Upstream's stated system
  requirements are 64-bit Linux (Ubuntu or RHEL-based recommended),
  1 vCPU / 1 GB RAM / 5 GB free disk, Docker CE 20.10+ and Docker
  Compose 2.0+, with ports 80 and 443 available (25 and 587 optional
  for own-domain email) (`docs/docs/installation/index.mdx:112-115`;
  also `README.md:73`). Both install shapes require Docker
  (`docs/docs/installation/index.mdx`, Self-host Install intro).
- So on a Windows machine the same Linux images run only inside a
  Linux layer: Docker Desktop in Linux-container mode (its WSL2
  backend), a WSL2 distro, or a Linux VM. The installer itself is a
  bash script that checks `uname -m` and accepts only
  x86_64/amd64/arm64/aarch64, then requires a reachable Docker daemon
  (`install.sh:624-640,647-677`) — inside WSL2 those checks see the
  WSL kernel/architecture, not Windows directly.
- Upstream's only Windows guidance is in the development docs, and it
  points the same way: tooling is built around Linux and bash scripts
  (e.g. `./scripts/dev.sh`), commands may need altering on Windows,
  and WSL is advised (`docs/docs/contributing/development/development-setup.md:9-10`);
  Docker Desktop is the listed Docker prerequisite there
  (`development-setup.md:16`).
- The concrete WSL2 gotcha upstream documents is filesystem
  ownership for Postgres: on first start under WSL, permission errors
  are fixed with `sudo chown -R 999:999 ./database/postgres && sudo chmod -R 700 ./database/postgres`,
  and WSL itself can be reset with `wsl --update` / `wsl --shutdown`
  (`development-setup.md:77-79`). That is the bind-mounted `/database`
  directory from section 2 landing on a WSL filesystem — keep the
  data directories inside the Linux filesystem, with Linux
  ownership, not on a Windows drive with translated permissions.
- Both install shapes share the same bind-mount directories
  (`/database`, `/certificates`, `/logs`, `/secrets`), so a Windows
  WSL2 instance and a Linux instance holding copies of those
  directories plus the database are interchangeable at the data
  level (section 7); nothing Windows-specific is stored.
- `UNVERIFIED:` upstream publishes no Windows-specific production
  install guide, no stated Docker Desktop/WSL2 version floor beyond
  the Linux Docker versions above, and no guidance on Windows
  firewall/NAT for ports 25/587 or on ARM Windows hosts.
