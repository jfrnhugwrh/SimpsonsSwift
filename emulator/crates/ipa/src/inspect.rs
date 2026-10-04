//! Deciding whether an archive really is *the* game this emulator can run.
//!
//! An `.ipa` is just a ZIP, so "it unzipped" says nothing.  This module walks the
//! archive the way a launcher would — find `Payload/<Name>.app/`, read its
//! `Info.plist`, take the executable named by `CFBundleExecutable` — and then
//! asks the three questions that decide whether the ARMv7 emulator can do
//! anything with it:
//!
//! * does the Mach-O have a 32-bit ARM slice at all?  (arm64-only builds and
//!   simulator builds have none)
//! * is that slice still FairPlay encrypted?  (App Store packages are, and no
//!   emulator can read them)
//! * is it the game we target, rather than some other iOS app the user happened
//!   to pick?
//!
//! Each of those produces its own [`IpaError`] variant so the message the user
//! sees says what to do about it.

use macho::{MachO, CPU_SUBTYPE_ARM_V6, CPU_SUBTYPE_ARM_V7, CPU_SUBTYPE_ARM_V7K, CPU_SUBTYPE_ARM_V7S, CPU_TYPE_ARM, CPU_TYPE_ARM64, CPU_TYPE_X86, CPU_TYPE_X86_64, FAT_MAGIC, MH_MAGIC, MH_MAGIC_64};

use crate::error::{IpaError, Result};
use crate::plist::Plist;
use crate::zip::Zip;

/// The game this emulator exists for.  Everything else is "a valid IPA that
/// happens not to be the one we can run".
pub const EXPECTED_TITLE: &str = "The Simpsons Arcade";

/// The version the decompilation in the repository root (`simpsons3_iphone_en.txt`)
/// was produced from, and the one the emulator's HLE surface is written against.
pub const EXPECTED_VERSION: &str = "1.1.43";

/// Identification metadata for a supported title.
///
/// Only publicly documented facts about the release live here (bundle
/// identifier, display name, version) — no part of the game itself, which the
/// user has to supply.
pub struct KnownApp {
    pub title: &'static str,
    pub bundle_ids: &'static [&'static str],
    pub versions: &'static [&'static str],
    pub display_names: &'static [&'static str],
    pub executables: &'static [&'static str],
}

pub const SUPPORTED_APPS: &[KnownApp] = &[KnownApp {
    title: "The Simpsons Arcade",
    // The App Store build ships as `com.ea.simpsonsarcade.bv`; older
    // re-packaged copies of the same title have been seen without the suffix.
    bundle_ids: &["com.ea.simpsonsarcade.bv", "com.ea.simpsonsarcade"],
    versions: &["1.1.43"],
    display_names: &["TheSimpsons", "The Simpsons Arcade", "Simpsons Arcade", "Simpsons"],
    executables: &["TheSimpsons", "Simpsons"],
}];

/// How closely an imported bundle matches a supported title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMatch {
    /// Right title, right version.
    Exact,
    /// Right title, different version — the emulator will try, but the HLE
    /// surface was written against [`EXPECTED_VERSION`].
    OtherVersion,
    /// Not a title this emulator targets.
    Unknown,
}

impl TitleMatch {
    pub fn describe(self) -> &'static str {
        match self {
            TitleMatch::Exact => "supported title and version",
            TitleMatch::OtherVersion => "supported title, different version",
            TitleMatch::Unknown => "not a title this emulator targets",
        }
    }
}

/// What `Info.plist` says about the bundle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InfoPlist {
    pub bundle_id: Option<String>,
    pub bundle_name: Option<String>,
    pub display_name: Option<String>,
    pub executable: Option<String>,
    /// `CFBundleShortVersionString` — the marketing version (`1.1.43`).
    pub short_version: Option<String>,
    /// `CFBundleVersion` — the build number.
    pub bundle_version: Option<String>,
    pub minimum_os: Option<String>,
    pub platform: Option<String>,
    pub device_families: Vec<i64>,
}

impl InfoPlist {
    /// The name to show the user: display name, else bundle name, else the
    /// bundle identifier.
    pub fn label(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| self.bundle_name.clone())
            .or_else(|| self.bundle_id.clone())
            .unwrap_or_else(|| "unknown app".to_string())
    }

    pub fn version(&self) -> Option<String> {
        self.short_version.clone().or_else(|| self.bundle_version.clone())
    }
}

/// One architecture in a (possibly universal) Mach-O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchSlice {
    pub cputype: u32,
    pub cpusubtype: u32,
    pub name: String,
}

/// A located, validated application bundle inside an archive.
#[derive(Debug, Clone, PartialEq)]
pub struct AppBundle {
    /// Path of the bundle directory inside the archive (`Payload/TheSimpsons.app`).
    pub app_dir: String,
    /// Bundle directory name (`TheSimpsons.app`).
    pub app_name: String,
    /// Executable file name inside the bundle (`TheSimpsons`).
    pub executable_name: String,
    /// Full path of the executable inside the archive.
    pub executable_entry: String,
    pub info: InfoPlist,
    pub architectures: Vec<ArchSlice>,
    /// True when a 32-bit ARM slice is present.
    pub has_arm: bool,
    /// True when the ARM slice is still FairPlay encrypted.
    pub encrypted: bool,
    pub executable_size: usize,
    pub title: &'static str,
    pub title_match: TitleMatch,
}

impl AppBundle {
    /// The version string to show: `CFBundleShortVersionString`, falling back to
    /// the build number.
    pub fn version(&self) -> Option<String> {
        self.info.version()
    }

    /// A one-line summary, used by the CLI and the preview server.
    pub fn summary(&self) -> String {
        format!(
            "{} {} ({}) — {}, {}",
            self.info.label(),
            self.version().unwrap_or_else(|| "?".to_string()),
            self.info.bundle_id.clone().unwrap_or_else(|| "no bundle id".to_string()),
            if self.architectures.is_empty() {
                "no readable architecture".to_string()
            } else {
                self.architectures.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join("+")
            },
            self.title_match.describe()
        )
    }
}

/// Every `<top>/<Name>.app` directory the archive contains.
///
/// Directory entries are optional in a ZIP, so this is derived from the paths of
/// the files rather than from `foo.app/` records.
pub fn app_bundle_dirs(zip: &Zip) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    for entry in zip.entries() {
        let parts: Vec<&str> = entry.name.split('/').collect();
        // `Payload/App.app/anything`: three components, `.app` in the middle.
        if parts.len() >= 3 && parts[1].ends_with(".app") && !parts[1].is_empty() {
            let dir = format!("{}/{}", parts[0], parts[1]);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs.sort();
    dirs
}

/// Any `.app`-looking path anywhere in the archive, for the "there is no bundle
/// here at all" message.
fn app_looking_paths(zip: &Zip) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for entry in zip.entries() {
        if let Some(part) = entry.name.split('/').find(|part| part.ends_with(".app")) {
            if !found.contains(&part.to_string()) {
                found.push(part.to_string());
            }
        }
    }
    found.sort();
    found.truncate(5);
    found
}

/// Locate and validate the application bundle in an archive.
///
/// `want` optionally names the bundle (`TheSimpsons` or `TheSimpsons.app`) for
/// archives that hold more than one.
pub fn inspect(zip: &Zip, want: Option<&str>) -> Result<AppBundle> {
    let dirs = app_bundle_dirs(zip);
    if dirs.is_empty() {
        return Err(IpaError::NoAppBundle { found: app_looking_paths(zip) });
    }

    let with_plist: Vec<String> =
        dirs.iter().filter(|dir| zip.find(&format!("{dir}/Info.plist")).is_some()).cloned().collect();
    let candidates: Vec<String> = match want {
        Some(name) => {
            let needle = name.trim_end_matches('/');
            let selected: Vec<String> = with_plist
                .iter()
                .filter(|dir| {
                    let last = dir.rsplit('/').next().unwrap_or("");
                    last.eq_ignore_ascii_case(needle) || last.eq_ignore_ascii_case(&format!("{needle}.app"))
                })
                .cloned()
                .collect();
            if selected.is_empty() {
                return Err(IpaError::NotFound(format!(
                    "app bundle {name:?} (the archive holds {})",
                    with_plist.iter().map(|d| d.rsplit('/').next().unwrap_or("?")).collect::<Vec<_>>().join(", ")
                )));
            }
            selected
        }
        None => with_plist.clone(),
    };
    if candidates.is_empty() {
        return Err(IpaError::InfoPlist {
            app_dir: dirs[0].clone(),
            message: "the bundle has no Info.plist".to_string(),
        });
    }
    if candidates.len() > 1 {
        return Err(IpaError::AmbiguousBundles(
            candidates.iter().map(|d| d.rsplit('/').next().unwrap_or("?").to_string()).collect(),
        ));
    }
    let app_dir = candidates[0].clone();

    let app_name = app_dir.rsplit('/').next().unwrap_or("").to_string();

    // ---- Info.plist -------------------------------------------------------
    let plist_entry = zip.find(&format!("{app_dir}/Info.plist")).expect("checked above");
    let plist_bytes = zip.read(plist_entry)?;
    let plist = Plist::parse(&plist_bytes).map_err(|e| IpaError::InfoPlist {
        app_dir: app_dir.clone(),
        message: e.to_string(),
    })?;
    let info = InfoPlist {
        bundle_id: plist.string_at("CFBundleIdentifier"),
        bundle_name: plist.string_at("CFBundleName"),
        display_name: plist.string_at("CFBundleDisplayName"),
        executable: plist.string_at("CFBundleExecutable"),
        short_version: plist.string_at("CFBundleShortVersionString"),
        bundle_version: plist.string_at("CFBundleVersion"),
        minimum_os: plist.string_at("MinimumOSVersion"),
        platform: plist.string_at("DTPlatformName").or_else(|| {
            plist
                .get("CFBundleSupportedPlatforms")
                .and_then(|value| value.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.as_str())
                .map(|s| s.to_string())
        }),
        device_families: plist
            .get("UIDeviceFamily")
            .and_then(|v| v.as_array())
            .map(|items| items.iter().filter_map(|item| item.as_int()).collect())
            .unwrap_or_default(),
    };
    if info.bundle_id.is_none() && info.executable.is_none() {
        return Err(IpaError::InfoPlist {
            app_dir,
            message: "it has neither CFBundleIdentifier nor CFBundleExecutable, so it is not an iOS app bundle".to_string(),
        });
    }

    // ---- the executable ---------------------------------------------------
    let fallback = app_name.trim_end_matches(".app").to_string();
    let mut tried: Vec<String> = Vec::new();
    let mut executable_name: Option<String> = None;
    for candidate in [info.executable.clone(), Some(fallback.clone())] {
        let Some(candidate) = candidate else { continue };
        if candidate.is_empty() {
            continue;
        }
        tried.push(candidate.clone());
        if zip.find(&format!("{app_dir}/{candidate}")).is_some() {
            executable_name = Some(candidate);
            break;
        }
    }
    let Some(executable_name) = executable_name else {
        return Err(IpaError::NoExecutable { app_dir, tried });
    };
    let executable_entry = format!("{app_dir}/{executable_name}");
    let executable = zip.read(zip.find(&executable_entry).expect("checked above"))?;

    // ---- architectures, and whether the ARM slice is usable ---------------
    let architectures = mach_architectures(&executable);
    let arm_slice = armv7_slice(&executable);
    let has_arm = arm_slice.is_some();
    let encrypted = match arm_slice.clone() {
        Some(range) => MachO::from_bytes(executable[range].to_vec())
            .map(|image| image.is_encrypted())
            .unwrap_or(false),
        None => false,
    };
    if !has_arm {
        return Err(IpaError::NoArmSlice {
            entry: executable_entry,
            architectures: architectures.iter().map(|a| a.name.clone()).collect(),
        });
    }
    // Parsing the slice a second time catches "it is armv7 but not a Mach-O we
    // can load" (a 64-bit image sneaking through a fat header, a stub, ...).
    if let Some(range) = arm_slice {
        if let Err(error) = MachO::from_bytes(executable[range].to_vec()) {
            return Err(IpaError::NotMachO { entry: executable_entry, message: error.to_string() });
        }
    }
    if encrypted {
        return Err(IpaError::Encrypted { entry: executable_entry, cryptid: 1 });
    }

    let (title, title_match) = match_app(&info, &executable_name);
    Ok(AppBundle {
        app_dir,
        app_name,
        executable_name,
        executable_entry,
        info,
        architectures,
        has_arm,
        encrypted,
        executable_size: executable.len(),
        title,
        title_match,
    })
}

/// Compare a bundle's metadata against [`SUPPORTED_APPS`].
pub fn match_app(info: &InfoPlist, executable_name: &str) -> (&'static str, TitleMatch) {
    let equals_any = |value: &Option<String>, list: &[&'static str]| {
        value.as_deref().is_some_and(|v| list.iter().any(|candidate| candidate.eq_ignore_ascii_case(v)))
    };
    for app in SUPPORTED_APPS {
        let known = equals_any(&info.bundle_id, app.bundle_ids)
            || equals_any(&info.display_name, app.display_names)
            || equals_any(&info.bundle_name, app.display_names)
            || app.executables.iter().any(|name| name.eq_ignore_ascii_case(executable_name));
        if !known {
            continue;
        }
        let version = match_app_version(info, app);
        return (app.title, version);
    }
    ("unknown", TitleMatch::Unknown)
}

fn match_app_version(info: &InfoPlist, app: &KnownApp) -> TitleMatch {
    match info.version() {
        Some(version) if app.versions.iter().any(|known| *known == version) => TitleMatch::Exact,
        Some(_) => TitleMatch::OtherVersion,
        // No version at all: the bundle id/display name matched, which is
        // enough to call it the right title.
        None => TitleMatch::Exact,
    }
}

/// The architectures present in a Mach-O (a single entry for a thin image).
pub fn mach_architectures(data: &[u8]) -> Vec<ArchSlice> {
    if data.len() >= 8 && u32::from_be_bytes([data[0], data[1], data[2], data[3]]) == FAT_MAGIC {
        let count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut slices = Vec::new();
        for index in 0..count {
            let at = 8 + index * 20;
            if at + 20 > data.len() {
                break;
            }
            let cputype = be32(&data[at..]);
            let cpusubtype = be32(&data[at + 4..]);
            slices.push(ArchSlice { cputype, cpusubtype, name: arch_name(cputype, cpusubtype) });
        }
        return slices;
    }
    if data.len() < 12 {
        return Vec::new();
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != MH_MAGIC && magic != MH_MAGIC_64 {
        return Vec::new();
    }
    let cputype = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let cpusubtype = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    vec![ArchSlice { cputype, cpusubtype, name: arch_name(cputype, cpusubtype) }]
}

fn be32(data: &[u8]) -> u32 {
    u32::from_be_bytes([data[0], data[1], data[2], data[3]])
}

/// Human-readable architecture name, the way `lipo -info` writes them.
pub fn arch_name(cputype: u32, cpusubtype: u32) -> String {
    match (cputype, cpusubtype) {
        (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V6) => "armv6".to_string(),
        (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V7) => "armv7".to_string(),
        (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V7S) => "armv7s".to_string(),
        (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V7K) => "armv7k".to_string(),
        (CPU_TYPE_ARM, _) => "arm".to_string(),
        (CPU_TYPE_ARM64, _) => "arm64".to_string(),
        (CPU_TYPE_X86, _) => "i386".to_string(),
        (CPU_TYPE_X86_64, _) => "x86_64".to_string(),
        _ => format!("cputype {cputype:#x}"),
    }
}

/// Byte range of the 32-bit ARM slice, if the image has one.
pub fn armv7_slice(data: &[u8]) -> Option<core::ops::Range<usize>> {
    match macho::fat_slice(data, CPU_TYPE_ARM).ok()? {
        Some(range) => {
            // `fat_slice` falls back to the first slice when there is no ARM
            // one; make sure what comes back actually is ARM.
            let slice = &data[range.clone()];
            if slice.len() >= 8 && u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]) == CPU_TYPE_ARM {
                Some(range)
            } else {
                None
            }
        }
        None => {
            if data.len() >= 8
                && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == MH_MAGIC
                && u32::from_le_bytes([data[4], data[5], data[6], data[7]]) == CPU_TYPE_ARM
            {
                Some(0..data.len())
            } else {
                None
            }
        }
    }
}
