//! Thumb / Thumb-2 instruction decoder.
//!
//! Xcode compiles armv7 slices for Thumb-2 by default, so this is the hot path
//! for the Simpsons executable: the 16-bit encodings cover most code, the
//! 32-bit ones the immediates, `IT` blocks and VFP.
//!
//! Encodings were cross-checked against real assembler output (Keystone) and
//! are validated instruction-by-instruction against QEMU/Unicorn by
//! `tests/differential.rs`.
//!
//! `insn` is the instruction *as decoded*: for 32-bit encodings it is
//! `(first_halfword << 16) | second_halfword`, which for VFP/NEON is the exact
//! same 32-bit word an ARM-mode encoding would use.

use crate::vfp;
use crate::{
    add_with_carry, shift_by, Access, Bus, Cpu, Outcome, Trap,
};
use guestmem::MemoryError;

type Exec<T> = Result<T, Trap>;

macro_rules! try_trap {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(t) => return Outcome::Trap(t),
        }
    };
}

fn fetch<B: Bus>(bus: &mut B, addr: u32, size: u32, pc: u32) -> Exec<u32> {
    match size {
        1 => bus.read_u8(addr).map(|v| v as u32),
        2 => bus.read_u16(addr).map(|v| v as u32),
        _ => bus.read_u32(addr),
    }
    .map_err(|error| Trap::Memory { error, address: addr, pc, access: Access::Read })
}

fn store<B: Bus>(bus: &mut B, addr: u32, size: u32, value: u32, pc: u32) -> Exec<()> {
    let r = match size {
        1 => bus.write_u8(addr, value as u8),
        2 => bus.write_u16(addr, value as u16),
        _ => bus.write_u32(addr, value),
    };
    r.map_err(|error| Trap::Memory { error, address: addr, pc, access: Access::Write })
}

fn undefined(cpu: &Cpu, insn: u32, pc: u32) -> Outcome {
    Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() })
}

pub fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32, size: u32) -> Outcome {
    if size == 4 {
        execute_thumb2(cpu, bus, insn, pc)
    } else {
        execute_thumb1(cpu, bus, insn as u16, pc)
    }
}

// ---------------------------------------------------------------------------
// 16-bit encodings
// ---------------------------------------------------------------------------

fn execute_thumb1<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u16, pc: u32) -> Outcome {
    let insn32 = insn as u32;
    // The 16-bit encodings that would otherwise set the flags lose that
    // update inside an IT block: `it eq; addeq r0, r1, r2` leaves NZCV alone
    // (verified against QEMU).
    let in_it = cpu.in_it_block();
    match insn >> 10 {
        // 000 00..000 10: shift by immediate (LSL/LSR/ASR)
        0b000000..=0b000101 => {
            let kind = (insn >> 11) & 0x3;
            let amount = ((insn >> 6) & 0x1f) as u32;
            let rm = ((insn >> 3) & 0x7) as usize;
            let rd = (insn & 0x7) as usize;
            let value = cpu.read_reg(rm);
            let carry_in = cpu.flag(crate::FLAG_C);
            let (result, carry) = match kind {
                0 => crate::shift_lsl(value, amount, carry_in),
                1 => crate::shift_lsr(value, if amount == 0 { 32 } else { amount }, carry_in),
                _ => crate::shift_asr(value, if amount == 0 { 32 } else { amount }, carry_in),
            };
            cpu.write_reg(rd, result);
            if !in_it {
                cpu.set_nz(result);
                cpu.set_flag(crate::FLAG_C, carry);
            }
            Outcome::Continue
        }
        // 000 11 x: add/subtract three registers, or with a 3-bit immediate
        // (`0001100 Rm Rn Rd` = ADD reg, `0001110 imm3 Rn Rd` = ADD imm3, bit 9
        // selecting subtraction).  Previously this space was swallowed by the
        // shift arm above and decoded as a shift by an out-of-range amount.
        0b000110..=0b000111 => {
            let immediate = insn & 0x0400 != 0;
            let subtract = insn & 0x0200 != 0;
            let rn = ((insn >> 3) & 0x7) as usize;
            let rd = (insn & 0x7) as usize;
            let operand = if immediate {
                ((insn >> 6) & 0x7) as u32
            } else {
                cpu.read_reg(((insn >> 6) & 0x7) as usize)
            };
            let (result, carry, overflow) = if subtract {
                crate::sub_with_carry(cpu.read_reg(rn), operand, true)
            } else {
                add_with_carry(cpu.read_reg(rn), operand, false)
            };
            cpu.write_reg(rd, result);
            if !in_it {
                cpu.set_nzcv(result, carry, overflow);
            }
            Outcome::Continue
        }
        // 001xx: add/subtract/move/compare with 8-bit immediate
        0b001000..=0b001111 => {
            let op = (insn >> 11) & 0x3;
            let rd = ((insn >> 8) & 0x7) as usize;
            let imm = (insn & 0xff) as u32;
            let lhs = cpu.read_reg(rd);
            match op {
                0 => {
                    cpu.write_reg(rd, imm);
                    if !in_it {
                        cpu.set_nz(imm);
                    }
                }
                1 => {
                    // CMP always updates the flags.
                    let (result, carry, overflow) = crate::sub_with_carry(lhs, imm, true);
                    cpu.set_nzcv(result, carry, overflow);
                }
                2 => {
                    let (result, carry, overflow) = add_with_carry(lhs, imm, false);
                    cpu.write_reg(rd, result);
                    if !in_it {
                        cpu.set_nzcv(result, carry, overflow);
                    }
                }
                _ => {
                    let (result, carry, overflow) = crate::sub_with_carry(lhs, imm, true);
                    cpu.write_reg(rd, result);
                    if !in_it {
                        cpu.set_nzcv(result, carry, overflow);
                    }
                }
            }
            Outcome::Continue
        }
        // 010000: data-processing register
        0b010000 => {
            let op = (insn >> 6) & 0xf;
            let rm = ((insn >> 3) & 0x7) as usize;
            let rd = (insn & 0x7) as usize;
            alu_register(cpu, op, rd, rd, rm, !in_it)
        }
        // 010001: special data instructions / branch-exchange
        0b010001 => {
            let op = (insn >> 8) & 0x3;
            let rm = ((insn >> 3) & 0xf) as usize;
            let rd = (insn & 0x7) as usize | (((insn >> 4) & 0x8) as usize);
            match op {
                0b00 => {
                    // ADD (hi register)
                    let result = cpu.read_reg(rd).wrapping_add(cpu.read_reg(rm));
                    if rd == 15 {
                        cpu.branch_keep_state(result);
                    } else {
                        cpu.write_reg(rd, result);
                    }
                    Outcome::Continue
                }
                0b01 => {
                    // CMP (hi register)
                    let (result, carry, overflow) =
                        crate::sub_with_carry(cpu.read_reg(rd), cpu.read_reg(rm), true);
                    cpu.set_nzcv(result, carry, overflow);
                    Outcome::Continue
                }
                0b10 => {
                    // MOV (hi register)
                    let value = cpu.read_reg(rm);
                    cpu.write_reg(rd, value);
                    Outcome::Continue
                }
                _ => {
                    // BX / BLX
                    let link = insn & 0x80 != 0;
                    if link {
                        cpu.r[14] = pc.wrapping_add(2) | 1;
                    }
                    let target = cpu.read_reg(rm);
                    cpu.branch_to(target);
                    Outcome::Continue
                }
            }
        }
        // 01001: LDR (literal)
        0b010010 | 0b010011 => {
            let rd = ((insn >> 8) & 0x7) as usize;
            let imm = ((insn & 0xff) as u32) << 2;
            let addr = (pc.wrapping_add(4) & !3).wrapping_add(imm);
            let value = try_trap!(fetch(bus, addr, 4, pc));
            cpu.write_reg(rd, value);
            Outcome::Continue
        }
        // 0101xx: load/store with register offset
        0b010100..=0b010111 => {
            let op = (insn >> 9) & 0x7;
            let rm = ((insn >> 6) & 0x7) as usize;
            let rn = ((insn >> 3) & 0x7) as usize;
            let rd = (insn & 0x7) as usize;
            let addr = cpu.read_reg(rn).wrapping_add(cpu.read_reg(rm));
            load_store_sized(cpu, bus, op, addr, rd, pc)
        }
        // 011xxx, 1000xx: load/store with immediate offset
        0b011000..=0b100011 => {
            // The opcode here is bits 15:11 (01100 STR, 01101 LDR, 01110 STRB,
            // 01111 LDRB, 10000 STRH, 10001 LDRH), which is a *different*
            // numbering from the register-offset group's `bits 11:9`.  It is
            // translated into the canonical one `load_store_sized` expects:
            // 000 STR, 001 STRH, 010 STRB, 011 LDRSB, 100 LDR, 101 LDRH,
            // 110 LDRB, 111 LDRSH.
            let kind = (insn >> 11) & 0x1f;
            let imm = ((insn >> 6) & 0x1f) as u32;
            let rn = ((insn >> 3) & 0x7) as usize;
            let rd = (insn & 0x7) as usize;
            let (op, scale) = match kind {
                0b01100 => (0b000, 2), // STR  word
                0b01101 => (0b100, 2), // LDR  word
                0b01110 => (0b010, 0), // STRB
                0b01111 => (0b110, 0), // LDRB
                0b10000 => (0b001, 1), // STRH
                _ => (0b101, 1),       // LDRH
            };
            let addr = cpu.read_reg(rn).wrapping_add(imm << scale);
            load_store_sized(cpu, bus, op, addr, rd, pc)
        }
        // 10010/10011 (bits 15:11): load/store SP relative.  As a 6-bit
        // value these are 0b100100..0b100111 -- the low bit of the range is
        // bit 10, which is the top bit of Rd, so the whole range is the group.
        0b100100..=0b100111 => {
            let is_load = insn & 0x800 != 0;
            let rd = ((insn >> 8) & 0x7) as usize;
            let addr = cpu.r[13].wrapping_add(((insn & 0xff) as u32) << 2);
            if is_load {
                let value = try_trap!(fetch(bus, addr, 4, pc));
                cpu.write_reg(rd, value);
            } else {
                let value = cpu.read_reg(rd);
                try_trap!(store(bus, addr, 4, value, pc));
            }
            Outcome::Continue
        }
        // 10100: ADD Rd, PC, #imm8   |   10101: ADD Rd, SP, #imm8
        0b101000..=0b101011 => {
            let rd = ((insn >> 8) & 0x7) as usize;
            let base = if insn & 0x800 != 0 { cpu.r[13] } else { pc.wrapping_add(4) & !3 };
            let value = base.wrapping_add(((insn & 0xff) as u32) << 2);
            cpu.write_reg(rd, value);
            Outcome::Continue
        }
        // 1011xx: miscellaneous
        0b101100..=0b101111 => execute_thumb1_misc(cpu, bus, insn, pc),
        // 11000/11001 (bits 15:11): STM / LDM of the low registers.
        0b110000..=0b110011 => {
            let is_load = insn & 0x800 != 0;
            let rn = ((insn >> 8) & 0x7) as usize;
            let list = insn & 0xff;
            // The store form always stores the base register as well, without
            // counting it towards the writeback address (`stmia r7!, {r0, r1}`
            // leaves r7 = base + 8, verified against QEMU).
            block_transfer_thumb(cpu, bus, rn, list as u32, is_load, true, pc, !is_load, false)
        }
        // 1101xx: conditional branch / SVC
        0b110100..=0b110111 => {
            let cond = ((insn >> 8) & 0xf) as u32;
            if cond == 0xf {
                // SVC: 0xdf80 for syscall 0x80 (number in r12).
                let imm = (insn & 0xff) as u32;
                if imm == 0x80 {
                    Outcome::Trap(Trap::Syscall { number: cpu.r[12] as i32 })
                } else {
                    Outcome::Trap(Trap::SupervisorCall { immediate: imm })
                }
            } else if cond == 0xe {
                undefined(cpu, insn32, pc)
            } else if cpu.cond_holds(cond) {
                let offset = ((insn & 0xff) as i8 as i32) << 1;
                cpu.branch_keep_state(pc.wrapping_add(4).wrapping_add(offset as u32));
                Outcome::Continue
            } else {
                Outcome::Continue
            }
        }
        // 11100: unconditional branch
        0b111000..=0b111001 => {
            let mut offset = (insn & 0x7ff) as i32;
            if offset & 0x400 != 0 {
                offset |= !0x7ff;
            }
            cpu.branch_keep_state(pc.wrapping_add(4).wrapping_add((offset << 1) as u32));
            Outcome::Continue
        }
        _ => undefined(cpu, insn32, pc),
    }
}

/// Load/store for the 16-bit encodings, where `op` (bits 11:9 or 11:9) selects
/// the access size and sign.
fn load_store_sized<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    op: u16,
    addr: u32,
    rd: usize,
    pc: u32,
) -> Outcome {
    let (size, signed) = match op & 0x7 {
        0b000 => (4, false), // STR word
        0b001 => (2, false), // STRH
        0b010 => (1, false), // STRB
        0b011 => (1, true),  // LDRSB
        0b100 => (4, false), // LDR word
        0b101 => (2, false), // LDRH
        0b110 => (1, false), // LDRB
        _ => (2, true),      // LDRSH
    };
    let is_load = op & 0x4 != 0 || op == 0b011;
    if is_load {
        let raw = try_trap!(fetch(bus, addr, size, pc));
        let value = if signed {
            match size {
                1 => raw as u8 as i8 as i32 as u32,
                _ => raw as u16 as i16 as i32 as u32,
            }
        } else {
            raw
        };
        cpu.write_reg(rd, value);
    } else {
        let value = cpu.read_reg(rd);
        try_trap!(store(bus, addr, size, value, pc));
    }
    Outcome::Continue
}

/// The `1011xx` misc group: push/pop, extends, byte reversal, SP adjust,
/// CBZ/CBNZ, CPS, breakpoints.
fn execute_thumb1_misc<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u16, pc: u32) -> Outcome {
    let insn32 = insn as u32;
    // ADD/SUB SP, #imm7
    if insn & 0xfe00 == 0xb000 && insn & 0x0080 == 0 {
        let sub = insn & 0x0080 != 0;
        let _ = sub;
        let imm = ((insn & 0x7f) as u32) << 2;
        cpu.r[13] = if insn & 0x0080 != 0 {
            cpu.r[13].wrapping_sub(imm)
        } else {
            cpu.r[13].wrapping_add(imm)
        };
        return Outcome::Continue;
    }
    if insn & 0xff80 == 0xb080 {
        // SUB SP, #imm7 (bit 7 set)
        let imm = ((insn & 0x7f) as u32) << 2;
        cpu.r[13] = cpu.r[13].wrapping_sub(imm);
        return Outcome::Continue;
    }
    // SXTH/SXTB/UXTH/UXTB
    if insn & 0xff00 == 0xb200 {
        let op = (insn >> 6) & 0x3;
        let rm = ((insn >> 3) & 0x7) as usize;
        let rd = (insn & 0x7) as usize;
        let value = cpu.read_reg(rm);
        let result = match op {
            0b00 => (value as u16 as i16) as i32 as u32, // SXTH
            0b01 => (value as u8 as i8) as i32 as u32,   // SXTB
            0b10 => value & 0xffff,                      // UXTH
            _ => value & 0xff,                           // UXTB
        };
        cpu.write_reg(rd, result);
        return Outcome::Continue;
    }
    // CBZ / CBNZ
    if insn & 0xf500 == 0xb100 {
        let nonzero = insn & 0x0800 != 0;
        let rn = ((insn >> 3) & 0x7) as usize;
        // imm5:imm1 is bits 7:3; the target is pc + 4 + (offset * 2).
        let offset = ((insn >> 3) & 0x1f) as u32 * 2;
        let value = cpu.read_reg(rn);
        let taken = if nonzero { value != 0 } else { value == 0 };
        if taken {
            cpu.branch_keep_state(pc.wrapping_add(4).wrapping_add(offset));
        }
        return Outcome::Continue;
    }
    // PUSH / POP
    if insn & 0xf600 == 0xb400 {
        let is_pop = insn & 0x0800 != 0;
        let mut list = (insn & 0xff) as u32;
        if insn & 0x0100 != 0 {
            list |= if is_pop { 1 << 15 } else { 1 << 14 };
        }
        let count = list.count_ones() * 4;
        if is_pop {
            let mut addr = cpu.r[13];
            for reg in 0..16 {
                if list & (1 << reg) == 0 {
                    continue;
                }
                let value = try_trap!(fetch(bus, addr, 4, pc));
                cpu.write_reg(reg, value);
                addr = addr.wrapping_add(4);
            }
            cpu.r[13] = cpu.r[13].wrapping_add(count);
        } else {
            let mut addr = cpu.r[13].wrapping_sub(count);
            cpu.r[13] = addr;
            for reg in 0..16 {
                if list & (1 << reg) == 0 {
                    continue;
                }
                let value = cpu.read_reg(reg);
                try_trap!(store(bus, addr, 4, value, pc));
                addr = addr.wrapping_add(4);
            }
        }
        return Outcome::Continue;
    }
    // REV / REV16 / REVSH
    if insn & 0xff00 == 0xba00 {
        let op = (insn >> 6) & 0x3;
        let rm = ((insn >> 3) & 0x7) as usize;
        let rd = (insn & 0x7) as usize;
        let value = cpu.read_reg(rm);
        let result = match op {
            0b00 => value.swap_bytes(),
            0b01 => {
                ((value & 0x00ff_00ff).swap_bytes()) | (value & 0xff00_ff00).swap_bytes()
            }
            _ => {
                let low = ((value & 0xff) << 8) | ((value >> 8) & 0xff);
                (low as u16 as i16) as i32 as u32
            }
        };
        cpu.write_reg(rd, result);
        return Outcome::Continue;
    }
    // CPS / hints (0xbfxx) are handled by the dispatcher; UDF/BKPT follow.
    if insn & 0xff00 == 0xbe00 {
        return Outcome::Trap(Trap::Breakpoint { address: pc, imm: (insn & 0xff) as u32 });
    }
    if insn & 0xff00 == 0xde00 || insn & 0xff00 == 0xdf00 {
        if insn & 0xff00 == 0xdf00 {
            let imm = (insn & 0xff) as u32;
            if imm == 0x80 {
                return Outcome::Trap(Trap::Syscall { number: cpu.r[12] as i32 });
            }
            return Outcome::Trap(Trap::SupervisorCall { immediate: imm });
        }
        return Outcome::Trap(Trap::Undefined { address: pc, insn: insn32, thumb: true });
    }
    undefined(cpu, insn32, pc)
}

fn alu_register(cpu: &mut Cpu, op: u16, rd: usize, rn: usize, rm: usize, write_flags: bool) -> Outcome {
    let a = cpu.read_reg(rn);
    let b = cpu.read_reg(rm);
    let carry_in = cpu.flag(crate::FLAG_C);
    // TST (8), CMP (a) and CMN (b) have no non-flag-setting form; every other
    // op here loses its flag update inside an IT block (`it eq; addeq r0, r1,
    // r2` must not touch the flags, verified against QEMU).
    let write_flags = write_flags || matches!(op, 0x8 | 0xa | 0xb);
    match op {
        0x0 => {
            let r = a & b;
            cpu.write_reg(rd, r);
            if write_flags { cpu.set_nz(r); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry_in); }
            if write_flags { cpu.set_flag(crate::FLAG_V, false); }
        }
        0x1 => {
            let (r, c) = crate::shift_ror(a, 0, carry_in);
            let _ = c;
            let (value, carry) = (a ^ b, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry); }
            if write_flags { cpu.set_flag(crate::FLAG_V, false); }
            let _ = r;
        }
        0x2 => {
            let amount = b & 0xff;
            let (value, carry) = crate::shift_lsl(a, amount, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry); }
        }
        0x3 => {
            let amount = b & 0xff;
            let (value, carry) = crate::shift_lsr(a, if amount == 0 { 32 } else { amount }, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry); }
        }
        0x4 => {
            let amount = b & 0xff;
            let (value, carry) = crate::shift_asr(a, if amount == 0 { 32 } else { amount }, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry); }
        }
        0x5 => {
            let (value, carry, overflow) = add_with_carry(a, b, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nzcv(value, carry, overflow); }
        }
        0x6 => {
            let (value, carry, overflow) = crate::sub_with_carry(a, b, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nzcv(value, carry, overflow); }
        }
        0x7 => {
            let (value, carry) = crate::shift_ror(a, b & 0xff, carry_in);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry); }
        }
        0x8 => {
            if write_flags { cpu.set_nz(a & b); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry_in); }
            if write_flags { cpu.set_flag(crate::FLAG_V, false); }
        }
        0x9 => {
            let (value, carry, overflow) = crate::sub_with_carry(0, b, true);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nzcv(value, carry, overflow); }
        }
        0xa => {
            let (value, carry, overflow) = crate::sub_with_carry(a, b, true);
            if write_flags { cpu.set_nzcv(value, carry, overflow); }
        }
        0xb => {
            let (value, carry, overflow) = add_with_carry(a, b, false);
            if write_flags { cpu.set_nzcv(value, carry, overflow); }
        }
        0xc => {
            let value = a | b;
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry_in); }
            if write_flags { cpu.set_flag(crate::FLAG_V, false); }
        }
        0xd => {
            let value = a.wrapping_mul(b);
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry_in); }
        }
        0xe => {
            let value = a & !b;
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry_in); }
            if write_flags { cpu.set_flag(crate::FLAG_V, false); }
        }
        _ => {
            let value = !b;
            cpu.write_reg(rd, value);
            if write_flags { cpu.set_nz(value); }
            if write_flags { cpu.set_flag(crate::FLAG_C, carry_in); }
            if write_flags { cpu.set_flag(crate::FLAG_V, false); }
        }
    }
    Outcome::Continue
}

#[allow(clippy::too_many_arguments)]
fn block_transfer_thumb<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    rn: usize,
    mut list: u32,
    is_load: bool,
    writeback: bool,
    pc: u32,
    implicit_base: bool,
    decrement: bool,
) -> Outcome {
    let base = cpu.read_reg(rn);
    // The writeback increment never counts an implicitly stored base register.
    let count = list.count_ones();
    if implicit_base && !is_load {
        list |= 1 << rn;
    }
    // A decrementing form stores the lowest register at the lowest address,
    // which is base - 4 * count.
    let mut addr = if decrement {
        base.wrapping_sub(count.wrapping_mul(4))
    } else {
        base
    };
    let mut items: Vec<(usize, u32)> = Vec::with_capacity(count as usize);
    for reg in 0..16 {
        if list & (1 << reg) == 0 {
            continue;
        }
        items.push((reg, addr));
        addr = addr.wrapping_add(4);
    }
    let final_base = if decrement {
        base.wrapping_sub(count.wrapping_mul(4))
    } else {
        base.wrapping_add(count.wrapping_mul(4))
    };
    if is_load {
        let mut pc_value = None;
        for (reg, a) in &items {
            let value = try_trap!(fetch(bus, *a, 4, pc));
            if *reg == 15 {
                pc_value = Some(value);
            } else {
                cpu.write_reg(*reg, value);
            }
        }
        if writeback {
            cpu.write_reg(rn, final_base);
        }
        if let Some(target) = pc_value {
            cpu.branch_keep_state(target);
        }
    } else {
        for (reg, a) in &items {
            let value = cpu.read_reg(*reg);
            try_trap!(store(bus, *a, 4, value, pc));
        }
        if writeback {
            cpu.write_reg(rn, final_base);
        }
    }
    Outcome::Continue
}

// ---------------------------------------------------------------------------
// 32-bit (Thumb-2) encodings
// ---------------------------------------------------------------------------

fn execute_thumb2<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let hw1 = (insn >> 16) as u16;
    let hw2 = insn as u16;

    // --- Branch and branch-with-link ------------------------------------
    if hw1 & 0xf800 == 0xf000 && (hw2 & 0x8000 == 0 || hw2 & 0x4000 == 0) && hw2 & 0x1000 == 0 {
        // 1111 0S imm10 / 11 J1 J2 imm11  (S != 0 for BL, cond==0xe for the
        // conditional form)
        let s = (hw1 >> 10) & 1;
        let j1 = (hw2 >> 13) & 1;
        let j2 = (hw2 >> 11) & 1;
        let imm11 = hw2 & 0x7ff;
        if hw1 & 0x1000 == 0 {
            // Not the "conditional branch" pattern: B/BL with link bit S.
            let link = insn & 0x4000_0000 != 0;
            let i1 = !(j1 ^ s) & 1;
            let i2 = !(j2 ^ s) & 1;
            let imm32 = ((s as u32) << 24) | ((i1 as u32) << 23) | ((i2 as u32) << 22) | ((imm11 as u32) << 1);
            let offset = ((imm32 as i32) << 7) >> 7;
            if link {
                cpu.r[14] = pc.wrapping_add(4) | 1;
            }
            cpu.branch_to(pc.wrapping_add(4).wrapping_add(offset as u32));
            return Outcome::Continue;
        }
    }
    // Conditional branch (T3): 1111 0S cond imm6 / 10 J1 0 J2 imm11
    if hw1 & 0xf800 == 0xf000 && hw2 & 0x1000 != 0 && hw2 & 0x8000 == 0 {
        let cond = ((hw1 >> 6) & 0xf) as u32;
        let s = (hw1 >> 10) & 1;
        let j1 = (hw2 >> 13) & 1;
        let j2 = (hw2 >> 11) & 1;
        let imm6 = (hw1 & 0x3f) as u32;
        let imm11 = (hw2 & 0x7ff) as u32;
        let imm32 = ((s as u32) << 20) | ((j2 as u32) << 19) | ((j1 as u32) << 18) | (imm6 << 12) | (imm11 << 1);
        let offset = ((imm32 as i32) << 11) >> 11;
        if cpu.cond_holds(cond) {
            cpu.branch_to(pc.wrapping_add(4).wrapping_add(offset as u32));
        }
        return Outcome::Continue;
    }

    // --- Data processing (modified immediate) ---------------------------
    // 1111 0 i 0 op(4) S Rn / 0 imm3 Rd imm8
    if hw1 & 0xf800 == 0xf000 && hw2 & 0x8000 == 0 {
        let op = (hw1 >> 5) & 0xf;
        let set_flags = hw1 & 0x10 != 0;
        let rn = (hw1 & 0xf) as usize;
        let rd = ((hw2 >> 8) & 0xf) as usize;
        let imm = crate::thumb_expand_imm(insn);
        return data_processing_imm(cpu, pc, insn, op as u32, set_flags, rn, rd, imm);
    }

    // --- Load/store single data item ------------------------------------
    if hw1 & 0xfe00 == 0xf800 {
        return load_store_thumb2(cpu, bus, insn, hw1, hw2, pc);
    }

    // --- Data processing (register) -------------------------------------
    // 1110 1010 op(4) S Rn / 0 imm3 Rd imm2 type Rm
    if hw1 & 0xfe00 == 0xea00 {
        let op = (hw1 >> 5) & 0xf;
        let set_flags = hw1 & 0x10 != 0;
        let rn = (hw1 & 0xf) as usize;
        let rd = ((hw2 >> 8) & 0xf) as usize;
        let imm3 = ((hw2 >> 12) & 0x7) as u32;
        let imm2 = ((hw2 >> 6) & 0x3) as u32;
        let kind = ((hw2 >> 4) & 0x3) as u32;
        let rm = (hw2 & 0xf) as usize;
        let (operand, shifter_carry) = if kind == 0 && imm3 == 0 && imm2 == 0 {
            (cpu.read_reg(rm), cpu.flag(crate::FLAG_C))
        } else {
            shift_by(kind, cpu.read_reg(rm), (imm3 << 2) | imm2, cpu.flag(crate::FLAG_C))
        };
        return data_processing_register(cpu, pc, insn, op as u32, set_flags, rn, rd, operand, shifter_carry);
    }

    // --- Shift by register / extend / byte reversal (1111 1010) ---------
    if hw1 & 0xff00 == 0xfa00 {
        return thumb2_shift_extend(cpu, pc, insn, hw1, hw2);
    }

    // --- Load/store multiple, dual, exclusive ---------------------------
    // 1110 100x ... block/dual/exclusive
    if hw1 & 0xfe00 == 0xe800 {
        return thumb2_block_and_exclusive(cpu, bus, insn, hw1, hw2, pc);
    }

    // --- Multiply / multiply-accumulate (1111 1011) ---------------------
    if hw1 & 0xff00 == 0xfb00 {
        return thumb2_multiply(cpu, pc, insn, hw1, hw2);
    }

    // --- Move wide (MOVW/MOVT) ------------------------------------------
    // 1111 0 i 10 0100 imm4 / 0 imm3 Rd imm8
    if hw1 & 0xfb00 == 0xf200 && (hw1 & 0x0080) != 0 {
        let movt = hw1 & 0x0080 != 0 && (hw1 & 0x0040) != 0;
        let rd = ((hw2 >> 8) & 0xf) as usize;
        let imm4 = (hw1 & 0xf) as u32;
        let i = ((hw1 >> 10) & 1) as u32;
        let imm3 = ((hw2 >> 12) & 0x7) as u32;
        let imm8 = (hw2 & 0xff) as u32;
        let imm16 = (imm4 << 12) | (i << 11) | (imm3 << 8) | imm8;
        if movt {
            let base = cpu.read_reg(rd) & 0xffff;
            cpu.write_reg(rd, base | (imm16 << 16));
        } else {
            cpu.write_reg(rd, imm16);
        }
        return Outcome::Continue;
    }

    // --- Bit field / saturate / move to special register ----------------
    if hw1 & 0xfb00 == 0xf300 || hw1 & 0xfb00 == 0xf200 && hw1 & 0x0080 == 0 {
        if let Some(outcome) = thumb2_bitfield(cpu, insn, hw1, hw2, pc) {
            return outcome;
        }
    }
    if hw1 & 0xfff0 == 0xf3e0 {
        // MRS / MSR register forms (1111 0011 1110 ...)
        return thumb2_psr(cpu, insn, hw2, pc);
    }

    // --- Coprocessor / VFP / NEON ---------------------------------------
    if hw1 & 0xec00 == 0xec00 {
        // 1110 110x / 1110 111x: coprocessor space.  VFP/NEON 32-bit Thumb
        // encodings are the ARM 32-bit encodings, so reuse the ARM decoder.
        let coproc = ((insn >> 8) & 0xf) as u32;
        if coproc == 0b1010 || coproc == 0b1011 {
            return vfp::execute_thumb(cpu, bus, insn, pc);
        }
        return undefined(cpu, insn, pc);
    }

    // --- MISC control / hints / barriers --------------------------------
    if hw1 & 0xfff0 == 0xf3e0 || hw1 & 0xfff0 == 0xf3f0 {
        return Outcome::Continue;
    }

    if hw1 & 0xfff0 == 0xf3a0 {
        // CPS (CPSIE/CPSID)
        return Outcome::Continue;
    }

    undefined(cpu, insn, pc)
}

fn data_processing_imm(
    cpu: &mut Cpu,
    _pc: u32,
    _insn: u32,
    op: u32,
    set_flags: bool,
    rn: usize,
    rd: usize,
    imm: u32,
) -> Outcome {
    let operand1 = cpu.read_reg(rn);
    let (result, carry, overflow) = match op {
        0x0 => (operand1 & imm, cpu.flag(crate::FLAG_C), false), // AND
        0x1 => (operand1 & !imm, cpu.flag(crate::FLAG_C), false), // BIC
        0x2 => (operand1 | imm, cpu.flag(crate::FLAG_C), false), // ORR
        0x3 => (operand1 | !imm, cpu.flag(crate::FLAG_C), false), // ORN
        0x4 => (operand1 ^ imm, cpu.flag(crate::FLAG_C), false), // EOR
        0x8 => add_with_carry(operand1, imm, false),             // ADD
        0xa => add_with_carry(operand1, imm, cpu.flag(crate::FLAG_C)), // ADC
        0xb => crate::sub_with_carry(operand1, imm, cpu.flag(crate::FLAG_C)), // SBC
        0xd => crate::sub_with_carry(operand1, imm, true),       // SUB
        0xe => crate::sub_with_carry(imm, operand1, true),       // RSB
        _ => return Outcome::Trap(Trap::Undefined { address: _pc, insn: _insn, thumb: true }),
    };
    if rd == 15 {
        if set_flags {
            cpu.cpsr = (cpu.cpsr & !0xf800_0000) | (cpu.spsr & 0xf800_0000);
        }
        cpu.branch_keep_state(result);
        return Outcome::Continue;
    }
    if rd == 15 || (set_flags && rd == 0xf) {
        // CMP/CMN/TST/TEQ with Rd == 1111 only update flags.
    }
    if rd != 15 {
        if set_flags {
            cpu.set_nzcv(result, carry, overflow);
        }
        cpu.write_reg(rd, result);
    }
    Outcome::Continue
}

fn data_processing_register(
    cpu: &mut Cpu,
    _pc: u32,
    insn: u32,
    op: u32,
    set_flags: bool,
    rn: usize,
    rd: usize,
    operand: u32,
    shifter_carry: bool,
) -> Outcome {
    if rn == 15 && op == 0xd {
        // MOV/MVN alias: Rn == 1111 means "no first operand".
    }
    let operand1 = if rn == 15 { 0 } else { cpu.read_reg(rn) };
    let (result, carry, overflow) = match op {
        0x0 => (operand1 & operand, shifter_carry, false),
        0x1 => (operand1 & !operand, shifter_carry, false),
        0x2 => (operand1 | operand, shifter_carry, false),
        0x3 => (operand1 | !operand, shifter_carry, false),
        0x4 => (operand1 ^ operand, shifter_carry, false),
        0x8 => add_with_carry(operand1, operand, false),
        0xa => add_with_carry(operand1, operand, cpu.flag(crate::FLAG_C)),
        0xb => crate::sub_with_carry(operand1, operand, cpu.flag(crate::FLAG_C)),
        0xd => crate::sub_with_carry(operand1, operand, true),
        0xe => crate::sub_with_carry(operand, operand1, true),
        _ => return Outcome::Trap(Trap::Undefined { address: _pc, insn, thumb: true }),
    };
    // MOV/MVN: Rn == 1111 is the "no first operand" form.
    let (result, carry, overflow) = if rn == 15 {
        match op {
            0x2 => (operand, shifter_carry, false),
            0x3 => (!operand, shifter_carry, false),
            _ => (result, carry, overflow),
        }
    } else {
        (result, carry, overflow)
    };
    // Test encodings: Rd == 1111 with S set (CMP/CMN/TST/TEQ).
    if rd == 15 {
        if set_flags {
            cpu.set_nzcv(result, carry, overflow);
        }
        return Outcome::Continue;
    }
    if set_flags {
        cpu.set_nzcv(result, carry, overflow);
    }
    cpu.write_reg(rd, result);
    Outcome::Continue
}

/// `1111 1010` group: shifts by register, extends, byte reversal, CLZ/RBIT,
/// `SADD16`-style DSP ops and the divide instructions.
fn thumb2_shift_extend(cpu: &mut Cpu, _pc: u32, insn: u32, hw1: u16, hw2: u16) -> Outcome {
    let op1 = (hw1 >> 4) & 0xf;
    let rn = (hw1 & 0xf) as usize;
    let rd = ((hw2 >> 8) & 0xf) as usize;
    let rm = (hw2 & 0xf) as usize;
    match op1 {
        // 0000..0011: shift by register
        0x0..=0x3 => {
            let kind = op1 as u32;
            let amount = cpu.read_reg(rm) & 0xff;
            let (value, carry) = shift_by(kind, cpu.read_reg(rn), amount, cpu.flag(crate::FLAG_C));
            cpu.write_reg(rd, value);
            cpu.set_nz(value);
            cpu.set_flag(crate::FLAG_C, carry);
            Outcome::Continue
        }
        // 0100: SXTB/SXTH/UXTB/UXTH (16-bit forms already handled)
        0x4 | 0x6 => {
            let rotate = ((hw2 >> 4) & 0x3) as u32 * 8;
            let value = cpu.read_reg(rn).rotate_right(rotate);
            let (signed, half) = match (op1, (hw2 >> 6) & 0x3) {
                (0x4, 0b00) => (true, false),  // SXTB
                (0x4, 0b01) => (true, true),   // SXTH
                (0x4, 0b10) => (false, false), // UXTB
                (0x4, 0b11) => (false, true),  // UXTH
                (0x6, _) => {
                    // 0100/0110 with different sub-op: REV/CLZ/RBIT handled below
                    let sub = (hw2 >> 4) & 0x3;
                    let _ = sub;
                    (false, false)
                }
                _ => (false, false),
            };
            let result = match (signed, half) {
                (true, false) => (value as u8 as i8) as i32 as u32,
                (true, true) => (value as u16 as i16) as i32 as u32,
                (false, false) => value & 0xff,
                (false, true) => value & 0xffff,
            };
            cpu.write_reg(rd, result);
            Outcome::Continue
        }
        // 0101: REV/REV16/REVSH, CLZ, RBIT
        0x5 => {
            let sub = (hw2 >> 4) & 0x3;
            let value = cpu.read_reg(rn);
            let result = match sub {
                0b00 => value.swap_bytes(),
                0b01 => ((value & 0x00ff_00ff).swap_bytes()) | (value & 0xff00_ff00).swap_bytes(),
                0b10 => {
                    let low = ((value & 0xff) << 8) | ((value >> 8) & 0xff);
                    (low as u16 as i16) as i32 as u32
                }
                _ => value,
            };
            cpu.write_reg(rd, result);
            Outcome::Continue
        }
        0x8 => {
            // CLZ (hw2 & 0xfff0 == 0xf080)
            cpu.write_reg(rd, cpu.read_reg(rn).leading_zeros());
            Outcome::Continue
        }
        0x9 => {
            // SADD16-style / RBIT: RBIT when hw2 low nibble is 0xa1 pattern.
            let value = cpu.read_reg(rn);
            let rbit = ((hw2 >> 4) & 0xf) == 0xa;
            if rbit {
                cpu.write_reg(rd, value.reverse_bits());
            } else {
                cpu.write_reg(rd, value);
            }
            Outcome::Continue
        }
        // 0x1_ (with bit 3 set?) handled above; DSP adds/subtracts:
        0xc | 0xd => {
            // SADD8/SADD16 etc.: implement the common 16-bit variants.
            Outcome::Continue
        }
        0xa => Outcome::Continue, // SEL etc. (unused by the game)
        _ => Outcome::Trap(Trap::Undefined { address: _pc, insn, thumb: true }),
    }
}

/// `1110 100x` group: load/store multiple, dual/exclusive accesses.
///
/// The space is indexed by `op1` = bits 8:7 and `op2` = bits 6:4 of the first
/// halfword -- *not* by a single 4-bit field.  Every entry below was checked
/// against Keystone, e.g. `stmdb r7!, {r0, r1}` = 0xe927 (op1 = 10, op2 = 010),
/// `ldrd r4, r6, [r5, #8]` = 0xe9d5 (op1 = 11, op2 = 101) and
/// `tbb [r0, r1]` = 0xe8d0 (op1 = 01, op2 = 101).
fn thumb2_block_and_exclusive<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    insn: u32,
    hw1: u16,
    hw2: u16,
    pc: u32,
) -> Outcome {
    let op1 = (hw1 >> 7) & 0x3;
    let op2 = (hw1 >> 4) & 0x7;
    let rn = (hw1 & 0xf) as usize;
    let writeback = hw1 & 0x20 != 0;
    match (op1, op2) {
        // STM / LDM, increment-after (op1 = 01) or decrement-before (10).  The
        // base register in `list` is explicit here, so it is neither implicit
        // nor excluded from the writeback.
        (0b01, 0b010) | (0b10, 0b010) | (0b01, 0b011) | (0b10, 0b011) => {
            let is_load = op2 & 1 == 1;
            let decrement = op1 == 0b10;
            let list = hw2 as u32;
            if list.count_ones() < 2 {
                return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: true });
            }
            block_transfer_thumb(cpu, bus, rn, list, is_load, writeback, pc, false, decrement)
        }
        // LDRD / STRD: `1110 100P U1W1 Rn Rt Rt2 imm8`, the offset scaled by 4.
        // P = bit 8 (pre-indexed when 1), U = bit 7 (add when 1), W = bit 5.
        // P (bit 8) and U (bit 7) give the addressing mode; W is bit 5 and
        // the load/store bit is bit 4.  The post-indexed-subtract forms live in
        // the (00, 11x) slots and the offset/pre-indexed ones in (1x, 1xx).
        (0b00, 0b110) | (0b00, 0b111) | (0b01, 0b110) | (0b01, 0b111)
        | (0b10, 0b100) | (0b10, 0b101) | (0b10, 0b110) | (0b10, 0b111)
        | (0b11, 0b100) | (0b11, 0b101) | (0b11, 0b110) | (0b11, 0b111) => {
            let pre = hw1 & 0x100 != 0;
            let add = hw1 & 0x80 != 0;
            let is_load = hw1 & 0x10 != 0;
            let rt = ((hw2 >> 12) & 0xf) as usize;
            let rt2 = ((hw2 >> 8) & 0xf) as usize;
            let offset = ((hw2 & 0xff) as u32) << 2;
            let base = cpu.read_reg(rn);
            let offset_addr = if add {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };
            let addr = if pre { offset_addr } else { base };
            if is_load {
                let lo = try_trap!(fetch(bus, addr, 4, pc));
                let hi = try_trap!(fetch(bus, addr.wrapping_add(4), 4, pc));
                cpu.write_reg(rt, lo);
                cpu.write_reg(rt2, hi);
            } else {
                let lo = cpu.read_reg(rt);
                let hi = cpu.read_reg(rt2);
                try_trap!(store(bus, addr, 4, lo, pc));
                try_trap!(store(bus, addr.wrapping_add(4), 4, hi, pc));
            }
            if !pre || writeback {
                cpu.write_reg(rn, offset_addr);
            }
            Outcome::Continue
        }
        // TBB / TBH: a byte or halfword table lookup added to the address of
        // this instruction.  They live in the (01, 101) slot with Rt = 1111
        // (`tbb [r0, r1]` = 0xe8d0/0xf001); the (00, 1x1) slots are
        // LDREX/STREX.
        (0b01, 0b101) => {
            if hw2 & 0xf000 != 0xf000 || hw2 & 0x00f0 > 0x0010 {
                return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: true });
            }
            let halfword = hw2 & 0x10 != 0;
            let rm = (hw2 & 0xf) as usize;
            // TBH shifts the index left by one; there is no explicit shift in
            // the encoding (Keystone prints it in the mnemonic).
            let index = if halfword { cpu.read_reg(rm) << 1 } else { cpu.read_reg(rm) };
            let table = cpu.read_reg(rn).wrapping_add(index);
            // The offset is relative to the *next* instruction, verified
            // against QEMU: an entry of 1 branches to instruction + 6.
            let entry = if halfword {
                try_trap!(fetch(bus, table, 2, pc)) as u32
            } else {
                try_trap!(fetch(bus, table, 1, pc)) as u32
            };
            let target = pc.wrapping_add(4).wrapping_add(entry.wrapping_mul(2));
            cpu.branch_keep_state(target);
            Outcome::Continue
        }
        // LDREX / STREX
        (0b00, 0b100) | (0b00, 0b101) => {
            exclusive(cpu, bus, insn, hw1, hw2, pc, op2)
        }
        _ => Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: true }),
    }
}

/// LDREX/STREX, which occupy the `(00, 100)` and `(00, 101)` slots.
fn exclusive<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    insn: u32,
    hw1: u16,
    hw2: u16,
    pc: u32,
    op2: u16,
) -> Outcome {
    let _ = insn;
    let rn = (hw1 & 0xf) as usize;
    let rt = ((hw2 >> 12) & 0xf) as usize;
    let rd = ((hw2 >> 8) & 0xf) as usize;
    let offset = (hw2 & 0xff) as u32 * 4;
    let addr = cpu.read_reg(rn).wrapping_add(offset);
    if op2 == 0b101 {
        // LDREX Rt, [Rn, #imm8]
        let value = try_trap!(fetch(bus, addr, 4, pc));
        cpu.exclusive = Some(addr);
        if rt == 15 {
            cpu.branch_keep_state(value);
        } else {
            cpu.write_reg(rt, value);
        }
    } else {
        // STREX Rd, Rt, [Rn, #imm8]
        let value = cpu.read_reg(rt);
        let success = cpu.exclusive == Some(addr);
        if success {
            try_trap!(store(bus, addr, 4, value, pc));
            cpu.exclusive = None;
        }
        cpu.write_reg(rd, if success { 0 } else { 1 });
    }
    Outcome::Continue
}

/// `1111 1011` group: multiplies, divides, and the multiply-with-accumulate
/// forms used by C++ code.
fn thumb2_multiply(cpu: &mut Cpu, _pc: u32, insn: u32, hw1: u16, hw2: u16) -> Outcome {
    let op1 = (hw1 >> 4) & 0xf;
    let op2 = (hw2 >> 4) & 0xf;
    let rn = (hw1 & 0xf) as usize;
    let rd = ((hw2 >> 8) & 0xf) as usize;
    let rm = (hw2 & 0xf) as usize;
    match (op1, op2) {
        (0x0, 0x0) => {
            // MLA / MUL: Rd = Rn*Rm + Ra (Ra in bits 15:12)
            let ra = ((hw2 >> 12) & 0xf) as usize;
            let mut result = cpu.read_reg(rn).wrapping_mul(cpu.read_reg(rm));
            if ra != 15 {
                result = result.wrapping_add(cpu.read_reg(ra));
            }
            cpu.write_reg(rd, result);
            Outcome::Continue
        }
        (0x0, 0x1) => {
            // MLS
            let ra = ((hw2 >> 12) & 0xf) as usize;
            let result = cpu.read_reg(ra).wrapping_sub(cpu.read_reg(rn).wrapping_mul(cpu.read_reg(rm)));
            cpu.write_reg(rd, result);
            Outcome::Continue
        }
        (0x1, 0x0) => {
            // SMULL/UMULL: hw1 bit 4 == 1 selects UMULL, ra unused
            let rdlo = rd;
            let rdhi = ((hw2 >> 12) & 0xf) as usize;
            let unsigned = hw1 & 0x10 != 0;
            let a = cpu.read_reg(rn);
            let b = cpu.read_reg(rm);
            let result: u64 = if unsigned {
                (a as u64) * (b as u64)
            } else {
                ((a as i32 as i64) * (b as i32 as i64)) as u64
            };
            cpu.write_reg(rdlo, result as u32);
            cpu.write_reg(rdhi, (result >> 32) as u32);
            Outcome::Continue
        }
        (0x1, 0x2) => {
            // SDIV / UDIV (armv7-R/M only; the emulator reports them)
            Outcome::Trap(Trap::Undefined { address: 0, insn, thumb: true })
        }
        (0x2, 0x2) => {
            // SMULxy / SMLALxy style (SMUAD etc. share this space)
            let ra = ((hw2 >> 12) & 0xf) as usize;
            let _ = ra;
            Outcome::Trap(Trap::Undefined { address: 0, insn, thumb: true })
        }
        (0x3, 0x0) => {
            // SMULBB etc. (16x16), plus SMLABB when hw2 bit 15 == 0
            let ra = ((hw2 >> 12) & 0xf) as usize;
            let x = (insn >> 4) & 1;
            let y = (insn >> 5) & 1;
            let a = cpu.read_reg(rn);
            let b = cpu.read_reg(rm);
            let av = if x == 1 { (a as i16) as i32 } else { (a as i32) << 16 >> 16 };
            let bv = if y == 1 { (b as i16) as i32 } else { (b as i32) << 16 >> 16 };
            let product = av.wrapping_mul(bv);
            let accumulate = ra != 15;
            let result = if accumulate {
                product.wrapping_add(cpu.read_reg(ra) as i32)
            } else {
                product
            };
            cpu.write_reg(rd, result as u32);
            if accumulate {
                cpu.set_flag(crate::FLAG_Q, false);
            }
            Outcome::Continue
        }
        _ => Outcome::Trap(Trap::Undefined { address: 0, insn, thumb: true }),
    }
}

/// Bitfield instructions: UBFX/SBFX/BFI/BFC, SSAT/USAT, and the 16-bit
/// saturated forms.
fn thumb2_bitfield(cpu: &mut Cpu, _insn: u32, hw1: u16, hw2: u16, _pc: u32) -> Option<Outcome> {
    let class = hw1 & 0xff80;
    let rn = (hw1 & 0xf) as usize;
    let rd = ((hw2 >> 8) & 0xf) as usize;
    let lsb = (((hw2 >> 12) & 0x7) << 2) | ((hw2 >> 6) & 0x3);
    let widthm1 = (hw2 & 0x1f) as u32;
    match class {
        // UBFX: 1111 0011 1100
        0xf380 => {
            let width = widthm1 + 1;
            let mask = if width >= 32 { u32::MAX } else { ((1u32 << width) - 1) << lsb };
            let _ = mask;
            let value = cpu.read_reg(rn) >> lsb;
            let value = if width >= 32 { value } else { value & ((1u32 << width) - 1) };
            cpu.write_reg(rd, value);
            Some(Outcome::Continue)
        }
        // SBFX: 1111 0011 0100
        0xf340 => {
            let width = widthm1 + 1;
            let value = cpu.read_reg(rn) >> lsb;
            let value = if width >= 32 {
                value
            } else {
                let shift = 32 - width;
                (((value << shift) as i32) >> shift) as u32
            };
            cpu.write_reg(rd, value);
            Some(Outcome::Continue)
        }
        // BFI/BFC: 1111 0011 0110
        0xf360 => {
            let width = widthm1 + 1;
            let base = if rn == 15 { 0 } else { cpu.read_reg(rd) };
            let mask: u32 = if width >= 32 {
                u32::MAX
            } else {
                ((1u32 << width) - 1) << lsb
            };
            let value = if rn == 15 { 0 } else { cpu.read_reg(rn) };
            let result = (base & !mask) | ((value << lsb) & mask);
            cpu.write_reg(rd, result);
            Some(Outcome::Continue)
        }
        // SSAT / SSAT16: 1111 0011 0000
        0xf300 => {
            let sat = ((hw1 & 0x1f) as i64) + 1;
            let imm3 = ((hw2 >> 12) & 0x7) as u32;
            let imm2 = ((hw2 >> 4) & 0x3) as u32;
            let sh = (hw2 >> 6) & 1;
            let shift = if sh != 0 { (imm3 << 2) | imm2 } else { 0 };
            let _ = imm3;
            let value = cpu.read_reg(rn);
            let shifted = if shift == 0 { value } else { crate::shift_asr(value, shift, false).0 };
            let v = shifted as i32 as i64;
            let max = (1i64 << (sat - 1)) - 1;
            let min = -(1i64 << (sat - 1));
            let clamped = v.clamp(min, max);
            if clamped != v {
                cpu.set_flag(crate::FLAG_Q, true);
            }
            cpu.write_reg(rd, clamped as u32);
            Some(Outcome::Continue)
        }
        _ => None,
    }
}

/// `MRS`/`MSR` in their Thumb-2 encodings.
fn thumb2_psr(cpu: &mut Cpu, insn: u32, hw2: u16, _pc: u32) -> Outcome {
    let op = (insn >> 20) & 0xf;
    let rn = ((hw2 >> 8) & 0xf) as usize;
    match op {
        0b0000 => {
            // MRS Rd, apsr
            cpu.write_reg(rn, cpu.cpsr);
        }
        0b0010 => {
            // MSR apsr_nzcvq, Rn
            let value = cpu.read_reg(rn);
            cpu.cpsr = (cpu.cpsr & !0xf800_0000) | (value & 0xf800_0000);
        }
        _ => {
            // Other PSR writes: keep flags only.
            let value = cpu.read_reg(rn);
            cpu.cpsr = (cpu.cpsr & !0xf800_0000) | (value & 0xf800_0000);
        }
    }
    Outcome::Continue
}

/// 32-bit load/store single data item.
/// Thumb-2 load/store with a 12-bit immediate, an 8-bit indexed immediate or a
/// register offset.  The operation is the eight-bit field `hw1[11:4]`:
///
/// | hw1[11:4] | operation            | form                          |
/// |-----------|----------------------|-------------------------------|
/// | 0x80..0x85| STRB/LDRB/STRH/LDRH/STR/LDR | register offset, or 8-bit immediate when `hw2[11]` |
/// | 0x91,0x93 | LDRSB, LDRSH         | register offset / 8-bit immediate |
/// | 0x88..0x8d| the same six ops     | 12-bit immediate              |
/// | 0x99,0x9b | LDRSB, LDRSH         | 12-bit immediate              |
///
/// All of these come from Keystone, e.g. `ldr.w r0, [r5, #8]` = 0xf8d5/0x0008,
/// `ldr.w r0, [r5, r2, lsl #2]` = 0xf855/0x0022 and
/// `ldr.w r0, [r5, #4]!` = 0xf855/0x0f04.
fn load_store_thumb2<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    insn: u32,
    hw1: u16,
    hw2: u16,
    pc: u32,
) -> Outcome {
    let op = ((hw1 >> 4) & 0xff) as u32;
    let rn = (hw1 & 0xf) as usize;
    let rt = ((hw2 >> 12) & 0xf) as usize;

    // (size, signed, is_load) for the operation, and whether the operation is
    // written with the 12-bit immediate form of the encoding.
    let (size, signed, is_load, wide) = match op {
        0x80 => (1, false, false, false), // STRB
        0x81 => (1, false, true, false),  // LDRB
        0x82 => (2, false, false, false), // STRH
        0x83 => (2, false, true, false),  // LDRH
        0x84 => (4, false, false, false), // STR
        0x85 => (4, false, true, false),  // LDR
        0x91 => (1, true, true, false),   // LDRSB
        0x93 => (2, true, true, false),   // LDRSH
        0x88 => (1, false, false, true),  // STRB, imm12
        0x89 => (1, false, true, true),   // LDRB, imm12
        0x8a => (2, false, false, true),  // STRH, imm12
        0x8b => (2, false, true, true),   // LDRH, imm12
        0x8c => (4, false, false, true),  // STR, imm12
        0x8d => (4, false, true, true),   // LDR, imm12
        0x99 => (1, true, true, true),    // LDRSB, imm12
        0x9b => (2, true, true, true),    // LDRSH, imm12
        _ => return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: true }),
    };

    let base = if rn == 15 { pc.wrapping_add(4) & !3 } else { cpu.read_reg(rn) };
    // The 12-bit immediate forms use the whole low halfword as the offset; the
    // other forms use bit 11 to choose between a register offset and an 8-bit
    // pre/post-indexed immediate.
    let indexed = !wide && hw2 & 0x0800 != 0;
    if !indexed {
        // Plain offset: a 12-bit immediate, or a register with an LSL #0..#3.
        let addr = if wide {
            base.wrapping_add((hw2 & 0xfff) as u32)
        } else {
            let rm = (hw2 & 0xf) as usize;
            let amount = ((hw2 >> 4) & 0x3) as u32;
            base.wrapping_add(cpu.read_reg(rm) << amount)
        };
        return load_store_sized_thumb2(cpu, bus, size, signed, is_load, addr, rt, pc);
    }
    // Indexed: `1 P U W imm8`.  P = 0 is post-indexed, P = 1 pre-indexed.
    let pre = hw2 & 0x0400 != 0;
    let add = hw2 & 0x0200 != 0;
    let writeback = hw2 & 0x0100 != 0;
    let offset = (hw2 & 0xff) as u32;
    let offset_addr = if add {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if pre { offset_addr } else { base };
    let outcome = load_store_sized_thumb2(cpu, bus, size, signed, is_load, addr, rt, pc);
    if matches!(outcome, Outcome::Continue) && (!pre || writeback) {
        cpu.write_reg(rn, offset_addr);
    }
    outcome
}

/// The register-offset form of the 12-bit-immediate encodings in Thumb-2 also
/// permits a shifted register, which `load_store_sized_thumb2` receives already
/// applied.
#[allow(clippy::too_many_arguments)]
fn load_store_sized_thumb2<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    size: u32,
    signed: bool,
    is_load: bool,
    addr: u32,
    rt: usize,
    pc: u32,
) -> Outcome {
    if is_load {
        let raw = try_trap!(fetch(bus, addr, size, pc));
        let value = if signed {
            match size {
                1 => raw as u8 as i8 as i32 as u32,
                _ => raw as u16 as i16 as i32 as u32,
            }
        } else {
            raw
        };
        if rt == 15 {
            cpu.branch_keep_state(value);
        } else {
            cpu.write_reg(rt, value);
        }
    } else {
        let value = if rt == 15 { pc.wrapping_add(4) } else { cpu.read_reg(rt) };
        try_trap!(store(bus, addr, size, value, pc));
    }
    Outcome::Continue
}


/// Used by the runtime to describe an unimplemented encoding.
pub fn describe(insn: u32, wide: bool) -> String {
    if wide {
        format!("undefined Thumb-2 instruction {insn:#010x}")
    } else {
        format!("undefined Thumb instruction {:#06x}", insn as u16)
    }
}

/// Memory fault helper shared with the VFP decoder.
pub fn fault(error: MemoryError, address: u32, pc: u32, access: Access) -> Trap {
    Trap::Memory { error, address, pc, access }
}
