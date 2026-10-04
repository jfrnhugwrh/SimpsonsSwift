//! Errors surfaced while reading, validating or extracting an IPA.
//!
//! Every variant carries enough detail for the CLI (and the preview server's
//! import panel) to tell the user *what* is wrong with their file and, where
//! there is one, what to do about it: an encrypted App Store binary and an
//! arm64-only binary are both "this cannot be run" but need very different
//! fixes, so they are different variants rather than one generic string.

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpaError {
    /// A filesystem operation failed.
    Io { path: String, message: String },
    /// The file is not a ZIP archive at all (bad magic or no end-of-directory).
    NotAZip(String),
    /// The ZIP structure is internally inconsistent or damaged.
    Corrupt { what: String },
    /// A well-formed ZIP feature this reader does not implement (e.g. a
    /// compression method other than stored/deflate, a multi-disk archive).
    Unsupported { what: String },
    /// The archive has no `Payload/<Something>.app/` bundle.
    NoAppBundle { found: Vec<String> },
    /// More than one `.app` bundle with an executable; the caller must say which.
    AmbiguousBundles(Vec<String>),
    /// The bundle has no `Info.plist`, or it could not be read.
    InfoPlist { app_dir: String, message: String },
    /// `CFBundleExecutable` is missing from `Info.plist` and no file with the
    /// bundle's name exists either.
    NoExecutable { app_dir: String, tried: Vec<String> },
    /// The bundle's executable is not a Mach-O we can read.
    NotMachO { entry: String, message: String },
    /// The executable has no 32-bit ARM slice, so the ARMv7 emulator cannot
    /// run it (an arm64-only app, a simulator build, ...).
    NoArmSlice { entry: String, architectures: Vec<String> },
    /// The executable is still FairPlay encrypted (`LC_ENCRYPTION_INFO`
    /// `cryptid != 0`), which is what an App Store `.ipa` contains.
    Encrypted { entry: String, cryptid: u32 },
    /// The IPA is a valid iOS app but not the game this emulator targets.
    UnsupportedApp { bundle_id: Option<String>, version: Option<String>, display_name: Option<String> },
    /// An import directory for this bundle already exists.
    AlreadyImported { dir: String },
    /// A ZIP entry tried to escape the destination directory (`..`, an absolute
    /// path, a drive letter, ...).
    UnsafePath(String),
    /// Something was bigger than the configured cap.
    TooLarge { what: String, limit: u64 },
    /// A Mach-O error from the `macho` crate.
    MachO(String),
    /// A requested import/game could not be found.
    NotFound(String),
}

impl fmt::Display for IpaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpaError::Io { path, message } => write!(f, "{path}: {message}"),
            IpaError::NotAZip(what) => write!(
                f,
                "not a ZIP archive, so not an .ipa ({what}); an .ipa is a ZIP with a Payload/ directory"
            ),
            IpaError::Corrupt { what } => write!(f, "corrupt or truncated archive: {what}"),
            IpaError::Unsupported { what } => write!(f, "unsupported: {what}"),
            IpaError::NoAppBundle { found } => {
                write!(f, "no Payload/<Name>.app/ bundle in the archive")?;
                if found.is_empty() {
                    write!(f, " (no .app directory at any depth)")
                } else {
                    write!(f, " (found: {})", found.join(", "))
                }
            }
            IpaError::AmbiguousBundles(bundles) => {
                write!(f, "the archive holds {} app bundles ({}); pass the one you want with --app", bundles.len(), bundles.join(", "))
            }
            IpaError::InfoPlist { app_dir, message } => {
                write!(f, "{app_dir}/Info.plist: {message}")
            }
            IpaError::NoExecutable { app_dir, tried } => write!(
                f,
                "{app_dir}: no executable (CFBundleExecutable missing and none of {} exist)",
                tried.join(", ")
            ),
            IpaError::NotMachO { entry, message } => write!(f, "{entry}: not a loadable Mach-O: {message}"),
            IpaError::NoArmSlice { entry, architectures } => write!(
                f,
                "{entry} has no 32-bit ARM slice (architectures: {}) — this emulator runs ARMv7 only, \
                 so it needs the iPhone/iPod build, not an arm64-only or simulator build",
                if architectures.is_empty() { "none readable".to_string() } else { architectures.join(", ") }
            ),
            IpaError::Encrypted { entry, cryptid } => write!(
                f,
                "{entry} is still FairPlay encrypted (cryptid {cryptid}); App Store .ipa files are \
                 encrypted and cannot be read by any emulator — supply a decrypted dump of a copy \
                 you own"
            ),
            IpaError::UnsupportedApp { bundle_id, version, display_name } => write!(
                f,
                "this is not {} (bundle id {}, version {}, display name {}); pass --allow-other-app \
                 to import it anyway",
                crate::EXPECTED_TITLE,
                bundle_id.as_deref().unwrap_or("?"),
                version.as_deref().unwrap_or("?"),
                display_name.as_deref().unwrap_or("?")
            ),
            IpaError::AlreadyImported { dir } => write!(
                f,
                "already imported at {dir} — run it with `simpsons-emu run {dir}`, or pass --force to re-import"
            ),
            IpaError::UnsafePath(path) => {
                write!(f, "archive entry {path:?} would write outside the destination directory; refusing")
            }
            IpaError::TooLarge { what, limit } => {
                write!(f, "{what} exceeds the {limit}-byte limit")
            }
            IpaError::MachO(message) => write!(f, "Mach-O: {message}"),
            IpaError::NotFound(what) => write!(f, "{what} not found"),
        }
    }
}

impl std::error::Error for IpaError {}

impl From<macho::MachOError> for IpaError {
    fn from(e: macho::MachOError) -> Self {
        IpaError::MachO(e.to_string())
    }
}

pub type Result<T> = core::result::Result<T, IpaError>;
