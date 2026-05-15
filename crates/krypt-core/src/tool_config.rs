//! Tool-level config at `${XDG_CONFIG}/krypt/config.toml`.
//!
//! Records where the dotfiles repo lives and (optionally) where it was
//! cloned from. Written by `krypt init`; read by future commands that
//! need to locate the repo without the user passing `--repo-path` each
//! time.

// `ToolConfigError` wraps `io::Error` + `PathBuf` and (boxed) TOML errors;
// on Windows the combined enum exceeds clippy's 128-byte threshold.
#![allow(clippy::result_large_err)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::Resolver;

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors loading, saving, or resolving the tool config.
#[derive(Debug, Error)]
pub enum ToolConfigError {
    /// I/O failure reading or writing the config file.
    #[error("tool config io {path:?}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },

    /// TOML deserialize failure.
    #[error("tool config parse {path:?}: {source}")]
    Parse {
        /// Path of the bad file.
        path: PathBuf,
        /// Underlying serde error (boxed to keep the enum variant small).
        #[source]
        source: Box<toml::de::Error>,
    },

    /// TOML serialize failure.
    #[error("tool config encode: {0}")]
    Encode(#[source] Box<toml::ser::Error>),

    /// XDG path resolution failed.
    #[error("resolving default config path: {0}")]
    Resolve(#[source] crate::paths::ResolveError),
}

// ─── Schema ─────────────────────────────────────────────────────────────────

/// `[repo]` section of `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// Absolute path to the cloned (or bare-initialised) dotfiles repo.
    pub path: PathBuf,

    /// Remote URL the repo was cloned from. Absent in `--bare` mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Top-level `config.toml` schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    /// Repository location + origin.
    pub repo: RepoConfig,
}

impl ToolConfig {
    /// Resolve the default path: `${XDG_CONFIG}/krypt/config.toml`.
    pub fn default_path() -> Result<PathBuf, ToolConfigError> {
        let r = Resolver::new();
        let xdg = r
            .resolve_var("XDG_CONFIG")
            .map_err(ToolConfigError::Resolve)?;
        Ok(PathBuf::from(xdg).join("krypt").join("config.toml"))
    }

    /// Load from disk. Returns `Ok(None)` if the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>, ToolConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ToolConfigError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let cfg: ToolConfig = toml::from_str(&text).map_err(|source| ToolConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        Ok(Some(cfg))
    }

    /// Atomically write to disk. Creates parent directories.
    pub fn save(&self, path: &Path) -> Result<(), ToolConfigError> {
        let mk_io = |source: io::Error| ToolConfigError::Io {
            path: path.to_path_buf(),
            source,
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(mk_io)?;
        }

        let text =
            toml::to_string_pretty(self).map_err(|e| ToolConfigError::Encode(Box::new(e)))?;

        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".krypt-tmp-{}", std::process::id()));
        let tmp = path.with_file_name(tmp_name);
        let _ = fs::remove_file(&tmp);
        fs::write(&tmp, text.as_bytes()).map_err(mk_io)?;
        fs::rename(&tmp, path).map_err(mk_io)?;
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir().unwrap();
        assert!(
            ToolConfig::load(&dir.path().join("config.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn save_then_load_roundtrips_with_url() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = ToolConfig {
            repo: RepoConfig {
                path: PathBuf::from("/home/x/.config/krypt/repo"),
                url: Some("https://github.com/me/dotfiles".into()),
            },
        };
        cfg.save(&path).unwrap();
        let loaded = ToolConfig::load(&path).unwrap().unwrap();
        assert_eq!(loaded.repo.path, cfg.repo.path);
        assert_eq!(loaded.repo.url, cfg.repo.url);
    }

    #[test]
    fn save_then_load_roundtrips_bare() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = ToolConfig {
            repo: RepoConfig {
                path: PathBuf::from("/home/x/.config/krypt/repo"),
                url: None,
            },
        };
        cfg.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("url"), "url should be omitted when None");
        let loaded = ToolConfig::load(&path).unwrap().unwrap();
        assert!(loaded.repo.url.is_none());
    }

    #[test]
    fn unknown_field_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[repo]\npath = \"/x\"\nunknown_field = true\n").unwrap();
        assert!(matches!(
            ToolConfig::load(&path),
            Err(ToolConfigError::Parse { .. })
        ));
    }
}
