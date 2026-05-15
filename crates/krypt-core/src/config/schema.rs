//! Typed representation of a parsed `.krypt.toml`.
//!
//! All structs use `#[serde(deny_unknown_fields)]` so typos in user config
//! produce loud errors at parse time rather than silently no-oping.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level parsed config.
///
/// Build via [`super::parse_file`] or [`super::parse_str`].
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Metadata block.
    #[serde(default)]
    pub meta: Meta,

    /// Other `.krypt.toml` files to merge in. Resolution is handled by the
    /// `include` pass, not the parser.
    #[serde(default)]
    pub include: Vec<String>,

    /// User-defined path variable overrides. Values may reference other vars
    /// via `${NAME}`; resolution is deferred to the path resolver.
    #[serde(default)]
    pub paths: BTreeMap<String, String>,

    /// File-deploy entries. Renamed because TOML uses `[[link]]` but Rust
    /// idiom is `links` for the collection.
    #[serde(default, rename = "link")]
    pub links: Vec<Link>,

    /// Templated file entries. Deployed like links, then optionally patched
    /// by `krypt setup` based on the named prompt sections.
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,

    /// Wizard prompt definitions, keyed by section name.
    #[serde(default)]
    pub prompts: BTreeMap<String, PromptSection>,

    /// Package dependency groups, one per cross-distro mapping.
    #[serde(default, rename = "deps")]
    pub deps: Vec<DepsGroup>,

    /// Lifecycle hooks (post-update plugin updates, etc.).
    #[serde(default, rename = "hook")]
    pub hooks: Vec<Hook>,

    /// Custom subcommands surfaced as `krypt <group> <name>`.
    #[serde(default, rename = "command")]
    pub commands: Vec<Command>,
}

/// `[meta]` section.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    /// Human-readable repo name.
    #[serde(default)]
    pub name: String,

    /// Free-form short description.
    #[serde(default)]
    pub description: String,

    /// Minimum `krypt` binary version this repo expects. If the installed
    /// `krypt` is older, the loader emits a warning (or errors on
    /// `--strict`). Format: SemVer.
    #[serde(default)]
    pub krypt_min: Option<String>,
}

/// `[[link]]` entry — a file (or glob) to deploy from the repo to a path
/// under `$HOME`.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// Source path within the dotfiles repo. Mutually exclusive with
    /// `src_glob`.
    #[serde(default)]
    pub src: Option<String>,

    /// Glob pattern matched against the repo. Expanded at deploy time.
    /// Mutually exclusive with `src`.
    #[serde(default)]
    pub src_glob: Option<String>,

    /// Destination path. May contain `${VAR}` placeholders.
    pub dst: String,

    /// Optional OS gate. One of `linux`, `macos`, `windows`. Multiple OSes:
    /// duplicate the link entry. Omitted = all platforms.
    #[serde(default)]
    pub platform: Option<String>,
}

/// `[[template]]` entry — a file copied like a `[[link]]`, but with prompt-
/// section names that wire it to `krypt setup`.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// Source path within the repo.
    pub src: String,

    /// Destination path. May contain `${VAR}` placeholders.
    pub dst: String,

    /// Names of `[prompts.<name>]` sections that drive this template's
    /// post-copy patching.
    #[serde(default)]
    pub prompts: Vec<String>,

    /// Optional OS gate. Same semantics as [`Link::platform`].
    #[serde(default)]
    pub platform: Option<String>,
}

/// `[prompts.<name>]` section — an interactive wizard subsection.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    /// Heading shown above this group of prompts.
    #[serde(default)]
    pub heading: String,

    /// Ordered list of fields to prompt for.
    pub fields: Vec<PromptField>,

    /// Named built-in writer that applies the collected values. e.g.
    /// `gitconfig`, `hypr_vars`, `env`, `generic_template`.
    pub writer: String,
}

/// A single field inside a [`PromptSection`].
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PromptField {
    /// Variable name the value is stored under.
    pub key: String,

    /// Question presented to the user.
    pub prompt: String,

    /// Field type: `string` (default), `bool`, `int`. Future: `password`,
    /// `path`, `enum`.
    #[serde(default = "default_prompt_type")]
    pub r#type: String,

    /// Literal default value.
    #[serde(default)]
    pub default: Option<toml::Value>,

    /// Lookup expression for the default. Examples:
    /// - `git:user.name`     — run `git config --get user.name`
    /// - `env:USER`          — read env var
    /// - `field:email`       — value of an earlier field
    /// - `read_var:terminal` — read existing Hyprland `$terminal` from dest
    #[serde(default)]
    pub default_from: Option<String>,

    /// Read the current value of a Hyprland-style variable from the
    /// destination file when computing the default. Shorthand alternative
    /// to `default_from = "read_var:..."`.
    #[serde(default)]
    pub read_var: Option<String>,

    /// If true, blank answers are allowed.
    #[serde(default)]
    pub optional: bool,

    /// Only ask this field if the named earlier field has a non-empty value.
    #[serde(default)]
    pub requires: Option<String>,
}

fn default_prompt_type() -> String {
    "string".into()
}

/// `[[deps]]` entry — one cross-distro dependency group.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct DepsGroup {
    /// Group name (e.g. `core`, `fonts`).
    pub group: String,

    /// Required platforms for this group. `["all"]` or omitted = required
    /// everywhere; otherwise restrict to listed OSes.
    #[serde(default)]
    pub required_platforms: Vec<String>,

    /// Packages on each manager. Empty list = unavailable on that manager.
    #[serde(default)]
    pub pacman: Vec<String>,
    /// Packages on apt (Debian/Ubuntu).
    #[serde(default)]
    pub apt: Vec<String>,
    /// Packages on dnf (Fedora/RHEL).
    #[serde(default)]
    pub dnf: Vec<String>,
    /// Packages on brew (macOS).
    #[serde(default)]
    pub brew: Vec<String>,
    /// Packages on scoop (Windows).
    #[serde(default)]
    pub scoop: Vec<String>,
    /// Packages on winget (Windows).
    #[serde(default)]
    pub winget: Vec<String>,
}

/// `[[hook]]` entry — a lifecycle hook tied to a phase.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Hook name, useful in logs.
    pub name: String,

    /// Phase: `post-update`, `post-link`, etc.
    pub when: String,

    /// Optional predicate gating execution. Same syntax as `[[command]]`
    /// step predicates.
    #[serde(default)]
    pub r#if: Option<String>,

    /// Command + args to execute.
    pub run: Vec<String>,

    /// Don't fail the parent command if this hook errors.
    #[serde(default)]
    pub ignore_failure: bool,
}

/// `[[command]]` entry — a custom user subcommand built from step primitives.
///
/// Exposed at the CLI as `krypt <group> <name>`.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// Group bucket (e.g. `menu`, `battery`).
    pub group: String,

    /// Command name within the group.
    pub name: String,

    /// Help text shown by `krypt <group> --help`.
    #[serde(default)]
    pub description: String,

    /// Optional OS gate.
    #[serde(default)]
    pub platform: Option<String>,

    /// Ordered execution steps.
    pub steps: Vec<Step>,
}

/// One step inside a [`Command`] or [`Hook`].
///
/// Step kinds are mutually exclusive but all carry optional shared fields
/// like `capture`, `if`, `on_fail`. Validation enforces exactly one of
/// `run` / `pipe` / `notify` per step.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Spawn a subprocess with these args. String args may interpolate
    /// captured vars via `{name}`.
    #[serde(default)]
    pub run: Option<Vec<String>>,

    /// Spawn a subprocess that receives `input` on stdin.
    #[serde(default)]
    pub pipe: Option<Vec<String>>,

    /// Send a desktop notification with this title (`notify[0]`) and body
    /// (`notify[1]`).
    #[serde(default)]
    pub notify: Option<Vec<String>>,

    /// Variable name to capture stdout into.
    #[serde(default)]
    pub capture: Option<String>,

    /// Input string fed to stdin (used with `pipe`). May reference captures.
    #[serde(default)]
    pub input: Option<String>,

    /// Predicate gating this step. e.g. `command_exists:fish`,
    /// `platform:linux`, `env:FOO=bar`, `!file_exists:/etc/passwd`.
    #[serde(default)]
    pub r#if: Option<String>,

    /// Failure mode: `abort` (default), `notify`, `ignore`, `prompt`.
    #[serde(default)]
    pub on_fail: Option<String>,

    /// Boolean shortcut for `on_fail = "ignore"`.
    #[serde(default)]
    pub ignore_failure: bool,
}
