//! IPA import: turn a user's own copy of *The Simpsons Arcade* into something
//! the emulator can run.
//!
//! The emulator itself never ships, downloads or hard-codes a game file — the
//! user supplies `The Simpsons Arcade v1.1.43.ipa`, and this crate does the rest:
//!
//! ```text
//! .ipa  ->  ZIP central directory          (zip.rs, inflate.rs)
//!       ->  Payload/<Name>.app/            (inspect.rs)
//!       ->  Info.plist -> CFBundleExecutable
//!       ->  Mach-O: armv7 slice? encrypted?
//!       ->  extract the bundle to the game library   (extract.rs, library.rs)
//!       ->  import.json manifest
//! ```
//!
//! ```no_run
//! // Validate and import a file the user picked.
//! let file = std::path::Path::new("The Simpsons Arcade v1.1.43.ipa");
//! let report = ipa::import_path(file, &ipa::ImportOptions::default())?;
//! println!("{} -> {}", report.bundle.summary(), report.game.executable().display());
//!
//! // The extracted bundle is an ordinary iOS app bundle, so the emulator's
//! // existing entry point is all that is needed:
//! //   simpsons-emu run <executable> --bundle <bundle dir>
//! # Ok::<(), ipa::IpaError>(())
//! ```
//!
//! Layout of the crate:
//!
//! * [`zip`] — central directory, local headers, CRC-32, ZIP64.
//! * [`inflate`] — the DEFLATE decoder the ZIP reader uses.
//! * [`plist`] — `Info.plist` in either the XML or the binary dialect.
//! * [`inspect`] — bundle discovery and the armv7/encryption/title checks.
//! * [`extract`] — safe extraction to a destination directory.
//! * [`library`] — where imported bundles live and the manifest beside them.
//! * [`json`] — the manifest's format, and the preview server's responses.

pub mod error;
pub mod extract;
pub mod inflate;
pub mod inspect;
pub mod json;
pub mod library;
pub mod plist;
pub mod test_support;
pub mod zip;

pub use error::{IpaError, Result};
pub use extract::{extract_app, ExtractOptions, ExtractReport};
pub use inflate::inflate;
pub use inspect::{
    app_bundle_dirs, arch_name, armv7_slice, inspect, mach_architectures, match_app, AppBundle,
    ArchSlice, InfoPlist, TitleMatch, EXPECTED_TITLE, EXPECTED_VERSION, SUPPORTED_APPS,
};
pub use library::{default_root, list, slug, ImportedGame, Manifest, MANIFEST_NAME};
pub use zip::{crc32, Entry, Zip};

use std::path::{Path, PathBuf};

/// Largest `.ipa` we will read into memory.  The game is ~50 MB; this is only a
/// guard against pointing the importer at something enormous by mistake.
pub const MAX_IPA_BYTES: u64 = 2 << 30;

/// How an import should behave.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Library root to import into (`None` = [`default_root`]).
    pub root: Option<PathBuf>,
    /// Which bundle to use when the archive holds more than one.
    pub app: Option<String>,
    /// Replace an existing import instead of refusing.
    pub force: bool,
    /// Accept an existing import as-is instead of extracting again (what
    /// `run`/`info`/`dump` want when handed an `.ipa` twice).
    pub reuse: bool,
    /// Import an IPA that is a valid iOS app but not the game we target.
    pub allow_other_app: bool,
    /// Extraction limits.
    pub max_entries: usize,
    pub max_total_bytes: u64,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            root: None,
            app: None,
            force: false,
            reuse: false,
            allow_other_app: false,
            max_entries: ExtractOptions::default().max_entries,
            max_total_bytes: ExtractOptions::default().max_total_bytes,
        }
    }
}

/// Everything an import produced.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// Where the bundle now lives.
    pub game: ImportedGame,
    /// What the archive was found to contain.
    pub bundle: AppBundle,
    /// Extraction statistics (empty when the import was reused).
    pub report: ExtractReport,
    /// True when an existing import was accepted instead of re-extracting.
    pub reused: bool,
    /// Library root used.
    pub root: PathBuf,
}

/// True when `path` looks like an IPA by name — what the CLI uses to decide
/// whether `run foo.ipa` means "import first".
pub fn looks_like_ipa(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("ipa"))
}

/// True when `bytes` start with a ZIP signature.
pub fn has_zip_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == b'P' && bytes[1] == b'K' && matches!(bytes[2..4], [3, 4] | [5, 6] | [7, 8])
}

/// Read an `.ipa` from disk, rejecting files that are too large to be one.
pub fn read_ipa(path: &Path) -> Result<Vec<u8>> {
    let size = std::fs::metadata(path)
        .map_err(|e| IpaError::Io { path: path.display().to_string(), message: e.to_string() })?
        .len();
    if size > MAX_IPA_BYTES {
        return Err(IpaError::TooLarge { what: format!("{size}-byte file"), limit: MAX_IPA_BYTES });
    }
    std::fs::read(path).map_err(|e| IpaError::Io { path: path.display().to_string(), message: e.to_string() })
}

/// Validate an archive without extracting anything.
pub fn inspect_bytes(data: &[u8], app: Option<&str>) -> Result<AppBundle> {
    let archive = Zip::open(data)?;
    inspect(&archive, app)
}

/// Validate a file on disk without extracting anything.
pub fn inspect_path(path: &Path, app: Option<&str>) -> Result<AppBundle> {
    let data = read_ipa(path)?;
    inspect_bytes(&data, app)
}

/// Validate and extract an archive that is already in memory (what the preview
/// server's upload endpoint uses).
pub fn import_bytes(data: &[u8], source_name: &str, options: &ImportOptions) -> Result<ImportReport> {
    let archive = Zip::open(data)?;
    let bundle = inspect(&archive, options.app.as_deref())?;
    if bundle.title_match == TitleMatch::Unknown && !options.allow_other_app {
        return Err(IpaError::UnsupportedApp {
            bundle_id: bundle.info.bundle_id.clone(),
            version: bundle.version(),
            display_name: bundle.info.display_name.clone().or_else(|| bundle.info.bundle_name.clone()),
        });
    }

    let root = options.root.clone().unwrap_or_else(default_root);
    let directory_name = slug(bundle.info.bundle_id.as_deref(), &bundle.app_name);
    let dir = root.join(&directory_name);
    if !extract::is_within(&root, &dir) {
        return Err(IpaError::UnsafePath(dir.display().to_string()));
    }

    // An existing import is either reused (`run` calling back in), replaced
    // (`--force`) or reported so the user is not silently overwritten.
    if dir.join(&bundle.app_name).join(&bundle.executable_name).is_file() {
        if options.reuse || options.force {
            if options.reuse {
                let game = ImportedGame::load(&dir).unwrap_or_else(|_| ImportedGame {
                    dir: dir.clone(),
                    manifest: manifest_for(&bundle, source_name, data.len() as u64, &ExtractReport::default()),
                });
                return Ok(ImportReport {
                    game,
                    bundle,
                    report: ExtractReport::default(),
                    reused: true,
                    root,
                });
            }
        } else {
            return Err(IpaError::AlreadyImported { dir: dir.display().to_string() });
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| IpaError::Io { path: dir.display().to_string(), message: e.to_string() })?;
    }

    let extract_options = ExtractOptions {
        max_entries: options.max_entries,
        max_total_bytes: options.max_total_bytes,
        force_executable: vec![bundle.executable_name.clone()],
    };
    // The bundle keeps its own directory name, so what ends up on disk is an
    // ordinary iOS app bundle: `<library>/<slug>/<App>.app/...`, which is what
    // `simpsons-emu run <binary> --bundle <bundle dir>` wants.
    let bundle_dir = dir.join(&bundle.app_name);
    let report = extract_app(&archive, &bundle.app_dir, &bundle_dir, &extract_options)?;
    let manifest = manifest_for(&bundle, source_name, data.len() as u64, &report);
    manifest.write(&dir)?;
    Ok(ImportReport {
        game: ImportedGame { dir, manifest },
        bundle,
        report,
        reused: false,
        root,
    })
}

/// Validate and extract an `.ipa` from disk.
pub fn import_path(path: &Path, options: &ImportOptions) -> Result<ImportReport> {
    let data = read_ipa(path)?;
    let source_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    import_bytes(&data, &source_name, options)
}

/// Import an `.ipa` unless it is already in the library, in which case reuse it.
/// This is what `simpsons-emu run <file>.ipa` wants: the second time it is run it
/// must not fail, and it must not re-extract 50 MB.
pub fn ensure_imported(path: &Path, options: &ImportOptions) -> Result<ImportReport> {
    let mut options = options.clone();
    options.reuse = true;
    import_path(path, &options)
}

/// Look an already-imported game up by slug, label or bundle name.
pub fn find_game(root: &Path, needle: &str) -> Option<ImportedGame> {
    list(root).into_iter().find(|game| {
        let manifest = &game.manifest;
        game.dir.file_name().is_some_and(|name| name.eq_ignore_ascii_case(needle))
            || game.label().eq_ignore_ascii_case(needle)
            || manifest.title.eq_ignore_ascii_case(needle)
            || manifest.app_bundle.eq_ignore_ascii_case(needle)
            || manifest.bundle_id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(needle))
    })
}

fn manifest_for(
    bundle: &AppBundle,
    source_file: &str,
    source_size: u64,
    report: &ExtractReport,
) -> Manifest {
    Manifest {
        title: bundle.title.to_string(),
        title_match: bundle.title_match.as_key().to_string(),
        app_bundle: bundle.app_name.clone(),
        executable: bundle.executable_name.clone(),
        bundle_id: bundle.info.bundle_id.clone(),
        display_name: bundle.info.display_name.clone(),
        version: bundle.version(),
        build: bundle.info.bundle_version.clone(),
        architectures: bundle.architectures.iter().map(|a| a.name.clone()).collect(),
        source_file: source_file.to_string(),
        source_size,
        files: report.files,
        bytes: report.bytes,
        imported_unix: library::now_unix(),
    }
}

#[cfg(test)]
mod tests;
