//! Interactive setup wizard for `krypt setup`.
//!
//! Reads `[prompts.<name>]` sections from `.krypt.toml`, asks the user
//! questions via [`Prompter`], and writes the collected values to destination
//! files using one of four built-in writers:
//!
//! - **`gitconfig`** — merges key=value pairs into a git-style ini file.
//! - **`hypr_vars`** — patches `$VAR = value` lines in a Hyprland config.
//! - **`env`** — writes `export K=V` lines to a shell env file.
//! - **`generic_template`** — substitutes `{{key}}` placeholders in a template.
//!
//! # Default resolution order (first hit wins)
//!
//! 1. `read_var = "X"` — read `$X = <value>` from the destination file.
//! 2. `default_from` prefix:
//!    - `git:<key>` — shell out to `git config --get <key>`.  We deliberately
//!      keep this one shell-out because git-level user config (name, email,
//!      signing key) lives inside git's own config system and cannot be read any
//!      other way without reimplementing the full git config search order.  gix
//!      is only used for repo operations in `update`; it is not used here.
//!    - `env:<VAR>` — read env var.
//!    - `field:<key>` — value of an earlier field in this same run.
//!    - `read_var:<X>` — same as `read_var` shorthand.
//! 3. `default = <toml value>`.
//! 4. No default.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use thiserror::Error;

use crate::config::{PromptField, PromptSection};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Errors from [`setup`].
#[derive(Debug, Error)]
pub enum SetupError {
    /// A section name passed via `--prompts` is not in the config.
    #[error("unknown prompt section: {0:?}")]
    UnknownPromptSection(String),

    /// The `writer` field names an unsupported writer.
    #[error("unknown writer {0:?} — must be one of: gitconfig, hypr_vars, env, generic_template")]
    UnknownWriter(String),

    /// A required field has no default and `--yes` was requested.
    #[error("required field {key:?} has no default; cannot run unattended")]
    RequiredFieldHasNoDefault {
        /// The field key.
        key: String,
    },

    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Default resolver failure.
    #[error("resolve: {0}")]
    Resolve(#[from] ResolveError),
}

/// Errors from the default resolver.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// `git config --get` was called but the binary was not found.
    #[error("git binary not found when resolving git: default")]
    GitBinaryNotFound,

    /// `git config --get` exited non-zero or produced no output.
    #[error("git config key {0:?} not set")]
    GitKeyNotSet(String),
}

// ─── Options & report ────────────────────────────────────────────────────────

/// Inputs to [`setup`].
pub struct SetupOpts {
    /// Ordered prompt sections to run. If empty, all sections are run in
    /// BTreeMap key order.
    pub sections: Vec<String>,

    /// When true, use defaults for every field without prompting. Error if a
    /// required field has no default.
    pub yes: bool,

    /// The parsed prompt sections from the config (clone out of `Config`).
    pub prompt_sections: BTreeMap<String, PromptSection>,
}

/// Summary of a [`setup`] run.
#[derive(Debug, Default)]
pub struct SetupReport {
    /// Names of prompt sections that were processed.
    pub sections_run: Vec<String>,

    /// Total number of fields that were prompted (or auto-filled in `--yes`).
    pub fields_collected: usize,

    /// Destination file paths that each writer touched.
    pub files_written: Vec<PathBuf>,

    /// Field keys that were silently skipped due to a `requires` gate.
    pub skipped_by_requires: Vec<String>,
}

// ─── Prompter trait ──────────────────────────────────────────────────────────

/// Abstraction over interactive prompts so tests can inject scripted answers.
pub trait Prompter {
    /// Ask for a free-form string.
    fn ask_string(
        &mut self,
        prompt: &str,
        default: Option<&str>,
        optional: bool,
    ) -> io::Result<String>;

    /// Ask for a boolean (y/n).
    fn ask_bool(&mut self, prompt: &str, default: bool) -> io::Result<bool>;

    /// Ask for an integer.
    fn ask_int(&mut self, prompt: &str, default: Option<i64>) -> io::Result<i64>;
}

// ─── RealPrompter (dialoguer) ────────────────────────────────────────────────

/// Dialoguer-backed prompter used in production.
pub struct RealPrompter;

impl Prompter for RealPrompter {
    fn ask_string(
        &mut self,
        prompt: &str,
        default: Option<&str>,
        _optional: bool,
    ) -> io::Result<String> {
        let mut input = dialoguer::Input::<String>::new().with_prompt(prompt);
        if let Some(d) = default {
            input = input.with_initial_text(d).allow_empty(true);
        } else {
            input = input.allow_empty(true);
        }
        input
            .interact_text()
            .map_err(|e| io::Error::other(e.to_string()))
    }

    fn ask_bool(&mut self, prompt: &str, default: bool) -> io::Result<bool> {
        dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(default)
            .interact()
            .map_err(|e| io::Error::other(e.to_string()))
    }

    fn ask_int(&mut self, prompt: &str, default: Option<i64>) -> io::Result<i64> {
        let mut input = dialoguer::Input::<i64>::new().with_prompt(prompt);
        if let Some(d) = default {
            input = input.default(d);
        }
        input
            .interact_text()
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

// ─── YesPrompter (--yes / unattended) ────────────────────────────────────────

/// Returns each field's computed default without prompting. Errors if a
/// required field has no default.
pub struct YesPrompter;

impl Prompter for YesPrompter {
    fn ask_string(
        &mut self,
        _prompt: &str,
        default: Option<&str>,
        optional: bool,
    ) -> io::Result<String> {
        match default {
            Some(d) => Ok(d.to_owned()),
            None if optional => Ok(String::new()),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "required field has no default",
            )),
        }
    }

    fn ask_bool(&mut self, _prompt: &str, default: bool) -> io::Result<bool> {
        Ok(default)
    }

    fn ask_int(&mut self, _prompt: &str, default: Option<i64>) -> io::Result<i64> {
        default.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "required field has no default")
        })
    }
}

// ─── ScriptedPrompter (tests) ─────────────────────────────────────────────────

/// Pre-canned answers for unit tests. Pops answers in FIFO order.
pub struct ScriptedPrompter {
    /// String answers (used for string + int fields).
    pub answers: std::collections::VecDeque<String>,
    /// Bool answers (used for bool fields).
    pub bool_answers: std::collections::VecDeque<bool>,
}

impl ScriptedPrompter {
    /// Build from plain slices.
    pub fn new(answers: &[&str], bool_answers: &[bool]) -> Self {
        Self {
            answers: answers.iter().map(|s| s.to_string()).collect(),
            bool_answers: bool_answers.iter().copied().collect(),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn ask_string(
        &mut self,
        _prompt: &str,
        default: Option<&str>,
        _optional: bool,
    ) -> io::Result<String> {
        if let Some(a) = self.answers.pop_front() {
            Ok(a)
        } else {
            Ok(default.unwrap_or("").to_owned())
        }
    }

    fn ask_bool(&mut self, _prompt: &str, default: bool) -> io::Result<bool> {
        Ok(self.bool_answers.pop_front().unwrap_or(default))
    }

    fn ask_int(&mut self, _prompt: &str, default: Option<i64>) -> io::Result<i64> {
        if let Some(a) = self.answers.pop_front() {
            a.parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{e}")))
        } else {
            Ok(default.unwrap_or(0))
        }
    }
}

// ─── GitConfig trait (injectable for tests) ──────────────────────────────────

/// Abstraction over `git config --get <key>` so tests can avoid a real `git`
/// binary.
pub trait GitConfig {
    /// Return the value of `git config --get <key>`, or `None` if unset /
    /// unavailable.
    fn get(&self, key: &str) -> Option<String>;
}

/// Shells out to the system `git` binary.
///
/// We deliberately keep this one shell-out because resolving user-level git
/// config (name, email, signing key) requires honouring git's own config
/// search order (system → global → local). Reimplementing that search is
/// fragile; shelling out to `git config --get` is the safe, forward-compatible
/// choice.  Failure (binary not found, key unset) is silently treated as "no
/// default" — it never hard-errors the wizard.
pub struct RealGitConfig;

impl GitConfig for RealGitConfig {
    fn get(&self, key: &str) -> Option<String> {
        let output = StdCommand::new("git")
            .args(["config", "--get", key])
            .output()
            .ok()?;
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if s.is_empty() { None } else { Some(s) }
        } else {
            None
        }
    }
}

/// Fake git config for tests.
pub struct FakeGitConfig(pub BTreeMap<String, String>);

impl GitConfig for FakeGitConfig {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

// ─── Default resolver ─────────────────────────────────────────────────────────

/// Read `$<var_name> = <value>` from `dst`. Returns `None` if not found.
fn read_hypr_var(dst: &Path, var_name: &str) -> Option<String> {
    let content = fs::read_to_string(dst).ok()?;
    let needle = format!("${var_name} = ");
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(&needle) {
            return Some(rest.trim().to_owned());
        }
    }
    None
}

/// Resolve the default value for a single field.
///
/// `collected` holds values gathered earlier in the same section run.
fn resolve_default(
    field: &PromptField,
    collected: &BTreeMap<String, String>,
    dst: Option<&Path>,
    git: &dyn GitConfig,
) -> Option<String> {
    // 1. read_var shorthand
    if let Some(var_name) = &field.read_var
        && let Some(dst_path) = dst
        && let Some(val) = read_hypr_var(dst_path, var_name)
    {
        return Some(val);
    }

    // 2. default_from
    if let Some(from) = &field.default_from {
        if let Some(key) = from.strip_prefix("git:") {
            if let Some(val) = git.get(key) {
                return Some(val);
            }
        } else if let Some(var) = from.strip_prefix("env:") {
            if let Ok(val) = std::env::var(var)
                && !val.is_empty()
            {
                return Some(val);
            }
        } else if let Some(key) = from.strip_prefix("field:") {
            if let Some(val) = collected.get(key)
                && !val.is_empty()
            {
                return Some(val.clone());
            }
        } else if let Some(var_name) = from.strip_prefix("read_var:")
            && let Some(dst_path) = dst
            && let Some(val) = read_hypr_var(dst_path, var_name)
        {
            return Some(val);
        }
    }

    // 3. literal default
    if let Some(dv) = &field.default {
        let s = match dv {
            toml::Value::String(s) => s.clone(),
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            other => other.to_string(),
        };
        return Some(s);
    }

    None
}

// ─── Writers ──────────────────────────────────────────────────────────────────

/// Write a value atomically: write to `<dst>.krypt-tmp-<pid>`, then rename.
fn atomic_write(dst: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp_name = dst.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".krypt-tmp-{}", std::process::id()));
    let tmp = dst.with_file_name(tmp_name);
    let _ = fs::remove_file(&tmp);
    fs::write(&tmp, content.as_bytes())?;
    fs::rename(&tmp, dst)?;
    Ok(())
}

// ── gitconfig writer ──────────────────────────────────────────────────────────

/// Merge `values` into a git-style ini file at `dst`.
///
/// Keys are dot-separated: `user.name` → `[user]` section, key `name`.
/// Existing sections/keys not in `values` are preserved. Keys with empty
/// string values are skipped entirely.
fn write_gitconfig(values: &BTreeMap<String, String>, dst: &Path) -> io::Result<()> {
    // Parse existing file.
    let existing = if dst.exists() {
        fs::read_to_string(dst)?
    } else {
        String::new()
    };

    // Build a mutable representation: Vec<(section, Vec<(key, value)>)>
    // preserving order from the file, then we'll overwrite / append.
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();

    let mut current_section: Option<String> = None;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let sec = trimmed[1..trimmed.len() - 1].trim().to_owned();
            current_section = Some(sec.clone());
            sections.push((sec, Vec::new()));
        } else if let Some(ref sec) = current_section
            && let Some(pos) = trimmed.find('=')
        {
            let k = trimmed[..pos].trim().to_owned();
            let v = trimmed[pos + 1..].trim().to_owned();
            if let Some(entry) = sections.iter_mut().find(|(s, _)| s == sec) {
                entry.1.push((k, v));
            }
        }
    }

    // Apply our values: group by section prefix.
    let mut by_section: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (dotkey, val) in values {
        if val.is_empty() {
            continue;
        }
        let (sec, key) = if let Some(pos) = dotkey.find('.') {
            (dotkey[..pos].to_owned(), dotkey[pos + 1..].to_owned())
        } else {
            ("core".to_owned(), dotkey.clone())
        };
        by_section.entry(sec).or_default().insert(key, val.clone());
    }

    // Overwrite existing keys and add new ones within existing sections.
    for (sec, kv_map) in &by_section {
        if let Some(section) = sections.iter_mut().find(|(s, _)| s == sec) {
            for (k, v) in kv_map {
                if let Some(pair) = section.1.iter_mut().find(|(sk, _)| sk == k) {
                    pair.1 = v.clone();
                } else {
                    section.1.push((k.clone(), v.clone()));
                }
            }
        } else {
            // New section.
            sections.push((
                sec.clone(),
                kv_map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            ));
        }
    }

    // Render.
    let mut out = String::new();
    for (i, (sec, pairs)) in sections.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("[{sec}]\n"));
        for (k, v) in pairs {
            out.push_str(&format!("  {k} = {v}\n"));
        }
    }

    atomic_write(dst, &out)
}

// ── hypr_vars writer ──────────────────────────────────────────────────────────

/// Patch `$key = value` lines in a Hyprland config at `dst`.
///
/// If a `$key = ...` line is not found, append it to the end. All other lines
/// are preserved verbatim.
fn write_hypr_vars(values: &BTreeMap<String, String>, dst: &Path) -> io::Result<()> {
    let mut lines: Vec<String> = if dst.exists() {
        fs::read_to_string(dst)?
            .lines()
            .map(|l| l.to_owned())
            .collect()
    } else {
        Vec::new()
    };

    let mut written: BTreeMap<&str, bool> = BTreeMap::new();

    for (key, val) in values {
        let needle = format!("${key} = ");
        let mut found = false;
        for line in lines.iter_mut() {
            if line.starts_with(&needle) || line == &format!("${key} =") {
                *line = format!("${key} = {val}");
                found = true;
                break;
            }
        }
        if !found {
            lines.push(format!("${key} = {val}"));
        }
        written.insert(key, true);
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    atomic_write(dst, &out)
}

// ── env writer ────────────────────────────────────────────────────────────────

/// Write `export KEY=value` lines to `dst`. Skips empty values. Quotes values
/// with embedded whitespace.
fn write_env(values: &BTreeMap<String, String>, dst: &Path) -> io::Result<()> {
    let mut out = String::new();
    for (key, val) in values {
        if val.is_empty() {
            continue;
        }
        let quoted = if val.chars().any(|c| c.is_whitespace()) {
            format!("\"{val}\"")
        } else {
            val.clone()
        };
        out.push_str(&format!("export {key}={quoted}\n"));
    }
    atomic_write(dst, &out)
}

// ── generic_template writer ───────────────────────────────────────────────────

/// Substitute `{{key}}` placeholders in a template file and write to `dst`.
/// Missing keys leave the placeholder intact.
pub fn write_generic_template(
    values: &BTreeMap<String, String>,
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    let mut content = fs::read_to_string(src)?;
    for (key, val) in values {
        let placeholder = format!("{{{{{key}}}}}");
        content = content.replace(&placeholder, val);
    }
    atomic_write(dst, &content)
}

// ─── Section orchestration ────────────────────────────────────────────────────

/// Run a single prompt section. Returns the collected `(key, value)` map and
/// the list of skipped field keys.
fn run_section(
    section: &PromptSection,
    prompter: &mut dyn Prompter,
    git: &dyn GitConfig,
    dst: Option<&Path>,
    yes: bool,
) -> Result<(BTreeMap<String, String>, Vec<String>), SetupError> {
    let mut collected: BTreeMap<String, String> = BTreeMap::new();
    let mut skipped: Vec<String> = Vec::new();

    for field in &section.fields {
        // requires gate
        if let Some(req_key) = &field.requires {
            let gate_val = collected
                .get(req_key.as_str())
                .map(|s| s.as_str())
                .unwrap_or("");
            if gate_val.is_empty() {
                skipped.push(field.key.clone());
                continue;
            }
        }

        let default = resolve_default(field, &collected, dst, git);

        let value = match field.r#type.as_str() {
            "bool" => {
                let def_bool = default.as_deref().map(|s| s == "true").unwrap_or(false);
                if yes {
                    if def_bool { "true" } else { "false" }.to_owned()
                } else {
                    let b = prompter.ask_bool(&field.prompt, def_bool)?;
                    if b { "true" } else { "false" }.to_owned()
                }
            }
            "int" => {
                let def_int = default.as_deref().and_then(|s| s.parse::<i64>().ok());
                if yes {
                    match def_int {
                        Some(d) => d.to_string(),
                        None if field.optional => String::new(),
                        None => {
                            return Err(SetupError::RequiredFieldHasNoDefault {
                                key: field.key.clone(),
                            });
                        }
                    }
                } else {
                    let i = prompter.ask_int(&field.prompt, def_int)?;
                    i.to_string()
                }
            }
            _ => {
                // string (default)
                if yes {
                    match &default {
                        Some(d) => d.clone(),
                        None if field.optional => String::new(),
                        None => {
                            return Err(SetupError::RequiredFieldHasNoDefault {
                                key: field.key.clone(),
                            });
                        }
                    }
                } else {
                    prompter.ask_string(&field.prompt, default.as_deref(), field.optional)?
                }
            }
        };

        collected.insert(field.key.clone(), value);
    }

    Ok((collected, skipped))
}

// ─── Entry point ──────────────────────────────────────────────────────────────

/// Run the interactive setup wizard.
///
/// `git` is injected so tests can avoid shelling out to a real `git` binary.
/// In production, pass [`RealGitConfig`].
pub fn setup(
    opts: &SetupOpts,
    prompter: &mut dyn Prompter,
    git: &dyn GitConfig,
) -> Result<SetupReport, SetupError> {
    let mut report = SetupReport::default();

    // Determine run order.
    let names: Vec<String> = if opts.sections.is_empty() {
        opts.prompt_sections.keys().cloned().collect()
    } else {
        opts.sections.clone()
    };

    // Validate all names upfront so we don't write anything on error.
    for name in &names {
        if !opts.prompt_sections.contains_key(name.as_str()) {
            return Err(SetupError::UnknownPromptSection(name.clone()));
        }
    }

    for name in &names {
        let section = &opts.prompt_sections[name.as_str()];

        if !section.heading.is_empty() {
            println!("\n── {} ──", section.heading);
        }

        // For writers that read existing dest file (hypr_vars, read_var), we
        // need a dst path.  The section schema has `dst` on the writer side, so
        // we pass None here and let writers handle the file themselves.
        // read_var needs dst too — but dst is on the writer, not the section.
        // We pass None; read_var without a known dst simply produces no default.
        let (collected, skipped) = run_section(section, prompter, git, None, opts.yes)?;

        report.fields_collected += collected.len();
        report.skipped_by_requires.extend(skipped);

        // Dispatch writer.
        match section.writer.as_str() {
            "gitconfig" | "hypr_vars" | "env" | "generic_template" => {
                // Writers that need a dst path cannot operate here without a
                // dst configured.  In practice callers supply dst via a
                // SetupOpts extension or the CLI resolves it.  For now we
                // surface a noop so the API stays stable while we complete the
                // integration.  Tests use the writer functions directly.
            }
            other => {
                return Err(SetupError::UnknownWriter(other.to_owned()));
            }
        }

        report.sections_run.push(name.clone());
    }

    Ok(report)
}

/// Run setup with per-section destination paths and optional template sources.
///
/// This is the full-featured entry point used by the CLI. Each section name
/// maps to an optional destination path; the writer uses it to read existing
/// content and write the output. For `generic_template` sections, `srcs`
/// provides the template source path.
pub fn setup_with_destinations(
    opts: &SetupOpts,
    dsts: &BTreeMap<String, PathBuf>,
    prompter: &mut dyn Prompter,
    git: &dyn GitConfig,
) -> Result<SetupReport, SetupError> {
    setup_with_destinations_and_srcs(opts, dsts, &BTreeMap::new(), prompter, git)
}

/// Full entry point with both destination and source paths.
///
/// `srcs` maps section name → template source path for `generic_template`
/// sections. Other writers ignore `srcs`.
pub fn setup_with_destinations_and_srcs(
    opts: &SetupOpts,
    dsts: &BTreeMap<String, PathBuf>,
    srcs: &BTreeMap<String, PathBuf>,
    prompter: &mut dyn Prompter,
    git: &dyn GitConfig,
) -> Result<SetupReport, SetupError> {
    let mut report = SetupReport::default();

    let names: Vec<String> = if opts.sections.is_empty() {
        opts.prompt_sections.keys().cloned().collect()
    } else {
        opts.sections.clone()
    };

    for name in &names {
        if !opts.prompt_sections.contains_key(name.as_str()) {
            return Err(SetupError::UnknownPromptSection(name.clone()));
        }
    }

    for name in &names {
        let section = &opts.prompt_sections[name.as_str()];

        if !section.heading.is_empty() {
            println!("\n── {} ──", section.heading);
        }

        let dst = dsts.get(name).map(|p| p.as_path());

        let (collected, skipped) = run_section(section, prompter, git, dst, opts.yes)?;

        report.fields_collected += collected.len();
        report.skipped_by_requires.extend(skipped);

        let dst_path = match dst {
            Some(p) => p,
            None => {
                report.sections_run.push(name.clone());
                continue;
            }
        };

        match section.writer.as_str() {
            "gitconfig" => {
                write_gitconfig(&collected, dst_path)?;
                report.files_written.push(dst_path.to_path_buf());
            }
            "hypr_vars" => {
                write_hypr_vars(&collected, dst_path)?;
                report.files_written.push(dst_path.to_path_buf());
            }
            "env" => {
                write_env(&collected, dst_path)?;
                report.files_written.push(dst_path.to_path_buf());
            }
            "generic_template" => {
                if let Some(src_path) = srcs.get(name) {
                    write_generic_template(&collected, src_path, dst_path)?;
                    report.files_written.push(dst_path.to_path_buf());
                }
                // No src → no-op; dst still counts as "touched" for section tracking.
            }
            other => {
                return Err(SetupError::UnknownWriter(other.to_owned()));
            }
        }

        report.sections_run.push(name.clone());
    }

    Ok(report)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PromptField, PromptSection};
    use tempfile::tempdir;

    fn make_field(key: &str, prompt: &str) -> PromptField {
        PromptField {
            key: key.to_owned(),
            prompt: prompt.to_owned(),
            r#type: "string".to_owned(),
            default: None,
            default_from: None,
            read_var: None,
            optional: false,
            requires: None,
        }
    }

    fn make_section(fields: Vec<PromptField>, writer: &str) -> PromptSection {
        PromptSection {
            heading: String::new(),
            fields,
            writer: writer.to_owned(),
        }
    }

    // 1. Fields run in declared order and values are collected.
    #[test]
    fn fields_collected_in_order() {
        let mut sections = BTreeMap::new();
        sections.insert(
            "git".to_owned(),
            make_section(
                vec![make_field("name", "Name"), make_field("email", "Email")],
                "gitconfig",
            ),
        );

        let opts = SetupOpts {
            sections: vec!["git".to_owned()],
            yes: false,
            prompt_sections: sections,
        };

        let mut p = ScriptedPrompter::new(&["Alice", "alice@example.com"], &[]);
        let git = FakeGitConfig(BTreeMap::new());
        let report = setup(&opts, &mut p, &git).unwrap();

        assert_eq!(report.sections_run, vec!["git"]);
        assert_eq!(report.fields_collected, 2);
    }

    // 2. `requires` skips a field when gating field is empty.
    #[test]
    fn requires_skips_field_when_gate_empty() {
        let gated = PromptField {
            requires: Some("key".to_owned()),
            ..make_field("sign", "Sign commits?")
        };
        let mut sections = BTreeMap::new();
        sections.insert(
            "git".to_owned(),
            make_section(
                vec![
                    PromptField {
                        optional: true,
                        ..make_field("key", "GPG key")
                    },
                    gated,
                ],
                "gitconfig",
            ),
        );

        let opts = SetupOpts {
            sections: vec!["git".to_owned()],
            yes: false,
            prompt_sections: sections,
        };

        // key = "" (empty optional), sign should be skipped.
        let mut p = ScriptedPrompter::new(&[""], &[]);
        let git = FakeGitConfig(BTreeMap::new());
        let report = setup(&opts, &mut p, &git).unwrap();

        assert!(
            report.skipped_by_requires.contains(&"sign".to_owned()),
            "sign should be skipped"
        );
    }

    // 3. `default_from = "env:..."` picks up the env value.
    //
    // We can't mutate env vars without `unsafe` (forbidden in this crate), so
    // we pick a var that is always set in CI and on developer machines.
    #[test]
    fn default_from_env() {
        // HOME is always set; PATH is always set. Either works.
        // We skip the test gracefully if neither is set (extremely unlikely).
        let (var_name, expected) = if let Ok(v) = std::env::var("HOME") {
            ("HOME".to_owned(), v)
        } else if let Ok(v) = std::env::var("PATH") {
            ("PATH".to_owned(), v)
        } else {
            return; // nothing to test; skip silently
        };

        let field = PromptField {
            default_from: Some(format!("env:{var_name}")),
            ..make_field("val", "Value")
        };

        let git = FakeGitConfig(BTreeMap::new());
        let default = resolve_default(&field, &BTreeMap::new(), None, &git);

        assert_eq!(default, Some(expected));
    }

    // 4. `default_from = "field:..."` picks up a prior field's value.
    #[test]
    fn default_from_field() {
        let field = PromptField {
            default_from: Some("field:email".to_owned()),
            ..make_field("key", "Key")
        };

        let mut prior = BTreeMap::new();
        prior.insert("email".to_owned(), "mx@example.com".to_owned());

        let git = FakeGitConfig(BTreeMap::new());
        let default = resolve_default(&field, &prior, None, &git);
        assert_eq!(default, Some("mx@example.com".to_owned()));
    }

    // 5. `default_from = "git:..."` uses injected FakeGitConfig.
    #[test]
    fn default_from_git() {
        let mut git_map = BTreeMap::new();
        git_map.insert("user.name".to_owned(), "Mx Addict".to_owned());
        let git = FakeGitConfig(git_map);

        let field = PromptField {
            default_from: Some("git:user.name".to_owned()),
            ..make_field("name", "Name")
        };

        let default = resolve_default(&field, &BTreeMap::new(), None, &git);
        assert_eq!(default, Some("Mx Addict".to_owned()));
    }

    // 6. `read_var` reads from an existing hypr config file.
    #[test]
    fn read_var_from_dst_file() {
        let dir = tempdir().unwrap();
        let dst = dir.path().join("hyprland.conf");
        fs::write(&dst, "$terminal = kitty\n$bar = waybar\n").unwrap();

        let field = PromptField {
            read_var: Some("terminal".to_owned()),
            ..make_field("terminal", "Terminal")
        };

        let git = FakeGitConfig(BTreeMap::new());
        let default = resolve_default(&field, &BTreeMap::new(), Some(&dst), &git);
        assert_eq!(default, Some("kitty".to_owned()));
    }

    // 7. `--yes` with no default + required field → errors before writing.
    #[test]
    fn yes_mode_no_default_required_errors() {
        let mut sections = BTreeMap::new();
        sections.insert(
            "git".to_owned(),
            make_section(vec![make_field("name", "Name")], "gitconfig"),
        );

        let opts = SetupOpts {
            sections: vec!["git".to_owned()],
            yes: true,
            prompt_sections: sections,
        };

        let mut p = YesPrompter;
        let git = FakeGitConfig(BTreeMap::new());
        let err = setup(&opts, &mut p, &git).unwrap_err();

        assert!(
            matches!(err, SetupError::RequiredFieldHasNoDefault { .. }),
            "expected RequiredFieldHasNoDefault, got {err:?}"
        );
    }

    // 8. gitconfig writer merges: old keys preserved, new keys added.
    #[test]
    fn gitconfig_writer_merges() {
        let dir = tempdir().unwrap();
        let dst = dir.path().join(".gitconfig");
        fs::write(&dst, "[user]\n  name = Old\n  old_key = keep\n").unwrap();

        let mut values = BTreeMap::new();
        values.insert("user.name".to_owned(), "New".to_owned());
        values.insert("user.email".to_owned(), "new@example.com".to_owned());

        write_gitconfig(&values, &dst).unwrap();

        let content = fs::read_to_string(&dst).unwrap();
        assert!(content.contains("name = New"), "name should be updated");
        assert!(
            content.contains("old_key = keep"),
            "old_key should be preserved"
        );
        assert!(
            content.contains("email = new@example.com"),
            "email should be added"
        );
    }

    // 9. hypr_vars writer: existing `$terminal = kitty` replaced, other lines kept.
    #[test]
    fn hypr_vars_writer_replaces_and_preserves() {
        let dir = tempdir().unwrap();
        let dst = dir.path().join("hyprland.conf");
        fs::write(&dst, "$terminal = kitty\n$bar = waybar\n").unwrap();

        let mut values = BTreeMap::new();
        values.insert("terminal".to_owned(), "alacritty".to_owned());

        write_hypr_vars(&values, &dst).unwrap();

        let content = fs::read_to_string(&dst).unwrap();
        assert!(
            content.contains("$terminal = alacritty"),
            "terminal replaced"
        );
        assert!(content.contains("$bar = waybar"), "bar preserved");
        assert!(!content.contains("kitty"), "old value gone");
    }

    // 10. env writer: produces `export K=V`, skips empty, quotes whitespace.
    #[test]
    fn env_writer_output() {
        let dir = tempdir().unwrap();
        let dst = dir.path().join("env");

        let mut values = BTreeMap::new();
        values.insert("FOO".to_owned(), "bar".to_owned());
        values.insert("EMPTY".to_owned(), String::new());
        values.insert("WITH_SPACE".to_owned(), "hello world".to_owned());

        write_env(&values, &dst).unwrap();

        let content = fs::read_to_string(&dst).unwrap();
        assert!(content.contains("export FOO=bar"), "FOO written");
        assert!(!content.contains("EMPTY"), "empty skipped");
        assert!(
            content.contains("export WITH_SPACE=\"hello world\""),
            "whitespace quoted"
        );
    }

    // 11. generic_template: `{{k}}` substituted, missing `{{x}}` left intact.
    #[test]
    fn generic_template_substitution() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("template.txt");
        let dst = dir.path().join("output.txt");
        fs::write(&src, "Hello {{name}}! Unknown: {{missing}}").unwrap();

        let mut values = BTreeMap::new();
        values.insert("name".to_owned(), "World".to_owned());

        write_generic_template(&values, &src, &dst).unwrap();

        let content = fs::read_to_string(&dst).unwrap();
        assert_eq!(content, "Hello World! Unknown: {{missing}}");
    }

    // 12. `--yes` with defaults present → succeeds without prompting.
    #[test]
    fn yes_mode_with_defaults_succeeds() {
        let mut sections = BTreeMap::new();
        sections.insert(
            "env_section".to_owned(),
            make_section(
                vec![PromptField {
                    default: Some(toml::Value::String("alice".to_owned())),
                    ..make_field("USER", "User")
                }],
                "env",
            ),
        );

        let opts = SetupOpts {
            sections: vec!["env_section".to_owned()],
            yes: true,
            prompt_sections: sections,
        };

        let dir = tempdir().unwrap();
        let dst = dir.path().join("env_out");
        let mut dsts = BTreeMap::new();
        dsts.insert("env_section".to_owned(), dst.clone());

        let mut p = YesPrompter;
        let git = FakeGitConfig(BTreeMap::new());
        let report = setup_with_destinations(&opts, &dsts, &mut p, &git).unwrap();

        assert_eq!(report.sections_run, vec!["env_section"]);
        let content = fs::read_to_string(&dst).unwrap();
        assert!(
            content.contains("export USER=alice"),
            "USER written from default"
        );
    }

    // 13. `--prompts a,b` — only those sections run.
    #[test]
    fn prompts_filter_runs_only_named_sections() {
        let mut sections = BTreeMap::new();
        sections.insert(
            "a".to_owned(),
            make_section(vec![make_field("x", "X")], "env"),
        );
        sections.insert(
            "b".to_owned(),
            make_section(vec![make_field("y", "Y")], "env"),
        );
        sections.insert(
            "c".to_owned(),
            make_section(vec![make_field("z", "Z")], "env"),
        );

        let opts = SetupOpts {
            sections: vec!["a".to_owned(), "b".to_owned()],
            yes: false,
            prompt_sections: sections,
        };

        let mut p = ScriptedPrompter::new(&["val_a", "val_b"], &[]);
        let git = FakeGitConfig(BTreeMap::new());
        let report = setup(&opts, &mut p, &git).unwrap();

        assert_eq!(report.sections_run, vec!["a", "b"]);
        assert!(
            !report.sections_run.contains(&"c".to_owned()),
            "c should not run"
        );
    }

    // 14. Unknown section in --prompts filter → error.
    #[test]
    fn unknown_section_errors() {
        let mut sections = BTreeMap::new();
        sections.insert(
            "a".to_owned(),
            make_section(vec![make_field("x", "X")], "env"),
        );

        let opts = SetupOpts {
            sections: vec!["unknown".to_owned()],
            yes: false,
            prompt_sections: sections,
        };

        let mut p = ScriptedPrompter::new(&[], &[]);
        let git = FakeGitConfig(BTreeMap::new());
        let err = setup(&opts, &mut p, &git).unwrap_err();

        assert!(
            matches!(err, SetupError::UnknownPromptSection(ref s) if s == "unknown"),
            "expected UnknownPromptSection(unknown), got {err:?}"
        );
    }

    // 15. `default_from = "read_var:terminal"` reads from existing hypr file via default_from.
    #[test]
    fn default_from_read_var() {
        let dir = tempdir().unwrap();
        let dst = dir.path().join("hyprland.conf");
        fs::write(&dst, "$terminal = wezterm\n").unwrap();

        let field = PromptField {
            default_from: Some("read_var:terminal".to_owned()),
            ..make_field("terminal", "Terminal")
        };

        let git = FakeGitConfig(BTreeMap::new());
        let default = resolve_default(&field, &BTreeMap::new(), Some(&dst), &git);
        assert_eq!(default, Some("wezterm".to_owned()));
    }

    // 16. ScriptedPrompter answers two env fields; verify file contents.
    #[test]
    fn scripted_prompter_env_writer() {
        let mut sections = BTreeMap::new();
        sections.insert(
            "env_sec".to_owned(),
            make_section(
                vec![make_field("FOO", "Foo"), make_field("BAR", "Bar")],
                "env",
            ),
        );

        let opts = SetupOpts {
            sections: vec!["env_sec".to_owned()],
            yes: false,
            prompt_sections: sections,
        };

        let dir = tempdir().unwrap();
        let dst = dir.path().join("vars.env");
        let mut dsts = BTreeMap::new();
        dsts.insert("env_sec".to_owned(), dst.clone());

        let mut p = ScriptedPrompter::new(&["hello", "world"], &[]);
        let git = FakeGitConfig(BTreeMap::new());
        setup_with_destinations(&opts, &dsts, &mut p, &git).unwrap();

        let content = fs::read_to_string(&dst).unwrap();
        assert!(content.contains("export FOO=hello"));
        assert!(content.contains("export BAR=world"));
    }

    // 17. generic_template writer end-to-end via setup_with_destinations_and_srcs.
    #[test]
    fn generic_template_via_setup() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("template.conf");
        let dst = dir.path().join("output.conf");
        fs::write(&src, "name = {{name}}\nemail = {{email}}\n").unwrap();

        let mut sections = BTreeMap::new();
        sections.insert(
            "tmpl".to_owned(),
            make_section(
                vec![make_field("name", "Name"), make_field("email", "Email")],
                "generic_template",
            ),
        );

        let opts = SetupOpts {
            sections: vec!["tmpl".to_owned()],
            yes: false,
            prompt_sections: sections,
        };

        let mut dsts = BTreeMap::new();
        dsts.insert("tmpl".to_owned(), dst.clone());
        let mut srcs = BTreeMap::new();
        srcs.insert("tmpl".to_owned(), src.clone());

        let mut p = ScriptedPrompter::new(&["Alice", "alice@example.com"], &[]);
        let git = FakeGitConfig(BTreeMap::new());
        setup_with_destinations_and_srcs(&opts, &dsts, &srcs, &mut p, &git).unwrap();

        let content = fs::read_to_string(&dst).unwrap();
        assert!(content.contains("name = Alice"));
        assert!(content.contains("email = alice@example.com"));
    }
}
