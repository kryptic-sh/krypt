//! Battery status reader.
//!
//! Provides a platform-abstracted API for reading battery state. On Linux,
//! [`LinuxSysfsReader`] walks `/sys/class/power_supply/` for the first
//! `Battery`-typed entry and reads `capacity`, `status`, `energy_now`,
//! `power_now` (or `charge_now` / `current_now` as fallback).
//!
//! Non-Linux targets get [`UnsupportedReader`] which always returns
//! [`BatteryError::Unsupported`]. [`default_reader`] picks the right
//! implementation for the current platform.
//!
//! # Example
//!
//! ```no_run
//! use krypt_core::battery::default_reader;
//!
//! let reader = default_reader();
//! match reader.read() {
//!     Ok(r) => println!("{}% ({})", r.percent, r.status),
//!     Err(e) => eprintln!("battery error: {e}"),
//! }
//! ```

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ─── Error ───────────────────────────────────────────────────────────────────

/// Errors that can occur while reading battery state.
#[derive(Debug)]
pub enum BatteryError {
    /// No battery device found on this system.
    NotFound,
    /// An I/O error occurred while reading sysfs files.
    Io(std::io::Error),
    /// A sysfs file contained an unexpected value.
    Parse(String),
    /// Battery reading is not implemented on this platform.
    Unsupported(&'static str),
}

impl fmt::Display for BatteryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BatteryError::NotFound => write!(f, "no battery device found"),
            BatteryError::Io(e) => write!(f, "I/O error: {e}"),
            BatteryError::Parse(msg) => write!(f, "parse error: {msg}"),
            BatteryError::Unsupported(platform) => {
                write!(f, "battery reading not supported on {platform}")
            }
        }
    }
}

impl std::error::Error for BatteryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BatteryError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BatteryError {
    fn from(e: std::io::Error) -> Self {
        BatteryError::Io(e)
    }
}

// ─── Status enum ─────────────────────────────────────────────────────────────

/// Battery charge/discharge status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryStatus {
    /// Battery is charging.
    Charging,
    /// Battery is discharging.
    Discharging,
    /// Battery is full.
    Full,
    /// Battery is not charging (plugged in but not charging, some firmware).
    NotCharging,
    /// Status could not be determined.
    Unknown,
}

impl BatteryStatus {
    /// Parse a sysfs status string into a [`BatteryStatus`].
    pub fn from_sysfs(s: &str) -> Self {
        match s.trim() {
            "Charging" => BatteryStatus::Charging,
            "Discharging" => BatteryStatus::Discharging,
            "Full" => BatteryStatus::Full,
            "Not charging" => BatteryStatus::NotCharging,
            _ => BatteryStatus::Unknown,
        }
    }

    /// Return the original sysfs casing string.
    ///
    /// Used in CSV log output to preserve the exact format of the bash scripts.
    pub fn sysfs_str(self) -> &'static str {
        match self {
            BatteryStatus::Charging => "Charging",
            BatteryStatus::Discharging => "Discharging",
            BatteryStatus::Full => "Full",
            BatteryStatus::NotCharging => "Not charging",
            BatteryStatus::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for BatteryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Lowercase form for human-readable output.
        match self {
            BatteryStatus::Charging => write!(f, "charging"),
            BatteryStatus::Discharging => write!(f, "discharging"),
            BatteryStatus::Full => write!(f, "full"),
            BatteryStatus::NotCharging => write!(f, "not charging"),
            BatteryStatus::Unknown => write!(f, "unknown"),
        }
    }
}

// ─── Reading ─────────────────────────────────────────────────────────────────

/// A single battery reading.
#[derive(Debug, Clone)]
pub struct BatteryReading {
    /// Charge percentage (0..=100).
    pub percent: u8,
    /// Current charge/discharge status.
    pub status: BatteryStatus,
    /// Estimated time to empty.
    ///
    /// `None` when charging, full, or the sysfs files needed to compute it
    /// are absent or report zero power draw.
    pub time_to_empty: Option<Duration>,
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Abstraction over battery hardware.
///
/// Implement this trait to provide battery readings from a real or simulated
/// source. Implementations must be `Send + Sync` so they can be shared across
/// threads without restriction.
pub trait BatteryReader: Send + Sync {
    /// Read the current battery state.
    fn read(&self) -> Result<BatteryReading, BatteryError>;
}

// ─── Linux sysfs reader ───────────────────────────────────────────────────────

/// Reads battery state from the Linux sysfs power-supply interface.
///
/// Scans `/sys/class/power_supply/` (or an override root for tests) for the
/// first entry whose `type` file contains `Battery`. Reads `capacity`,
/// `status`, and optionally `energy_now` + `power_now` (or `charge_now` +
/// `current_now`) to compute [`BatteryReading::time_to_empty`].
pub struct LinuxSysfsReader {
    root: PathBuf,
}

impl LinuxSysfsReader {
    /// Create a reader backed by the real sysfs tree.
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/sys/class/power_supply"),
        }
    }

    /// Create a reader backed by a custom root directory.
    ///
    /// Used in tests to point at a fake sysfs tree built with `tempfile`.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn find_battery_dir(&self) -> Result<PathBuf, BatteryError> {
        let entries = std::fs::read_dir(&self.root).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BatteryError::NotFound
            } else {
                BatteryError::Io(e)
            }
        })?;

        let mut candidates: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let type_file = p.join("type");
                std::fs::read_to_string(&type_file)
                    .map(|s| s.trim() == "Battery")
                    .unwrap_or(false)
            })
            .collect();

        // Sort for deterministic BAT0-before-BAT1 ordering.
        candidates.sort();
        candidates.into_iter().next().ok_or(BatteryError::NotFound)
    }

    fn read_u64(path: &Path) -> Option<u64> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    }

    fn compute_time_to_empty(bat_dir: &Path) -> Option<Duration> {
        // Try energy_now (µWh) / power_now (µW) first.
        let energy_now = Self::read_u64(&bat_dir.join("energy_now"));
        let power_now = Self::read_u64(&bat_dir.join("power_now"));

        if let (Some(e), Some(p)) = (energy_now, power_now)
            && let Some(secs) = (e * 3600).checked_div(p)
        {
            return Some(Duration::from_secs(secs));
        }

        // Fallback: charge_now (µAh) / current_now (µA).
        let charge_now = Self::read_u64(&bat_dir.join("charge_now"));
        let current_now = Self::read_u64(&bat_dir.join("current_now"));

        if let (Some(c), Some(i)) = (charge_now, current_now)
            && let Some(secs) = (c * 3600).checked_div(i)
        {
            return Some(Duration::from_secs(secs));
        }

        None
    }
}

impl Default for LinuxSysfsReader {
    fn default() -> Self {
        Self::new()
    }
}

impl BatteryReader for LinuxSysfsReader {
    fn read(&self) -> Result<BatteryReading, BatteryError> {
        let bat_dir = self.find_battery_dir()?;

        // capacity: 0..=100
        let capacity_str =
            std::fs::read_to_string(bat_dir.join("capacity")).map_err(BatteryError::Io)?;
        let percent = capacity_str
            .trim()
            .parse::<u8>()
            .map_err(|e| BatteryError::Parse(format!("capacity: {e}")))?;

        // status
        let status_str =
            std::fs::read_to_string(bat_dir.join("status")).map_err(BatteryError::Io)?;
        let status = BatteryStatus::from_sysfs(&status_str);

        // time_to_empty: only when discharging
        let time_to_empty = if status == BatteryStatus::Discharging {
            Self::compute_time_to_empty(&bat_dir)
        } else {
            None
        };

        Ok(BatteryReading {
            percent,
            status,
            time_to_empty,
        })
    }
}

// ─── Unsupported stub ─────────────────────────────────────────────────────────

/// Battery reader for platforms where sysfs is unavailable.
///
/// Always returns [`BatteryError::Unsupported`].
pub struct UnsupportedReader {
    /// Name of the platform (e.g. `"macos"`, `"windows"`).
    pub platform: &'static str,
}

impl BatteryReader for UnsupportedReader {
    fn read(&self) -> Result<BatteryReading, BatteryError> {
        Err(BatteryError::Unsupported(self.platform))
    }
}

// ─── Default reader factory ───────────────────────────────────────────────────

/// Return the best [`BatteryReader`] for the current platform.
///
/// - Linux → [`LinuxSysfsReader`] backed by `/sys/class/power_supply`.
/// - All other targets → [`UnsupportedReader`].
pub fn default_reader() -> Box<dyn BatteryReader> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxSysfsReader::new())
    }
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(target_os = "macos")]
        let platform = "macos";
        #[cfg(target_os = "windows")]
        let platform = "windows";
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let platform = "this platform";
        Box::new(UnsupportedReader { platform })
    }
}

// ─── Mock reader (test helper) ────────────────────────────────────────────────

/// A mock [`BatteryReader`] that returns a fixed result.
///
/// Useful in unit tests where a real sysfs tree is unavailable.
pub struct MockBatteryReader {
    /// The result to return from [`BatteryReader::read`].
    pub reading: Result<BatteryReading, BatteryError>,
}

impl BatteryReader for MockBatteryReader {
    fn read(&self) -> Result<BatteryReading, BatteryError> {
        match &self.reading {
            Ok(r) => Ok(r.clone()),
            Err(BatteryError::NotFound) => Err(BatteryError::NotFound),
            Err(BatteryError::Unsupported(p)) => Err(BatteryError::Unsupported(p)),
            Err(BatteryError::Parse(s)) => Err(BatteryError::Parse(s.clone())),
            // io::Error isn't Clone; rebuild a fresh one preserving kind + message
            // so the mock reports the same variant the caller configured.
            Err(BatteryError::Io(e)) => Err(BatteryError::Io(std::io::Error::new(
                e.kind(),
                e.to_string(),
            ))),
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── Helper to build a fake sysfs tree ────────────────────────────────────

    struct FakeSysfs {
        dir: TempDir,
    }

    impl FakeSysfs {
        fn new() -> Self {
            Self {
                dir: TempDir::new().expect("create tempdir"),
            }
        }

        /// Add a power_supply entry with a given name and `type` value.
        fn add_entry(&self, name: &str, type_str: &str) -> PathBuf {
            let entry_dir = self.dir.path().join(name);
            fs::create_dir_all(&entry_dir).expect("create entry dir");
            fs::write(entry_dir.join("type"), type_str).expect("write type");
            entry_dir
        }

        /// Add a Battery entry with capacity + status.
        fn add_battery(&self, name: &str, capacity: u8, status: &str) -> PathBuf {
            let d = self.add_entry(name, "Battery\n");
            fs::write(d.join("capacity"), format!("{capacity}\n")).expect("write capacity");
            fs::write(d.join("status"), format!("{status}\n")).expect("write status");
            d
        }

        fn reader(&self) -> LinuxSysfsReader {
            LinuxSysfsReader::with_root(self.dir.path())
        }
    }

    // ── Status round-trip ─────────────────────────────────────────────────────

    #[test]
    fn status_from_sysfs_round_trip() {
        let cases = [
            ("Charging", BatteryStatus::Charging, "Charging", "charging"),
            (
                "Discharging",
                BatteryStatus::Discharging,
                "Discharging",
                "discharging",
            ),
            ("Full", BatteryStatus::Full, "Full", "full"),
            (
                "Not charging",
                BatteryStatus::NotCharging,
                "Not charging",
                "not charging",
            ),
            ("Unknown", BatteryStatus::Unknown, "Unknown", "unknown"),
            ("Bogus", BatteryStatus::Unknown, "Unknown", "unknown"),
        ];

        for (input, expected_variant, expected_sysfs, expected_display) in cases {
            let parsed = BatteryStatus::from_sysfs(input);
            assert_eq!(parsed, expected_variant, "from_sysfs({input:?})");
            assert_eq!(
                parsed.sysfs_str(),
                expected_sysfs,
                "sysfs_str() for {input:?}"
            );
            assert_eq!(
                parsed.to_string(),
                expected_display,
                "Display for {input:?}"
            );
        }
    }

    // ── Basic read: BAT0 ─────────────────────────────────────────────────────

    #[test]
    fn reads_basic_battery_info() {
        let fs = FakeSysfs::new();
        fs.add_battery("BAT0", 72, "Discharging");

        let reading = fs.reader().read().expect("read ok");
        assert_eq!(reading.percent, 72);
        assert_eq!(reading.status, BatteryStatus::Discharging);
    }

    // ── BAT0 vs BAT1 discovery: skips non-Battery type ───────────────────────

    #[test]
    fn skips_non_battery_entry() {
        let fs = FakeSysfs::new();
        // ACAD is an AC adapter — type is "Mains", not "Battery".
        fs.add_entry("ACAD", "Mains\n");
        fs.add_battery("BAT1", 55, "Charging");

        let reading = fs.reader().read().expect("read ok");
        assert_eq!(reading.percent, 55);
        assert_eq!(reading.status, BatteryStatus::Charging);
    }

    // ── BAT0 before BAT1 (sort order) ────────────────────────────────────────

    #[test]
    fn picks_bat0_before_bat1() {
        let fs = FakeSysfs::new();
        fs.add_battery("BAT1", 10, "Discharging");
        fs.add_battery("BAT0", 90, "Charging");

        let reading = fs.reader().read().expect("read ok");
        // BAT0 < BAT1 lexicographically → should be selected.
        assert_eq!(reading.percent, 90);
    }

    // ── NotFound when no Battery entries exist ────────────────────────────────

    #[test]
    fn not_found_when_no_battery() {
        let fs = FakeSysfs::new();
        fs.add_entry("ACAD", "Mains\n");

        let err = fs.reader().read().expect_err("should be NotFound");
        assert!(
            matches!(err, BatteryError::NotFound),
            "expected NotFound, got {err}"
        );
    }

    // ── time_to_empty math: energy_now / power_now ────────────────────────────

    #[test]
    fn time_to_empty_from_energy() {
        let fs = FakeSysfs::new();
        let d = fs.add_battery("BAT0", 50, "Discharging");
        // energy_now = 36_000_000 µWh, power_now = 18_000_000 µW
        // → secs = (36_000_000 * 3600) / 18_000_000 = 7200 s = 2 h
        fs::write(d.join("energy_now"), "36000000\n").unwrap();
        fs::write(d.join("power_now"), "18000000\n").unwrap();

        let reading = fs.reader().read().expect("read ok");
        assert_eq!(reading.time_to_empty, Some(Duration::from_secs(7200)));
    }

    // ── time_to_empty: fallback to charge_now / current_now ──────────────────

    #[test]
    fn time_to_empty_from_charge() {
        let fs = FakeSysfs::new();
        let d = fs.add_battery("BAT0", 50, "Discharging");
        // No energy_now; use charge_now = 3600 µAh, current_now = 1800 µA
        // → secs = (3600 * 3600) / 1800 = 7200 s
        fs::write(d.join("charge_now"), "3600\n").unwrap();
        fs::write(d.join("current_now"), "1800\n").unwrap();

        let reading = fs.reader().read().expect("read ok");
        assert_eq!(reading.time_to_empty, Some(Duration::from_secs(7200)));
    }

    // ── time_to_empty: None when neither energy_now nor charge_now ───────────

    #[test]
    fn time_to_empty_none_when_files_missing() {
        let fs = FakeSysfs::new();
        fs.add_battery("BAT0", 50, "Discharging");
        // No energy_now, no charge_now files.

        let reading = fs.reader().read().expect("read ok");
        assert!(
            reading.time_to_empty.is_none(),
            "expected None for time_to_empty"
        );
    }

    // ── time_to_empty: None when not discharging ──────────────────────────────

    #[test]
    fn time_to_empty_none_when_charging() {
        let fs = FakeSysfs::new();
        let d = fs.add_battery("BAT0", 80, "Charging");
        fs::write(d.join("energy_now"), "36000000\n").unwrap();
        fs::write(d.join("power_now"), "18000000\n").unwrap();

        let reading = fs.reader().read().expect("read ok");
        assert!(
            reading.time_to_empty.is_none(),
            "time_to_empty should be None while charging"
        );
    }

    // ── time_to_empty: None when power_now is zero ────────────────────────────

    #[test]
    fn time_to_empty_none_when_power_zero() {
        let fs = FakeSysfs::new();
        let d = fs.add_battery("BAT0", 50, "Discharging");
        fs::write(d.join("energy_now"), "36000000\n").unwrap();
        fs::write(d.join("power_now"), "0\n").unwrap();

        let reading = fs.reader().read().expect("read ok");
        assert!(
            reading.time_to_empty.is_none(),
            "time_to_empty should be None when power_now is 0"
        );
    }

    // ── UnsupportedReader always errors ───────────────────────────────────────

    #[test]
    fn unsupported_reader_errors() {
        let reader = UnsupportedReader {
            platform: "testplatform",
        };
        let err = reader.read().expect_err("should be Unsupported");
        assert!(
            matches!(err, BatteryError::Unsupported("testplatform")),
            "expected Unsupported, got {err}"
        );
    }

    // ── MockBatteryReader returns fixed reading ────────────────────────────────

    #[test]
    fn mock_reader_ok() {
        let mock = MockBatteryReader {
            reading: Ok(BatteryReading {
                percent: 42,
                status: BatteryStatus::Discharging,
                time_to_empty: Some(Duration::from_secs(3600)),
            }),
        };
        let r = mock.read().expect("mock ok");
        assert_eq!(r.percent, 42);
        assert_eq!(r.time_to_empty, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn mock_reader_not_found() {
        let mock = MockBatteryReader {
            reading: Err(BatteryError::NotFound),
        };
        assert!(matches!(mock.read(), Err(BatteryError::NotFound)));
    }
}
