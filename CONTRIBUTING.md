# Contributing

Thanks for helping. This is a small project with a few hard rules that come from its design (ADR 0001, `docs/adr/0001-sync-design.md`); the rest is ordinary.

## Hard rules

- **No upstream code.** The agent is a clean implementation written from the verified protocol facts. Never copy, link, or vendor upstream AliasVault source, in whole or in part. If a change needs a new upstream surface, list it in `docs/THIRD_PARTY.md` in the same change.
- **Ciphertext only.** No change may give the agent access to a master password or any derived key, add a decryption step, or put ciphertext bodies or secret material into logs, command-line arguments, or the snapshot manifest.
- **No silent data loss.** Replication halts on a fork; it never overwrites a divergent branch. Every destructive step requires a verified second copy to exist first.
- **Fake data only.** Tests and experiments use throwaway instances with fake accounts and fake data, on loopback ports in the 18000–18999 range, torn down afterwards. Never test against a real vault or someone else's instance.
- **Docs honesty.** Documentation uses placeholder values only (for example `node-a`, `node-b`, `100.64.0.1`, `example.ts.net`). A command is written as real only when it has been verified; anything else is marked `UNVERIFIED` or `TODO: verified in experiment E<n>`.

## Development

The agent is a single Rust binary. Before opening a pull request, run the same checks CI runs:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Keep dependencies few and well-known; every dependency is listed with its license in `docs/DEPENDENCIES.md`. No telemetry, and no network access beyond the tailnet peer and the local `tailscale status --json` command.

## Licensing of contributions

The project is dual-licensed under MIT OR Apache-2.0. By contributing, you agree that your contribution is licensed under the same dual terms, and that it contains no upstream AliasVault code and no code you do not have the right to contribute under those terms.

## Pull requests

One topic per pull request, with the design reference (ADR section or runbook) named in the description when the change implements one. Update the docs in the same pull request as the behaviour they describe.
