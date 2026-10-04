//! ARM (A32) instruction decoder.
//!
//! Decoding follows the ARM ARM's "top level encoding" table: the primary
//! switch is on `insn[27:25]` (`op1`), then on `insn[7:4]` (`op`) for the
//! miscellaneous group.  Every unimplemented encoding ends up as
//! [`Trap::Undefined`] with its address, so the runtime can report exactly which
//! instruction a real game binary hit instead of dying with a wrong result.

use crate::vfp;
use crate::{
    add_with_carry, decode_arm_immediate, shift_by, shift_rrx, Access, Bus, Cpu, Outcome, Trap,
    FLAG_Q,
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

/// Sign or zero extend a loaded value.
fn extend(value: u32, size: u32, signed: bool) -> u32 {
    if signed {
        match size {
            1 => value as u8 as i8 as i32 as u32,
            2 => value as u16 as i16 as i32 as u32,
            _ => value,
        }
    } else {
        match size {
            1 => value & 0xff,
            2 => value & 0xffff,
            _ => value,
        }
    }
}

/// Shifter operand for the register/immediate data-processing encodings.
fn shifter_operand<B: Bus>(cpu: &Cpu, bus: &mut B, insn: u32, pc: u32) -> Exec<(u32, bool)> {
    let _ = bus;
    let _ = pc;
    if insn & (1 << 25) != 0 {
        let imm = decode_arm_immediate(insn & 0xfff);
        let rotate = (insn >> 8) & 0xf;
        let carry = if rotate == 0 { cpu.flag(crate::FLAG_C) } else { imm & 0x8000_0000 != 0 };
        return Ok((imm, carry));
    }
    let rm = (insn & 0xf) as usize;
    let kind = (insn >> 5) & 0x3;
    let mut value = cpu.read_reg(rm);
    let mut carry = cpu.flag(crate::FLAG_C);
    if insn & (1 << 4) != 0 {
        // Register controlled shift.
        let rs = ((insn >> 8) & 0xf) as usize;
        let amount = cpu.read_reg(rs) & 0xff;
        if amount == 0 {
            return Ok((value, carry));
        }
        let (v, c) = shift_by(kind, value, amount, carry);
        value = v;
        carry = c;
    } else {
        let amount = (insn >> 7) & 0x1f;
        if amount == 0 {
            match kind {
                0 => {} // LSL #0: no shift, carry unchanged
                1 => {
                    let (v, c) = crate::shift_lsr(value, 32, carry);
                    value = v;
                    carry = c;
                }
                2 => {
                    let (v, c) = crate::shift_asr(value, 32, carry);
                    value = v;
                    carry = c;
                }
                _ => {
                    let (v, c) = shift_rrx(value, carry);
                    value = v;
                    carry = c;
                }
            }
        } else {
            let (v, c) = shift_by(kind, value, amount, carry);
            value = v;
            carry = c;
        }
    }
    Ok((value, carry))
}

pub fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let op1 = (insn >> 25) & 0x7;
    match op1 {
        0 => {
            if insn & (1 << 4) == 0 {
                data_processing(cpu, bus, insn, pc)
            } else {
                misc(cpu, bus, insn, pc)
            }
        }
        1 => data_processing(cpu, bus, insn, pc),
        // op1 == 010: load/store word or byte, *immediate* offset.  Bit 4 is
        // part of the offset, not a discriminator (`ldr r1, [pc, #0x14]` has it
        // set), which is the bug this table was fixed for.
        2 => load_store(cpu, bus, insn, pc),
        3 => {
            if insn & (1 << 4) == 0 {
                // op1 == 011, bit 4 clear: load/store word or byte, register
                // offset (`ldr r0, [r1, r2]`).
                load_store(cpu, bus, insn, pc)
            } else {
                // op1 == 011, bit 4 set: the media space.  It holds the extend
                // instructions (`sxtb r0, r1` is 0xe6af0071), REV/REV16/REVSH,
                // SSAT/USAT, SEL/PKH and the parallel add/sub family.
                media(cpu, bus, insn, pc)
            }
        }
        4 => block_transfer(cpu, bus, insn, pc),
        5 => branch(cpu, bus, insn, pc),
        6 => coprocessor_load_store(cpu, bus, insn, pc),
        _ => {
            if insn & (1 << 24) != 0 {
                // SVC: the Darwin syscall ABI is `svc #0x80` with the number in
                // r12 (negative numbers select a Mach trap).
                let imm = insn & 0x00ff_ffff;
                if imm == 0x80 {
                    let number = cpu.r[12] as i32;
                    Outcome::Trap(Trap::Syscall { number })
                } else {
                    Outcome::Trap(Trap::SupervisorCall { immediate: imm })
                }
            } else if insn & (1 << 4) != 0 {
                vfp::mcr_mrc(cpu, insn, pc, Outcome::Continue)
            } else if (insn >> 20) & 0xf == 0b0100 {
                vfp::mcrr_mrrc(cpu, insn, pc, Outcome::Continue)
            } else {
                vfp::cdp(cpu, insn, pc, Outcome::Continue)
            }
        }
    }
}

/// Instructions in the `cond = 0b1111` space: BLX immediate, PLD, barriers,
/// CPS/SETEND and the unconditional VFP/NEON encodings.
pub fn execute_unconditional<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    // BLX immediate: 1111 101H imm24
    if (insn >> 25) & 0x7 == 0b101 {
        let h = (insn >> 24) & 1;
        let offset = (((insn & 0x00ff_ffff) << 2) | (h << 1)) as i32;
        let target = pc.wrapping_add(8).wrapping_add(offset as u32);
        cpu.r[14] = pc.wrapping_add(4);
        cpu.branch_to(target);
        return Outcome::Continue;
    }
    // PLD / PLI: 1111 01x1 ... -> no architectural effect.
    if (insn >> 26) & 1 == 1 && (insn >> 24) & 1 == 1 {
        return Outcome::Continue;
    }
    // Barriers: DMB/DSB/ISB (0xf57ff04x / 0xf57ff05x / 0xf57ff06x).
    if insn & 0xffff_ff00 == 0xf57f_f000 {
        return Outcome::Continue;
    }
    // CPS / SETEND.
    if insn & 0xfff1_fe20 == 0xf100_0000 || insn & 0xffff_ffdf == 0xf101_0000 {
        return Outcome::Continue;
    }
    // Unconditional VFP (VMOV immediate, VCVT fixed-point, ...).
    let coproc = (insn >> 8) & 0xf;
    if coproc == 0b1010 || coproc == 0b1011 {
        return vfp::unconditional(cpu, insn, pc);
    }
    // NEON lives here; the Simpsons engine is fixed-function GLES 1.1 and the
    // compiler emitted scalar VFP for it, so report rather than mis-execute.
    if insn & 0x0e00_0000 == 0x0e00_0000 {
        return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: false });
    }
    let _ = bus;
    undefined(cpu, insn, pc)
}

fn undefined(cpu: &Cpu, insn: u32, pc: u32) -> Outcome {
    Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() })
}

// ---------------------------------------------------------------------------
// Data processing
// ---------------------------------------------------------------------------

/// MRS / MSR (both forms).
///
/// These share the *data-processing* opcode space (Keystone: `mrs r0, cpsr` is
/// 0xe10f0000 and `msr cpsr_f, r0` is 0xe128f000, both with bit 4 clear), so
/// they are checked before the ALU decode rather than in `misc`.
fn psr_transfer<B: Bus>(cpu: &mut Cpu, insn: u32) -> Option<Outcome> {
    // MRS Rd, <spec_reg>: cond 0001 0R00 1111 Rd 0000 0000 0000
    if insn & 0x0fbf_0fff == 0x010f_0000 {
        let spsr = insn & (1 << 22) != 0;
        let rd = ((insn >> 12) & 0xf) as usize;
        let value = if spsr { cpu.spsr } else { cpu.cpsr };
        cpu.write_reg(rd, value);
        return Some(Outcome::Continue);
    }
    // MSR <spec_reg>, Rn: cond 0001 0R10 mask 1111 0000 0000 Rn.  The `mask`
    // field (bits 19:16) is the only free nibble, so bits 11:4 must be checked
    // too -- without that, `blx r3` (bits 15:12 == 1111, bits 11:4 == 0xff) is
    // decoded as `msr cpsr, r3`, which sets the Thumb bit and never branches.
    if insn & 0x0fb0_f000 == 0x0120_f000 && insn & 0x0ff0 == 0 {
        let field_mask = (insn >> 16) & 0xf;
        let value = cpu.read_reg((insn & 0xf) as usize);
        write_psr_field(cpu, false, field_mask, value);
        return Some(Outcome::Continue);
    }
    // MSR <spec_reg>, #imm: cond 0011 0R10 mask 1111 rotate imm8
    if (insn >> 23) & 0x1f == 0b00110 && insn & 0x0fb0_f000 == 0x0320_f000 {
        let field_mask = (insn >> 16) & 0xf;
        let value = crate::decode_arm_immediate(insn & 0xfff);
        write_psr_field(cpu, false, field_mask, value);
        return Some(Outcome::Continue);
    }
    None
}

/// Apply an MSR write to the CPSR byte/flag fields named by `field_mask`.
fn write_psr_field(cpu: &mut Cpu, spsr: bool, field_mask: u32, value: u32) {
    let mut new = if spsr { cpu.spsr } else { cpu.cpsr };
    if field_mask & 0x1 != 0 {
        new = (new & !0xff) | (value & 0xff);
    }
    if field_mask & 0x2 != 0 {
        new = (new & !0xff00) | (value & 0xff00);
    }
    if field_mask & 0x4 != 0 {
        new = (new & !0xff_0000) | (value & 0xff_0000);
    }
    if field_mask & 0x8 != 0 {
        new = (new & !0xff00_0000) | (value & 0xff00_0000);
    }
    // The emulator never leaves USER/SYS mode.
    new = (new & !0x1f) | crate::MODE_SYS;
    if spsr {
        cpu.spsr = new;
    } else {
        cpu.cpsr = new;
    }
}


// ---------------------------------------------------------------------------
// Media and extend instructions (op1 == 011 with bit 4 set)
// ---------------------------------------------------------------------------

/// The extend instructions (`sxtb`, `sxth`, `uxtb`, `uxth`), verified against
/// Keystone: `sxtb r0, r1` is 0xe6af0071 (bits 23:20 = 1010), `sxth` = 0xe6bf0071
/// (1011), `uxtb` = 0xe6ef0071 (1110), `uxth` = 0xe6ff0071 (1111), all with
/// bits 7:4 = 0111 and the rotate amount in bits 11:10.
fn sign_extend<B: Bus>(cpu: &mut Cpu, insn: u32, pc: u32) -> Option<Outcome> {
    if insn & 0x0000_00f0 != 0x70 {
        return None;
    }
    let kind = (insn >> 20) & 0xf;
    let signed = match kind {
        0b1010 => true,  // SXTB
        0b1011 => true,  // SXTH
        0b1110 => false, // UXTB
        0b1111 => false, // UXTH
        _ => return None,
    };
    let halfword = matches!(kind, 0b1011 | 0b1111);
    let rd = ((insn >> 12) & 0xf) as usize;
    let rm = (insn & 0xf) as usize;
    let rotate = ((insn >> 10) & 0x3) * 8;
    let value = cpu.read_reg(rm).rotate_right(rotate);
    let size = if halfword { 16 } else { 8 };
    let result = if signed {
        let shift = 32 - size;
        ((value << shift) as i32 >> shift) as u32
    } else {
        match size {
            8 => value & 0xff,
            _ => value & 0xffff,
        }
    };
    cpu.write_reg(rd, result);
    let _ = pc;
    Some(Outcome::Continue)
}

/// `REV`/`REV16`/`REVSH` (`rev r0, r1` = 0xe6bf0f31, `rev16` = 0xe6bf0fb1,
/// `revsh` = 0xe6ff0fb1).
fn byte_reverse<B: Bus>(cpu: &mut Cpu, insn: u32, pc: u32) -> Option<Outcome> {
    if insn & 0x0000_0f00 != 0x0f00 {
        return None;
    }
    let kind = (insn >> 20) & 0xf;
    let field = (insn >> 4) & 0xf;
    let rd = ((insn >> 12) & 0xf) as usize;
    let value = cpu.read_reg((insn & 0xf) as usize);
    let result = match (kind, field) {
        (0b1011, 0b0011) => value.swap_bytes(),                       // REV
        (0b1011 | 0b1111, 0b1011) => {
            // REV16 (REVSH is the same swap, then the low half is sign extended)
            let swapped = ((value & 0x00ff_00ff) << 8) | ((value >> 8) & 0x00ff_00ff);
            if kind == 0b1111 {
                (swapped as u16 as i16 as i32) as u32
            } else {
                swapped
            }
        }
        _ => return None,
    };
    cpu.write_reg(rd, result);
    let _ = pc;
    Some(Outcome::Continue)
}

/// `SSAT`/`USAT` and their 16-bit forms.  Anchors from Keystone:
/// `ssat r0, #8, r1` = 0xe6a70011, `usat` = 0xe6e80011, `ssat16` = 0xe6a70f31,
/// `usat16` = 0xe6e80f31, `ssat r0, #8, r1, lsl #2` = 0xe6a70111.
fn saturate<B: Bus>(cpu: &mut Cpu, insn: u32, pc: u32) -> Option<Outcome> {
    let top = (insn >> 20) & 0xff;
    let unsigned = match top {
        0x6a => false, // SSAT / SSAT16
        0x6e => true,  // USAT / USAT16
        _ => return None,
    };
    let doubleword = insn & 0xf0 == 0x30;
    if !doubleword && insn & 0xf0 != 0x10 {
        return None;
    }
    let sat_imm = (insn >> 16) & 0x1f;
    let rd = ((insn >> 12) & 0xf) as usize;
    let rm = (insn & 0xf) as usize;
    let value = cpu.read_reg(rm);

    if doubleword {
        // No shift in the 16-bit forms; each half is saturated on its own.
        let low = cpu_sat_half(cpu, value as u16 as i16 as i32, sat_imm, unsigned);
        let high = cpu_sat_half(cpu, (value >> 16) as u16 as i16 as i32, sat_imm, unsigned);
        cpu.write_reg(rd, (high << 16) | (low & 0xffff));
        let _ = pc;
        return Some(Outcome::Continue);
    }
    let shift = ((insn >> 7) & 0x1f) as u32;
    let arithmetic = insn & (1 << 6) != 0;
    let operand = if arithmetic {
        ((value as i32) >> shift) as u32
    } else {
        value << shift
    };
    let result = if unsigned {
        saturate_unsigned(cpu, operand as i32, sat_imm)
    } else {
        saturate_signed(cpu, operand as i32, sat_imm + 1)
    };
    cpu.write_reg(rd, result);
    let _ = pc;
    Some(Outcome::Continue)
}

/// One halfword of SSAT16/USAT16: `value` is already sign extended.
fn cpu_sat_half(cpu: &mut Cpu, value: i32, sat_imm: u32, unsigned: bool) -> u32 {
    if unsigned {
        saturate_unsigned(cpu, value, sat_imm)
    } else {
        saturate_signed(cpu, value, sat_imm + 1)
    }
}

/// `SignedSat(value, bits)`: the range [-2^(bits-1), 2^(bits-1) - 1].
fn saturate_signed(cpu: &mut Cpu, value: i32, bits: u32) -> u32 {
    if bits >= 32 {
        return value as u32;
    }
    let max = (1i32 << (bits - 1)) - 1;
    let min = -max - 1;
    if value > max {
        cpu.set_flag(FLAG_Q, true);
        max as u32
    } else if value < min {
        cpu.set_flag(FLAG_Q, true);
        min as u32
    } else {
        value as u32
    }
}

/// `UnsignedSat(value, bits)`: the range [0, 2^bits - 1].
fn saturate_unsigned(cpu: &mut Cpu, value: i32, bits: u32) -> u32 {
    let max = if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 };
    if value < 0 {
        cpu.set_flag(FLAG_Q, true);
        0
    } else if value as u32 > max {
        cpu.set_flag(FLAG_Q, true);
        max
    } else {
        value as u32
    }
}

/// `SEL Rd, Rn, Rm` (`sel r0, r1, r2` = 0xe6810fb2): per-byte select on GE.
fn select_bytes<B: Bus>(cpu: &mut Cpu, insn: u32, pc: u32) -> Option<Outcome> {
    if insn & 0x0ff0_0ff0 != 0x0680_0fb0 {
        return None;
    }
    let rn = ((insn >> 16) & 0xf) as usize;
    let rd = ((insn >> 12) & 0xf) as usize;
    let rm = (insn & 0xf) as usize;
    let (a, b) = (cpu.read_reg(rn), cpu.read_reg(rm));
    let ge = (cpu.cpsr >> 16) & 0xf;
    let mut result = 0u32;
    for byte in 0..4 {
        let shift = byte * 8;
        let pick_a = ge & (1 << byte) != 0;
        let value = if pick_a { (a >> shift) & 0xff } else { (b >> shift) & 0xff };
        result |= value << shift;
    }
    cpu.write_reg(rd, result);
    let _ = pc;
    Some(Outcome::Continue)
}

/// The media space (`op1 == 011`, bit 4 set).  Only the instructions an
/// ARMv7 iOS binary actually uses are modelled; anything else is reported as
/// undefined rather than mis-executed.
fn media<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    if let Some(outcome) = sign_extend::<B>(cpu, insn, pc) {
        return outcome;
    }
    if let Some(outcome) = byte_reverse::<B>(cpu, insn, pc) {
        return outcome;
    }
    if let Some(outcome) = saturate::<B>(cpu, insn, pc) {
        return outcome;
    }
    if let Some(outcome) = select_bytes::<B>(cpu, insn, pc) {
        return outcome;
    }
    let _ = bus;
    undefined(cpu, insn, pc)
}

/// The 16-bit multiplies: `smulbb` (0xe1600281), `smlabb` (0xe1003281),
/// `smlalbb` (0xe1410382), `smulwb` (0xe12002a1) and `smlawb` (0xe1203281).
/// They share the register form of the data-processing space.
fn multiply_halfword(cpu: &mut Cpu, insn: u32) -> Option<Outcome> {
    let top = (insn >> 20) & 0xff;
    let x = (insn >> 5) & 1;
    let y = (insn >> 6) & 1;
    let half = |value: u32, high: u32| -> i32 {
        if high == 1 { (value >> 16) as u16 as i32 } else { value as u16 as i32 }
    };
    let rd = ((insn >> 16) & 0xf) as usize;
    let ra = ((insn >> 12) & 0xf) as usize;
    let rs = ((insn >> 8) & 0xf) as usize;
    let rm = (insn & 0xf) as usize;
    let (rm_value, rs_value) = (cpu.read_reg(rm), cpu.read_reg(rs));

    if !matches!((insn >> 4) & 0xf, 0b1000 | 0b1010 | 0b1100 | 0b1110) {
        return None;
    }
    match top {
        // SMULxy / SMLAxy: Rd = (Rm<x> * Rs<y>) [+ Ra]
        0x16 | 0x10 => {
            let product = half(rm_value, x).wrapping_mul(half(rs_value, y));
            let result = if top == 0x10 {
                let added = product.wrapping_add(cpu.read_reg(ra) as i32);
                // An overflowing addition sets Q.
                if (product as i64 + cpu.read_reg(ra) as i32 as i64) != added as i64 {
                    cpu.set_flag(crate::FLAG_Q, true);
                }
                added
            } else {
                product
            };
            cpu.write_reg(rd, result as u32);
            Some(Outcome::Continue)
        }
        // SMLALxy: accumulates into the RdHi:RdLo pair
        0x14 => {
            let rdhi = ((insn >> 16) & 0xf) as usize;
            let rdlo = ((insn >> 12) & 0xf) as usize;
            let product = half(rm_value, x) as i64 * half(rs_value, y) as i64;
            let accumulator = ((cpu.read_reg(rdhi) as u64) << 32) | cpu.read_reg(rdlo) as u64;
            let result = (product as u64).wrapping_add(accumulator);
            cpu.write_reg(rdlo, result as u32);
            cpu.write_reg(rdhi, (result >> 32) as u32);
            Some(Outcome::Continue)
        }
        // SMULWy / SMLAWy: the top half of Rm times the half of Rs selected by
        // bit 6.  Bit 5 is 1 for the plain form (SMULW, 0xe12002a1) and 0 for
        // the accumulating one (SMLAW, 0xe1203281).
        0x12 => {
            let product = half(rm_value, 1).wrapping_mul(half(rs_value, y));
            let result = if insn & 0x20 == 0 {
                product.wrapping_add(cpu.read_reg(ra) as i32)
            } else {
                product
            };
            cpu.write_reg(rd, result as u32);
            Some(Outcome::Continue)
        }
        _ => None,
    }
}

fn data_processing<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    if let Some(outcome) = psr_transfer::<B>(cpu, insn) {
        return outcome;
    }
    let opcode = (insn >> 21) & 0xf;
    let set_flags = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xf) as usize;
    let rd = ((insn >> 12) & 0xf) as usize;

    // Halfword multiplies (SMLAxy/SMULxy/SMLALxy/SMULWy/SMLAWy) share the
    // *register* form of this space (bit 25 clear, Keystone: `smlabb` is
    // 0xe1003281) and are recognised by their top byte.
    if insn & (1 << 25) == 0 {
        if let Some(outcome) = multiply_halfword(cpu, insn) {
            return outcome;
        }
    }

    // ARMv7 hides MOVW/MOVT in this space: opcode 8 with S == 0 is MOVW and
    // opcode 10 with S == 0 is MOVT, both taking an extra immediate nibble from
    // bits 19:16.  (Without this, `movw r0, #0x8074` decodes as `and r0, r8,
    // #0x74`, which is how the GL tests first went wrong.)  They only exist in
    // the immediate form, so bit 25 must be set -- otherwise every SMLAxy
    // (0xe1003281, opcode 8 with S clear) would be read as a MOVW.
    if insn & (1 << 25) != 0 && !set_flags && (opcode == 8 || opcode == 10) {
        let imm4 = (insn >> 16) & 0xf;
        let imm12 = insn & 0xfff;
        // MSR (immediate) shares MOVT's opcode: it has bits 19:16 == 0b10R1 and
        // a zero high nibble in the low half.  It only appears in kernel or
        // startup code, so it is logged rather than modelled.
        let is_msr = opcode == 10 && matches!(imm4, 0b1000 | 0b1001) && (insn >> 8) & 0xf == 0 && rd != 0;
        if is_msr {
            return Outcome::Continue;
        }
        let imm16 = (imm4 << 12) | imm12;
        let value = if opcode == 8 {
            imm16
        } else {
            // MOVT: the top half of Rd is replaced, the bottom half kept.
            (cpu.read_reg(rd) & 0xffff) | (imm16 << 16)
        };
        if rd == 15 {
            cpu.branch_keep_state(value);
        } else {
            cpu.write_reg(rd, value);
        }
        return Outcome::Continue;
    }

    let (op2, shifter_carry) = try_trap!(shifter_operand(cpu, bus, insn, pc));
    let operand1 = cpu.read_reg(rn);

    let (result, carry, overflow) = match opcode {        0 | 8 => (operand1 & op2, shifter_carry, false),           // AND / TST
        1 | 9 => (operand1 ^ op2, shifter_carry, false),           // EOR / TEQ
        2 | 10 => {                                               // SUB / CMP
            let (r, c, v) = crate::sub_with_carry(operand1, op2, true);
            (r, c, v)
        }
        3 => {                                                    // RSB
            let (r, c, v) = crate::sub_with_carry(op2, operand1, true);
            (r, c, v)
        }
        4 | 11 => {                                               // ADD / CMN
            let (r, c, v) = add_with_carry(operand1, op2, false);
            (r, c, v)
        }
        5 => {                                                    // ADC
            let (r, c, v) = add_with_carry(operand1, op2, cpu.flag(crate::FLAG_C));
            (r, c, v)
        }
        6 => {                                                    // SBC
            let (r, c, v) = crate::sub_with_carry(operand1, op2, cpu.flag(crate::FLAG_C));
            (r, c, v)
        }
        7 => {                                                    // RSC
            let (r, c, v) = crate::sub_with_carry(op2, operand1, cpu.flag(crate::FLAG_C));
            (r, c, v)
        }
        12 => (operand1 | op2, shifter_carry, false),              // ORR
        13 => (op2, shifter_carry, false),                         // MOV
        14 => (operand1 & !op2, shifter_carry, false),             // BIC
        _ => (!op2, shifter_carry, false),                         // MVN
    };

    // Test instructions only update flags.
    let is_test = matches!(opcode, 8..=11);
    if is_test {
        if set_flags {
            cpu.set_nzcv(result, carry, overflow);
        }
    } else if rd == 15 {
        if set_flags {
            // Exception return in a real OS; the SPSR holds the target mode's
            // flags, which the HLE layer keeps in sync.
            cpu.cpsr = (cpu.cpsr & !0xf800_0000) | (cpu.spsr & 0xf800_0000);
        }
        cpu.branch_keep_state(result);
    } else {
        if set_flags {
            cpu.set_nzcv(result, carry, overflow);
        }
        cpu.write_reg(rd, result);
    }
    Outcome::Continue
}

// ---------------------------------------------------------------------------
// Miscellaneous group: multiplies, PSR transfers, BX/CLZ/REV/SSAT, ...
// ---------------------------------------------------------------------------

fn misc<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let op = (insn >> 4) & 0xf;
    let bits_27_23 = (insn >> 23) & 0x1f;

    // --- Extra load/store: LDRH/STRH, LDRSB, LDRD/STRD, LDRSH ---------------
    // Encoding: `... Rn Rd ... 1 S H 1`.  The space shares op1 == 000 with the
    // multiplies and BX, so it is discriminated by bits 7:4 (Keystone-verified
    // against `ldrh`/`ldrsb`/`ldrd`/`ldrsh`).
    if op == 0b1011 || op == 0b1101 || op == 0b1111 {
        return extra_load_store(cpu, bus, insn, pc);
    }

    // --- Multiply / multiply-accumulate (32 bit) -------------------------
    // `bits_27_23 == 0` exactly: bit 0 of that field distinguishes UMULL/SMULL
    // (00001), which would otherwise be decoded as a 32-bit MUL.
    if op == 0b1001 && bits_27_23 == 0 && insn & (1 << 24) == 0 {
        let accumulate = insn & (1 << 21) != 0;
        let set_flags = insn & (1 << 20) != 0;
        let rd = ((insn >> 16) & 0xf) as usize;
        let rn = ((insn >> 12) & 0xf) as usize;
        let rs = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let mut result = cpu.read_reg(rm).wrapping_mul(cpu.read_reg(rs));
        if accumulate {
            result = result.wrapping_add(cpu.read_reg(rn));
        }
        cpu.write_reg(rd, result);
        if set_flags {
            cpu.set_nz(result);
            // C is unpredictable for MUL/MLA in older revisions; ARMv7 leaves
            // it unchanged, which is what we do.
        }
        return Outcome::Continue;
    }

    // --- Swap / swap byte -------------------------------------------------
    // `swp r0, r1, [r2]` is 0xe1020091: bits 7:4 == 1001, bit 24 set, bit 22
    // selecting byte, and bits 11:8 zero (which separates it from QADD, whose
    // bits 7:4 are 0101).
    if insn & 0x0fb0_0f90 == 0x0100_0090 && insn & 0xf00 == 0 {
        let byte = insn & (1 << 22) != 0;
        let rn = ((insn >> 16) & 0xf) as usize;
        let rd = ((insn >> 12) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let address = cpu.read_reg(rn);
        let value = cpu.read_reg(rm);
        let size = if byte { 1 } else { 4 };
        // Read then write: the classic atomic swap.
        let old = try_trap!(fetch(bus, address, size, pc));
        try_trap!(store(bus, address, size, value, pc));
        cpu.write_reg(rd, old);
        cpu.exclusive = None;
        return Outcome::Continue;
    }

    // --- Long multiply (UMULL/UMLAL/SMULL/SMLAL) --------------------------
    // Encoding: cond 00001UAS RdHi RdLo Rs 1001 Rm
    if (insn >> 23) & 0x1f == 0b00001 && op == 0b1001 {
        let signed = insn & (1 << 22) != 0;
        let accumulate = insn & (1 << 21) != 0;
        let set_flags = insn & (1 << 20) != 0;
        let rdhi = ((insn >> 16) & 0xf) as usize;
        let rdlo = ((insn >> 12) & 0xf) as usize;
        let rs = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        if rdhi == 15 || rdlo == 15 {
            return undefined(cpu, insn, pc);
        }
        let a = cpu.read_reg(rm);
        let b = cpu.read_reg(rs);
        let mut result: u64 = if signed {
            ((a as i32 as i64) * (b as i32 as i64)) as u64
        } else {
            (a as u64) * (b as u64)
        };
        if accumulate {
            let acc = ((cpu.read_reg(rdhi) as u64) << 32) | cpu.read_reg(rdlo) as u64;
            result = result.wrapping_add(acc);
        }
        cpu.write_reg(rdlo, result as u32);
        cpu.write_reg(rdhi, (result >> 32) as u32);
        if set_flags {
            cpu.set_nz64(result);
        }
        return Outcome::Continue;
    }

    // --- BX / BLX (register) ---------------------------------------------
    if insn & 0x0fff_fff0 == 0x012f_ff10 {
        let rm = (insn & 0xf) as usize;
        cpu.branch_to(cpu.read_reg(rm));
        return Outcome::Continue;
    }
    if insn & 0x0fff_fff0 == 0x012f_ff30 {
        let rm = (insn & 0xf) as usize;
        cpu.r[14] = pc.wrapping_add(4);
        cpu.branch_to(cpu.read_reg(rm));
        return Outcome::Continue;
    }

    // --- BKPT -------------------------------------------------------------
    if insn & 0x0ff0_00f0 == 0x0120_0070 {
        let imm = ((insn >> 4) & 0xfff0) | (insn & 0xf);
        return Outcome::Trap(Trap::Breakpoint { address: pc, imm });
    }

    // --- CLZ --------------------------------------------------------------
    if insn & 0x0fff_0ff0 == 0x016f_0f10 {
        let rd = ((insn >> 12) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        cpu.write_reg(rd, cpu.read_reg(rm).leading_zeros());
        return Outcome::Continue;
    }

    // --- Saturating add/sub (QADD/QSUB/QDADD/QDSUB) -----------------------
    if insn & 0x0f80_00f0 == 0x0100_0050 {
        let op2 = ((insn >> 21) & 0x3) as usize;
        let rn = ((insn >> 16) & 0xf) as usize;
        let rd = ((insn >> 12) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let a = cpu.read_reg(rm) as i32;
        let b = cpu.read_reg(rn) as i32;
        let wide = |value: i64| -> i32 { value.clamp(i32::MIN as i64, i32::MAX as i64) as i32 };
        let (result, saturated) = match op2 {
            // QADD/QSUB saturate the 32-bit result and set Q on saturation.
            0 => {
                let d = a as i64 + b as i64;
                (wide(d), d != wide(d) as i64)
            }
            1 => {
                let d = a as i64 - b as i64;
                (wide(d), d != wide(d) as i64)
            }
            // QDADD: saturate(b + b) then add, saturating as well.
            2 => {
                let doubled = wide(b as i64 * 2);
                if (b as i64 * 2) != doubled as i64 {
                    cpu.set_flag(crate::FLAG_Q, true);
                }
                let d = a as i64 + doubled as i64;
                (wide(d), (doubled as i64 * 2) != (doubled as i64 * 2) || d != wide(d) as i64)
            }
            // QDSUB: a - saturate(b + b).
            _ => {
                let doubled = wide(b as i64 * 2);
                if (b as i64 * 2) != doubled as i64 {
                    cpu.set_flag(crate::FLAG_Q, true);
                }
                let d = a as i64 - doubled as i64;
                (wide(d), d != wide(d) as i64)
            }
        };
        cpu.write_reg(rd, result as u32);
        if saturated {
            cpu.set_flag(crate::FLAG_Q, true);
        }
        return Outcome::Continue;
    }

    // --- Extend: SXTB/SXTH/UXTB/UXTH --------------------------------------
    // Encoding: 0110 101A 1111 Rd rotate 0000 0111 Rm with A selecting
    // signed (0) or unsigned (1) and bits 27:20 distinguishing byte/halfword.
    let ext_kind = (insn >> 20) & 0xff;
    if matches!(ext_kind, 0x6a | 0x6b | 0x6e | 0x6f) && insn & 0x0f0f_00f0 == 0x060f_0070 {
        let rd = ((insn >> 12) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let rotate = ((insn >> 10) & 0x3) * 8;
        let value = cpu.read_reg(rm).rotate_right(rotate);
        let signed = matches!(ext_kind, 0x6a | 0x6b);
        let half = matches!(ext_kind, 0x6b | 0x6f);
        let result = match (signed, half) {
            (true, false) => (value as u8 as i8) as i32 as u32,
            (true, true) => (value as u16 as i16) as i32 as u32,
            (false, false) => value & 0xff,
            (false, true) => value & 0xffff,
        };
        cpu.write_reg(rd, result);
        return Outcome::Continue;
    }
    // --- Byte reversal: REV / REV16 / REVSH -------------------------------
    let rev_kind = (insn >> 20) & 0xff;
    if matches!(rev_kind, 0x6b | 0x6f) && insn & 0x0fbf_0ff0 == 0x06bf_0f30 {
        let rd = ((insn >> 12) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let value = cpu.read_reg(rm);
        let halfword = insn & (1 << 6) != 0;
        let result = match (halfword, rev_kind) {
            // REV: swap all four bytes.
            (false, _) => value.swap_bytes(),
            // REV16: swap bytes within each halfword.
            (true, 0x6b) => {
                ((value & 0x00ff_00ff).swap_bytes()) | (value & 0xff00_ff00).swap_bytes()
            }
            // REVSH: swap the low halfword and sign extend.
            (true, _) => {
                let low = ((value & 0xff) << 8) | ((value >> 8) & 0xff);
                (low as u16 as i16) as i32 as u32
            }
        };
        cpu.write_reg(rd, result);
        return Outcome::Continue;
    }

    // --- Saturate: SSAT / USAT --------------------------------------------
    // SSAT: 0110 101 sat_imm Rd imm5 sh 01 Rm  (bits 27:21 == 0b0110101)
    // USAT: 0110 111 sat_imm Rd imm5 sh 01 Rm  (bits 27:21 == 0b0110111)
    let sat_kind = (insn >> 21) & 0x7f;
    if (sat_kind == 0b0110101 || sat_kind == 0b0110111) && insn & 0x30 == 0x10 {
        let unsigned = sat_kind == 0b0110111;
        let sat_imm = (insn >> 16) & 0x1f;
        let rd = ((insn >> 12) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let imm5 = (insn >> 7) & 0x1f;
        let value = if insn & (1 << 6) != 0 {
            crate::shift_asr(cpu.read_reg(rm), if imm5 == 0 { 32 } else { imm5 }, false).0
        } else {
            cpu.read_reg(rm) << imm5
        } as i32 as i64;
        let (clamped, saturated) = if unsigned {
            let max = if sat_imm >= 32 { i64::from(u32::MAX) } else { (1i64 << sat_imm) - 1 };
            (value.clamp(0, max), value < 0 || value > max)
        } else {
            let max = if sat_imm == 0 { 0 } else { (1i64 << (sat_imm - 1)) - 1 };
            let min = if sat_imm == 0 { 0 } else { -(1i64 << (sat_imm - 1)) };
            (value.clamp(min, max), value < min || value > max)
        };
        cpu.write_reg(rd, clamped as u32);
        if saturated {
            cpu.set_flag(crate::FLAG_Q, true);
        }
        return Outcome::Continue;
    }

    let _ = bus;
    undefined(cpu, insn, pc)
}

// ---------------------------------------------------------------------------
// Load / store
// ---------------------------------------------------------------------------

fn load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let is_load = insn & (1 << 20) != 0;
    let byte = insn & (1 << 22) != 0;
    let add = insn & (1 << 23) != 0;
    let pre_index = insn & (1 << 24) != 0;
    let writeback = insn & (1 << 21) != 0;
    let rn = ((insn >> 16) & 0xf) as usize;
    let rd = ((insn >> 12) & 0xf) as usize;

    // Offset: immediate (rotated) or scaled register.  Bit 25 is the "I" bit
    // and is *clear* for the immediate form (`ldr r3, [pc, #0xc0]` has bit 25
    // zero), which is also why the op1 field selects the form before we get
    // here: 010 immediate, 011 register.
    let (offset, offset_carry) = if insn & (1 << 25) == 0 {
        (decode_arm_immediate(insn & 0xfff), cpu.flag(crate::FLAG_C))
    } else {
        let rm = (insn & 0xf) as usize;
        let amount = (insn >> 7) & 0x1f;
        let value = cpu.read_reg(rm);
        let (v, c) = if amount == 0 {
            (value, cpu.flag(crate::FLAG_C))
        } else {
            crate::shift_by((insn >> 5) & 0x3, value, amount, cpu.flag(crate::FLAG_C))
        };
        (v, c)
    };

    let base = cpu.read_reg(rn);
    let offset_addr = if add { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
    let address = if pre_index { offset_addr } else { base };
    let size = if byte { 1 } else { 4 };

    let mut value = None;
    if is_load {
        let loaded = try_trap!(fetch(bus, address, size, pc));
        value = Some(loaded);
    } else {
        let value = cpu.read_reg(rd);
        try_trap!(store(bus, address, size, value, pc));
    }

    if !pre_index || writeback {
        cpu.write_reg(rn, offset_addr);
    }
    if let Some(loaded) = value {
        if rd == 15 {
            cpu.branch_keep_state(loaded);
        } else {
            cpu.write_reg(rd, loaded);
        }
    }
    let _ = offset_carry;
    Outcome::Continue
}

fn extra_load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    // Encoding: `cond 000 P U I W L Rn Rd imm4 1SH1 imm4`.  Keystone confirms
    // I == 1 selects the immediate offset in this space, and that the L bit is
    // *inverted* for the doubleword forms (LDRD has L = 0, STRD has L = 1).
    let pre_index = insn & (1 << 24) != 0;
    let add = insn & (1 << 23) != 0;
    let immediate = insn & (1 << 22) != 0;
    let writeback = insn & (1 << 21) != 0;
    let l_bit = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xf) as usize;
    let rd = ((insn >> 12) & 0xf) as usize;
    let extra = (insn >> 4) & 0xf;

    let offset = if immediate {
        ((insn >> 4) & 0xf0) | (insn & 0xf)
    } else {
        cpu.read_reg((insn & 0xf) as usize)
    };
    let base = cpu.read_reg(rn);
    let offset_addr = if add { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
    let address = if pre_index { offset_addr } else { base };

    match extra {
        // LDRH / STRH
        0b1011 => {
            if l_bit {
                let value = try_trap!(fetch(bus, address, 2, pc));
                cpu.write_reg(rd, value & 0xffff);
            } else {
                let value = cpu.read_reg(rd);
                try_trap!(store(bus, address, 2, value, pc));
            }
        }
        // LDRSB (L = 1) or LDRD (L = 0, register pair, Rd must be even)
        0b1101 => {
            if l_bit {
                let value = try_trap!(fetch(bus, address, 1, pc));
                cpu.write_reg(rd, extend(value, 1, true));
            } else {
                if rd == 15 || rd & 1 != 0 {
                    return undefined(cpu, insn, pc);
                }
                let low = try_trap!(fetch(bus, address, 4, pc));
                let high = try_trap!(fetch(bus, address.wrapping_add(4), 4, pc));
                cpu.write_reg(rd, low);
                cpu.write_reg(rd + 1, high);
            }
        }
        // LDRSH (L = 1) or STRD (L = 0)
        _ => {
            if l_bit {
                let value = try_trap!(fetch(bus, address, 2, pc));
                cpu.write_reg(rd, extend(value, 2, true));
            } else {
                if rd == 15 || rd & 1 != 0 || writeback {
                    return undefined(cpu, insn, pc);
                }
                let low = cpu.read_reg(rd);
                let high = cpu.read_reg(rd + 1);
                try_trap!(store(bus, address, 4, low, pc));
                try_trap!(store(bus, address.wrapping_add(4), 4, high, pc));
            }
        }
    }

    if !pre_index || writeback {
        cpu.write_reg(rn, offset_addr);
    }
    Outcome::Continue
}

fn block_transfer<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let pre_index = insn & (1 << 24) != 0;
    let add = insn & (1 << 23) != 0;
    let psr_or_user = insn & (1 << 22) != 0;
    let writeback = insn & (1 << 21) != 0;
    let is_load = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xf) as usize;
    let list = insn & 0xffff;
    let count = list.count_ones();

    if count == 0 {
        return undefined(cpu, insn, pc);
    }

    let base = cpu.read_reg(rn);
    // Registers are always transferred in increasing register order, from the
    // lowest address to the highest; only the starting address differs per
    // addressing mode (IA/IB/DA/DB).
    let ascending = add;
    let mut addr = if ascending {
        if pre_index { base.wrapping_add(4) } else { base }
    } else if pre_index {
        base.wrapping_sub(count * 4)
    } else {
        base.wrapping_sub((count - 1) * 4)
    };
    let mut addresses = Vec::with_capacity(count as usize);
    for i in 0..16 {
        if list & (1 << i) == 0 {
            continue;
        }
        addresses.push((i, addr));
        addr = if ascending { addr.wrapping_add(4) } else { addr.wrapping_sub(4) };
    }

    if is_load {
        let mut loaded_pc: Option<u32> = None;
        for (reg, addr) in &addresses {
            let value = try_trap!(fetch(bus, *addr, 4, pc));
            if *reg == 15 {
                loaded_pc = Some(value);
            } else {
                cpu.write_reg(*reg, value);
            }
        }
        if writeback {
            let new_base = if add { base.wrapping_add(count * 4) } else { base.wrapping_sub(count * 4) };
            cpu.write_reg(rn, new_base);
        }
        if let Some(target) = loaded_pc {
            cpu.branch_keep_state(target);
        }
    } else {
        let mut regs: Vec<(usize, u32)> = Vec::with_capacity(count as usize);
        for (reg, _) in &addresses {
            regs.push((*reg, cpu.read_reg(*reg)));
        }
        for ((_, addr), (_, value)) in addresses.iter().zip(regs.iter()) {
            try_trap!(store(bus, *addr, 4, *value, pc));
        }
        if writeback {
            let new_base = if add { base.wrapping_add(count * 4) } else { base.wrapping_sub(count * 4) };
            cpu.write_reg(rn, new_base);
        }
    }
    let _ = psr_or_user; // S bit: only meaningful for exception return
    Outcome::Continue
}

fn branch<B: Bus>(cpu: &mut Cpu, _bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let _ = insn;
    let link = insn & (1 << 24) != 0;
    let offset = ((insn & 0x00ff_ffff) << 2) as i32;
    let offset = (offset << 6) >> 6; // sign extend 26 bits
    let target = pc.wrapping_add(8).wrapping_add(offset as u32);
    if link {
        cpu.r[14] = pc.wrapping_add(4);
    }
    cpu.branch_keep_state(target);
    Outcome::Continue
}

fn coprocessor_load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    let coproc = (insn >> 8) & 0xf;
    if coproc == 0b1010 || coproc == 0b1011 {
        return vfp::ldc_stc(cpu, bus, insn, pc, Outcome::Continue);
    }
    let _ = bus;
    undefined(cpu, insn, pc)
}

/// Reported when the guest executes an instruction the interpreter does not
/// implement; the runtime turns this into a diagnostic with the faulting PC.
pub fn describe_undefined(insn: u32, thumb: bool) -> String {
    if thumb {
        format!("undefined Thumb instruction {insn:#010x}")
    } else {
        let cond = insn >> 28;
        format!(
            "undefined ARM instruction {insn:#010x} (cond {} op1 {:03b})",
            Cpu::condition_name(cond),
            (insn >> 25) & 0x7
        )
    }
}

/// Small helper used by the VFP module to report a memory fault.
pub fn memory_trap(error: MemoryError, address: u32, pc: u32, access: Access) -> Trap {
    Trap::Memory { error, address, pc, access }
}
