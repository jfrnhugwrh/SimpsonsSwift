//! Writing an app bundle out of an archive and onto disk.
//!
//! Two rules matter here and both are enforced for every entry, not just for the
//! ones that look suspicious: nothing may be written outside the destination
//! directory (a ZIP entry named `../../../.ssh/authorized_keys` is legal ZIP and
//! is exactly what a hostile archive would use), and nothing may be created as a
//! symlink (which could point back out again).  Unix permission bits are taken
//! from the archive so the Mach-O comes out executable without the caller having
//! to know its name.

use std::path::{Component, Path, PathBuf};

use crate::error::{IpaError, Result};
use crate::zip::{Entry, Zip};

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Refuse archives with more entries than this.
    pub max_entries: usize,
    /// Refuse to write more than this many bytes in total.
    pub max_total_bytes: u64,
    /// Files to make executable even if the archive did not record permission
    /// bits (Windows-written archives have none).
    pub force_executable: Vec<String>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            max_entries: 200_000,
            max_total_bytes: 8 << 30,
            force_executable: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub files: usize,
    pub directories: usize,
    pub bytes: u64,
    /// Entries that were deliberately not written (symlinks, sockets, ...).
    pub skipped: Vec<String>,
}

/// Extract every entry under `app_dir` into `dest`, flattening away the
/// `Payload/<Name>.app/` prefix: `Payload/App.app/Info.plist` becomes
/// `dest/Info.plist`, so `dest` *is* the bundle directory afterwards.
pub fn extract_app(
    zip: &Zip,
    app_dir: &str,
    dest: &Path,
    options: &ExtractOptions,
) -> Result<ExtractReport> {
    let prefix = format!("{}/", app_dir.trim_end_matches('/'));
    let selected: Vec<&Entry> = zip.entries().iter().filter(|entry| entry.name.starts_with(&prefix)).collect();
    if selected.is_empty() {
        return Err(IpaError::Corrupt {
            what: format!("the archive has no files under {app_dir}/"),
        });
    }
    if selected.len() > options.max_entries {
        return Err(IpaError::TooLarge {
            what: format!("the {app_dir} bundle holds {} files", selected.len()),
            limit: options.max_entries as u64,
        });
    }

    std::fs::create_dir_all(dest)
        .map_err(|e| IpaError::Io { path: dest.display().to_string(), message: e.to_string() })?;

    let mut report = ExtractReport::default();
    for entry in selected {
        let relative = match relative_components(&entry.name, &prefix) {
            Ok(components) => components,
            Err(IpaError::UnsafePath(_)) => return Err(IpaError::UnsafePath(entry.name.clone())),
            // An entry that is not inside the bundle at all (cannot happen for
            // the filtered set, but the filter is cheap to re-check).
            Err(_) => continue,
        };
        if relative.is_empty() {
            continue;
        }
        let mut path = dest.to_path_buf();
        for component in &relative {
            path.push(component);
        }

        if entry.is_directory() {
            create_dir(&path)?;
            report.directories += 1;
            continue;
        }
        if entry.is_symlink() {
            report.skipped.push(entry.name.clone());
            continue;
        }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o17_0000 != 0o10_0000 && mode & 0o17_0000 != 0 {
                // Sockets, fifos and devices have no business in an app bundle.
                report.skipped.push(entry.name.clone());
                continue;
            }
        }

        let contents = zip.read(entry)?;
        report.bytes += contents.len() as u64;
        if report.bytes > options.max_total_bytes {
            return Err(IpaError::TooLarge {
                what: format!("the extracted {app_dir} bundle"),
                limit: options.max_total_bytes,
            });
        }
        if let Some(parent) = path.parent() {
            create_dir(parent)?;
        }
        std::fs::write(&path, &contents)
            .map_err(|e| IpaError::Io { path: path.display().to_string(), message: e.to_string() })?;

        let name = relative.last().cloned().unwrap_or_default();
        let executable =
            entry.is_executable() || options.force_executable.iter().any(|wanted| *wanted == name);
        set_mode(&path, if executable { 0o755 } else { 0o644 })?;
        report.files += 1;
    }
    if report.files == 0 {
        return Err(IpaError::Corrupt {
            what: format!("{app_dir} contained no regular files"),
        });
    }
    Ok(report)
}

/// Split an archive path into safe components relative to `prefix`.
fn relative_components(name: &str, prefix: &str) -> Result<Vec<String>> {
    let rest = name.strip_prefix(prefix).ok_or_else(|| IpaError::NotFound(name.to_string()))?;
    let mut components = Vec::new();
    for part in rest.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        // `..` is the zip-slip escape; a backslash is a separator on Windows,
        // where this crate also has to be safe.
        if part == ".." || part.contains('\\') || part.contains(':') {
            return Err(IpaError::UnsafePath(name.to_string()));
        }
        components.push(part.to_string());
    }
    Ok(components)
}

/// Create a directory, tolerating one that is already there.
fn create_dir(path: &Path) -> Result<()> {
    match std::fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) => Err(IpaError::Io { path: path.display().to_string(), message: error.to_string() }),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions)
        .map_err(|e| IpaError::Io { path: path.display().to_string(), message: e.to_string() })
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// Final safety net: confirm a path really is inside `root`.  [`relative_components`]
/// already refuses the escapes, but the check is cheap and the destination is
/// user-controlled.
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut seen = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                seen.pop();
            }
            Component::CurDir => {}
            other => seen.push(other.as_os_str()),
        }
    }
    seen.starts_with(&root) || candidate.starts_with(&root)
}
