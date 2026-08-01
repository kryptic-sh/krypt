# Migrating from stow + bash dotfiles

Practical guide for converting an existing stow-based, bash-scripted dotfiles
repo to a `.krypt.toml` config.

Canonical worked example:
[mxaddict/dotfiles](https://github.com/mxaddict/dotfiles) — an Arch + Hyprland
setup driven entirely from `.krypt.toml`.

> **krypt copies, it does not symlink.** `krypt link` reads `[[link]]` and
> `[[template]]` entries and writes a real copy of each source to its
> destination, preserving mtime and (on Unix) file mode. A manifest at
> `${XDG_STATE}/krypt/manifest.json` records a sha256 of every source and
> destination, which is how `krypt diff` spots drift and how `krypt link`
> decides an existing destination is safe to overwrite. The subcommand is called
> `link` for stow muscle-memory; nothing in krypt creates a symlink.

## Why migrate

If your dotfiles look like this:

```
.files/
├── .config/          (stowed into ~/.config/)
├── .local/bin/
│   ├── .update       (700-line orchestrator)
│   ├── .deps         (package install + post-install setup)
│   ├── .menu-power   (rofi/dmenu picker)
│   ├── .menu-wifi    (nmcli + picker)
│   └── ...
└── .stow-local-ignore
```

…then you have two problems:

1. **`.update` is fragile**: every new piece of logic (clone if missing,
   self-update via `exec`, stash + pop, conditional reload, etc.) means more
   bash, more edge cases, more "works on my machine" outcomes.
2. **Cross-platform is bash-shaped**: getting any of it to run on macOS or
   Windows means duplicate scripts or sprawling `if [[ "$(uname)" == Darwin ]]`
   trees.

krypt's deal: the bash gets demoted to "the part where shell is the right tool".
Everything else (clone, file deploy, deps, prompts, hooks, dispatchable
commands) is declared in TOML and run by a Rust binary.

## Conceptual mapping

| Bash / stow concept                                | krypt equivalent                                                                                 |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `git clone <url> ~/.files`                         | `krypt init <url>` (HTTPS remotes only)                                                          |
| `stow .`                                           | `[[link]]` entries deployed by `krypt link` — copies, not symlinks                               |
| `.update` orchestrator                             | `krypt update`                                                                                   |
| Interactive setup wizard (custom bash)             | `[prompts.*]` blocks + `krypt setup`                                                             |
| `pacman -S … && apt … && brew …` chains            | `[[deps]] group = "core" pacman = [...] apt = [...]`                                             |
| `if active; then reload; fi` post-install logic    | `[[hook]] when = "post-update" if = "command_exists:foo"`                                        |
| `.menu-*` rofi launcher scripts called by Hyprland | `[[command]] group = "menu" name = "..." steps = [...]`, invoked via `krypt menu <name>`         |
| `.batrep` / `.batlog` (generic OS concept)         | Built-in subcommand (`krypt battery`) — Rust, cross-platform                                     |
| `.kanata` / `.envup` (tool-specific bash)          | `[[command]]` shelling out to the existing bash script — script stays for shellcheck / `ft=bash` |

The rule of thumb: **stay in bash** when the logic is genuinely shell-shaped
(parallel `&` / `wait`, `case` statements over `$1`, piping `pacman` through
`awk`). **Absorb to Rust** only when the concept is generic and cross-platform
(battery, notifications). Inline bash inside TOML is the worst of both worlds —
no shellcheck, no syntax highlighting, escape-quote hell.

## Step-by-step migration

### 1. Install krypt

```sh
paru -S krypt-bin                  # Arch
brew install kryptic-sh/tap/krypt  # macOS
```

Windows and other channels — plus the caveat that crates.io currently trails
this repo — are covered in [the README's install section](../README.md#install).

Verify: `krypt --version`.

### 2. Add `.krypt.toml` at repo root

Minimum viable config:

```toml
[meta]
name        = "my dotfiles"
description = "Arch + Hyprland workstation"
# Optional: the oldest krypt you want this repo used with. Advisory only —
# `krypt update` prints a warning when the running binary is older, and
# nothing else consults it. Pick whatever version you actually tested on.
krypt_min   = "0.2.0"

[paths]
# All paths below resolve against $HOME by default.
# Override here if you need different roots.
```

`krypt validate` should already pass.

### 3. Replace `stow` with `[[link]]` entries

Inspect what stow would deploy:

```sh
stow -nv .   # dry-run, prints planned symlinks
```

Each line becomes a `[[link]]` entry. Optionally split into a dedicated
`.krypt/links.toml` and include it from the top-level config:

```toml
# .krypt.toml
include = [".krypt/links.toml"]

# .krypt/links.toml
[[link]]
src = ".config/hypr"
dst = "${XDG_CONFIG}/hypr"

[[link]]
src = ".gitconfig"
dst = "${HOME}/.gitconfig"
```

Run `krypt link` to deploy (copy). `krypt link --dry-run` to plan.
`krypt unlink` deletes every destination the manifest records, keeping drifted
ones unless you pass `--force`.

A destination that already exists and does **not** match the manifest is
reported as a conflict and skipped — `krypt link` will not clobber a file it
didn't write unless you pass `--force`. That is what makes hand-edited files
survive a re-link, and it's why `[[template]]` entries behave like "seeded
once": they deploy the same way as `[[link]]`, and the only extra thing
`[[template]]` gives you is a `prompts` list wiring the destination to
`krypt setup`:

```toml
[[template]]
src = ".gitconfig.local.template"
dst = "${HOME}/.gitconfig.local"
prompts = ["git"]   # references a [prompts.git] block
```

### 4. Replace `.deps` with `[[deps]]`

Group your packages by purpose. Each `[[deps]]` block maps one logical group
across distros:

```toml
# .krypt/deps.toml
[[deps]]
group = "core"
pacman = ["fish", "neovim", "tmux", "ripgrep", "fd", "stow"]
apt    = ["fish", "neovim", "tmux", "ripgrep", "fd-find"]
dnf    = ["fish", "neovim", "tmux", "ripgrep", "fd-find"]
brew   = ["fish", "neovim", "tmux", "ripgrep", "fd"]

[[deps]]
group = "hyprland"
required_platforms = ["linux"]
pacman = ["hyprland", "hyprlock", "hypridle", "waybar", "swaync"]
# Other distros: empty arrays mean "not available here, skip".
```

`krypt deps` installs missing packages from every group. `required_platforms`
gates the whole group; per-distro empty arrays gate individual managers.

### 5. Replace interactive wizard with `[prompts.*]`

If your `.setup` script asked the user for git name/email/etc., that becomes a
prompts block:

```toml
[prompts.git]
heading = "Git identity"
writer  = "gitconfig"

[[prompts.git.fields]]
key          = "user.name"
prompt       = "Your full name"
default_from = "git:user.name"

[[prompts.git.fields]]
key          = "user.email"
prompt       = "Your email"
default_from = "git:user.email"
```

`krypt setup` runs every prompts block (or just the ones named by
`--prompts a,b`); the `writer = "gitconfig"` directive makes krypt write the
answers via `git config` instead of into a template file. Other writers:
`hypr_vars`, `env`, `generic_template`.

`krypt setup` does **only** this. It does not install `[[deps]]` and it does not
deploy anything — despite the name, it is not a first-run "do everything"
command. The full first run is `krypt init` → `krypt deps` → `krypt setup` →
`krypt link`.

### 6. Replace post-install reloads with `[[hook]]`

Wherever your `.update` script does things like "rebuild bat cache", "reload
hyprland", "install tmux plugins", that's a `post-update` hook:

```toml
[[hook]]
name = "bat-cache"
when = "post-update"
if   = "command_exists:bat"
run  = ["bat", "cache", "--build"]
ignore_failure = true

[[hook]]
name = "hyprctl-reload"
when = "post-update"
if   = "command_exists:hyprctl"
run  = ["hyprctl", "reload"]
ignore_failure = true
```

Predicates available in `if =`:

- `command_exists:foo` — `which foo` succeeds
- `env:VAR` — env var is set; `env:VAR=value` — set to exact value
- `platform:linux|macos|windows`
- `file_exists:/path` — `${VAR}` interpolation supported
- `!negation` — binds tighter than `,` (AND)
- comma-separated terms AND together; an empty predicate is vacuously true

There is no OR. `post-update` is also the only `when` value the binary acts on
today — `krypt update` filters hooks with `h.when == "post-update"` and ignores
every other phase.

### 7. Replace menu launchers with `[[command]]`

Hyprland keybinds that called `~/.local/bin/.menu-foo` become:

```toml
# .krypt/commands.toml
[[command]]
group = "menu"
name  = "wifi"
description = "Wi-Fi picker via nmcli + pikr"
platform    = "linux"
steps = [
    { run = ["bash", "${HOME}/.local/bin/.menu-wifi"], ignore_failure = true },
]
```

Then update the Hyprland bind:

```
bind = $mod, w, exec, krypt menu wifi
```

Args after `--` forward as `{0}`..`{9}` into steps. For an autofill command with
positional args:

```toml
[[command]]
group = "menu"
name  = "autofill"
steps = [
    { run = ["bash", "${HOME}/.local/bin/.menu-autofill", "{0}"] },
]
```

```
bind = $mod ctrl, l, exec, krypt menu autofill -- pass
```

Run `krypt menu` (no name) to list everything. `krypt menu <name> --dry-run` to
preview a step plan without spawning processes.

### 8. Generic dispatcher

Any `[[command]] group = "X"` is reachable as `krypt X <name>` — no clap wiring
needed. Group names are yours to choose; anything from a one-off toolbox to a
per-application namespace works:

```sh
krypt system mirror     # [[command]] group = "system", name = "mirror"
krypt env up            # group = "env",    name = "up"
krypt kanata toggle     # group = "kanata", name = "toggle"
```

`krypt <group>` (no name) lists everything in the group. The one restriction: a
group named after a built-in subcommand (`link`, `update`, `deps`, `doctor`,
`battery`, …) never reaches the generic dispatcher, because clap matches the
built-in first. `menu` is the deliberate exception — `krypt menu` is a built-in
that dispatches the `menu` group.

## Patterns and gotchas

### Keep bash bash; do not inline it into TOML

Tempting:

```toml
steps = [
    { run = ["bash", "-c", "if systemctl is-active --quiet kanata.service; then sudo systemctl stop kanata.service && notify-send -u low 'Keyboard' 'OFF'; else sudo systemctl start kanata.service && notify-send -u normal 'Keyboard' 'ON'; fi"] },
]
```

Don't. You lose shellcheck, you lose syntax highlighting, you escape-quote
yourself into a corner. Use:

```toml
steps = [
    { run = ["bash", "${HOME}/.local/bin/.kanata"], ignore_failure = true },
]
```

…and keep `.kanata` as a real `.sh` file with `set -euo pipefail` and a shebang.

### `${HOME}`, `${XDG_CONFIG}`, etc. resolution

In `[[link]]` / `[[template]]` destinations, krypt resolves `${VAR}` against its
own variable table: `[paths]` overrides from `.krypt.toml` first, then built-ins
(`HOME`, `XDG_CONFIG`, `XDG_DATA`, `XDG_STATE`, `XDG_CACHE`, `XDG_RUNTIME`,
`LOCAL_BIN`, `DOCUMENTS`, plus `MAC_LIBRARY` on macOS and `WIN_LOCALAPPDATA` /
`WIN_APPDATA` on Windows). Run `krypt paths` to print the whole table as
resolved on the current host. An unknown name is a hard error
(`unknown path variable ${NOPE}`), not a passthrough.

In `[[command]]` / `[[hook]]` **step arguments** (`run`, `pipe`, `notify`,
`input`), `${VAR}` is expanded eagerly at config-load time with one extra tier:

1. krypt's own variable (built-in or `[paths]` override)
2. the process environment
3. otherwise — a hard error naming the file, the step, and the variable

So unrecognised variables are **not** left literal for the shell to expand
later. Note also that steps are argv, not a command line: `run = [...]` spawns
the binary directly with no shell, so `$FOO` (no braces) reaches the child
process as four literal characters. If you want a variable expanded by bash, put
it inside a real script file and call that. To pass a literal `${VAR}` through,
escape it as `\${VAR}`.

### Predicates apply per-step, not per-command

```toml
# Wrong — krypt validate rejects this:
#   unknown field `if`, expected one of `group`, `name`, `description`,
#   `platform`, `steps`
[[command]]
group = "env"
name  = "up"
if    = "command_exists:gcloud"     # ✗

# Right — gate the step
[[command]]
group = "env"
name  = "up"
steps = [
    { if = "command_exists:gcloud", run = [...] },
]
```

`platform = "linux"` IS a `[[command]]`-level field — that one's the exception.

### One repo, multiple machines

krypt has no machine-specific config layer. Per-machine variation lives in:

- Templates seeded once and edited by hand (e.g. `monitors.conf` per laptop) —
  once you edit the deployed copy it stops matching the manifest, so subsequent
  `krypt link` runs report it as a conflict and leave it alone
- Predicates that detect the machine state (`command_exists:`, `env:`,
  `file_exists:`, `platform:`)

If you need real conditional config branching, write it as predicates on hooks
or commands — don't try to fork the toml.

### Don't delete bash scripts called by `[[command]]` entries

If a step shells to `bash ${HOME}/.local/bin/.foo`, the script needs to be on
disk after `krypt link`. Keep them under version control; delete only the ones
absorbed wholesale into the krypt binary (the `[[command]]` entry goes too).

## Reference

- [mxaddict/dotfiles](https://github.com/mxaddict/dotfiles) — full working
  example
- [`crates/krypt-core/src/config/schema.rs`](../crates/krypt-core/src/config/schema.rs)
  — every config field
- `krypt validate` — fail-fast TOML check
- `krypt paths` — every `${VAR}` as resolved on this host
- `krypt doctor` — sanity check (deploy state, hook predicates, etc.)
- `krypt <subcommand> --help` — flags + behaviour for every command

Issue or question? Open one at
[kryptic-sh/krypt/issues](https://github.com/kryptic-sh/krypt/issues).
