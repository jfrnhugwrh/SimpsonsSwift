//! The user-facing half of IPA import: the `import` and `games` commands, and
//! the resolution that lets `info`, `dump` and `run` take an `.ipa` (or an app
//! bundle directory) wherever they used to take a bare Mach-O.
//!
//! The emulator never ships or fetches the game — the user supplies their own
//! `The Simpsons Arcade v1.1.43.ipa`, [`ipa`] validates and extracts it into a
//! game library, and from then on the existing `run <binary> --bundle <dir>`
//! path is all that is involved.

use std::io::Read;
use std::path::{Path, PathBuf};

/// What a positional `<binary>` argument turned out to be.
pub struct Target {
    /// Mach-O to load.
    pub binary: String,
    /// Bundle directory the game's assets should be read from, if known.
    pub bundle: Option<String>,
    /// Lines to print before the command's own output (what was imported).
    pub notes: Vec<String>,
}

/// Build [`ipa::ImportOptions`] from the command line.
pub fn options_from(args: &[String]) -> ipa::ImportOptions {
    let mut options = ipa::ImportOptions::default();
    options.root = crate::flag(args, "--dest").map(PathBuf::from);
    options.app = crate::flag(args, "--app").map(|s| s.to_string());
    options.force = args.iter().any(|a| a == "--force");
    options.allow_other_app = args.iter().any(|a| a == "--allow-other-app");
    options
}

/// True when the file is a ZIP archive, whatever it is called.
fn is_archive(path: &Path) -> bool {
    if ipa::looks_like_ipa(path) {
        return true;
    }
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && ipa::has_zip_magic(&magic)
}

/// Resolve the positional argument: a Mach-O stays as it is, an `.ipa` is
/// imported into the library first and the extracted executable is used.
pub fn resolve(args: &[String], command: &str) -> Result<Target, String> {
    let given = args.first().ok_or_else(|| format!("{command}: missing <binary>"))?.clone();
    let path = Path::new(&given);
    let explicit_bundle = crate::flag(args, "--bundle").map(|s| s.to_string());
    if !path.exists() || !is_archive(path) {
        return Ok(Target { binary: given, bundle: explicit_bundle, notes: Vec::new() });
    }

    let options = options_from(args);
    let report = ipa::ensure_imported(path, &options).map_err(|e| e.to_string())?;
    let game = &report.game;
    let mut notes = Vec::new();
    if report.reused {
        notes.push(format!(
            "using the already-imported {} at {}",
            game.label(),
            game.dir.display()
        ));
    } else {
        notes.push(format!(
            "imported {} files ({} bytes) from {} into {}",
            report.report.files,
            report.report.bytes,
            given,
            game.dir.display()
        ));
    }
    if report.bundle.title_match != ipa::TitleMatch::Exact {
        notes.push(format!("warning: {}", report.bundle.summary()));
    }
    Ok(Target {
        binary: game.executable().to_string_lossy().into_owned(),
        bundle: explicit_bundle.or_else(|| Some(game.bundle_dir().to_string_lossy().into_owned())),
        notes,
    })
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

pub fn cmd_import(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("import: missing <file.ipa>")?;
    let path_buf = PathBuf::from(path);
    if !path_buf.exists() {
        return Err(format!("{path}: no such file"));
    }
    if !is_archive(&path_buf) {
        return Err(format!(
            "{path} is not a ZIP archive, so it cannot be an .ipa \
             (an .ipa is a ZIP containing Payload/<Name>.app/)"
        ));
    }

    let options = options_from(args);
    let root = options.root.clone().unwrap_or_else(ipa::default_root);
    let report = ipa::import_path(&path_buf, &options).map_err(|e| e.to_string())?;
    let game = &report.game;
    let bundle = &report.bundle;

    println!("{path}");
    println!("  title        {}", game.label());
    println!(
        "  bundle id    {}",
        bundle.info.bundle_id.clone().unwrap_or_else(|| "(none)".to_string())
    );
    println!(
        "  version      {} (build {})",
        bundle.version().unwrap_or_else(|| "?".to_string()),
        bundle.info.bundle_version.clone().unwrap_or_else(|| "?".to_string())
    );
    println!("  app          {}", bundle.app_dir);
    println!(
        "  executable   {} ({}, {} bytes)",
        bundle.executable_name,
        bundle.architectures.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join("+"),
        bundle.executable_size
    );
    if let Some(minimum) = &bundle.info.minimum_os {
        println!("  min ios      {minimum}");
    }
    println!("  match        {}", bundle.title_match.describe());
    println!(
        "  extracted    {} files, {} bytes",
        report.report.files, report.report.bytes
    );
    if !report.report.skipped.is_empty() {
        println!("  skipped      {} entries (symlinks and other non-files)", report.report.skipped.len());
    }
    println!("  library      {}", game.dir.display());
    println!("  games root   {}", root.display());

    if bundle.title_match != ipa::TitleMatch::Exact {
        println!("\nwarning: this emulator targets {} {}", ipa::EXPECTED_TITLE, ipa::EXPECTED_VERSION);
        println!("         {}", bundle.summary());
    }
    println!("\nrun it with:");
    println!(
        "    simpsons-emu run {} --bundle {} --serve 8080",
        game.executable().display(),
        game.bundle_dir().display()
    );
    println!("  or straight from the .ipa:");
    println!("    simpsons-emu run {path} --serve 8080");
    Ok(())
}

// ---------------------------------------------------------------------------
// games
// ---------------------------------------------------------------------------

pub fn cmd_games(args: &[String]) -> Result<(), String> {
    let root = crate::flag(args, "--dest").map(PathBuf::from).unwrap_or_else(ipa::default_root);
    let games = ipa::list(&root);
    println!("imported games in {}", root.display());
    if games.is_empty() {
        println!("\n  (none yet)");
        println!("\nimport your own copy of the game with:");
        println!("    simpsons-emu import \"The Simpsons Arcade v1.1.43.ipa\"");
        println!("\nthe emulator does not ship, download or distribute the game; the .ipa has to");
        println!("be a copy you obtained legally, and it must be decrypted.");
        return Ok(());
    }
    for game in &games {
        let manifest = &game.manifest;
        println!(
            "\n  {}   [{}]",
            game.label(),
            manifest.bundle_id.clone().unwrap_or_else(|| "no bundle id".to_string())
        );
        println!(
            "    version    {}   architectures   {}   match   {}",
            manifest.version.clone().unwrap_or_else(|| "?".to_string()),
            if manifest.architectures.is_empty() { "?".to_string() } else { manifest.architectures.join("+") },
            manifest.title_match
        );
        println!("    files      {}   bytes   {}", manifest.files, manifest.bytes);
        println!("    imported   {}", ipa::library::format_unix(manifest.imported_unix));
        println!("    from       {}", manifest.source_file);
        println!("    bundle     {}", game.bundle_dir().display());
        if game.is_usable() {
            println!("    run        simpsons-emu run {} --bundle {}", game.executable().display(), game.bundle_dir().display());
        } else {
            println!("    !! the executable {} is missing — re-import with --force", game.executable().display());
        }
    }
    Ok(())
}
