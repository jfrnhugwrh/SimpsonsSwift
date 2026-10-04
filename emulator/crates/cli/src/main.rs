//! `simpsons-emu` — command line frontend.
//!
//! ```
//! simpsons-emu import "The Simpsons Arcade v1.1.43.ipa"  # validate + extract your own copy
//! simpsons-emu games                                     # what is in the game library
//! simpsons-emu info  Simpsons.app/Simpsons       # load commands, segments, imports
//! simpsons-emu dump  Simpsons.app/Simpsons       # hexdump of the image
//! simpsons-emu run   Simpsons.app/Simpsons       # boot it, with a boot trace
//! simpsons-emu run   Simpsons.app/Simpsons --serve 8080   # live framebuffer preview
//! simpsons-emu run   "The Simpsons Arcade v1.1.43.ipa"    # import on demand, then boot
//! ```
//!
//! `info`, `dump` and `run` all accept an `.ipa` in place of a Mach-O: the
//! archive is validated, extracted into the game library and the extracted
//! executable is what gets loaded.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use macho::{MachO, CPU_TYPE_ARM};
use runtime::{LoadOptions, Machine, StopReason};

mod import;
mod serve;

const USAGE: &str = "\
simpsons-emu — The Simpsons Arcade (iOS, ARMv7) emulator

USAGE:
    simpsons-emu import <file.ipa> [options]    Validate and extract your own copy of the game
    simpsons-emu games [--dest <dir>]           List the imported games
    simpsons-emu info  <binary|ipa> [--verbose]
    simpsons-emu dump  <binary|ipa> [--section __TEXT.__text] [--offset N] [--length N]
    simpsons-emu run   <binary|ipa> [options]

IMPORT OPTIONS:
    --dest <dir>              Game library to import into (default: $XDG_DATA_HOME/simpsons-emu/games)
    --app <name>              Which bundle to use when the archive holds more than one
    --force                   Re-extract over an existing import
    --allow-other-app         Import an IPA that is a valid iOS app but not this game

The emulator does not ship, download or distribute the game.  The .ipa has to be a
copy you obtained legally, and it must be decrypted (App Store packages are
FairPlay encrypted and no emulator can read them).

RUN OPTIONS:
    --args <a> [b ...]        Arguments passed to the guest's main()
    --bundle <dir>            Directory the game's assets are loaded from
    --dest <dir>              Game library an .ipa argument is imported into
    --app <name>              Which bundle to use when the .ipa holds more than one
    --allow-other-app         Import an .ipa that is a valid iOS app but not this game
    --max-insns <n>           Instruction budget (default 200000000)
    --slice <n>               Instructions between frame publishes (default 2000000)
    --trace                   Log every HLE call and syscall
    --verbose                 Print the guest's log when it stops
    --tolerate-undefined      Skip over unknown instructions instead of stopping
    --screenshot <file.bmp>   Write the last presented frame
    --serve <port>            Serve a live framebuffer preview on <port>
    --bind <addr>             Address the preview binds to (default 0.0.0.0)
    --keep-serving            Keep the preview up after the guest stops
    --stats                   Print call statistics
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print!("{USAGE}");
        return ExitCode::from(2);
    }
    let result = match args[0].as_str() {
        "import" => import::cmd_import(&args[1..]),
        "games" | "list" => import::cmd_games(&args[1..]),
        "info" => cmd_info(&args[1..]),
        "dump" => cmd_dump(&args[1..]),
        "run" => cmd_run(&args[1..]),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Decode the `MH_*` header flags the reader knows about.
fn flag_summary(flags: u32) -> String {
    let mut parts = Vec::new();
    for (bit, name) in [
        (macho::MH_NOUNDEFS, "NOUNDEFS"),
        (macho::MH_DYLDLINK, "DYLDLINK"),
        (macho::MH_TWOLEVEL, "TWOLEVEL"),
        (macho::MH_PIE, "PIE"),
        (macho::MH_HAS_TLV_DESCRIPTORS, "TLV"),
    ] {
        if flags & bit != 0 {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(" | "))
    }
}

pub(crate) fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn number(args: &[String], name: &str, default: u64) -> u64 {
    flag(args, name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn command_line(args: &[String]) -> Vec<String> {
    match args.iter().position(|a| a == "--args") {
        Some(start) => args[start + 1..]
            .iter()
            .take_while(|a| !a.starts_with("--"))
            .cloned()
            .collect(),
        None => Vec::new(),
    }
}

fn load(path: &str) -> Result<MachO, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() >= 4 {
        let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic == macho::FAT_MAGIC {
            return MachO::from_path_slice(path, CPU_TYPE_ARM)
                .map_err(|e| format!("{path}: {e}"));
        }
    }
    MachO::from_bytes(bytes).map_err(|e| format!("{path}: {e}"))
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

fn cmd_info(args: &[String]) -> Result<(), String> {
    let target = import::resolve(args, "info")?;
    for note in &target.notes {
        println!("{note}");
    }
    let path = target.binary.as_str();
    let verbose = args.iter().any(|a| a == "--verbose");
    let image = load(path)?;

    let header = &image.header;
    println!("Mach-O {} bytes", image.data.len());
    println!(
        "  magic          0x{:08x} ({})",
        header.magic,
        if header.magic == macho::MH_MAGIC { "little endian 32-bit" } else { "?" }
    );
    println!("  cputype        0x{:08x} ({})", header.cputype, header.cputype_name());
    println!("  cpusubtype     0x{:08x} ({})", header.cpusubtype, header.cpusubtype_name());
    println!("  filetype       {} (0x{:x})", header.filetype_name(), header.filetype);
    println!("  ncmds          {}", header.ncmds);
    println!("  flags          0x{:08x}{}", header.flags, flag_summary(header.flags));
    if let Some(uuid) = image.uuid {
        let text: Vec<String> = uuid.iter().map(|b| format!("{b:02x}")).collect();
        println!("  uuid           {}", text.join(""));
    }
    if let Some((platform, version, sdk)) = image.version_min {
        println!(
            "  min platform   {} {}.{}.{} (sdk {}.{})",
            platform,
            version >> 16,
            (version >> 8) & 0xff,
            version & 0xff,
            sdk >> 16,
            (sdk >> 8) & 0xff
        );
    }
    if let Some((cryptoff, cryptsize, cryptid)) = image.encryption {
        println!("  encrypted      offset {cryptoff:#x} size {cryptsize:#x} id {cryptid}");
    }
    if let Some(dylinker) = &image.dylinker {
        println!("  dylinker       {dylinker}");
    }

    println!("\nSegments:");
    for segment in &image.segments {
        println!(
            "  {:<12} vmaddr {:#010x} vmsize {:#08x} fileoff {:#08x} filesize {:#08x} prot {}{}{}",
            segment.segname,
            segment.vmaddr,
            segment.vmsize,
            segment.fileoff,
            segment.filesize,
            if segment.initprot & macho::VM_PROT_READ != 0 { 'r' } else { '-' },
            if segment.initprot & macho::VM_PROT_WRITE != 0 { 'w' } else { '-' },
            if segment.initprot & macho::VM_PROT_EXECUTE != 0 { 'x' } else { '-' },
        );
        if verbose {
            for section in &segment.sections {
                println!(
                    "      {:<18} addr {:#010x} size {:#08x}  {}",
                    section.sectname,
                    section.addr,
                    section.size,
                    section.kind()
                );
            }
        }
    }

    println!("\nEntry:");
    match image.entry {
        Some(macho::EntryPoint::Main { entryoff, stacksize }) => {
            println!("  LC_MAIN entryoff {entryoff:#x} stacksize {stacksize:#x}");
            if let Some(pc) = image.entry_pc().ok() {
                println!("  -> pc {pc:#010x}");
            }
        }
        Some(macho::EntryPoint::Thread(registers)) => {
            println!("  LC_UNIXTHREAD pc {:#010x} sp {:#010x}", registers.pc, registers.sp);
        }
        None => println!("  (none)"),
    }

    if !image.dylibs.is_empty() {
        println!("\nLinked libraries ({}):", image.dylibs.len());
        for dylib in &image.dylibs {
            println!("  {dylib}");
        }
    }

    if let Some(symtab) = &image.symtab {
        println!("\nSymbols: {} (string table {} bytes)", symtab.nsyms, symtab.strsize);
        if verbose {
            for symbol in image.symbols.iter().take(64) {
                let kind = if symbol.is_undefined() { "import" } else { "export" };
                println!("  {kind:<7} {:#010x} {}", symbol.n_value, symbol.name);
            }
            if image.symbols.len() > 64 {
                println!("  ... {} more", image.symbols.len() - 64);
            }
        }
    }
    if let Some(dyld_info) = &image.dyld_info {
        println!(
            "\nLC_DYLD_INFO: rebase {} bytes, bind {} bytes, lazy {:#x}/{} bytes, export {} bytes",
            dyld_info.rebase_size, dyld_info.bind_size, dyld_info.lazy_bind_off, dyld_info.lazy_bind_size, dyld_info.export_size
        );
    }
    if !image.unknown_commands.is_empty() {
        println!("\nUnhandled load commands: {}", image.unknown_commands.len());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dump
// ---------------------------------------------------------------------------

fn cmd_dump(args: &[String]) -> Result<(), String> {
    let target = import::resolve(args, "dump")?;
    for note in &target.notes {
        println!("{note}");
    }
    let image = load(&target.binary)?;
    let bytes = &image.data;

    let (start, length) = if let Some(section) = flag(args, "--section") {
        let (segment, name) = section.split_once('.').ok_or("--section wants __SEG.__sect")?;
        let found = image
            .segments
            .iter()
            .filter(|s| s.segname == segment)
            .flat_map(|s| s.sections.iter())
            .find(|s| s.sectname == name)
            .ok_or_else(|| format!("no such section: {section}"))?;
        (found.offset as usize, found.size as usize)
    } else {
        (
            number(args, "--offset", 0) as usize,
            number(args, "--length", 256) as usize,
        )
    };

    let end = (start + length).min(bytes.len());
    if start >= bytes.len() {
        return Err(format!("offset {start:#x} is past the end of the file ({} bytes)", bytes.len()));
    }
    let mut offset = start;
    while offset < end {
        let line_end = (offset + 16).min(end);
        let chunk = &bytes[offset..line_end];
        let mut hex = String::with_capacity(48);
        let mut ascii = String::with_capacity(16);
        for (i, byte) in chunk.iter().enumerate() {
            hex.push_str(&format!("{byte:02x} "));
            if i == 7 {
                hex.push(' ');
            }
            ascii.push(if (0x20..0x7f).contains(byte) { *byte as char } else { '.' });
        }
        println!("{offset:08x}  {hex:<50} {ascii}");
        offset = line_end;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

fn cmd_run(args: &[String]) -> Result<(), String> {
    let target = import::resolve(args, "run")?;
    for note in &target.notes {
        println!("{note}");
    }
    let path = target.binary.clone();
    let image = load(&path)?;

    let mut options = LoadOptions::default();
    options.program_name = std::fs::canonicalize(&path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.clone());
    options.args = command_line(args);
    if let Some(bundle) = target.bundle {
        // argv[0] is the executable inside the bundle, the way iOS sets it up.
        let executable = std::path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Simpsons".to_string());
        options.program_name = format!("{}/{}", bundle.trim_end_matches('/'), executable);
    }

    let trace = args.iter().any(|a| a == "--trace");
    let verbose = args.iter().any(|a| a == "--verbose");
    let tolerate = args.iter().any(|a| a == "--tolerate-undefined");
    let budget = number(args, "--max-insns", 200_000_000);
    let slice = number(args, "--slice", 2_000_000).max(1000);
    let screenshot = flag(args, "--screenshot").map(|s| s.to_string());
    let serve_port = flag(args, "--serve").and_then(|p| p.parse::<u16>().ok());
    let stats = args.iter().any(|a| a == "--stats");

    let frames: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    if let Some(port) = serve_port {
        let games = import::options_from(args).root.unwrap_or_else(ipa::default_root);
        // Everything but a sandbox wants this on loopback; the Android app
        // passes `--bind 127.0.0.1` so the preview is not on the user's Wi-Fi.
        let host = flag(args, "--bind").unwrap_or("0.0.0.0").to_string();
        serve::start(&host, port, Arc::clone(&frames), Arc::clone(&logs), games)?;
        println!("preview: http://{host}:{port}/  (open the port's preview URL)");
    }

    println!("loaded {path}: {} bytes", image.data.len());
    let mut machine = Machine::boot(image, &options).map_err(|e| e.to_string())?;
    machine.trace = trace;
    machine.tolerate_undefined = tolerate;
    println!(
        "mapped {} segments, {} imports bound to HLE trampolines at {:#010x}",
        machine.image.segments.len(),
        machine.image.imports.len(),
        machine.hle_base()
    );
    println!("entry {:#010x}, stack {:#010x}, argc {}", machine.image.entry, machine.stack.sp, machine.stack.argc);
    if let Some(symbol) = machine.image.symbol_at_trampoline(machine.image.entry) {
        println!("(entry is the HLE trampoline for {symbol})");
    }

    let mut executed = 0u64;
    let reason = loop {
        let slice_budget = slice.min(budget.saturating_sub(executed)).max(1);
        let reason = machine.run(slice_budget).map_err(|e| e.to_string())?;
        executed += slice_budget;
        publish(&machine, &frames, &logs);
        match reason {
            StopReason::Budget if executed < budget => continue,
            other => break other,
        }
    };

    publish(&machine, &frames, &logs);
    if let Some(path) = &screenshot {
        if let Some(framebuffer) = machine.sys.framebuffer.as_ref() {
            std::fs::write(path, framebuffer.to_bmp()).map_err(|e| format!("{path}: {e}"))?;
            println!("wrote {path} ({0}x{1})", framebuffer.width, framebuffer.height);
        } else {
            println!("no frame was rendered, nothing written to {path}");
        }
    }

    let stdout = String::from_utf8_lossy(&machine.sys.stdout).into_owned();
    if !stdout.is_empty() {
        println!("\n--- guest stdout ---");
        print!("{stdout}");
        if !stdout.ends_with('\n') {
            println!();
        }
    }

    println!("\n--- stop reason ---");
    match &reason {
        StopReason::Exited(code) => println!("guest exited with code {code}"),
        StopReason::Budget => println!("instruction budget exhausted ({executed} instructions)"),
        StopReason::Trap(trap) => println!("{}", runtime::describe_trap(trap)),
        StopReason::Idle => println!("nothing left to run"),
    }
    println!(
        "instructions {} ({} HLE calls, {} syscalls, {} memory faults, {} undefined)",
        machine.stats.instructions,
        machine.stats.hle_calls,
        machine.stats.syscalls,
        machine.stats.memory_faults,
        machine.stats.undefined_instructions
    );
    println!(
        "frames presented {} ({} with content), {} triangles, {} draw calls",
        machine.sys.frames_presented, machine.sys.frames_with_content, machine.sys.gl.triangles, machine.sys.gl.draws
    );

    if verbose {
        println!("\n--- guest log ({} lines) ---", machine.sys.log.len());
        for line in machine.sys.log.iter().rev().take(200).collect::<Vec<_>>().into_iter().rev() {
            println!("{line}");
        }
    }

    if stats {
        println!("\n--- most called HLE symbols ---");
        let mut calls = machine.stats.hle_by_symbol.clone();
        calls.sort_by(|a, b| b.1.cmp(&a.1));
        for (name, count) in calls.iter().take(40) {
            println!("  {count:>10}  {name}");
        }
        if !machine.sys.unimplemented.is_empty() {
            println!("\n--- unimplemented symbols ---");
            let mut unimplemented: Vec<_> = machine.sys.unimplemented.iter().collect();
            unimplemented.sort_by(|a, b| b.1.cmp(a.1));
            for (name, count) in unimplemented.iter().take(60) {
                println!("  {count:>10}  {name}");
            }
        }
        if !machine.image.imports.is_empty() {
            println!("\n--- bound imports ({} symbols) ---", machine.image.imports.len());
            for import in machine.image.imports.iter().take(40) {
                println!("  {:<40} slot {:#010x} -> {:#010x}", import.symbol, import.slot, import.trampoline);
            }
        }
    }

    // The guest stopping does not have to take the preview with it: the last
    // frame, the log and the import panel are worth more after the run than
    // during it.  The Android app relies on this — a guest that exits in
    // milliseconds would otherwise never show a preview at all.
    if let Some(port) = serve_port {
        if args.iter().any(|a| a == "--keep-serving") {
            let host = flag(args, "--bind").unwrap_or("0.0.0.0");
            println!("\npreview still serving on http://{host}:{port}/ — interrupt to stop");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    }
    Ok(())
}

fn publish(machine: &Machine, frames: &Arc<Mutex<Vec<u8>>>, logs: &Arc<Mutex<Vec<String>>>) {
    if let Some(framebuffer) = machine.sys.framebuffer.as_ref() {
        if let Ok(mut frame) = frames.lock() {
            if frame.is_empty() || framebuffer.dirty || machine.since_present < 100_000 {
                *frame = framebuffer.to_bmp();
            }
        }
    }
    if let Ok(mut log) = logs.lock() {
        if log.len() != machine.sys.log.len() {
            *log = machine.sys.log.clone();
        }
    }
}
