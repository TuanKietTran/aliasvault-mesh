# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through this repository's GitHub Security Advisories: open the repository's **Security** tab and choose **Report a vulnerability**. Private advisories keep the report, and any proof of concept, visible only to the maintainers until a fix is available.

Please do not open a public issue for a vulnerability, and do not include real vault data, real instance secrets, or anyone's master password in a report. A report with a fake account on a throwaway instance is enough to reproduce almost anything — this project's tests use fake data only, and reports should too.

## What to include

- The affected version or commit.
- What an attacker can do: what they must already have (a tailnet seat, a node key, local access) and what they gain.
- Reproduction steps against a throwaway instance with fake data.
- Any suggested fix, if you have one.

## Scope notes

Findings in these areas are especially valuable:

- Anything that lets the agent, its logs, its snapshots, or its network traffic expose plaintext vault data, secret material, or a master password or derived key. By design the agent handles ciphertext only and never sees a master password; a counterexample is a critical report.
- Snapshot handling: permissions, staging, verification, or retention mistakes that could destroy the only copy of data or accept an unverified snapshot.
- Peer authentication: anything that lets a node that is not on the allowlist be treated as a mesh peer, or that treats a reachable port as proof of identity.
- Fork handling: any path where replication overwrites a divergent branch instead of halting.

Vulnerabilities in upstream AliasVault itself should be reported to the upstream project through its own security process; see `docs/THIRD_PARTY.md` for the boundary between this project and upstream.

## Supported versions

This project is pre-release (design accepted, MVP in progress). Security fixes land on the main development line; there are no maintained release branches yet.
