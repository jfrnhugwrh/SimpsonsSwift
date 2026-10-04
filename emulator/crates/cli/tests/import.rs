//! End-to-end tests of the command line: build an `.ipa` in memory, write it to
//! disk, and drive the real `simpsons-emu` binary against it.
//!
//! These are the tests for the user-facing flow, so they go through the process
//! boundary rather than calling into the crates.

use std::path::PathBuf;
use std::process::{Command, Output};

fn emu() -> Command {
    Command::new(env!("CARGO_BIN_EXE_simpsons-emu"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("simpsons-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> Output {
    emu().args(args).output().expect("the emulator binary runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A directory holding a copy of the fixture archive under its real name.
fn game_file(dir: &PathBuf) -> PathBuf {
    let path = dir.join("The Simpsons Arcade v1.1.43.ipa");
    std::fs::write(&path, ipa::test_support::simpsons_ipa()).unwrap();
    path
}

#[test]
fn games_starts_empty_and_says_how_to_fill_it() {
    let dir = scratch("games");
    let library = dir.join("games");
    let output = run(&["games", "--dest", library.to_str().unwrap()]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = text(&output);
    assert!(stdout.contains("(none yet)"), "{stdout}");
    assert!(stdout.contains("simpsons-emu import"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_validates_extracts_and_tells_you_what_to_run() {
    let dir = scratch("import");
    let library = dir.join("games");
    let ipa = game_file(&dir);
    let library_arg = library.to_str().unwrap();

    let output = run(&["import", ipa.to_str().unwrap(), "--dest", library_arg]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = text(&output);
    assert!(stdout.contains("The Simpsons Arcade 1.1.43"), "{stdout}");
    assert!(stdout.contains("com.ea.simpsonsarcade.bv"), "{stdout}");
    assert!(stdout.contains("armv7"), "{stdout}");
    assert!(stdout.contains("supported title and version"), "{stdout}");
    assert!(stdout.contains("simpsons-emu run"), "{stdout}");

    // The bundle is where the CLI said it is.
    let bundle = library.join("com.ea.simpsonsarcade.bv/TheSimpsons.app");
    assert!(bundle.join("TheSimpsons").is_file());
    assert!(bundle.join("Info.plist").is_file());
    assert!(library.join("com.ea.simpsonsarcade.bv/import.json").is_file());

    // ...and `games` lists it.
    let output = run(&["games", "--dest", library_arg]);
    let stdout = text(&output);
    assert!(stdout.contains("The Simpsons Arcade 1.1.43"), "{stdout}");
    assert!(stdout.contains("armv7"), "{stdout}");

    // Importing again without --force is refused, not silently overwritten.
    let output = run(&["import", ipa.to_str().unwrap(), "--dest", library_arg]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("--force"), "{}", text(&output));

    let output = run(&["import", ipa.to_str().unwrap(), "--dest", library_arg, "--force"]);
    assert!(output.status.success(), "{}", text(&output));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn info_dump_and_run_accept_an_ipa_directly() {
    let dir = scratch("direct");
    let library = dir.join("games");
    let ipa = game_file(&dir);
    let library_arg = library.to_str().unwrap();

    let output = run(&["info", ipa.to_str().unwrap(), "--dest", library_arg]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = text(&output);
    assert!(stdout.contains("imported"), "{stdout}");
    assert!(stdout.contains("armv7"), "{stdout}");
    assert!(stdout.contains("Segments:"), "{stdout}");

    let output = run(&[
        "dump",
        ipa.to_str().unwrap(),
        "--dest",
        library_arg,
        "--section",
        "__TEXT.__text",
        "--length",
        "16",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("00001000"), "{}", text(&output));

    // Booting it: the fixture executable is two instructions, so a small budget
    // is plenty; what matters is that the import-then-load path is wired up.
    let output = run(&[
        "run",
        ipa.to_str().unwrap(),
        "--dest",
        library_arg,
        "--max-insns",
        "10000",
        "--stats",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = text(&output);
    assert!(stdout.contains("using the already-imported"), "{stdout}");
    assert!(stdout.contains("stop reason"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_explains_what_is_wrong_with_a_bad_file() {
    let dir = scratch("bad");
    let library = dir.join("games");
    let library_arg = library.to_str().unwrap();

    // Not a ZIP at all.
    let text_file = dir.join("not-a-game.ipa");
    std::fs::write(&text_file, b"this is a text file with an .ipa extension").unwrap();
    let output = run(&["import", text_file.to_str().unwrap(), "--dest", library_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("not a ZIP archive"), "{}", text(&output));

    // A ZIP that is not an app.
    let mut archive = ipa::test_support::ZipBuilder::new();
    archive.file("readme.txt", b"nothing to see here");
    let zip = dir.join("empty.ipa");
    std::fs::write(&zip, archive.build()).unwrap();
    let output = run(&["import", zip.to_str().unwrap(), "--dest", library_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("Payload"), "{}", text(&output));

    // The right shape, but arm64-only: the ARMv7 emulator cannot run it.
    let arm64 = dir.join("arm64.ipa");
    std::fs::write(
        &arm64,
        ipa::test_support::build_ipa(&ipa::test_support::FakeIpa {
            arm64_only: true,
            ..Default::default()
        }),
    )
    .unwrap();
    let output = run(&["import", arm64.to_str().unwrap(), "--dest", library_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("no 32-bit ARM slice"), "{}", text(&output));

    // Still FairPlay encrypted, as an App Store download would be.
    let encrypted = dir.join("encrypted.ipa");
    std::fs::write(
        &encrypted,
        ipa::test_support::build_ipa(&ipa::test_support::FakeIpa {
            encrypted: true,
            ..Default::default()
        }),
    )
    .unwrap();
    let output = run(&["import", encrypted.to_str().unwrap(), "--dest", library_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("FairPlay"), "{}", text(&output));

    // A different iOS app: refused unless the user insists.
    let mut builder = ipa::test_support::ZipBuilder::new();
    let plist = ipa::test_support::INFO_PLIST
        .replace("com.ea.simpsonsarcade.bv", "com.example.other")
        .replace("TheSimpsons", "OtherApp")
        .replace("The Simpsons Arcade", "Other App");
    builder.file("Payload/OtherApp.app/Info.plist", plist.as_bytes());
    builder.entry(
        "Payload/OtherApp.app/OtherApp",
        &ipa::test_support::armv7_executable(),
        ipa::test_support::METHOD_DEFLATE,
        0o755,
    );
    let other = dir.join("other.ipa");
    std::fs::write(&other, builder.build()).unwrap();
    let output = run(&["import", other.to_str().unwrap(), "--dest", library_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("--allow-other-app"), "{}", text(&output));

    let output = run(&[
        "import",
        other.to_str().unwrap(),
        "--dest",
        library_arg,
        "--allow-other-app",
    ]);
    assert!(output.status.success(), "{}", text(&output));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_usage_message_documents_the_import_flow() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let stdout = text(&output);
    assert!(stdout.contains("import <file.ipa>"), "{stdout}");
    assert!(stdout.contains("games"), "{stdout}");
    assert!(stdout.contains("does not ship, download or distribute"), "{stdout}");
}
