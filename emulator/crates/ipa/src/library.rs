//! The imported-game library: where extracted bundles live and how they are
//! found again.
//!
//! ```text
//! $XDG_DATA_HOME/simpsons-emu/games/          (default; override with $SIMPSONS_EMU_GAMES)
//!   com.ea.simpsonsarcade.bv/
//!     import.json                             written by the importer
//!     Info.plist, TheSimpsons, *.png, ...     the bundle, payload prefix removed
//! ```
//!
//! Keeping the extracted bundle instead of the `.ipa` means the emulator's
//! existing `run <binary> --bundle <dir>` path is all that is needed to play it,
//! and no copyrighted file is ever copied twice.

use std::path::{Path, PathBuf};

use crate::error::{IpaError, Result};
use crate::json::Json;
use crate::TitleMatch;

/// Name of the manifest written next to an imported bundle.
pub const MANIFEST_NAME: &str = "import.json";

/// Where imported games live unless the caller says otherwise.
pub fn default_root() -> PathBuf {
    if let Ok(dir) = std::env::var("SIMPSONS_EMU_GAMES") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("simpsons-emu").join("games")
}

/// A filesystem-safe directory name for a bundle: the bundle identifier when
/// there is one, otherwise the app name.
pub fn slug(bundle_id: Option<&str>, app_name: &str) -> String {
    let base = bundle_id.unwrap_or(app_name);
    let mut out = String::with_capacity(base.len());
    for character in base.chars() {
        match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => out.push(character),
            _ => out.push('-'),
        }
    }
    let trimmed = out.trim_matches(|character| character == '.' || character == '-').to_string();
    if trimmed.is_empty() {
        "imported-game".to_string()
    } else {
        trimmed
    }
}

/// What the importer recorded about a bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub title: String,
    pub title_match: String,
    /// Bundle directory name as extracted (`TheSimpsons.app`).
    pub app_bundle: String,
    /// Executable file name inside that directory.
    pub executable: String,
    pub bundle_id: Option<String>,
    pub display_name: Option<String>,
    pub version: Option<String>,
    pub build: Option<String>,
    pub architectures: Vec<String>,
    /// Name of the `.ipa` this came from (never its contents).
    pub source_file: String,
    pub source_size: u64,
    pub files: usize,
    pub bytes: u64,
    pub imported_unix: u64,
}

impl Manifest {
    pub fn to_json(&self) -> Json {
        Json::Object(vec![
            ("title".to_string(), Json::Str(self.title.clone())),
            ("title_match".to_string(), Json::Str(self.title_match.clone())),
            ("app_bundle".to_string(), Json::Str(self.app_bundle.clone())),
            ("executable".to_string(), Json::Str(self.executable.clone())),
            (
                "bundle_id".to_string(),
                self.bundle_id.clone().map(Json::Str).unwrap_or(Json::Null),
            ),
            (
                "display_name".to_string(),
                self.display_name.clone().map(Json::Str).unwrap_or(Json::Null),
            ),
            ("version".to_string(), self.version.clone().map(Json::Str).unwrap_or(Json::Null)),
            ("build".to_string(), self.build.clone().map(Json::Str).unwrap_or(Json::Null)),
            (
                "architectures".to_string(),
                Json::Array(self.architectures.iter().map(|a| Json::Str(a.clone())).collect()),
            ),
            ("source_file".to_string(), Json::Str(self.source_file.clone())),
            ("source_size".to_string(), Json::Number(self.source_size as f64)),
            ("files".to_string(), Json::Number(self.files as f64)),
            ("bytes".to_string(), Json::Number(self.bytes as f64)),
            ("imported_unix".to_string(), Json::Number(self.imported_unix as f64)),
        ])
    }

    pub fn from_json(value: &Json) -> Result<Manifest> {
        let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(|s| s.to_string());
        let optional = |key: &str| match value.get(key) {
            None | Some(Json::Null) => None,
            Some(other) => other.as_str().map(|s| s.to_string()),
        };
        Ok(Manifest {
            title: text("title").unwrap_or_else(|| "unknown".to_string()),
            title_match: text("title_match").unwrap_or_else(|| "unknown".to_string()),
            app_bundle: text("app_bundle").ok_or_else(|| IpaError::Corrupt {
                what: format!("the {MANIFEST_NAME} manifest has no app_bundle"),
            })?,
            executable: text("executable").ok_or_else(|| IpaError::Corrupt {
                what: format!("the {MANIFEST_NAME} manifest has no executable"),
            })?,
            bundle_id: optional("bundle_id"),
            display_name: optional("display_name"),
            version: optional("version"),
            build: optional("build"),
            architectures: value
                .get("architectures")
                .and_then(|v| match v {
                    Json::Array(items) => Some(items.clone()),
                    _ => None,
                })
                .map(|items| items.iter().filter_map(|item| item.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            source_file: text("source_file").unwrap_or_default(),
            source_size: value.get("source_size").and_then(|v| v.as_u64()).unwrap_or(0),
            files: value.get("files").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            bytes: value.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0),
            imported_unix: value.get("imported_unix").and_then(|v| v.as_u64()).unwrap_or(0),
        })
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        let path = dir.join(MANIFEST_NAME);
        let mut text = self.to_json().to_string_pretty();
        text.push('\n');
        std::fs::write(&path, text)
            .map_err(|e| IpaError::Io { path: path.display().to_string(), message: e.to_string() })
    }

    pub fn read(dir: &Path) -> Result<Manifest> {
        let path = dir.join(MANIFEST_NAME);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| IpaError::Io { path: path.display().to_string(), message: e.to_string() })?;
        let value = crate::json::parse(&text)?;
        Manifest::from_json(&value)
    }
}

impl TitleMatch {
    /// The spelling used in the manifest.
    pub fn as_key(self) -> &'static str {
        match self {
            TitleMatch::Exact => "exact",
            TitleMatch::OtherVersion => "other-version",
            TitleMatch::Unknown => "unknown",
        }
    }
}

/// An imported game: its directory plus the manifest describing it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedGame {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl ImportedGame {
    /// The bundle directory (`<dir>/TheSimpsons.app`).
    pub fn bundle_dir(&self) -> PathBuf {
        self.dir.join(&self.manifest.app_bundle)
    }

    /// The Mach-O the emulator should be given.
    pub fn executable(&self) -> PathBuf {
        self.bundle_dir().join(&self.manifest.executable)
    }

    /// A label for lists: `The Simpsons Arcade 1.1.43`.
    pub fn label(&self) -> String {
        let name = self
            .manifest
            .display_name
            .clone()
            .unwrap_or_else(|| self.manifest.title.clone());
        match &self.manifest.version {
            Some(version) => format!("{name} {version}"),
            None => name,
        }
    }

    /// True when the executable the manifest points at is actually there.
    pub fn is_usable(&self) -> bool {
        self.executable().is_file()
    }

    /// Build a game record from a directory that has a manifest.
    pub fn load(dir: &Path) -> Result<ImportedGame> {
        let manifest = Manifest::read(dir)?;
        Ok(ImportedGame { dir: dir.to_path_buf(), manifest })
    }
}

/// Every imported game under `root`, sorted by label.
pub fn list(root: &Path) -> Vec<ImportedGame> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut games: Vec<ImportedGame> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| ImportedGame::load(&entry.path()).ok())
        .collect();
    games.sort_by(|a, b| a.label().cmp(&b.label()));
    games
}

/// Seconds since the Unix epoch, or 0 when the clock is unavailable.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// `2026-10-04T13:22:07Z` from a Unix timestamp, without pulling in a date
/// library for one format string.
pub fn format_unix(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        (seconds % 86_400) / 3_600,
        (seconds % 3_600) / 60 % 60,
        seconds % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month,
/// day) in the proleptic Gregorian calendar.
fn civil_from_days(z0: i64) -> (i64, u32, u32) {
    let z = z0 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}
