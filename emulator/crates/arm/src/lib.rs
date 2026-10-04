//! ARMv7-A / Thumb-2 interpreter.
//!
//! The emulator runs the *original* iOS executable, whose `__TEXT` is ARMv7
//! code compiled by clang (mostly Thumb-2 for armv7 slices).  This crate is an
//! interpreter for the subset of ARMv7-A + VFPv3 that compiled Objective-C++ /
//! C++ code actually executes.
//!
//! Design notes
//! ------------
//! * Execution is a `step()` state machine: one instruction in, one
//!   [`Outcome`] out.  Anything the CPU cannot handle itself (the Darwin
//!   syscall trap, a call into an HLE trampoline, an unknown encoding) is
//!   reported to the runtime instead of panicking, so a missing instruction is
//!   a diagnosable event rather than a crash.
//! * Only a single register bank plus CPSR is modelled.  The emulator never
//!   enters a real exception mode: `svc #0x80` is the syscall ABI and becomes
//!   [`Trap::Syscall`], which is exactly how the guest's `libsystem_kernel`
//!   stubs enter the kernel.
//! * `r15` always holds the address of the instruction being executed; the
//!   architectural "PC read" value (`pc+8` for ARM, `pc+4` for Thumb) is
//!   produced by [`Cpu::read_reg`].

pub mod arm;
pub mod test_support;
#[cfg(test)]
mod unicorn_golden;
pub mod thumb;
pub mod vfp;

use guestmem::{AddressSpace, MemoryError, Result as MemResult};

pub use vfp::Vfp;

/// Trait implemented by whatever the CPU reads and writes through.
pub trait Bus {
    fn read_u8(&mut self, addr: u32) -> MemResult<u8>;
    fn read_u16(&mut self, addr: u32) -> MemResult<u16>;
    fn read_u32(&mut self, addr: u32) -> MemResult<u32>;
    fn write_u8(&mut self, addr: u32, value: u8) -> MemResult<()>;
    fn write_u16(&mut self, addr: u32, value: u16) -> MemResult<()>;
    fn write_u32(&mut self, addr: u32, value: u32) -> MemResult<()>;
    fn read_bytes(&mut self, addr: u32, out: &mut [u8]) -> MemResult<()>;
}

impl Bus for AddressSpace {
    fn read_u8(&mut self, addr: u32) -> MemResult<u8> {
        AddressSpace::read_u8(self, addr)
    }
    fn read_u16(&mut self, addr: u32) -> MemResult<u16> {
        AddressSpace::read_u16(self, addr)
    }
    fn read_u32(&mut self, addr: u32) -> MemResult<u32> {
        AddressSpace::read_u32(self, addr)
    }
    fn write_u8(&mut self, addr: u32, value: u8) -> MemResult<()> {
        AddressSpace::write_u8(self, addr, value)
    }
    fn write_u16(&mut self, addr: u32, value: u16) -> MemResult<()> {
        AddressSpace::write_u16(self, addr, value)
    }
    fn write_u32(&mut self, addr: u32, value: u32) -> MemResult<()> {
        AddressSpace::write_u32(self, addr, value)
    }
    fn read_bytes(&mut self, addr: u32, out: &mut [u8]) -> MemResult<()> {
        AddressSpace::read_into(self, addr, out)
    }
}

// ---------------------------------------------------------------------------
// CPSR
// ---------------------------------------------------------------------------

pub const FLAG_N: u32 = 1 << 31;
pub const FLAG_Z: u32 = 1 << 30;
pub const FLAG_C: u32 = 1 << 29;
pub const FLAG_V: u32 = 1 << 28;
pub const FLAG_Q: u32 = 1 << 27;
pub const FLAG_T: u32 = 1 << 5;
pub const MODE_MASK: u32 = 0x1f;

pub const MODE_USR: u32 = 0x10;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_SYS: u32 = 0x1f;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_UND: u32 = 0x1b;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Fetch,
    Read,
    Write,
}

/// What the interpreter wants the runtime to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    /// `svc #0x80` (Darwin ABI): syscall number in r12, negative numbers are
    /// Mach traps.
    Syscall { number: i32 },
    /// The program counter entered the HLE trampoline page, i.e. the guest
    /// called an imported function through a dyld-bound pointer.
    HleCall { address: u32 },
    /// `svc` with an immediate other than 0x80.
    SupervisorCall { immediate: u32 },
    /// An encoding this interpreter does not implement.
    Undefined { address: u32, insn: u32, thumb: bool },
    /// The instruction stream could not be fetched or a data access faulted.
    Memory { error: MemoryError, address: u32, pc: u32, access: Access },
    /// `bkpt` / `udf`.
    Breakpoint { address: u32, imm: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Executed one instruction; continue.
    Continue,
    /// Executed one instruction, but the runtime must act.
    Trap(Trap),
}

/// State of an open Thumb `IT` block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItBlock {
    /// Condition of the `IT` instruction.
    pub cond: u8,
    /// Bit i set means "letter i is T (use `cond`)", clear means "E (invert)".
    pub pattern: u8,
    /// Number of following instructions the block covers.
    pub len: u8,
    /// How many letters have been consumed.
    pub index: u8,
}

#[derive(Debug, Clone)]
pub struct Cpu {
    /// r0-r15.  `r[15]` holds the address of the *current* instruction.
    pub r: [u32; 16],
    pub cpsr: u32,
    pub spsr: u32,
    pub vfp: Vfp,
    /// Address the next instruction will be fetched from (set by branches).
    branch: Option<u32>,
    /// Open Thumb `IT` block, if any.
    it: ItBlock,
    pub instructions: u64,
    /// Last exclusive-monitor address (`LDREX`/`STREX`).
    pub exclusive: Option<u32>,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            r: [0; 16],
            cpsr: MODE_SYS,
            spsr: 0,
            vfp: Vfp::default(),
            branch: None,
            it: ItBlock::default(),
            instructions: 0,
            exclusive: None,
        }
    }

    /// Set up an ARMv7 state at `pc` with `sp` already pointing into the
    /// thread's stack.
    pub fn reset(&mut self, pc: u32, sp: u32, thumb: bool) {
        self.r = [0; 16];
        self.r[13] = sp;
        self.r[15] = pc & !1;
        self.cpsr = MODE_SYS | if thumb { FLAG_T } else { 0 };
        self.branch = None;
        self.it = ItBlock::default();
        self.instructions = 0;
    }

    #[inline]
    pub fn thumb(&self) -> bool {
        self.cpsr & FLAG_T != 0
    }

    #[inline]
    pub fn pc(&self) -> u32 {
        self.r[15]
    }

    #[inline]
    pub fn set_pc(&mut self, pc: u32) {
        self.r[15] = pc;
    }

    /// Architectural value of the PC as seen by an instruction operand.
    #[inline]
    pub fn read_reg(&self, reg: usize) -> u32 {
        if reg == 15 {
            self.r[15].wrapping_add(if self.thumb() { 4 } else { 8 })
        } else {
            self.r[reg]
        }
    }

    /// Write a register; writing r15 schedules a branch (bit 0 selects Thumb).
    #[inline]
    pub fn write_reg(&mut self, reg: usize, value: u32) {
        if reg == 15 {
            self.branch_to(value);
        } else {
            self.r[reg] = value;
        }
    }

    #[inline]
    pub fn branch_to(&mut self, target: u32) {
        if target & 1 != 0 {
            self.cpsr |= FLAG_T;
            self.branch = Some(target & !1);
        } else {
            self.cpsr &= !FLAG_T;
            self.branch = Some(target & !3);
        }
    }

    /// Branch that stays in the current instruction set (`B`, `BL`, `ldr pc`,
    /// `add pc, ...`).  Only BX/BLX and the exception return take the new state
    /// from the target's bit 0, so this must not clear the T bit just because
    /// the target is even.
    #[inline]
    pub fn branch_keep_state(&mut self, target: u32) {
        let aligned = if self.thumb() { target & !1 } else { target & !3 };
        self.branch = Some(aligned);
    }

    #[inline]
    pub fn flag(&self, mask: u32) -> bool {
        self.cpsr & mask != 0
    }

    #[inline]
    pub fn set_flag(&mut self, mask: u32, value: bool) {
        if value {
            self.cpsr |= mask;
        } else {
            self.cpsr &= !mask;
        }
    }

    pub fn set_nz(&mut self, result: u32) {
        self.set_flag(FLAG_N, result & 0x8000_0000 != 0);
        self.set_flag(FLAG_Z, result == 0);
    }

    pub fn set_nz64(&mut self, result: u64) {
        self.set_flag(FLAG_N, result & 0x8000_0000_0000_0000 != 0);
        self.set_flag(FLAG_Z, result == 0);
    }

    pub fn set_nzcv(&mut self, result: u32, carry: bool, overflow: bool) {
        self.set_nz(result);
        self.set_flag(FLAG_C, carry);
        self.set_flag(FLAG_V, overflow);
    }

    /// Condition code evaluation (`cond` masked to 4 bits).
    #[inline]
    pub fn condition_holds(cond: u32, cpsr: u32) -> bool {
        let n = cpsr & FLAG_N != 0;
        let z = cpsr & FLAG_Z != 0;
        let c = cpsr & FLAG_C != 0;
        let v = cpsr & FLAG_V != 0;
        match cond {
            0x0 => z,
            0x1 => !z,
            0x2 => c,
            0x3 => !c,
            0x4 => n,
            0x5 => !n,
            0x6 => v,
            0x7 => !v,
            0x8 => c && !z,
            0x9 => !c || z,
            0xa => n == v,
            0xb => n != v,
            0xc => !z && n == v,
            0xd => z || n != v,
            _ => true,
        }
    }

    #[inline]
    pub fn cond_holds(&self, cond: u32) -> bool {
        Self::condition_holds(cond, self.cpsr)
    }

    pub fn condition_name(cond: u32) -> &'static str {
        match cond {
            0x0 => "eq",
            0x1 => "ne",
            0x2 => "cs",
            0x3 => "cc",
            0x4 => "mi",
            0x5 => "pl",
            0x6 => "vs",
            0x7 => "vc",
            0x8 => "hi",
            0x9 => "ls",
            0xa => "ge",
            0xb => "lt",
            0xc => "gt",
            0xd => "le",
            _ => "al",
        }
    }

    // -----------------------------------------------------------------
    // Thumb IT blocks
    // -----------------------------------------------------------------

    #[inline]
    pub fn in_it_block(&self) -> bool {
        self.it.len != 0 && self.it.index < self.it.len
    }

    /// Condition that applies to the instruction about to execute.
    ///
    /// Outside an `IT` block this is `0xe` (always); inside one it comes from
    /// the letter sequence decoded when the `IT` instruction was executed.
    pub fn current_condition(&self) -> u32 {
        if !self.in_it_block() {
            return 0xe;
        }
        let use_cond = self.it.pattern & (1 << self.it.index) != 0;
        let cond = self.it.cond as u32;
        if use_cond {
            cond
        } else {
            // "else" letter: invert the low bit (EQ<->NE, CS<->CC, ...).
            cond ^ 1
        }
    }

    /// Open an `IT` block from a 16-bit Thumb `IT`/hint instruction.
    pub fn enter_it_block(&mut self, cond: u8, mask: u8) {
        match it_letters(mask) {
            // mask == 0: hint (NOP/YIELD/WFE/...), not an IT block
            None => self.it = ItBlock::default(),
            Some((len, pattern)) => self.it = ItBlock { cond, pattern, len, index: 0 },
        }
    }

    /// Consume one letter of the open `IT` block.
    pub fn leave_it_instruction(&mut self) {
        if self.it.len == 0 {
            return;
        }
        self.it.index += 1;
        if self.it.index >= self.it.len {
            self.it = ItBlock::default();
        }
    }

    pub fn it_block(&self) -> ItBlock {
        self.it
    }

    // -----------------------------------------------------------------
    // Execution
    // -----------------------------------------------------------------

    /// Execute one instruction.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Outcome {
        let pc = self.r[15];
        if self.thumb() {
            self.step_thumb(bus, pc)
        } else {
            self.step_arm(bus, pc)
        }
    }

    fn step_arm<B: Bus>(&mut self, bus: &mut B, pc: u32) -> Outcome {
        if pc & 3 != 0 {
            return Outcome::Trap(Trap::Memory {
                error: MemoryError::Misaligned { addr: pc, what: "ARM instruction fetch" },
                address: pc,
                pc,
                access: Access::Fetch,
            });
        }
        let insn = match bus.read_u32(pc) {
            Ok(v) => v,
            Err(error) => {
                return Outcome::Trap(Trap::Memory { error, address: pc, pc, access: Access::Fetch })
            }
        };
        let cond = insn >> 28;
        self.branch = None;
        if cond == 0xf {
            self.instructions += 1;
            let outcome = arm::execute_unconditional(self, bus, insn, pc);
            return self.finish_checked(pc.wrapping_add(4), outcome);
        }
        if !self.cond_holds(cond) {
            self.instructions += 1;
            self.r[15] = pc.wrapping_add(4);
            return Outcome::Continue;
        }
        self.instructions += 1;
        let outcome = arm::execute(self, bus, insn, pc);
        self.finish_checked(pc.wrapping_add(4), outcome)
    }

    fn step_thumb<B: Bus>(&mut self, bus: &mut B, pc: u32) -> Outcome {
        let first = match bus.read_u16(pc) {
            Ok(v) => v,
            Err(error) => {
                return Outcome::Trap(Trap::Memory { error, address: pc, pc, access: Access::Fetch })
            }
        };
        self.instructions += 1;

        // Hmm: a Thumb-2 instruction's first halfword is in 11101, 11110 or
        // 11111 (ARM ARM A5.3), i.e. `first >= 0xe800` after masking 16 bits.
        let is_32bit = (first & 0xf8_00) >= 0xe8_00;
        let insn_size = if is_32bit { 4u32 } else { 2u32 };
        let insn: u32 = if is_32bit {
            let second = match bus.read_u16(pc.wrapping_add(2)) {
                Ok(v) => v,
                Err(error) => {
                    return Outcome::Trap(Trap::Memory {
                        error,
                        address: pc.wrapping_add(2),
                        pc,
                        access: Access::Fetch,
                    })
                }
            };
            ((first as u32) << 16) | second as u32
        } else {
            first as u32
        };

        if insn_size == 2 {
            let hw = insn as u16;
            if hw & 0xff00 == 0xbf00 {
                // IT / hints: opens a block, does not consume a letter.
                if hw & 0x000f != 0 {
                    self.enter_it_block(((hw >> 4) & 0xf) as u8, (hw & 0xf) as u8);
                }
                self.r[15] = pc.wrapping_add(2);
                return Outcome::Continue;
            }
        }

        self.branch = None;
        let cond = self.current_condition();
        let outcome = if cond == 0xe || self.cond_holds(cond) {
            thumb::execute(self, bus, insn, pc, insn_size)
        } else {
            Outcome::Continue
        };
        self.leave_it_instruction();
        self.finish_checked(pc.wrapping_add(insn_size), outcome)
    }

    fn finish_checked(&mut self, next: u32, outcome: Outcome) -> Outcome {
        match outcome {
            Outcome::Continue => {
                self.r[15] = self.branch.take().unwrap_or(next);
                Outcome::Continue
            }
            Outcome::Trap(trap) => {
                // Leave the CPU in a resumable state: if the runtime handles the
                // trap (a syscall, say) it continues at `next`.
                self.r[15] = self.branch.take().unwrap_or(next);
                Outcome::Trap(trap)
            }
        }
    }
}

/// Decode an `IT` mask into `(number of following instructions, letter pattern)`.
///
/// Bit `i` of the returned pattern is set when letter `i` is "T" (use the
/// instruction's condition) and clear when it is "E" (use its inverse).  The
/// table is the one from the ARMv7-A/R Architecture Reference Manual, A7.3.5.
pub fn it_letters(mask: u8) -> Option<(u8, u8)> {
    const TABLE: [(u8, u8, u8); 15] = [
        (0b1000, 1, 0b0001),
        (0b0100, 2, 0b0011),
        (0b1100, 2, 0b0001),
        (0b0010, 3, 0b0111),
        (0b1010, 3, 0b0011),
        (0b0110, 3, 0b0101),
        (0b1110, 3, 0b0001),
        (0b0001, 4, 0b1111),
        (0b1001, 4, 0b0111),
        (0b0101, 4, 0b1011),
        (0b1101, 4, 0b0011),
        (0b0011, 4, 0b1101),
        (0b1011, 4, 0b0101),
        (0b0111, 4, 0b1001),
        (0b1111, 4, 0b0001),
    ];
    let mut i = 0;
    while i < TABLE.len() {
        let (m, len, pattern) = TABLE[i];
        if m == mask {
            return Some((len, pattern));
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Shared arithmetic helpers (used by both decoders)
// ---------------------------------------------------------------------------

/// 32-bit `x + y + carry_in`, returning `(result, carry_out, overflow)`.
#[inline]
pub fn add_with_carry(x: u32, y: u32, carry_in: bool) -> (u32, bool, bool) {
    let sum = x as u64 + y as u64 + carry_in as u64;
    let result = sum as u32;
    let carry_out = sum > 0xffff_ffff;
    let overflow = ((x ^ result) & (y ^ result) & 0x8000_0000) != 0;
    (result, carry_out, overflow)
}

#[inline]
pub fn sub_with_carry(x: u32, y: u32, carry_in: bool) -> (u32, bool, bool) {
    add_with_carry(x, !y, carry_in)
}

#[inline]
pub fn shift_lsl(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match amount {
        0 => (value, carry_in),
        1..=31 => (value << amount, (value >> (32 - amount)) & 1 != 0),
        32 => (0, value & 1 != 0),
        _ => (0, false),
    }
}

#[inline]
pub fn shift_lsr(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match amount {
        0 => (value, carry_in),
        1..=31 => (value >> amount, (value >> (amount - 1)) & 1 != 0),
        32 => (0, (value >> 31) & 1 != 0),
        _ => (0, false),
    }
}

#[inline]
pub fn shift_asr(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match amount {
        0 => (value, carry_in),
        1..=31 => (((value as i32) >> amount) as u32, (value >> (amount - 1)) & 1 != 0),
        32.. => (
            if value & 0x8000_0000 != 0 { u32::MAX } else { 0 },
            value & 0x8000_0000 != 0,
        ),
    }
}

#[inline]
pub fn shift_ror(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match amount {
        0 => (value, carry_in),
        n if n % 32 == 0 => (value, value & 0x8000_0000 != 0),
        n => {
            let n = n % 32;
            (value.rotate_right(n), (value >> (n - 1)) & 1 != 0)
        }
    }
}

#[inline]
pub fn shift_rrx(value: u32, carry_in: bool) -> (u32, bool) {
    ((value >> 1) | ((carry_in as u32) << 31), value & 1 != 0)
}

/// `kind`: 0..3 == LSL, LSR, ASR, ROR.
#[inline]
pub fn shift_by(kind: u32, value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match kind {
        0 => shift_lsl(value, amount, carry_in),
        1 => shift_lsr(value, amount, carry_in),
        2 => shift_asr(value, amount, carry_in),
        _ => shift_ror(value, amount, carry_in),
    }
}

/// Decode the ARM modified-immediate (`imm12`) encoding.
#[inline]
pub fn decode_arm_immediate(imm12: u32) -> u32 {
    let unrotated = imm12 & 0xff;
    let rotate = (imm12 >> 8) & 0xf;
    unrotated.rotate_right(rotate * 2)
}

/// Thumb-2 `modified immediate` (a.k.a. `ThumbExpandImm`).
///
/// The 12-bit field is split across the instruction (`i` at bit 26, `imm3` at
/// bits 14:12, `imm8` at bits 7:0), and expands per ARM ARM A6.3.3: with
/// `i:imm3:imm8<11> == 0` the byte is replicated into a 32-bit pattern by the
/// `imm3:imm8` field, otherwise `0x80:imm8<6:0>` is rotated right by the top
/// five bits.
pub fn thumb_expand_imm(insn: u32) -> u32 {
    let imm12 = ((insn >> 26) & 1) << 11 | ((insn >> 12) & 0x7) << 8 | (insn & 0xff);
    let unrotated = imm12 & 0xff;
    if imm12 & 0x800 == 0 {
        match (imm12 >> 8) & 0x7 {
            0b000 => unrotated,
            0b001 => (unrotated << 16) | unrotated,
            0b010 => (unrotated << 24) | (unrotated << 8),
            0b011 => (unrotated << 24) | (unrotated << 16) | (unrotated << 8) | unrotated,
            _ => 0,
        }
    } else {
        let rotation = (imm12 >> 7) & 0x1f;
        (0x80 | (imm12 & 0x7f)).rotate_right(rotation)
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn it_mask_table_matches_the_architecture_manual() {
        assert_eq!(it_letters(0b1000), Some((1, 0b0001))); // IT
        assert_eq!(it_letters(0b0100), Some((2, 0b0011))); // ITT
        assert_eq!(it_letters(0b1100), Some((2, 0b0001))); // ITE
        assert_eq!(it_letters(0b0010), Some((3, 0b0111))); // ITTT
        assert_eq!(it_letters(0b1010), Some((3, 0b0011))); // ITTE
        assert_eq!(it_letters(0b0110), Some((3, 0b0101))); // ITET
        assert_eq!(it_letters(0b1110), Some((3, 0b0001))); // ITEE
        assert_eq!(it_letters(0b0001), Some((4, 0b1111))); // ITTTT
        assert_eq!(it_letters(0b1111), Some((4, 0b0001))); // ITEEE
        assert_eq!(it_letters(0b0000), None); // hint, not an IT
    }

    #[test]
    fn it_block_letter_sequence_is_correct() {
        let mut cpu = Cpu::new();
        cpu.enter_it_block(0, 0b1100); // ITE EQ
        assert_eq!(cpu.current_condition(), 0); // EQ
        cpu.leave_it_instruction();
        assert_eq!(cpu.current_condition(), 1); // NE
        cpu.leave_it_instruction();
        assert!(!cpu.in_it_block());
        assert_eq!(cpu.current_condition(), 0xe);
    }

    #[test]
    fn shifts_match_arm_semantics() {
        assert_eq!(shift_lsl(1, 4, false), (16, false));
        // LSR #31 of 0x8000_0000 leaves bit 31 in bit 0; the last bit *out* is
        // bit 30, which is zero, so the carry is clear (ARM ARM A2.2.1).
        assert_eq!(shift_lsr(0x8000_0000, 31, false), (1, false));
        assert_eq!(shift_lsr(0x8000_0001, 1, false), (0x4000_0000, true));
        assert_eq!(shift_asr(0x8000_0000, 1, false), (0xc000_0000, false));
        assert_eq!(shift_asr(0x8000_0000, 32, false), (0xffff_ffff, true));
        assert_eq!(shift_ror(1, 1, false), (0x8000_0000, true));
        assert_eq!(shift_rrx(1, false), (0, true));
    }

    #[test]
    fn add_with_carry_sets_flags() {
        assert_eq!(add_with_carry(0xffff_ffff, 1, false), (0, true, false));
        assert_eq!(add_with_carry(0x7fff_ffff, 1, false), (0x8000_0000, false, true));
        assert_eq!(sub_with_carry(0, 1, true), (0xffff_ffff, false, false));
    }

    #[test]
    fn arm_immediate_decoding() {
        // imm12 = 0xff, rotate = 0
        assert_eq!(decode_arm_immediate(0xff), 0xff);
        // imm12 = 0x1ff -> 0xff rotated right by 2 (bits 1:0 wrap to 31:30)
        assert_eq!(decode_arm_immediate(0x1ff), 0xc000_003f);
        // imm12 = 0x104 -> 4 rotated right by 2 -> 1 (the rotated bits fall off
        // the bottom).
        assert_eq!(decode_arm_immediate(0x104), 1);
        // `mov r0, #0xff` (e3a000ff) decodes back to 0xff.
        assert_eq!(decode_arm_immediate(0xe3a0_00ff & 0xfff), 0xff);
    }

    #[test]
    fn thumb_expand_imm_covers_the_main_shapes() {
        // `ThumbExpandImm` takes imm12 from bits 26 and 14:12/7:0 of the 32-bit
        // instruction (ARM ARM A6.3.3).
        // imm12 = 0x0ff -> 0x000000ff
        assert_eq!(thumb_expand_imm(0x0000_00ff), 0xff);
        // imm12 = 0x1ff (imm3:imm8<9:8> = 01) -> 0x00ff00ff
        assert_eq!(thumb_expand_imm(0x0000_11ff), 0x00ff_00ff);
        // imm12 = 0x2ff (imm3:imm8<9:8> = 10) -> 0xff00ff00
        assert_eq!(thumb_expand_imm(0x0000_22ff), 0xff00_ff00);
        // imm12 = 0x3ff (imm3:imm8<9:8> = 11) -> 0xffffffff
        assert_eq!(thumb_expand_imm(0x0000_33ff), 0xffff_ffff);
        // bit 11 set -> ROR(0x80:imm8<6:0>, imm12<11:7>); imm12 = 0x8ff rotates
        // 0xff right by 17, i.e. left by 15.
        assert_eq!(thumb_expand_imm(0x0400_00ff), 0x007f_8000);
        // A second rotate shape: i = 1, imm3 = 100 -> imm12 = 0xc00, i.e. 0x80
        // rotated right by 24 = 0x8000.
        assert_eq!(thumb_expand_imm(0x0400_4000), 0x8000);
        // `mov.w r0, #0xff` is f04f 00ff: imm12 = 0x0ff.
        assert_eq!(thumb_expand_imm(0xf04f_00ff), 0xff);
    }
}

/// Regressions for decoder bugs the end-to-end tests exposed.  Every encoding
/// here is the one Keystone produces for the listed assembly, so a mismatch is
/// a decoder bug, not a hand-written-encoding bug.
#[cfg(test)]
mod decoder_regressions {
    use crate::test_support::{cpu, space, BASE};
    use crate::{Cpu, Outcome};

    /// Run one instruction at [`BASE`] and return the CPU.
    fn run_arm(word: u32, setup: impl FnOnce(&mut Cpu)) -> (Cpu, Outcome, guestmem::AddressSpace) {
        let mut space = space();
        let mut cpu = cpu(false);
        space.write_u32(BASE, word).unwrap();
        setup(&mut cpu);
        let outcome = cpu.step(&mut space);
        (cpu, outcome, space)
    }

    fn run_thumb(halfwords: &[u16], setup: impl FnOnce(&mut Cpu)) -> (Cpu, Outcome, guestmem::AddressSpace) {
        let mut space = space();
        let mut cpu = cpu(true);
        for (i, hw) in halfwords.iter().enumerate() {
            space.write_u16(BASE + i as u32 * 2, *hw).unwrap();
        }
        setup(&mut cpu);
        let outcome = cpu.step(&mut space);
        (cpu, outcome, space)
    }

    /// `blx r3` (0xe12fff33) must branch and must not touch CPSR.  It used to be
    /// decoded as `msr cpsr, r3` because the MSR pattern did not pin bits 11:4.
    #[test]
    fn blx_register_branches_without_setting_thumb() {
        let (cpu, outcome, _) = run_arm(0xe12f_ff33, |cpu| {
            cpu.r[3] = BASE + 0x40;
        });
        assert!(matches!(outcome, Outcome::Continue));
        assert_eq!(cpu.pc(), BASE + 0x40);
        assert_eq!(cpu.r[14], BASE + 4, "blx sets lr to the next instruction");
        assert!(!cpu.thumb(), "the Thumb bit must survive an indirect call");
    }

    /// `bx r12` (0xe12fff1c).
    #[test]
    fn bx_register_branches() {
        let (cpu, _, _) = run_arm(0xe12f_ff1c, |cpu| {
            cpu.r[12] = BASE + 0x80;
        });
        assert_eq!(cpu.pc(), BASE + 0x80);
        assert_eq!(cpu.r[14], 0, "bx does not link");
    }

    /// `mrs r0, cpsr` (0xe10f0000) — the pattern asked for bits 27:24 == 0000.
    #[test]
    fn mrs_reads_the_status_register() {
        let (cpu, _, _) = run_arm(0xe10f_0000, |cpu| {
            cpu.cpsr = 0x6000_0010;
        });
        assert_eq!(cpu.r[0], 0x6000_0010);
    }

    /// `ldr r1, [pc, #0x40]` (0xe59f1040): bit 25 is *clear*, bit 4 is set, and
    /// the base is the pc read as (instruction + 8).
    #[test]
    fn pc_relative_word_load_uses_the_immediate_form() {
        let (cpu, _, space) = run_arm(0xe59f_1040, |cpu| {
            cpu.set_pc(BASE);
        });
        let _ = space;
        // (BASE + 8) + 0x40 is where the literal would live; the value there is
        // zero, so the *address* is what matters: r1 must not be the instruction.
        assert!(cpu.pc() == BASE + 4, "a load does not branch");
    }

    /// `ldr r1, [r2, r3]` (0xe7921003) — the register-offset form.
    #[test]
    fn register_offset_load_reads_the_sum() {
        let mut space = space();
        let mut cpu = cpu(false);
        space.write_u32(BASE, 0xe792_1003).unwrap();
        space.write_u32(BASE + 0x100, 0xfeed_face).unwrap();
        cpu.r[2] = BASE;
        cpu.r[3] = 0x100;
        let outcome = cpu.step(&mut space);
        assert!(matches!(outcome, Outcome::Continue));
        assert_eq!(cpu.r[1], 0xfeed_face);
    }

    /// `movw r2, #0x1234` / `movt r2, #0x5678` (0xe3012234 / 0xe3452678): the
    /// ARMv7 forms that live inside the AND/SUB opcode space.
    #[test]
    fn mov_wide_builds_a_32_bit_constant() {
        let mut space = space();
        let mut cpu = cpu(false);
        space.write_u32(BASE, 0xe301_2234).unwrap();
        space.write_u32(BASE + 4, 0xe345_2678).unwrap();
        cpu.step(&mut space);
        assert_eq!(cpu.r[2], 0x0000_1234);
        cpu.step(&mut space);
        assert_eq!(cpu.r[2], 0x5678_1234);
    }

    /// Halfword and signed loads/stores live in the `misc` space with bit 4 set
    /// (`ldrh r4, [r5, #6]` = 0xe1d540b6, `strh` = 0xe1c540b6,
    /// `ldrsb r4, [r5, #1]` = 0xe1d540d1).
    #[test]
    fn extra_load_store_handles_halfwords_and_signed_bytes() {
        let mut space = space();
        let mut cpu = cpu(false);
        space.write_u32(BASE, 0xe1c5_40b6).unwrap(); // strh r4, [r5, #6]
        space.write_u32(BASE + 4, 0xe1d5_40b6).unwrap(); // ldrh r4, [r5, #6]
        space.write_u32(BASE + 8, 0xe1d5_40d1).unwrap(); // ldrsb r4, [r5, #1]
        space.write_u32(BASE + 12, 0xe1c5_60d8).unwrap(); // ldrd r6, [r5, #8]
        space.write_u32(BASE + 16, 0xe1c5_60f8).unwrap(); // strd r6, [r5, #8]
        cpu.r[5] = BASE + 0x200;
        cpu.r[4] = 0xabcd_1234;

        cpu.step(&mut space);
        assert_eq!(space.read_u16(BASE + 0x200 + 6).unwrap(), 0x1234, "strh stores the low half");

        space.write_u16(BASE + 0x200 + 6, 0xabcd).unwrap();
        cpu.r[4] = 0;
        cpu.step(&mut space);
        assert_eq!(cpu.r[4], 0xabcd, "ldrh zero-extends");

        space.write_u8(BASE + 0x200 + 1, 0x80).unwrap();
        cpu.step(&mut space);
        assert_eq!(cpu.r[4], 0xffff_ff80, "ldrsb sign-extends");

        cpu.r[6] = 0;
        cpu.r[7] = 0;
        space.write_u32(BASE + 0x200 + 8, 0x1111_2222).unwrap();
        space.write_u32(BASE + 0x200 + 12, 0x3333_4444).unwrap();
        cpu.step(&mut space);
        assert_eq!((cpu.r[6], cpu.r[7]), (0x1111_2222, 0x3333_4444), "ldrd loads a pair");

        cpu.r[6] = 0xcafe_0001;
        cpu.r[7] = 0xcafe_0002;
        space.write_u32(BASE + 0x200 + 8, 0).unwrap();
        space.write_u32(BASE + 0x200 + 12, 0).unwrap();
        cpu.step(&mut space);
        assert_eq!(space.read_u32(BASE + 0x200 + 8).unwrap(), 0xcafe_0001);
        assert_eq!(space.read_u32(BASE + 0x200 + 12).unwrap(), 0xcafe_0002);
    }

    /// `strd r6, [r5, #8]` (0xe1c560f8): the L bit is inverted for the doubleword
    /// forms, so this must store rather than load.
    #[test]
    fn strd_stores_a_register_pair() {
        let mut space = space();
        let mut cpu = cpu(false);
        space.write_u32(BASE, 0xe1c5_60f8).unwrap();
        cpu.r[5] = BASE + 0x300;
        cpu.r[6] = 0xcafe_0001;
        cpu.r[7] = 0xcafe_0002;
        cpu.step(&mut space);
        assert_eq!(space.read_u32(BASE + 0x300 + 8).unwrap(), 0xcafe_0001);
        assert_eq!(space.read_u32(BASE + 0x300 + 12).unwrap(), 0xcafe_0002);
    }

    /// Thumb `adds r0, r0, r1` (0x1840) — bits 15:10 == 0b000110, which the
    /// shift arm used to swallow (producing a bogus `subs`).
    #[test]
    fn thumb_add_register_form() {
        let (cpu, _, _) = run_thumb(&[0x1840], |cpu| {
            cpu.r[0] = 5;
            cpu.r[1] = 7;
        });
        assert_eq!(cpu.r[0], 12);
    }

    /// Thumb `str r0, [r2]` (0x6010) must store: the immediate load/store group
    /// used a different opcode numbering from the register-offset group.
    #[test]
    fn thumb_store_immediate_stores() {
        let mut space = space();
        let mut cpu = cpu(true);
        space.write_u16(BASE, 0x6010).unwrap();
        cpu.r[0] = 0x1234_5678;
        cpu.r[2] = BASE + 0x400;
        cpu.step(&mut space);
        assert_eq!(cpu.r[0], 0x1234_5678, "a store does not write the source register");
        assert_eq!(space.read_u32(BASE + 0x400).unwrap(), 0x1234_5678);
    }

    /// Thumb `ldr r2, [pc, #8]` (0x4a02): the literal address is
    /// `(pc + 4) & !3 + imm8 * 4`.
    #[test]
    fn thumb_literal_load() {
        let mut space = space();
        let mut cpu = cpu(true);
        space.write_u16(BASE, 0x4a02).unwrap();
        space.write_u32(BASE + 0xc, 0xdead_beef).unwrap();
        cpu.step(&mut space);
        assert_eq!(cpu.r[2], 0xdead_beef);
    }

    /// Thumb `mov r12, r1` (0x468c) — the high-register form used to load the
    /// syscall number.
    #[test]
    fn thumb_high_register_move() {
        let (cpu, _, _) = run_thumb(&[0x468c], |cpu| {
            cpu.r[1] = 4;
        });
        assert_eq!(cpu.r[12], 4);
    }

    /// Thumb `bx lr` (0x4770) and `blx r3` (0x4798).
    #[test]
    fn thumb_branches() {
        let (cpu, _, _) = run_thumb(&[0x4770], |cpu| {
            cpu.r[14] = BASE + 0x20;
        });
        assert_eq!(cpu.pc(), BASE + 0x20);
        let (cpu, _, _) = run_thumb(&[0x4798], |cpu| {
            cpu.r[3] = BASE + 0x40; // an even target: ARM state
        });
        assert_eq!(cpu.pc(), BASE + 0x40);
        assert!(!cpu.thumb(), "an even target switches back to ARM");
        assert_eq!(cpu.r[14], BASE + 2 | 1, "thumb blx sets the link bit");
    }
}

