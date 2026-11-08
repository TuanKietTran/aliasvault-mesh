# Third-party software and how this project uses it

This project is an independent integration. It contains no
third-party source code. The upstream software it runs and talks
to:

## AliasVault (upstream)

- What: the self-hosted password and email-alias manager this
  project keeps in sync between machines.
- Source: https://github.com/aliasvault/aliasvault
- License: AGPL-3.0-only (`LICENSE.md` in the upstream repo).
  Trademarks are governed by upstream's `TRADEMARKS.md`; this
  project uses the AliasVault name only to refer to the upstream
  software, truthfully and without suggesting official status or
  endorsement. No AliasVault logos, icons or branded screenshots
  are included in this repository.
- How we use it:
  - We run the published, unmodified upstream Docker image
    (all-in-one shape, `ghcr.io/aliasvault/aliasvault`) pulled
    from upstream's registry, pinned to a release tag. We never
    patch, fork or redistribute it.
  - We use its operator surface only: the upstream installer
    (`install.sh`, including `db-export`) and the instance state
    directories it creates (`database/`, `secrets/`,
    `certificates/`, `logs/`).
  - We read the v2 sync protocol's published behaviour (status and
    revision semantics) as design input. We do not copy, link or
    vendor any upstream code, including `core/rust/vault_sync`
    and `core/rust/vault_merge`.
  - Merging of vault changes is performed by the user's own
    upstream client, never reimplemented from upstream source.

## Tailscale

- What: the mesh VPN providing peer discovery, identity and the
  HTTPS transport between nodes (`docs/design/tailnet.md`).
- How we use it: the sync agent reads local state with
  `tailscale status --json` and is published by the operator with
  `tailscale serve`. The agent never changes tailnet
  configuration. Tailscale is a separate product of Tailscale
  Inc., used under its own terms; nothing of it is redistributed
  here.
