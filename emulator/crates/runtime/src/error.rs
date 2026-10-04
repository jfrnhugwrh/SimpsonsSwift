//! Errors surfaced by the runtime.

use arm::Trap;
use guestmem::MemoryError;
use macho::MachOError;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    /// The image could not be parsed or is not loadable.
    Image(MachOError),
    /// A guest memory access failed inside the loader or the runtime.
    Memory(MemoryError),
    /// The interpreter hit something it cannot execute or that the runtime has
    /// no handler for.
    Trap(Trap),
    /// A HLE call could not be serviced.
    Hle { function: String, message: String },
    /// A syscall failed in a way the guest cannot recover from.
    Syscall { number: i32, message: String },
    /// The guest asked for something this emulator does not model yet.
    Unsupported(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Image(e) => write!(f, "Mach-O: {e}"),
            RuntimeError::Memory(e) => write!(f, "memory: {e}"),
            RuntimeError::Trap(trap) => f.write_str(&describe_trap(trap)),
            RuntimeError::Hle { function, message } => write!(f, "HLE {function}: {message}"),
            RuntimeError::Syscall { number, message } => write!(f, "syscall {number}: {message}"),
            RuntimeError::Unsupported(what) => write!(f, "unsupported: {what}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<MachOError> for RuntimeError {
    fn from(e: MachOError) -> Self {
        RuntimeError::Image(e)
    }
}

impl From<MemoryError> for RuntimeError {
    fn from(e: MemoryError) -> Self {
        RuntimeError::Memory(e)
    }
}

impl From<Trap> for RuntimeError {
    fn from(e: Trap) -> Self {
        RuntimeError::Trap(e)
    }
}

pub type Result<T> = std::result::Result<T, RuntimeError>;

/// `Trap` belongs to the `arm` crate, so the runtime describes it with a free
/// function rather than a `Display` impl (which the orphan rule forbids).
pub fn describe_trap(trap: &Trap) -> String {
    match trap {
        Trap::Syscall { number } => format!("syscall {number}"),
        Trap::HleCall { address } => format!("HLE call at {address:#010x}"),
        Trap::SupervisorCall { immediate } => format!("svc #{immediate:#x}"),
        Trap::Undefined { address, insn, thumb } => format!(
            "undefined {} instruction {insn:#010x} at {address:#010x}",
            if *thumb { "Thumb" } else { "ARM" }
        ),
        Trap::Memory { error, address, pc, access } => {
            format!("{access:?} fault at {address:#010x} (pc {pc:#010x}): {error}")
        }
        Trap::Breakpoint { address, imm } => format!("breakpoint #{imm} at {address:#010x}"),
    }
}
