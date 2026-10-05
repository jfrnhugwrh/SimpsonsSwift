//! Loads an ARMv7 Mach-O executable into guest memory the way XNU's
//! `exec_mach_imgact` + dyld would:
//!
//! 1. every `LC_SEGMENT` is mapped at its `vmaddr` with the segment's initial
//!    protection and its file contents copied in (`__PAGEZERO` is left unmapped
//!    so that null dereferences fault);
//! 2. the `rebase` opcode stream is applied (a no-op for a single image at its
//!    preferred address, but kept so a slid image would work too);
//! 3. every imported symbol is bound to an address in the HLE trampoline page,
//!    which the [`crate::Machine`] recognises when the program counter lands
//!    there;
//! 4. the initial thread state is built — `argc`/`argv`/`envp`/apple vector on
//!    the stack, exactly what `start` expects before it calls `main`.

use std::collections::HashMap;
use std::fmt;

use guestmem::{AddressSpace, Permissions, RegionKind};
use macho::{BindRecord, EntryPoint, MachO, RebaseLocation, Symbol};

use crate::error::{Result, RuntimeError};

/// Everything the loader needs to know about the environment.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Size of the main thread stack.
    pub stack_size: u32,
    /// `argv[0]`.
    pub program_name: String,
    /// `argv[1..]` (the `-NSDocumentRevisionsDebugMode`-style arguments).
    pub args: Vec<String>,
    /// Environment variables, `KEY=VALUE`.
    pub env: Vec<String>,
    /// Also resolve imports that only appear in the symbol table.
    pub bind_symbol_table: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            stack_size: 8 << 20,
            program_name: "/var/mobile/Applications/Simpsons.app/Simpsons".to_string(),
            args: Vec::new(),
            env: vec![
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
                "HOME=/var/mobile".to_string(),
                "TMPDIR=/tmp".to_string(),
            ],
            bind_symbol_table: true,
        }
    }
}

/// Fixed addresses for the regions the loader creates.  iOS leaves the whole
/// lower part of the address space to the executable, so the emulator puts its
/// own regions well above it — that also makes a stray pointer obvious in a
/// crash report instead of silently hitting a real segment.
pub const HLE_BASE: u32 = 0x7000_0000;
/// One 16-byte slot per imported symbol: the trampoline address the guest calls,
/// with a `udf` pattern behind it so a mis-computed jump is diagnosable.
pub const HLE_SLOT_SIZE: u32 = 16;
pub const HLE_SLOTS: u32 = 4096;
pub const HLE_SIZE: u32 = HLE_SLOT_SIZE * HLE_SLOTS;

/// Address a guest method returns to when the emulator transferred control into
/// it from a HLE call (see [`crate::hle::Hle::call_guest`]).
pub const HLE_RETURN: u32 = HLE_BASE + HLE_SIZE - HLE_SLOT_SIZE;

/// True when `addr` is one of the HLE trampoline slots.
///
/// The slots hold executable markers, not data, so anything that inspects a
/// guest pointer (`is_probable_class`, `lookup_imp`, the vtable readers) has to
/// be able to reject them instead of mis-reading a `udf` word as a structure.
#[inline]
pub fn is_trampoline_page(addr: u32) -> bool {
    addr >= HLE_BASE && addr < HLE_BASE + HLE_SIZE
}

/// Where the loader materialises synthetic Objective-C class objects for the
/// `_OBJC_CLASS_$_*` / `_OBJC_METACLASS_$_*` symbols the image imports.
///
/// These are *data* symbols: the guest loads the slot and hands the value to
/// `objc_msgSend` as a receiver.  Binding them to a trampoline would give the
/// runtime a page of `udf` words where a `struct objc_class` belongs, so they
/// get a real (if method-less) class object here, in the gap between the
/// trampoline page and the runtime's own object pool.
pub const OBJC_CLASSES_BASE: u32 = 0x7001_0000;
pub const OBJC_CLASSES_SIZE: u32 = 0x0001_0000;

pub const STACK_TOP: u32 = 0x4000_0000;

/// A synthetic Objective-C class the loader created for an imported class
/// symbol, so that `objc_msgSend` sees a class object rather than a trampoline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostObjcClass {
    /// The name with the `_OBJC_CLASS_$_` / `_OBJC_METACLASS_$_` prefix removed.
    pub name: String,
    /// Guest address of the class object.
    pub class: u32,
    /// Guest address of its metaclass (which carries the class methods).
    pub metaclass: u32,
}

/// A bound import: the trampoline address and the symbol it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBinding {
    /// Address written into the guest's `__nl_symbol_ptr`/`__la_symbol_ptr`
    /// slot.  For an Objective-C class symbol this is the synthetic class object
    /// rather than a trampoline (see [`synthetic_objc_class`]).
    pub trampoline: u32,
    /// Guest address of the pointer slot that was filled in.
    pub slot: u32,
    /// Symbol name as it appears in the Mach-O (leading `_` included).
    pub symbol: String,
    /// Library ordinal, for diagnostics.
    pub ordinal: i32,
    /// True when the record came from the lazy bind stream.
    pub lazy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPlacement {
    pub name: String,
    pub vmaddr: u32,
    pub vmsize: u32,
    pub perms: Permissions,
    pub file_bytes: u32,
}

/// A loaded image: the parsed Mach-O plus everything the loader did to memory.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    pub macho: MachO,
    /// Address the image was mapped at (its `__TEXT` `vmaddr` plus slide).
    pub base: u32,
    pub slide: u32,
    pub entry: u32,
    pub segments: Vec<SegmentPlacement>,
    pub rebases: Vec<RebaseLocation>,
    pub imports: Vec<ImportBinding>,
    /// `index -> symbol`, indexed by `(trampoline - HLE_BASE) / HLE_SLOT_SIZE`.
    pub trampolines: Vec<String>,
    /// Synthetic class objects created for the imported `_OBJC_CLASS_$_*`
    /// symbols, in the order they were first referenced.
    pub objc_classes: Vec<HostObjcClass>,
    pub stack_top: u32,
    pub stack_size: u32,
}

impl LoadedImage {
    /// Symbol behind a HLE trampoline address, if the address is one.
    #[inline]
    pub fn symbol_at_trampoline(&self, addr: u32) -> Option<&str> {
        if addr < HLE_BASE || addr >= HLE_BASE + HLE_SIZE {
            return None;
        }
        self.trampolines.get(((addr - HLE_BASE) / HLE_SLOT_SIZE) as usize).map(|s| s.as_str())
    }

    /// The guest address of a symbol this image defines (searching the export
    /// trie first, then the symbol table).
    pub fn lookup(&self, name: &str) -> Option<u32> {
        if let Ok(Some(export)) = macho::export_lookup(&self.macho, name) {
            if let Some(addr) = export.address {
                return Some(addr.wrapping_add(self.slide));
            }
        }
        self.macho.symbol(name).and_then(|s| {
            if s.is_undefined() {
                None
            } else {
                Some(s.n_value.wrapping_add(self.slide))
            }
        })
    }

    /// The entry point's `_main`, when the image uses `LC_MAIN` (which points at
    /// `start`, whose first argument is `main`).
    pub fn main_symbol(&self) -> Option<u32> {
        self.lookup("_main").or_else(|| self.lookup("main"))
    }

    pub fn image_range(&self) -> (u32, u32) {
        let start = self.segments.iter().map(|s| s.vmaddr).min().unwrap_or(self.base);
        let end = self.segments.iter().map(|s| s.vmaddr.saturating_add(s.vmsize)).max().unwrap_or(self.base);
        (start, end)
    }
}

impl fmt::Display for LoadedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}] at {:#010x}, entry {:#010x}, {} segments, {} imports",
            self.macho.header.filetype_name(),
            self.macho.header.cpusubtype_name(),
            self.base,
            self.entry,
            self.segments.len(),
            self.imports.len()
        )
    }
}

/// Where an executable wants to be mapped.  iOS executables have a `__TEXT`
/// `vmaddr` of `0x1000`/`0x4000`; honouring it keeps any absolute address baked
/// into the image valid, so the loader does not need to slide.
fn preferred_base(macho: &MachO) -> u32 {
    macho
        .segments
        .iter()
        .filter(|s| !s.is_pagezero())
        .map(|s| s.vmaddr)
        .min()
        .unwrap_or(0)
}

fn align_up(value: u32, align: u32) -> u32 {
    value.wrapping_add(align - 1) & !(align - 1)
}

/// Map the image's segments into `space`.
pub fn map_image(macho: &MachO, space: &mut AddressSpace, slide: u32) -> Result<Vec<SegmentPlacement>> {
    let mut placements = Vec::with_capacity(macho.segments.len());
    // A segment may only be mapped up to the next one's start: the kernel maps
    // `round_page(vmsize)`, and a malformed image (or one whose trailing
    // segment size is not a page multiple) must not swallow its neighbour.
    let mut starts: Vec<u32> = macho.segments.iter().map(|s| s.vmaddr.wrapping_add(slide)).collect();
    starts.sort_unstable();
    let next_start = |addr: u32| starts.iter().copied().find(|s| *s > addr);

    for segment in &macho.segments {
        if segment.is_pagezero() {
            // Deliberately unmapped: the guest must fault on a null pointer.
            placements.push(SegmentPlacement {
                name: segment.segname.clone(),
                vmaddr: segment.vmaddr,
                vmsize: segment.vmsize,
                perms: Permissions::NONE,
                file_bytes: 0,
            });
            continue;
        }
        let perms = if segment.is_linkedit() {
            // dyld keeps __LINKEDIT readable; the guest must never write it.
            Permissions::R
        } else {
            Permissions::from_vm_prot(segment.initprot)
        };
        let base = segment.vmaddr.wrapping_add(slide);
        let mut size = align_up(segment.vmsize.max(1), guestmem::PAGE_SIZE);
        if let Some(next) = next_start(base) {
            size = size.min(next - base).max(1);
        }
        let name = format!("image:{}", segment.segname);

        let (fileoff, filesize) = segment.file_range();
        let file_bytes: &[u8] = if segment.has_file_data() && fileoff < macho.data.len() {
            let end = (fileoff + filesize).min(macho.data.len());
            &macho.data[fileoff..end]
        } else {
            &[]
        };

        space.map(name, base, size, perms, RegionKind::Image, file_bytes)?;
        placements.push(SegmentPlacement {
            name: segment.segname.clone(),
            vmaddr: base,
            vmsize: size,
            perms,
            file_bytes: file_bytes.len() as u32,
        });
    }
    Ok(placements)
}

/// Read the image's `LC_DYLD_INFO` rebase/bind streams.
pub fn collect_fixups(macho: &MachO) -> Result<(Vec<RebaseLocation>, Vec<BindRecord>)> {
    let rebases = macho::rebase_locations(macho)?;
    let mut binds = macho::bind_records(macho)?;
    // Lazy binds point at the same slots as the lazy pointer section; when the
    // regular bind stream already covers a slot the lazy record is redundant.
    for lazy in macho::lazy_bind_records(macho)? {
        if !binds.iter().any(|b| b.address == lazy.address) {
            binds.push(lazy);
        }
    }
    Ok((rebases, binds))
}

/// Fall back to the classic `__nl_symbol_ptr` / `__la_symbol_ptr` +
/// `LC_DYSYMTAB` indirect symbol table mechanism (used by images that predate or
/// omit the compressed dyld info).
fn binds_from_symtab(macho: &MachO) -> Vec<BindRecord> {
    let mut out = Vec::new();
    let Some(dysym) = macho.dysymtab else { return out };
    for section in macho.sections() {
        if !matches!(
            section.section_type(),
            macho::S_NON_LAZY_SYMBOL_POINTERS | macho::S_LAZY_SYMBOL_POINTERS | macho::S_LAZY_DYLIB_SYMBOL_POINTERS
        ) {
            continue;
        }
        let start = section.reserved1 as usize;
        let count = (section.size / 4) as usize;
        for i in 0..count {
            let Some(&sym_index) = macho.indirect_symbols.get(start + i) else { break };
            if sym_index as u32 & macho::INDIRECT_SYMBOL_LOCAL != 0
                || sym_index as u32 & macho::INDIRECT_SYMBOL_ABS != 0
            {
                continue;
            }
            let Some(symbol) = macho.symbols.get(sym_index as usize) else { continue };
            let lazy = matches!(section.section_type(), macho::S_LAZY_SYMBOL_POINTERS | macho::S_LAZY_DYLIB_SYMBOL_POINTERS);
            out.push(BindRecord {
                address: section.addr + (i as u32) * 4,
                symbol: symbol.name.clone(),
                flags: 0,
                dylib_ordinal: -2,
                addend: 0,
                kind: macho::BIND_TYPE_POINTER,
                lazy,
            });
        }
    }
    let _ = dysym;
    out
}

/// Result of the binding pass.
pub struct BindingPass {
    pub imports: Vec<ImportBinding>,
    /// `slot address -> symbol` for every non-lazy pointer that was written.
    pub slots: HashMap<u32, String>,
    /// Symbols that are bound but for which no handler exists (diagnostics).
    pub unresolved: Vec<String>,
    /// Synthetic Objective-C classes created for imported class symbols.
    pub objc_classes: Vec<HostObjcClass>,
}

/// Split an imported Objective-C class symbol into `(name, is_metaclass)`.
///
/// The Mach-O names are `_OBJC_CLASS_$_X` and `_OBJC_METACLASS_$_X`; both refer
/// to the same class, so they have to resolve to one `(class, metaclass)` pair.
fn objc_class_symbol(symbol: &str) -> Option<(&str, bool)> {
    if let Some(name) = symbol.strip_prefix("_OBJC_CLASS_$_") {
        if !name.is_empty() {
            return Some((name, false));
        }
    }
    if let Some(name) = symbol.strip_prefix("_OBJC_METACLASS_$_") {
        if !name.is_empty() {
            return Some((name, true));
        }
    }
    None
}

/// Materialise (once) the `(class, metaclass)` pair for an imported
/// `_OBJC_CLASS_$_X`, and return the address the bind slot should hold.
///
/// Both objects are real `struct objc_class` values with a valid `class_ro_t`,
/// so the Objective-C bridge's generic metadata reader accepts them: they simply
/// have no methods of their own, which makes every message to them fall through
/// to the emulator's host method tables (`-[UIDevice currentDevice]` and the
/// rest) instead of dereferencing a trampoline.
fn synthetic_objc_class(
    space: &mut AddressSpace,
    next: &mut u32,
    created: &mut Vec<HostObjcClass>,
    name: &str,
    metaclass_wanted: bool,
) -> Result<u32> {
    if let Some(existing) = created.iter().find(|c| c.name == name) {
        return Ok(if metaclass_wanted { existing.metaclass } else { existing.class });
    }
    let size = crate::hle::objc::synthetic_class_size(name);
    // 16-byte aligned, and both objects have to fit in the pool.
    let class = (*next + 15) & !15;
    let metaclass = class + size;
    if metaclass + size > OBJC_CLASSES_BASE + OBJC_CLASSES_SIZE {
        return Err(RuntimeError::Unsupported(format!(
            "out of room for synthetic Objective-C class objects (at `{name}`)"
        )));
    }
    *next = metaclass + size;
    // The root metaclass is its own isa and has no superclass, which is exactly
    // how `NSObject`'s metaclass terminates the chain in libobjc.
    crate::hle::objc::write_synthetic_class(space, metaclass, name, metaclass, 0, 0)?;
    crate::hle::objc::write_synthetic_class(
        space,
        class,
        name,
        metaclass,
        0,
        crate::hle::objc::HOST_INSTANCE_SIZE,
    )?;
    created.push(HostObjcClass { name: name.to_string(), class, metaclass });
    Ok(if metaclass_wanted { metaclass } else { class })
}

/// Write a HLE trampoline address into every import slot.
///
/// Returns the pass results; trampoline indices are allocated in the order the
/// symbols are first seen so the mapping is stable for a given image.
///
/// Objective-C class symbols are the exception: they are bound to synthetic
/// class objects (see [`synthetic_objc_class`]) rather than to a trampoline,
/// because the guest uses the slot's value as a message receiver and never calls
/// it.
pub fn apply_bindings(
    binds: &[BindRecord],
    space: &mut AddressSpace,
    trampolines: &mut Vec<String>,
    objc_classes: &mut Vec<HostObjcClass>,
    _slide: u32,
) -> Result<BindingPass> {
    let mut index_of: HashMap<String, u32> = trampolines
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), i as u32))
        .collect();
    let mut imports = Vec::new();
    let mut slots = HashMap::new();
    let mut unresolved = Vec::new();
    let mut next_class = OBJC_CLASSES_BASE;

    for bind in binds {
        // A class symbol is data, not a function: give it a class object and do
        // not spend a trampoline slot on it.
        if let Some((name, metaclass_wanted)) = objc_class_symbol(&bind.symbol) {
            let value = synthetic_objc_class(
                space,
                &mut next_class,
                objc_classes,
                name,
                metaclass_wanted,
            )?;
            space.write_u32(bind.address, value.wrapping_add(bind.addend as u32))?;
            slots.insert(bind.address, bind.symbol.clone());
            imports.push(ImportBinding {
                trampoline: value,
                slot: bind.address,
                symbol: bind.symbol.clone(),
                ordinal: bind.dylib_ordinal,
                lazy: bind.lazy,
            });
            if !unresolved.contains(&bind.symbol) {
                unresolved.push(bind.symbol.clone());
            }
            continue;
        }

        let index = match index_of.get(&bind.symbol) {
            Some(&i) => i,
            None => {
                let i = trampolines.len() as u32;
                if i >= HLE_SLOTS {
                    return Err(RuntimeError::Unsupported(format!(
                        "more than {HLE_SLOTS} imported symbols"
                    )));
                }
                trampolines.push(bind.symbol.clone());
                index_of.insert(bind.symbol.clone(), i);
                i
            }
        };
        let trampoline = HLE_BASE + index * HLE_SLOT_SIZE;
        // BIND_TYPE_POINTER is by far the common case; for text-absolute
        // symbols the slot holds the trampoline address too, because the
        // trampoline is ordinary executable-looking code from the guest's view.
        let value = match bind.kind {
            macho::BIND_TYPE_TEXT_ABSOLUTE32 | macho::BIND_TYPE_TEXT_PCREL32 => {
                trampoline.wrapping_add(bind.addend as u32)
            }
            _ => trampoline.wrapping_add(bind.addend as u32),
        };
        space.write_u32(bind.address, value)?;
        slots.insert(bind.address, bind.symbol.clone());
        imports.push(ImportBinding {
            trampoline,
            slot: bind.address,
            symbol: bind.symbol.clone(),
            ordinal: bind.dylib_ordinal,
            lazy: bind.lazy,
        });
        if !unresolved.contains(&bind.symbol) {
            unresolved.push(bind.symbol.clone());
        }
    }
    Ok(BindingPass { imports, slots, unresolved, objc_classes: objc_classes.clone() })
}

/// Resolve `LC_MAIN`/`LC_UNIXTHREAD` into a start address, defaulting to the
/// `_main` symbol and finally to the start of `__TEXT`.
pub fn entry_address(macho: &MachO, slide: u32) -> u32 {
    match macho.entry {
        Some(EntryPoint::Main { entryoff, .. }) => macho
            .text_segment()
            .map(|s| s.vmaddr.wrapping_add(entryoff as u32).wrapping_add(slide))
            .unwrap_or(entryoff as u32),
        Some(EntryPoint::Thread(regs)) => regs.pc.wrapping_add(slide),
        None => macho
            .symbol("_main")
            .or_else(|| macho.symbol("main"))
            .map(|s| s.n_value.wrapping_add(slide))
            .or_else(|| macho.text_segment().map(|s| s.vmaddr.wrapping_add(slide)))
            .unwrap_or(0),
    }
}

/// Load `macho` into a fresh address space.
pub fn load(macho: MachO, options: &LoadOptions) -> Result<(LoadedImage, AddressSpace)> {
    let mut space = AddressSpace::new();
    let base = preferred_base(&macho);
    // No slide: the image runs at its preferred address (iOS only slides when
    // it has to, and a single executable with no dylibs always fits).
    let slide = 0u32;

    let segments = map_image(&macho, &mut space, slide)?;
    let (rebases, mut binds) = collect_fixups(&macho)?;
    if binds.is_empty() && options.bind_symbol_table {
        binds = binds_from_symtab(&macho);
    }

    // HLE trampoline page: one 16-byte slot per import.  The first instruction
    // of every slot is undefined so that executing a trampoline without the
    // runtime's knowledge is a clean trap rather than random code.
    // Mapped writable while the trampoline bodies are filled in below, then
    // dropped to execute-only: the guest may call the page but never patch it.
    space.map(
        "hle",
        HLE_BASE,
        HLE_SIZE,
        Permissions::RWX,
        RegionKind::Trampoline,
        &[],
    )?;

    // Synthetic Objective-C class objects for the imported `_OBJC_CLASS_$_*`
    // symbols.  Writable while they are being built, then dropped to read-only:
    // a class object is constant data from the guest's point of view.
    space.map(
        "objc-classes",
        OBJC_CLASSES_BASE,
        OBJC_CLASSES_SIZE,
        Permissions::RW,
        RegionKind::Anonymous,
        &[],
    )?;

    let mut trampolines: Vec<String> = Vec::new();
    let mut objc_classes: Vec<HostObjcClass> = Vec::new();
    let pass = apply_bindings(&binds, &mut space, &mut trampolines, &mut objc_classes, slide)?;
    if !objc_classes.is_empty() {
        space.protect(OBJC_CLASSES_BASE, OBJC_CLASSES_SIZE, Permissions::R)?;
    }

    // Fill each trampoline slot's body with a marker the disassembler shows up
    // as `udf`, so a wrong jump lands on a recognisable address.
    for index in 0..trampolines.len() as u32 {
        let addr = HLE_BASE + index * HLE_SLOT_SIZE;
        space.write_u32(addr, 0xe7fe_0000)?; // ARM: undef
        space.write_u16(addr + 4, 0xde00)?; // Thumb: udf #0
    }
    space.protect(HLE_BASE, HLE_SIZE, Permissions::RX)?;

    // Rebase pass: with a zero slide this rewrites an address with itself, but
    // it keeps a slid load correct and validates every rebase location.
    for location in &rebases {
        let addr = location.address.wrapping_add(slide);
        let value = space.read_u32(addr)?;
        if value != 0 {
            space.write_u32(addr, value.wrapping_add(slide))?;
        }
    }

    // Stack: everything the C runtime expects to find at sp on entry.
    let stack_top = STACK_TOP;
    let stack_size = options.stack_size.max(guestmem::PAGE_SIZE * 4);
    space.map(
        "stack",
        stack_top - stack_size,
        stack_size,
        Permissions::RW,
        RegionKind::Stack,
        &[],
    )?;

    let entry = entry_address(&macho, slide);
    let image = LoadedImage {
        macho,
        base,
        slide,
        entry,
        segments,
        rebases,
        imports: pass.imports,
        trampolines,
        objc_classes: pass.objc_classes,
        stack_top,
        stack_size,
    };

    Ok((image, space))
}

/// Build the initial stack: `argc`, `argv[]`, `envp[]`, the Apple vector and
/// finally the stack canary halves, exactly the layout `start` walks before it
/// calls `main(argc, argv, envp)`.
#[derive(Debug, Clone, Copy)]
pub struct InitialStack {
    pub sp: u32,
    pub argc: u32,
    pub argv: u32,
    pub envp: u32,
    pub apple: u32,
}

pub fn build_stack(space: &mut AddressSpace, options: &LoadOptions, stack_top: u32) -> Result<InitialStack> {
    let mut sp = stack_top & !0xf;
    let mut strings: Vec<(String, u32)> = Vec::new();

    let push_string = |space: &mut AddressSpace,
                           sp: &mut u32,
                           strings: &mut Vec<(String, u32)>,
                           text: &str|
     -> Result<u32> {
        let bytes = text.as_bytes();
        let len = bytes.len() as u32 + 1;
        *sp = (*sp - len) & !3;
        space.poke_bytes(*sp, bytes)?;
        space.write_u8(*sp + len - 1, 0)?;
        strings.push((text.to_string(), *sp));
        Ok(*sp)
    };

    let program_name = options.program_name.clone();
    let args: Vec<String> = std::iter::once(program_name).chain(options.args.iter().cloned()).collect();
    let env: Vec<String> = options.env.clone();

    // Strings last-first so that argv[0] ends up lowest, matching XNU.
    let mut arg_ptrs = Vec::with_capacity(args.len());
    for arg in args.iter().rev() {
        arg_ptrs.push(push_string(space, &mut sp, &mut strings, arg)?);
    }
    arg_ptrs.reverse();
    let mut env_ptrs = Vec::with_capacity(env.len());
    for item in env.iter().rev() {
        env_ptrs.push(push_string(space, &mut sp, &mut strings, item)?);
    }
    env_ptrs.reverse();
    let exec_path = push_string(space, &mut sp, &mut strings, &options.program_name)?;

    // The Apple vector: "executable_path=...", "ptr_munge=", "main_stack=".
    let apple_entries = [
        format!("executable_path={}", options.program_name),
        "ptr_munge=".to_string(),
        "main_stack=".to_string(),
    ];
    let mut apple_ptrs = Vec::new();
    for entry in apple_entries.iter().rev() {
        apple_ptrs.push(push_string(space, &mut sp, &mut strings, entry)?);
    }
    apple_ptrs.reverse();

    // Pointers and scalars, 16-byte aligned again at the end (the ABI requires
    // sp to be 16-byte aligned at a public interface).
    let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + 1 + apple_ptrs.len() + 1;
    sp = (sp - words as u32 * 4) & !0xf;

    let argc = args.len() as u32;
    let argv = sp + 4;
    let envp = argv + 4 * (argc + 1);
    let apple = envp + 4 * (env_ptrs.len() as u32 + 1);

    space.write_u32(sp, argc)?;
    for (i, ptr) in arg_ptrs.iter().enumerate() {
        space.write_u32(argv + 4 * i as u32, *ptr)?;
    }
    space.write_u32(argv + 4 * argc, 0)?;
    for (i, ptr) in env_ptrs.iter().enumerate() {
        space.write_u32(envp + 4 * i as u32, *ptr)?;
    }
    space.write_u32(envp + 4 * env_ptrs.len() as u32, 0)?;
    space.write_u32(apple, apple_ptrs.len() as u32)?;
    for (i, ptr) in apple_ptrs.iter().enumerate() {
        space.write_u32(apple + 4 + 4 * i as u32, *ptr)?;
    }
    space.write_u32(apple + 4 + 4 * apple_ptrs.len() as u32, 0)?;
    let _ = exec_path;

    Ok(InitialStack { sp, argc, argv, envp, apple })
}

/// Symbols that are bound but not implemented are still callable: the runtime
/// logs them and returns zero, which is how a real dyld behaves for a weak or
/// unresolved symbol.  This helper reports the total for the loader summary.
pub fn count_undefined(macho: &MachO) -> usize {
    macho.symbols.iter().filter(|s: &&Symbol| s.is_undefined()).count()
}
