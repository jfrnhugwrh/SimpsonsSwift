//! Data structures mirroring the on-disk Mach-O layout.

use crate::consts::*;
use core::fmt;

/// `struct mach_header` (32-bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub magic: u32,
    pub cputype: u32,
    pub cpusubtype: u32,
    pub filetype: u32,
    pub ncmds: u32,
    pub sizeofcmds: u32,
    pub flags: u32,
}

impl Header {
    pub const SIZE: usize = 28;

    pub fn cputype_name(&self) -> &'static str {
        match self.cputype {
            CPU_TYPE_ARM => "arm",
            CPU_TYPE_ARM64 => "arm64",
            CPU_TYPE_X86 => "i386",
            CPU_TYPE_X86_64 => "x86_64",
            _ => "unknown",
        }
    }

    pub fn cpusubtype_name(&self) -> &'static str {
        match (self.cputype, self.cpusubtype) {
            (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V6) => "armv6",
            (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V7) => "armv7",
            (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V7S) => "armv7s",
            (CPU_TYPE_ARM, CPU_SUBTYPE_ARM_V7K) => "armv7k",
            _ => "unknown",
        }
    }

    pub fn filetype_name(&self) -> &'static str {
        match self.filetype {
            MH_OBJECT => "object",
            MH_EXECUTE => "executable",
            MH_FVMLIB => "fvmlib",
            MH_CORE => "core",
            MH_PRELOAD => "preload",
            MH_DYLIB => "dylib",
            MH_DYLINKER => "dynamic linker",
            MH_BUNDLE => "bundle",
            MH_DYLIB_STUB => "dylib stub",
            MH_DSYM => "dSYM companion",
            MH_KEXT_BUNDLE => "kext",
            _ => "unknown",
        }
    }

    /// True when the image is a little-endian Mach-O we can load.
    pub fn is_loadable_image(&self) -> bool {
        self.magic == MH_MAGIC
            && (self.filetype == MH_EXECUTE || self.filetype == MH_DYLIB || self.filetype == MH_BUNDLE)
    }
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} (cputype {:#x} {}/{}, flags {:#x})",
            self.filetype_name(),
            self.cpusubtype_name(),
            self.cputype,
            self.cputype_name(),
            self.cpusubtype_name(),
            self.flags
        )
    }
}

/// One `section` entry inside an `LC_SEGMENT` command (68 bytes on disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub sectname: String,
    pub segname: String,
    pub addr: u32,
    pub size: u32,
    pub offset: u32,
    pub align: u32,
    pub reloff: u32,
    pub nreloc: u32,
    pub flags: u32,
    pub reserved1: u32,
    pub reserved2: u32,
}

impl Section {
    pub const SIZE: usize = 68;

    pub fn section_type(&self) -> u32 {
        self.flags & SECTION_TYPE
    }

    /// Sections that occupy memory but not file space.
    pub fn is_zerofill(&self) -> bool {
        matches!(self.section_type(), S_ZEROFILL | S_GB_ZEROFILL | S_THREAD_LOCAL_ZEROFILL)
    }

    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.addr && (addr as u64) < self.addr as u64 + self.size as u64
    }

    /// Human readable role, used by `simpsons-emu info` and the loader.
    pub fn kind(&self) -> &'static str {
        match self.section_type() {
            S_REGULAR if self.flags & S_ATTR_PURE_INSTRUCTIONS != 0 => "code",
            S_REGULAR => "data",
            S_ZEROFILL => "bss",
            S_CSTRING_LITERALS => "cstring",
            S_4BYTE_LITERALS => "4byte-literals",
            S_8BYTE_LITERALS => "8byte-literals",
            S_LITERAL_POINTERS => "literal-pointers",
            S_NON_LAZY_SYMBOL_POINTERS => "non-lazy-symbol-pointers",
            S_LAZY_SYMBOL_POINTERS => "lazy-symbol-pointers",
            S_LAZY_DYLIB_SYMBOL_POINTERS => "lazy-dylib-symbol-pointers",
            S_SYMBOL_STUBS => "symbol-stubs",
            S_MOD_INIT_FUNC_POINTERS => "mod-init-funcs",
            S_MOD_TERM_FUNC_POINTERS => "mod-term-funcs",
            S_COALESCED => "coalesced",
            S_INTERPOSING => "interposing",
            S_16BYTE_LITERALS => "16byte-literals",
            S_DTRACE_DOF => "dtrace-dof",
            S_THREAD_LOCAL_REGULAR => "tls",
            S_THREAD_LOCAL_ZEROFILL => "tls-bss",
            S_THREAD_LOCAL_VARIABLES => "tls-vars",
            S_THREAD_LOCAL_VARIABLE_POINTERS => "tls-var-pointers",
            S_THREAD_LOCAL_INIT_FUNCTION_POINTERS => "tls-init-funcs",
            _ => "other",
        }
    }
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{} addr {:#010x} size {:#x} offset {:#x} ({})",
            self.segname,
            self.sectname,
            self.addr,
            self.size,
            self.offset,
            self.kind()
        )
    }
}

/// `struct segment_command` (32-bit) plus its sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub segname: String,
    pub vmaddr: u32,
    pub vmsize: u32,
    pub fileoff: u32,
    pub filesize: u32,
    pub maxprot: u32,
    pub initprot: u32,
    pub nsects: u32,
    pub flags: u32,
    pub sections: Vec<Section>,
}

impl Segment {
    /// Bytes of this segment that are present in the file (the rest is zero
    /// filled by the kernel — `__bss`, `__common`, ...).
    pub fn file_range(&self) -> (usize, usize) {
        (self.fileoff as usize, self.filesize as usize)
    }

    pub fn has_file_data(&self) -> bool {
        self.filesize > 0
    }

    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.vmaddr && (addr as u64) < self.vmaddr as u64 + self.vmsize as u64
    }

    /// `__PAGEZERO` is mapped but inaccessible: it exists so that a null
    /// pointer dereference faults instead of silently corrupting the image.
    pub fn is_pagezero(&self) -> bool {
        self.segname == "__PAGEZERO"
    }

    pub fn is_linkedit(&self) -> bool {
        self.segname == "__LINKEDIT"
    }
}

impl fmt::Display for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<12} vmaddr {:#010x} vmsize {:#010x} fileoff {:#06x} filesize {:#08x} \
             prot {}{}{} ({} sections)",
            self.segname,
            self.vmaddr,
            self.vmsize,
            self.fileoff,
            self.filesize,
            if self.initprot & VM_PROT_READ != 0 { 'r' } else { '-' },
            if self.initprot & VM_PROT_WRITE != 0 { 'w' } else { '-' },
            if self.initprot & VM_PROT_EXECUTE != 0 { 'x' } else { '-' },
            self.sections.len()
        )
    }
}

/// `struct nlist` entry from `LC_SYMTAB`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub n_type: u8,
    pub n_sect: u8,
    pub n_desc: u16,
    pub n_value: u32,
}

impl Symbol {
    pub const SIZE: usize = 12;

    pub fn is_debug(&self) -> bool {
        self.n_type & N_STAB != 0
    }

    pub fn is_external(&self) -> bool {
        self.n_type & N_EXT != 0
    }

    pub fn is_undefined(&self) -> bool {
        !self.is_debug() && (self.n_type & N_TYPE) == N_UNDF
    }

    pub fn is_defined_section(&self) -> bool {
        !self.is_debug() && (self.n_type & N_TYPE) == N_SECT
    }

    /// Undefined symbols that the dynamic linker must supply.
    pub fn needs_binding(&self) -> bool {
        self.is_undefined() && self.is_external()
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.is_undefined() {
            "undef"
        } else if self.is_defined_section() {
            "sect"
        } else if self.is_debug() {
            "stab"
        } else {
            "other"
        };
        write!(
            f,
            "{:<40} {:#010x} {:>5} sect {} desc {:#06x}{}",
            self.name,
            self.n_value,
            kind,
            self.n_sect,
            self.n_desc,
            if self.is_external() { " ext" } else { "" }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DylibKind {
    Load,
    WeakLoad,
    Reexport,
    UpwardLoad,
    LazyLoad,
    Id,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dylib {
    pub kind: DylibKind,
    pub name: String,
    pub timestamp: u32,
    pub current_version: u32,
    pub compatibility_version: u32,
}

impl fmt::Display for Dylib {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<8} {} ({}.{}.{})",
            match self.kind {
                DylibKind::Load => "load",
                DylibKind::WeakLoad => "weak",
                DylibKind::Reexport => "reexport",
                DylibKind::UpwardLoad => "upward",
                DylibKind::LazyLoad => "lazy",
                DylibKind::Id => "id",
            },
            self.name,
            self.current_version >> 16,
            (self.current_version >> 8) & 0xff,
            self.current_version & 0xff
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Symtab {
    pub symoff: u32,
    pub nsyms: u32,
    pub stroff: u32,
    pub strsize: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dysymtab {
    pub ilocalsym: u32,
    pub nlocalsym: u32,
    pub iextdefsym: u32,
    pub nextdefsym: u32,
    pub iundefsym: u32,
    pub nundefsym: u32,
    pub tocoff: u32,
    pub ntoc: u32,
    pub modtaboff: u32,
    pub nmodtab: u32,
    pub extrefsymoff: u32,
    pub nextrefsyms: u32,
    pub indirectsymoff: u32,
    pub nindirectsyms: u32,
    pub extreloff: u32,
    pub nextrel: u32,
    pub locreloff: u32,
    pub nlocrel: u32,
}

/// `LC_DYLD_INFO` / `LC_DYLD_INFO_ONLY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DyldInfo {
    pub rebase_off: u32,
    pub rebase_size: u32,
    pub bind_off: u32,
    pub bind_size: u32,
    pub weak_bind_off: u32,
    pub weak_bind_size: u32,
    pub lazy_bind_off: u32,
    pub lazy_bind_size: u32,
    pub export_off: u32,
    pub export_size: u32,
}

impl DyldInfo {
    pub fn has_rebases(&self) -> bool {
        self.rebase_size > 0
    }
    pub fn has_binds(&self) -> bool {
        self.bind_size > 0
    }
    pub fn has_lazy_binds(&self) -> bool {
        self.lazy_bind_size > 0
    }
    pub fn has_weak_binds(&self) -> bool {
        self.weak_bind_size > 0
    }
    pub fn has_exports(&self) -> bool {
        self.export_size > 0
    }
}

/// A `linkedit_data` style command (code signature, function starts, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkeditData {
    pub kind: &'static str,
    pub dataoff: u32,
    pub datasize: u32,
}

/// Mach thread state from `LC_UNIXTHREAD` / `LC_THREAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadState {
    pub flavor: u32,
    pub count: u32,
    /// Raw words as stored in the command.  For `ARM_THREAD_STATE` these are
    /// r0-r12, sp, lr, pc, cpsr (see [`crate::ThreadRegisters`]).
    pub words: Vec<u32>,
}

impl ThreadState {
    pub fn registers(&self) -> Option<ThreadRegisters> {
        if self.flavor != ARM_THREAD_STATE && self.flavor != ARM_THREAD_STATE32 {
            return None;
        }
        if self.words.len() < ARM_THREAD_STATE_COUNT as usize {
            return None;
        }
        let mut r = [0u32; 17];
        r.copy_from_slice(&self.words[..17]);
        Some(ThreadRegisters {
            r,
            sp: r[13],
            lr: r[14],
            pc: r[15],
            cpsr: r[16],
        })
    }
}

/// Decoded `arm_thread_state_t` initial register set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThreadRegisters {
    pub r: [u32; 17], // r0-r12, sp, lr, pc, cpsr
    pub sp: u32,
    pub lr: u32,
    pub pc: u32,
    pub cpsr: u32,
}

impl ThreadRegisters {
    pub fn from_words(words: &[u32]) -> Option<Self> {
        if words.len() < 17 {
            return None;
        }
        let mut r = [0u32; 17];
        r.copy_from_slice(&words[..17]);
        Some(ThreadRegisters { r, sp: r[13], lr: r[14], pc: r[15], cpsr: r[16] })
    }
}
