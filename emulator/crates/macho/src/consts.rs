//! Numeric constants from `<mach-o/loader.h>`, `<mach-o/nlist.h>`,
//! `<mach-o/dyld.h>` and `<mach-o/arm/thread_status.h>`.
//!
//! Values are copied from the public XNU headers so that the dump in
//! `simpsons3_iphone_en.txt` (whose `struct mach_header` / `segment_command` /
//! `dysymtab_command` definitions match these layouts) can be cross-checked
//! field by field.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Magic numbers
// ---------------------------------------------------------------------------

pub const MH_MAGIC: u32 = 0xfeed_face;
pub const MH_CIGAM: u32 = 0xcefa_edfe;
pub const MH_MAGIC_64: u32 = 0xfeed_facf;
pub const MH_CIGAM_64: u32 = 0xcffa_edfe;
pub const FAT_MAGIC: u32 = 0xcafe_babe;
pub const FAT_CIGAM: u32 = 0xbeba_feca;

// ---------------------------------------------------------------------------
// CPU types
// ---------------------------------------------------------------------------

pub const CPU_ARCH_ABI64: u32 = 0x0100_0000;
pub const CPU_TYPE_ARM: u32 = 12;
pub const CPU_TYPE_ARM64: u32 = CPU_TYPE_ARM | CPU_ARCH_ABI64;
pub const CPU_TYPE_X86: u32 = 7;
pub const CPU_TYPE_X86_64: u32 = CPU_TYPE_X86 | CPU_ARCH_ABI64;

pub const CPU_SUBTYPE_ARM_ALL: u32 = 0;
pub const CPU_SUBTYPE_ARM_V6: u32 = 6;
pub const CPU_SUBTYPE_ARM_V7: u32 = 9;
pub const CPU_SUBTYPE_ARM_V7S: u32 = 11;
pub const CPU_SUBTYPE_ARM_V7K: u32 = 12;

// ---------------------------------------------------------------------------
// File types
// ---------------------------------------------------------------------------

pub const MH_OBJECT: u32 = 0x1;
pub const MH_EXECUTE: u32 = 0x2;
pub const MH_FVMLIB: u32 = 0x3;
pub const MH_CORE: u32 = 0x4;
pub const MH_PRELOAD: u32 = 0x5;
pub const MH_DYLIB: u32 = 0x6;
pub const MH_DYLINKER: u32 = 0x7;
pub const MH_BUNDLE: u32 = 0x8;
pub const MH_DYLIB_STUB: u32 = 0x9;
pub const MH_DSYM: u32 = 0xa;
pub const MH_KEXT_BUNDLE: u32 = 0xb;

// ---------------------------------------------------------------------------
// Header flags
// ---------------------------------------------------------------------------

pub const MH_NOUNDEFS: u32 = 0x1;
pub const MH_DYLDLINK: u32 = 0x4;
pub const MH_TWOLEVEL: u32 = 0x80;
pub const MH_PIE: u32 = 0x20_0000;
pub const MH_HAS_TLV_DESCRIPTORS: u32 = 0x80_0000;

// ---------------------------------------------------------------------------
// Load commands
// ---------------------------------------------------------------------------

pub const LC_REQ_DYLD: u32 = 0x8000_0000;

pub const LC_SEGMENT: u32 = 0x1;
pub const LC_SYMTAB: u32 = 0x2;
pub const LC_SYMSEG: u32 = 0x3;
pub const LC_THREAD: u32 = 0x4;
pub const LC_UNIXTHREAD: u32 = 0x5;
pub const LC_LOADFVMLIB: u32 = 0x6;
pub const LC_IDFVMLIB: u32 = 0x7;
pub const LC_IDENT: u32 = 0x8;
pub const LC_FVMFILE: u32 = 0x9;
pub const LC_PREPAGE: u32 = 0xa;
pub const LC_DYSYMTAB: u32 = 0xb;
pub const LC_LOAD_DYLIB: u32 = 0xc;
pub const LC_ID_DYLIB: u32 = 0xd;
pub const LC_LOAD_DYLINKER: u32 = 0xe;
pub const LC_ID_DYLINKER: u32 = 0xf;
pub const LC_PREBOUND_DYLIB: u32 = 0x10;
pub const LC_ROUTINES: u32 = 0x11;
pub const LC_SUB_FRAMEWORK: u32 = 0x12;
pub const LC_SUB_UMBRELLA: u32 = 0x13;
pub const LC_SUB_CLIENT: u32 = 0x14;
pub const LC_SUB_LIBRARY: u32 = 0x15;
pub const LC_TWOLEVEL_HINTS: u32 = 0x16;
pub const LC_PREBIND_CKSUM: u32 = 0x17;
pub const LC_LOAD_WEAK_DYLIB: u32 = 0x18 | LC_REQ_DYLD;
pub const LC_SEGMENT_64: u32 = 0x19;
pub const LC_ROUTINES_64: u32 = 0x1a;
pub const LC_UUID: u32 = 0x1b;
pub const LC_RPATH: u32 = 0x1c | LC_REQ_DYLD;
pub const LC_CODE_SIGNATURE: u32 = 0x1d;
pub const LC_SEGMENT_SPLIT_INFO: u32 = 0x1e;
pub const LC_REEXPORT_DYLIB: u32 = 0x1f | LC_REQ_DYLD;
pub const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
pub const LC_ENCRYPTION_INFO: u32 = 0x21;
pub const LC_DYLD_INFO: u32 = 0x22;
pub const LC_DYLD_INFO_ONLY: u32 = 0x22 | LC_REQ_DYLD;
pub const LC_LOAD_UPWARD_DYLIB: u32 = 0x23 | LC_REQ_DYLD;
pub const LC_VERSION_MIN_MACOSX: u32 = 0x24;
pub const LC_VERSION_MIN_IPHONEOS: u32 = 0x25;
pub const LC_FUNCTION_STARTS: u32 = 0x26;
pub const LC_DYLD_ENVIRONMENT: u32 = 0x27 | LC_REQ_DYLD;
pub const LC_MAIN: u32 = 0x28 | LC_REQ_DYLD;
pub const LC_DATA_IN_CODE: u32 = 0x29;
pub const LC_SOURCE_VERSION: u32 = 0x2a;
pub const LC_DYLIB_CODE_SIGN_DRS: u32 = 0x2b;
pub const LC_ENCRYPTION_INFO_64: u32 = 0x2c;
pub const LC_LINKER_OPTION: u32 = 0x2d;
pub const LC_LINKER_OPTIMIZATION_HINT: u32 = 0x2e;
pub const LC_VERSION_MIN_TVOS: u32 = 0x2f;
pub const LC_VERSION_MIN_WATCHOS: u32 = 0x30;

// ---------------------------------------------------------------------------
// VM protection bits (same values as <mach/vm_prot.h>)
// ---------------------------------------------------------------------------

pub const VM_PROT_NONE: u32 = 0x0;
pub const VM_PROT_READ: u32 = 0x1;
pub const VM_PROT_WRITE: u32 = 0x2;
pub const VM_PROT_EXECUTE: u32 = 0x4;
pub const VM_PROT_ALL: u32 = 0x7;

// ---------------------------------------------------------------------------
// Section flags / types (<mach-o/loader.h>)
// ---------------------------------------------------------------------------

pub const SECTION_TYPE: u32 = 0x0000_00ff;
pub const S_REGULAR: u32 = 0x0;
pub const S_ZEROFILL: u32 = 0x1;
pub const S_CSTRING_LITERALS: u32 = 0x2;
pub const S_4BYTE_LITERALS: u32 = 0x3;
pub const S_8BYTE_LITERALS: u32 = 0x4;
pub const S_LITERAL_POINTERS: u32 = 0x5;
pub const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;
pub const S_LAZY_SYMBOL_POINTERS: u32 = 0x7;
pub const S_SYMBOL_STUBS: u32 = 0x8;
pub const S_MOD_INIT_FUNC_POINTERS: u32 = 0x9;
pub const S_MOD_TERM_FUNC_POINTERS: u32 = 0xa;
pub const S_COALESCED: u32 = 0xb;
pub const S_GB_ZEROFILL: u32 = 0xc;
pub const S_INTERPOSING: u32 = 0xd;
pub const S_16BYTE_LITERALS: u32 = 0xe;
pub const S_DTRACE_DOF: u32 = 0xf;
pub const S_LAZY_DYLIB_SYMBOL_POINTERS: u32 = 0x10;

/// Special indices in the `LC_DYSYMTAB` indirect symbol table.
pub const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
pub const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;
pub const S_THREAD_LOCAL_REGULAR: u32 = 0x11;
pub const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
pub const S_THREAD_LOCAL_VARIABLES: u32 = 0x13;
pub const S_THREAD_LOCAL_VARIABLE_POINTERS: u32 = 0x14;
pub const S_THREAD_LOCAL_INIT_FUNCTION_POINTERS: u32 = 0x15;

pub const SECTION_ATTRIBUTES_USR: u32 = 0xff00_0000;
pub const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
pub const S_ATTR_NO_TOC: u32 = 0x4000_0000;
pub const S_ATTR_STRIP_STATIC_SYMS: u32 = 0x2000_0000;
pub const S_ATTR_NO_DEAD_STRIP: u32 = 0x1000_0000;
pub const S_ATTR_LIVE_SUPPORT: u32 = 0x0800_0000;
pub const S_ATTR_SELF_MODIFYING_CODE: u32 = 0x0400_0000;
pub const S_ATTR_DEBUG: u32 = 0x0200_0000;
pub const SECTION_ATTRIBUTES_SYS: u32 = 0x00ff_0000;
pub const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x0000_0400;
pub const S_ATTR_EXT_RELOC: u32 = 0x0000_0200;
pub const S_ATTR_LOC_RELOC: u32 = 0x0000_0100;

// ---------------------------------------------------------------------------
// nlist type bits
// ---------------------------------------------------------------------------

pub const N_STAB: u8 = 0xe0;
pub const N_PEXT: u8 = 0x10;
pub const N_TYPE: u8 = 0x0e;
pub const N_EXT: u8 = 0x01;

pub const N_UNDF: u8 = 0x0;
pub const N_ABS: u8 = 0x2;
pub const N_SECT: u8 = 0xe;
pub const N_PBUD: u8 = 0xc;
pub const N_INDR: u8 = 0xa;

// ---------------------------------------------------------------------------
// dyld rebase/bind opcodes (<mach-o/loader.h>, dyld `ImageLoaderMachOCompressed`)
// ---------------------------------------------------------------------------

pub const REBASE_OPCODE_MASK: u8 = 0xf0;
pub const REBASE_IMMEDIATE_MASK: u8 = 0x0f;

pub const REBASE_OPCODE_DONE: u8 = 0x00;
pub const REBASE_OPCODE_SET_TYPE_IMM: u8 = 0x10;
pub const REBASE_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB: u8 = 0x20;
pub const REBASE_OPCODE_ADD_ADDR_ULEB: u8 = 0x30;
pub const REBASE_OPCODE_ADD_ADDR_IMM_SCALED: u8 = 0x40;
pub const REBASE_OPCODE_DO_REBASE_IMM_TIMES: u8 = 0x50;
pub const REBASE_OPCODE_DO_REBASE_ULEB_TIMES: u8 = 0x60;
pub const REBASE_OPCODE_DO_REBASE_ADD_ADDR_ULEB: u8 = 0x70;
pub const REBASE_OPCODE_DO_REBASE_ULEB_TIMES_SKIPPING_ULEB: u8 = 0x80;

pub const REBASE_TYPE_POINTER: u8 = 1;
pub const REBASE_TYPE_TEXT_ABSOLUTE32: u8 = 2;
pub const REBASE_TYPE_TEXT_PCREL32: u8 = 3;

pub const BIND_OPCODE_MASK: u8 = 0xf0;
pub const BIND_IMMEDIATE_MASK: u8 = 0x0f;

pub const BIND_OPCODE_DONE: u8 = 0x00;
pub const BIND_OPCODE_SET_DYLIB_ORDINAL_IMM: u8 = 0x10;
pub const BIND_OPCODE_SET_DYLIB_ORDINAL_ULEB: u8 = 0x20;
pub const BIND_OPCODE_SET_DYLIB_SPECIAL_IMM: u8 = 0x30;
pub const BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM: u8 = 0x40;
pub const BIND_OPCODE_SET_TYPE_IMM: u8 = 0x50;
pub const BIND_OPCODE_SET_ADDEND_SLEB: u8 = 0x60;
pub const BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB: u8 = 0x70;
pub const BIND_OPCODE_ADD_ADDR_ULEB: u8 = 0x80;
pub const BIND_OPCODE_DO_BIND: u8 = 0x90;
pub const BIND_OPCODE_DO_BIND_ADD_ADDR_ULEB: u8 = 0xa0;
pub const BIND_OPCODE_DO_BIND_ADD_ADDR_IMM_SCALED: u8 = 0xb0;
pub const BIND_OPCODE_DO_BIND_ULEB_TIMES_SKIPPING_ULEB: u8 = 0xc0;
pub const BIND_OPCODE_THREADED: u8 = 0xd0;

pub const BIND_TYPE_POINTER: u8 = 1;
pub const BIND_TYPE_TEXT_ABSOLUTE32: u8 = 2;
pub const BIND_TYPE_TEXT_PCREL32: u8 = 3;

pub const BIND_SPECIAL_DYLIB_SELF: i32 = 0;
pub const BIND_SPECIAL_DYLIB_MAIN_EXECUTABLE: i32 = -1;
pub const BIND_SPECIAL_DYLIB_FLAT_LOOKUP: i32 = -2;
pub const BIND_SPECIAL_DYLIB_WEAK_LOOKUP: i32 = -3;

pub const BIND_SYMBOL_FLAGS_WEAK_IMPORT: u8 = 0x1;
pub const BIND_SYMBOL_FLAGS_NON_WEAK_DEFINITION: u8 = 0x8;

// ---------------------------------------------------------------------------
// ARM thread state flavours (<mach-o/arm/thread_status.h>)
// ---------------------------------------------------------------------------

pub const ARM_THREAD_STATE: u32 = 1;
pub const ARM_VFP_STATE: u32 = 2;
pub const ARM_EXCEPTION_STATE: u32 = 3;
pub const ARM_DEBUG_STATE: u32 = 10;
pub const ARM_THREAD_STATE_NONE: u32 = 13;
pub const ARM_THREAD_STATE32: u32 = 16;
pub const ARM_THREAD_STATE64: u32 = 6;

/// Number of 32-bit words in `arm_thread_state_t` (r0-r12, sp, lr, pc, cpsr).
pub const ARM_THREAD_STATE_COUNT: u32 = 17;
/// Number of 32-bit words in `arm_vfp_state_t` (d0-d31, fpscr) on ARMv7.
pub const ARM_VFP_STATE_COUNT: u32 = 33;

pub const ARM_CPU_MODE_USR: u32 = 0x10;
pub const ARM_CPU_MODE_SVC: u32 = 0x13;

/// Instruction used by 32-bit Darwin as the syscall trap, with r12 holding the
/// syscall number (positive: BSD, negative: Mach trap).  Encoded as
/// `svc #0x80` == 0xef00_0080 in ARM, or `svc #0x80` == 0xdf80 in Thumb.
pub const ARM_DARWIN_SYSCALL_SVC: u32 = 0x0000_0080;
