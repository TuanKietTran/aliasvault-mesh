# Dependencies

This file lists every crate in `Cargo.lock`, with the exact version resolved in the lockfile, its license, and whether it is a direct dependency of this project.

Regenerate this file by running `cargo metadata --offline --format-version 1` and rebuilding the table below from its `packages` and `resolve` output, rather than editing the table by hand.

RUSTSEC advisories were NOT checked offline (cargo audit is unavailable here) and should be run in CI.

| Crate | Version | License | Direct |
| --- | --- | --- | --- |
| avmesh | 0.1.0 | MIT OR Apache-2.0 | workspace (this project) |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | no |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | no |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | no |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | no |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | no |
| digest | 0.10.7 | MIT OR Apache-2.0 | no |
| errno | 0.3.14 | MIT OR Apache-2.0 | no |
| fastrand | 2.5.0 | Apache-2.0 OR MIT | no |
| generic-array | 0.14.7 | MIT | no |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | no |
| itoa | 1.0.18 | MIT OR Apache-2.0 | no |
| libc | 0.2.191 | MIT OR Apache-2.0 | no |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | no |
| memchr | 2.8.3 | Unlicense OR MIT | no |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | no |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | no |
| quote | 1.0.47 | MIT OR Apache-2.0 | no |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | no |
| rustix | 1.1.6 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | no |
| serde | 1.0.229 | MIT OR Apache-2.0 | yes |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | no |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 | no |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | yes |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | yes |
| syn | 3.0.9 | MIT OR Apache-2.0 | no |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | yes |
| typenum | 1.20.1 | MIT OR Apache-2.0 | no |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | no |
| version_check | 0.9.5 | MIT/Apache-2.0 | no |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | no |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | no |
| zmij | 1.0.23 | MIT | no |

## Notes

- Direct dependencies are declared in `Cargo.toml`: `serde`, `serde_json`, and `sha2` at runtime, plus `tempfile` for tests only.
- The transport layer uses only `std::net`, and peer discovery shells out to the operator-installed `tailscale` CLI rather than linking a Tailscale library, so neither adds a crate here.
- `.github/workflows/ci.yml` uses `actions/checkout@v4`, referenced by major-version tag with no verified commit SHA; actions are CI tooling, not code dependencies, and are recorded here only so the exception is explicit.
- The project itself is licensed MIT OR Apache-2.0 (see `LICENSE-MIT` and `LICENSE-APACHE`).
