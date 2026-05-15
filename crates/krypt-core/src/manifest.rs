//! Deployment manifest — record of what `krypt` has written to disk.
//!
//! The manifest is a versioned JSON document persisted at
//! `${XDG_STATE}/krypt/manifest.json`. It records, for every file the
//! engine has deployed, the source path, destination path, and a
//! SHA-256 hash of both. The hash of the destination at deploy time is
//! later used to detect **drift** — edits made to a deployed file
//! outside the repo.
//!
//! ## Schema
//!
//! ```json
//! {
//!   "version": 1,
//!   "krypt_version": "0.0.2",
//!   "deployed_at": 1715896800,
//!   "repo_path": "/home/x/.config/krypt/repo",
//!   "repo_commit": null,
//!   "entries": [
//!     { "src": ".gitconfig", "dst": "/home/x/.gitconfig",
//!       "kind": "link", "hash_src": "sha256:...", "hash_dst": "sha256:...",
//!       "deployed_at": 1715896800 }
//!   ]
//! }
//! ```
//!
//! ## Versioning
//!
//! The top-level `version` field is checked on load. A future bump (say,
//! v2 adding new fields) will land alongside a migration step here.
//!
//! ## Atomicity
//!
//! [`Manifest::save`] writes to a sibling tmp file and renames, mirroring
//! [`crate::copy`]'s deploy strategy. A torn write can't corrupt the
//! existing manifest on disk.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::copy::EntryKind;

/// Current manifest schema version.
pub const SCHEMA_VERSION: u32 = 1;

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors loading, saving, or comparing a manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// I/O failure reading or writing the manifest file.
    #[error("manifest io {path:?}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },

    /// JSON deserialize failure.
    #[error("manifest parse {path:?}: {source}")]
    Parse {
        /// Path of the bad file.
        path: PathBuf,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// JSON serialize failure (unexpected; would indicate a serde bug).
    #[error("manifest encode: {0}")]
    Encode(#[source] serde_json::Error),

    /// Schema version we don't know how to read.
    #[error("unsupported manifest version {found}, expected {expected}")]
    UnsupportedVersion {
        /// Version read from the file.
        found: u32,
        /// Version this build understands.
        expected: u32,
    },
}

// ─── Top-level manifest ─────────────────────────────────────────────────────

/// A complete deploy record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version. Currently [`SCHEMA_VERSION`].
    pub version: u32,

    /// The `krypt` binary version that wrote this manifest.
    pub krypt_version: String,

    /// Unix timestamp (seconds since epoch, UTC) of the last write.
    pub deployed_at: u64,

    /// Absolute path to the dotfiles repo root.
    pub repo_path: PathBuf,

    /// `git rev-parse HEAD` of the repo at deploy time, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_commit: Option<String>,

    /// Per-file deploy records, keyed by `dst` for fast lookup.
    ///
    /// We store this as a list on disk (more readable) but expose it as
    /// a map at the API layer so callers can look up by destination.
    #[serde(with = "entries_as_list")]
    pub entries: BTreeMap<PathBuf, ManifestEntry>,
}

/// Per-file deploy record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Source path *relative to the repo root* (e.g. `.gitconfig`).
    pub src: PathBuf,

    /// Absolute destination path.
    pub dst: PathBuf,

    /// Whether the entry came from a `[[link]]` or `[[template]]`.
    pub kind: EntryKind,

    /// SHA-256 hash of the source at deploy time, formatted as
    /// `sha256:<hex>`.
    pub hash_src: String,

    /// SHA-256 hash of the destination right after the copy — used by
    /// drift detection to spot post-deploy edits.
    pub hash_dst: String,

    /// Unix timestamp (seconds) when this entry was last (re)deployed.
    pub deployed_at: u64,
}

impl Manifest {
    /// Build an empty manifest stamped with the current time + crate
    /// version. Repo path defaults to "" — set it before saving.
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            version: SCHEMA_VERSION,
            krypt_version: crate::VERSION.to_string(),
            deployed_at: now_unix(),
            repo_path,
            repo_commit: None,
            entries: BTreeMap::new(),
        }
    }

    /// Load a manifest from disk. Returns `Ok(None)` if the file does
    /// not exist — callers treat that as "nothing deployed yet".
    pub fn load(path: &Path) -> Result<Option<Self>, ManifestError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ManifestError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let m: Manifest =
            serde_json::from_slice(&bytes).map_err(|source| ManifestError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        if m.version != SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                found: m.version,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(Some(m))
    }

    /// Atomically write the manifest to disk. Creates parent dirs.
    pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
        let mk_io = |source: io::Error| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(mk_io)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(ManifestError::Encode)?;

        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".krypt-tmp-{}", std::process::id()));
        let tmp = path.with_file_name(tmp_name);
        let _ = fs::remove_file(&tmp);
        fs::write(&tmp, &bytes).map_err(mk_io)?;
        fs::rename(&tmp, path).map_err(mk_io)?;
        Ok(())
    }

    /// Upsert a record for `dst`, refreshing hashes + timestamp.
    pub fn record(&mut self, entry: ManifestEntry) {
        self.entries.insert(entry.dst.clone(), entry);
        self.deployed_at = now_unix();
    }

    /// Forget a destination — used when a `[[link]]` is removed from
    /// the config and the file is unlinked.
    pub fn forget(&mut self, dst: &Path) -> Option<ManifestEntry> {
        self.entries.remove(dst)
    }
}

// ─── Hashing ────────────────────────────────────────────────────────────────

/// Compute the SHA-256 of a file as `sha256:<hex>`.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let f = File::open(path)?;
    let mut r = BufReader::new(f);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(format!("sha256:{:x}", digest))
}

// ─── Drift detection ────────────────────────────────────────────────────────

/// Why a manifest entry looks different from disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftStatus {
    /// Hashes match — destination is in sync with what was deployed.
    Clean,
    /// Destination exists but its hash differs from the recorded one.
    Drifted,
    /// Manifest knows about this entry but the destination file is gone.
    DstMissing,
}

/// One row in a drift report.
#[derive(Debug, Clone)]
pub struct DriftRecord {
    /// Source path (relative to repo root).
    pub src: PathBuf,
    /// Absolute destination path.
    pub dst: PathBuf,
    /// Whether it came from `[[link]]` or `[[template]]`.
    pub kind: EntryKind,
    /// What we wrote to disk on the last deploy.
    pub recorded_hash: String,
    /// What the destination hashes to now (None if missing or unreadable).
    pub current_hash: Option<String>,
    /// Drift classification.
    pub status: DriftStatus,
}

/// Walk every entry in the manifest, hashing the current destination and
/// classifying drift. Read errors (e.g. permission denied) are treated
/// as `Drifted` with `current_hash = None` — better to surface than swallow.
pub fn detect_drift(manifest: &Manifest) -> Vec<DriftRecord> {
    let mut out = Vec::with_capacity(manifest.entries.len());
    for entry in manifest.entries.values() {
        let (current_hash, status) = if !entry.dst.exists() {
            (None, DriftStatus::DstMissing)
        } else {
            match hash_file(&entry.dst) {
                Ok(h) if h == entry.hash_dst => (Some(h), DriftStatus::Clean),
                Ok(h) => (Some(h), DriftStatus::Drifted),
                Err(_) => (None, DriftStatus::Drifted),
            }
        };
        out.push(DriftRecord {
            src: entry.src.clone(),
            dst: entry.dst.clone(),
            kind: entry.kind,
            recorded_hash: entry.hash_dst.clone(),
            current_hash,
            status,
        });
    }
    out
}

// ─── Internals ──────────────────────────────────────────────────────────────

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Serialize the entries map as a JSON list (one object per entry) for
/// readable on-disk format. On the way in, list → map keyed by `dst`.
mod entries_as_list {
    use super::{ManifestEntry, PathBuf};
    use serde::Deserialize;
    use serde::de::{Deserializer, Error};
    use serde::ser::{SerializeSeq, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S>(map: &BTreeMap<PathBuf, ManifestEntry>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = ser.serialize_seq(Some(map.len()))?;
        for entry in map.values() {
            seq.serialize_element(entry)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D>(de: D) -> Result<BTreeMap<PathBuf, ManifestEntry>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let list: Vec<ManifestEntry> = Vec::deserialize(de)?;
        let mut map = BTreeMap::new();
        for entry in list {
            if map.insert(entry.dst.clone(), entry.clone()).is_some() {
                return Err(D::Error::custom(format!(
                    "duplicate manifest entry for {:?}",
                    entry.dst
                )));
            }
        }
        Ok(map)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn fake_entry(src: &str, dst: PathBuf, hash: &str) -> ManifestEntry {
        ManifestEntry {
            src: src.into(),
            dst,
            kind: EntryKind::Link,
            hash_src: hash.into(),
            hash_dst: hash.into(),
            deployed_at: 0,
        }
    }

    #[test]
    fn hash_file_matches_known_vector() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, b"hello").unwrap();
        // sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            hash_file(&p).unwrap(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir().unwrap();
        assert!(
            Manifest::load(&dir.path().join("nope.json"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");

        let mut m = Manifest::new(dir.path().to_path_buf());
        m.record(fake_entry(".gitconfig", dir.path().join("a"), "sha256:aa"));
        m.record(fake_entry(".tmux.conf", dir.path().join("b"), "sha256:bb"));
        m.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap().unwrap();
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[&dir.path().join("a")].hash_dst, "sha256:aa");
    }

    #[test]
    fn unsupported_version_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            r#"{{"version":999,"krypt_version":"x","deployed_at":0,"repo_path":"/","entries":[]}}"#
        )
        .unwrap();

        let err = Manifest::load(&path).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::UnsupportedVersion {
                found: 999,
                expected: SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn detect_drift_classifies_three_cases() {
        let dir = tempdir().unwrap();

        // Clean entry — write a file and record its actual hash.
        let clean = dir.path().join("clean.txt");
        fs::write(&clean, b"original").unwrap();
        let clean_hash = hash_file(&clean).unwrap();

        // Drifted entry — record one hash but write different bytes.
        let drifted = dir.path().join("drifted.txt");
        fs::write(&drifted, b"changed").unwrap();
        let stale_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        // Missing entry — recorded but no file on disk.
        let missing = dir.path().join("missing.txt");

        let mut m = Manifest::new(dir.path().to_path_buf());
        m.record(ManifestEntry {
            src: "a".into(),
            dst: clean.clone(),
            kind: EntryKind::Link,
            hash_src: clean_hash.clone(),
            hash_dst: clean_hash,
            deployed_at: 0,
        });
        m.record(ManifestEntry {
            src: "b".into(),
            dst: drifted.clone(),
            kind: EntryKind::Link,
            hash_src: stale_hash.into(),
            hash_dst: stale_hash.into(),
            deployed_at: 0,
        });
        m.record(ManifestEntry {
            src: "c".into(),
            dst: missing.clone(),
            kind: EntryKind::Template,
            hash_src: stale_hash.into(),
            hash_dst: stale_hash.into(),
            deployed_at: 0,
        });

        let drift = detect_drift(&m);
        let by_dst: BTreeMap<_, _> = drift.into_iter().map(|d| (d.dst.clone(), d)).collect();

        assert_eq!(by_dst[&clean].status, DriftStatus::Clean);
        assert_eq!(by_dst[&drifted].status, DriftStatus::Drifted);
        assert_eq!(by_dst[&missing].status, DriftStatus::DstMissing);
    }

    #[test]
    fn duplicate_dst_in_file_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let body = format!(
            r#"{{"version":{},"krypt_version":"x","deployed_at":0,"repo_path":"/","entries":[
                {{"src":"a","dst":"/x","kind":"link","hash_src":"sha256:1","hash_dst":"sha256:1","deployed_at":0}},
                {{"src":"a","dst":"/x","kind":"link","hash_src":"sha256:2","hash_dst":"sha256:2","deployed_at":0}}
            ]}}"#,
            SCHEMA_VERSION
        );
        fs::write(&path, body).unwrap();
        assert!(matches!(
            Manifest::load(&path),
            Err(ManifestError::Parse { .. })
        ));
    }
}
