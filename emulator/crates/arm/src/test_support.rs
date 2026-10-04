//! Test scaffolding shared by the interpreter's unit tests.
//!
//! The guest's real memory lives in `guestmem::AddressSpace`, which already
//! implements [`crate::Bus`]; these helpers just make the tests short: a
//! 1 MiB flat space with a small code assembler and a step loop.

use crate::{Bus, Cpu, Outcome, Trap};
use guestmem::{AddressSpace, Permissions, RegionKind};

pub const BASE: u32 = 0x1000;
// Above the 1 MiB code region so the two do not overlap.
pub const STACK: u32 = 0x0020_0000;
pub const STACK_SIZE: u32 = 0x0001_0000;

/// A flat, writable 1 MiB memory starting at [`BASE`], plus stack at
/// [`STACK`].  Good enough for encoding-level tests.
pub fn space() -> AddressSpace {
    let mut space = AddressSpace::new();
    space
        .map("code", BASE, 1 << 20, Permissions::RWX, RegionKind::Image, &[])
        .expect("map test memory");
    space
        .map("stack", STACK, STACK_SIZE, Permissions::RW, RegionKind::Stack, &[])
        .expect("map test stack");
    space
}

/// Build a CPU positioned at the start of the test memory with a valid stack.
pub fn cpu(thumb: bool) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.reset(BASE, STACK + STACK_SIZE, thumb);
    cpu
}

/// Run one instruction, returning the trap if there is one.
pub fn step<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Result<Outcome, Trap> {
    let outcome = cpu.step(bus);
    if let Outcome::Trap(ref trap) = outcome {
        return Err(trap.clone());
    }
    Ok(outcome)
}

/// Run at most `max` instructions, stopping early at a trap.
pub fn run<B: Bus>(cpu: &mut Cpu, bus: &mut B, max: usize) -> Option<Trap> {
    for _ in 0..max {
        match cpu.step(bus) {
            Outcome::Continue => {}
            Outcome::Trap(trap) => return Some(trap),
        }
    }
    None
}

/// Encode a sequence of 16-bit Thumb instructions.
pub fn thumb16(words: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Encode a sequence of 32-bit Thumb-2 instructions from their two halfwords.
pub fn thumb32(halfwords: &[(u16, u16)]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(halfwords.len() * 4);
    for (first, second) in halfwords {
        bytes.extend_from_slice(&first.to_le_bytes());
        bytes.extend_from_slice(&second.to_le_bytes());
    }
    bytes
}

/// Encode a sequence of ARM instructions.
pub fn arm32(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Write code at [`BASE`].
pub fn load(space: &mut AddressSpace, code: &[u8]) {
    space.poke_bytes(BASE, code).expect("load code");
}

/// Write a 32-bit word into the test memory.
pub fn poke(space: &mut AddressSpace, addr: u32, value: u32) {
    space.write_u32(addr, value).expect("poke");
}

/// Read a 32-bit word from the test memory.
pub fn peek(space: &mut AddressSpace, addr: u32) -> u32 {
    space.read_u32(addr).expect("peek")
}
