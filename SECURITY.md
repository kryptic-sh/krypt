# Security Policy

## Supported versions

krypt is pre-1.0. Only the latest published release receives security fixes;
older releases are best-effort. See the
[releases page](https://github.com/kryptic-sh/krypt/releases) for what that
currently is.

## Reporting a vulnerability

**Do not open a public GitHub issue for security reports.**

Email `mxaddict@kryptic.sh` with:

- Affected crate(s) and version(s)
- Description of the issue and impact
- Reproduction steps or proof-of-concept
- Disclosure timeline preference

Acknowledgment within 72 hours. Coordinated disclosure window is typically 30
days from acknowledgment, extendable for complex issues.

## Threat model

**krypt executes programs named by your `.krypt.toml`.** Treat a dotfiles repo
the same way you treat a shell script you are about to run: only point krypt at
repos you trust.

Code-execution paths that exist today:

- `[[command]]` steps — `krypt <group> <name>`, `krypt menu <name>`
  (`krypt_core::runner`, `krypt_core::dispatch`).
- `[[hook]] when = "post-update"` — run automatically by `krypt update` unless
  `--skip-hooks` is passed (`krypt_core::update`).
- `[[deps]]` installs — `krypt deps` shells out to the detected package manager,
  and the pacman backend invokes `sudo` (`krypt_pkg::manager`).
- `krypt setup` — the `gitconfig` writer runs `git config`
  (`krypt_core::setup`).
- `krypt notify` and `notify` steps — spawn `notify-send` / `osascript` /
  `terminal-notifier` / `powershell` (`krypt_core::notify`).

Mitigations that are already in place:

- No shell is injected. `RealProcessExec` wraps `std::process::Command`
  directly, so `run = [...]` is an argv, not a command line — there is no
  word-splitting or metacharacter interpretation of step arguments.
- `--dry-run` on `link`, `unlink`, `relink`, `update`, `deps`, `setup`, `menu`,
  and the generic dispatcher prints the plan without spawning anything.
- `${VAR}` expansion in step arguments is resolved at config-load time against
  krypt's own variables, then the process environment, and errors out on an
  unknown name rather than passing it through.

Not yet available: any opt-in/confirmation gate before hooks or command steps
execute. If you want that, track or open an issue.

## Dependencies

`cargo deny` runs as its own CI job on every push to `main` and every pull
request, checking RUSTSEC advisories along with the license and source rules in
[`deny.toml`](deny.toml). Dependabot opens grouped dependency PRs weekly (see
[`.github/dependabot.yml`](.github/dependabot.yml)). Neither files issues
automatically — a failing `cargo deny` shows up as a red CI job.
