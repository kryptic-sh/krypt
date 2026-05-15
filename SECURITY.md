# Security Policy

## Supported versions

krypt is pre-1.0. Only the latest 0.0.x patch release receives security fixes.
Once 0.1.0 ships, the latest minor receives fixes; older 0.x minors are
best-effort.

## Reporting a vulnerability

**Do not open a public GitHub issue for security reports.**

Email `mxaddict@kryptic.sh` with:

- Affected crate(s) and version(s)
- Description of the issue and impact
- Reproduction steps or proof-of-concept
- Disclosure timeline preference

Acknowledgment within 72 hours. Coordinated disclosure window is typically 30
days from acknowledgment, extendable for complex issues.

## Out-of-scope features

The following are **deferred** and will require explicit opt-in when
implemented:

- `[[command]]` step runner executing arbitrary shell commands
  ([#23](https://github.com/kryptic-sh/krypt/issues/23) — unshipped; will be
  opt-in only when it lands)
- Lifecycle hooks running arbitrary shell scripts (#23-related)

Until those land, krypt does not execute external code under any input. Today's
attack surface is limited to: parse TOML, copy files, resolve environment
variables — no exec.

## Dependencies

`cargo deny` runs in cron CI checking RUSTSEC advisories. Vulnerable transitive
dependencies trigger an issue automatically.
