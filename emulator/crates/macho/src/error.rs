//! Error type shared by every stage of Mach-O processing.

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachOError {
    /// Not enough bytes for the structure being read.
    Truncated {
        what: &'static str,
        offset: usize,
        need: usize,
        have: usize,
    },
    /// Header magic did not match a Mach-O image we can handle.
    BadMagic(u32),
    /// 64-bit images are recognised (so the CLI can say something useful) but
    /// not loadable: the guest CPU is ARMv7.
    UnsupportedFileType { what: &'static str, value: u64 },
    /// A `cmdsize` was smaller than the fixed part of the command or the
    /// command ran past the end of `sizeofcmds`.
    BadCommandSize { cmd: u32, cmdsize: u32, offset: usize },
    /// `ncmds`/`sizeofcmds` disagree with the actual command stream.
    InconsistentCommands { expected_end: usize, actual_end: usize },
    /// `LC_SEGMENT` (or one of its sections) points outside the file.
    SegmentOutOfFile { name: String, fileoff: u32, filesize: u32, file_len: usize },
    /// String table / symbol table index out of range.
    BadIndex { what: &'static str, index: u64, len: usize },
    /// dyld opcode stream contained something malformed.
    BadDyldOpcode { stream: &'static str, offset: usize, byte: u8 },
    /// Variable-length integer ran past the end of its blob.
    BadLeb { stream: &'static str, offset: usize },
    /// Encrypted (`LC_ENCRYPTION_INFO cryptid != 0`) image.
    Encrypted { cryptoff: u32, cryptsize: u32, cryptid: u32 },
    /// No entry point could be derived (no LC_MAIN and no LC_UNIXTHREAD).
    NoEntryPoint,
    /// A symbol or file could not be found.
    NotFound(String),
}

impl fmt::Display for MachOError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachOError::Truncated { what, offset, need, have } => write!(
                f,
                "truncated Mach-O: reading {what} at offset {offset:#x} needs {need} bytes, file has {have}"
            ),
            MachOError::BadMagic(m) => write!(f, "not a Mach-O file (magic {m:#010x})"),
            MachOError::UnsupportedFileType { what, value } => {
                write!(f, "unsupported Mach-O {what}: {value:#x}")
            }
            MachOError::BadCommandSize { cmd, cmdsize, offset } => write!(
                f,
                "load command {cmd:#x} at {offset:#x} has invalid cmdsize {cmdsize}"
            ),
            MachOError::InconsistentCommands { expected_end, actual_end } => write!(
                f,
                "load command stream ends at {actual_end:#x} but sizeofcmds implies {expected_end:#x}"
            ),
            MachOError::SegmentOutOfFile { name, fileoff, filesize, file_len } => write!(
                f,
                "segment {name} maps file range {fileoff:#x}..{:#x} which is outside the {file_len}-byte file",
                *fileoff as u64 + *filesize as u64
            ),
            MachOError::BadIndex { what, index, len } => {
                write!(f, "{what} index {index} out of range (len {len})")
            }
            MachOError::BadDyldOpcode { stream, offset, byte } => write!(
                f,
                "malformed {stream} opcode stream at {offset:#x}: byte {byte:#04x}"
            ),
            MachOError::BadLeb { stream, offset } => {
                write!(f, "malformed ULEB/SLEB in {stream} at {offset:#x}")
            }
            MachOError::Encrypted { cryptoff, cryptsize, cryptid } => write!(
                f,
                "image is encrypted (cryptid {cryptid}, {cryptoff:#x}+{cryptsize:#x}); \
                 decrypt it (e.g. with a dump from a jailbroken device) before loading"
            ),
            MachOError::NoEntryPoint => write!(
                f,
                "image has no LC_MAIN and no LC_UNIXTHREAD load command, so it has no entry point"
            ),
            MachOError::NotFound(what) => write!(f, "{what} not found"),
        }
    }
}

impl std::error::Error for MachOError {}

pub type Result<T> = core::result::Result<T, MachOError>;
