//! The runtime for the Simpsons Arcade iOS executable: load the Mach-O image
//! into guest memory, apply dyld's fixups, and execute it against a small HLE
//! implementation of the iOS libraries it was linked against.
//!
//! ```no_run
//! use runtime::{LoadOptions, Machine};
//! let image = macho::MachO::from_path_slice("Simpsons.app/Simpsons", macho::CPU_TYPE_ARM)?;
//! let mut machine = Machine::boot(image, &LoadOptions::default())?;
//! machine.run(50_000_000)?;
//! # Ok::<(), runtime::RuntimeError>(())
//! ```

pub mod error;
pub mod hle;
pub mod loader;
pub mod machine;
pub mod syscall;

pub use error::{describe_trap, Result, RuntimeError};
pub use loader::{LoadOptions, LoadedImage};
pub use machine::{Machine, Stats, StopReason};

/// Version reported by the CLI and written into diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
