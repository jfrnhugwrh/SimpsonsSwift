//! VFPv3 (floating point) for ARMv7.
//!
//! The Simpsons engine is a C++ codebase full of `float` maths
//! (`VectorSignedToFloat`, `sinf`, matrix code — see the reference dump), so the
//! guest executes VFP instructions constantly.  Registers are kept as `f64`
//! bit patterns; single precision operations round through `f32` exactly like
//! the hardware's single-precision datapath.
//!
//! Encodings were derived from real assembler output: the register fields are
//! `Vd` at bits 15:12, `Vn` at bits 19:16 and `Vm` at bits 3:0, with the high
//! register bits `D`/`N`/`M` at bits 22/7/5, and bit 8 selecting single (`sz=0`)
//! versus double (`sz=1`) precision.
//!
//! Thumb-2 encodes VFP with the very same 32-bit words as ARM mode (the halfword
//! order only differs on the bus), so [`execute_thumb`] simply funnels into the
//! ARM decoders.

use crate::{Access, Bus, Cpu, Outcome, Trap};

/// Floating point status/control register bits.
pub const FPSCR_N: u32 = 1 << 31;
pub const FPSCR_Z: u32 = 1 << 30;
pub const FPSCR_C: u32 = 1 << 29;
pub const FPSCR_V: u32 = 1 << 28;
pub const FPSCR_QC: u32 = 1 << 27;
pub const FPSCR_IOC: u32 = 1 << 0; // invalid operation
pub const FPSCR_DZC: u32 = 1 << 1; // divide by zero
pub const FPSCR_OFC: u32 = 1 << 2; // overflow
pub const FPSCR_UFC: u32 = 1 << 3; // underflow
pub const FPSCR_IXC: u32 = 1 << 4; // inexact
pub const FPSCR_IDC: u32 = 1 << 7; // input denormal
pub const FPSCR_RMODE_MASK: u32 = 3 << 22;
pub const FPSCR_RMODE_NEAREST: u32 = 0 << 22;
pub const FPSCR_RMODE_PLUS_INF: u32 = 1 << 22;
pub const FPSCR_RMODE_MINUS_INF: u32 = 2 << 22;
pub const FPSCR_RMODE_ZERO: u32 = 3 << 22;
/// The bits a guest can observe through `vmrs`.
pub const FPSCR_FLAG_MASK: u32 =
    FPSCR_N | FPSCR_Z | FPSCR_C | FPSCR_V | FPSCR_QC | FPSCR_IOC | FPSCR_DZC | FPSCR_OFC | FPSCR_UFC | FPSCR_IXC | FPSCR_IDC;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vfp {
    /// d0-d31, stored as raw IEEE-754 double bit patterns so that both the
    /// single and the double view of a register are bit exact.
    pub d: [u64; 32],
    pub fpscr: u32,
    /// Number of VFP instructions executed (used by `--stats`).
    pub instructions: u64,
}

impl Default for Vfp {
    fn default() -> Self {
        Vfp { d: [0; 32], fpscr: 0, instructions: 0 }
    }
}

impl Vfp {
    #[inline]
    pub fn s(&self, reg: usize) -> f32 {
        let word = self.d[reg / 2];
        if reg % 2 == 0 {
            f32::from_bits(word as u32)
        } else {
            f32::from_bits((word >> 32) as u32)
        }
    }

    #[inline]
    pub fn set_s(&mut self, reg: usize, value: f32) {
        let bits = value.to_bits() as u64;
        if reg % 2 == 0 {
            self.d[reg / 2] = (self.d[reg / 2] & 0xffff_ffff_0000_0000) | bits;
        } else {
            self.d[reg / 2] = (self.d[reg / 2] & 0x0000_0000_ffff_ffff) | (bits << 32);
        }
    }

    #[inline]
    pub fn s_bits(&self, reg: usize) -> u32 {
        let word = self.d[reg / 2];
        if reg % 2 == 0 {
            word as u32
        } else {
            (word >> 32) as u32
        }
    }

    #[inline]
    pub fn set_s_bits(&mut self, reg: usize, bits: u32) {
        if reg % 2 == 0 {
            self.d[reg / 2] = (self.d[reg / 2] & 0xffff_ffff_0000_0000) | bits as u64;
        } else {
            self.d[reg / 2] = (self.d[reg / 2] & 0x0000_0000_ffff_ffff) | ((bits as u64) << 32);
        }
    }

    #[inline]
    pub fn d(&self, reg: usize) -> f64 {
        f64::from_bits(self.d[reg])
    }

    #[inline]
    pub fn set_d(&mut self, reg: usize, value: f64) {
        self.d[reg] = value.to_bits();
    }

    pub fn set_flag(&mut self, mask: u32, on: bool) {
        if on {
            self.fpscr |= mask;
        } else {
            self.fpscr &= !mask;
        }
    }

    pub fn flag(&self, mask: u32) -> bool {
        self.fpscr & mask != 0
    }

    /// Record the IEEE comparison result the way VFP does.
    fn compare(&mut self, a: f64, b: f64) {
        let (flags, invalid) = if a.is_nan() || b.is_nan() {
            (FPSCR_C | FPSCR_V, true)
        } else if a == b {
            (FPSCR_Z | FPSCR_C, false)
        } else if a < b {
            (FPSCR_N, false)
        } else {
            (FPSCR_C, false)
        };
        let keep = !(FPSCR_N | FPSCR_Z | FPSCR_C | FPSCR_V);
        self.fpscr = (self.fpscr & keep) | flags;
        if invalid {
            self.fpscr |= FPSCR_IOC;
        }
    }
}

/// Zero register for the 2-operand `VMOV`-style forms.
#[inline]
fn reg_num(field: u32, high: u32) -> usize {
    ((field << 1) | high) as usize
}

fn is_double(insn: u32) -> bool {
    insn & (1 << 8) != 0
}

fn d_field(insn: u32) -> u32 {
    (insn >> 22) & 1
}

/// Read the operands the way the instruction class requires.
fn read_vd(cpu: &Cpu, insn: u32) -> f64 {
    if is_double(insn) {
        cpu.vfp.d(((insn >> 12) & 0xf) as usize | ((d_field(insn) as usize) << 4))
    } else {
        cpu.vfp.s(reg_num((insn >> 12) & 0xf, d_field(insn))) as f64
    }
}

fn write_vd(cpu: &mut Cpu, insn: u32, value: f64) {
    if is_double(insn) {
        let reg = ((insn >> 12) & 0xf) as usize | ((d_field(insn) as usize) << 4);
        cpu.vfp.set_d(reg, value);
    } else {
        let reg = reg_num((insn >> 12) & 0xf, d_field(insn));
        cpu.vfp.set_s(reg, value as f32);
    }
}

fn read_vn(cpu: &Cpu, insn: u32) -> f64 {
    let high = (insn >> 7) & 1;
    if is_double(insn) {
        cpu.vfp.d(((insn >> 16) & 0xf) as usize | ((high as usize) << 4))
    } else {
        cpu.vfp.s(reg_num((insn >> 16) & 0xf, high)) as f64
    }
}

fn read_vm(cpu: &Cpu, insn: u32) -> f64 {
    let high = (insn >> 5) & 1;
    if is_double(insn) {
        cpu.vfp.d((insn & 0xf) as usize | ((high as usize) << 4))
    } else {
        cpu.vfp.s(reg_num(insn & 0xf, high)) as f64
    }
}

fn single_result(value: f64) -> f64 {
    value as f32 as f64
}

/// The double-precision register number named by the Vd field (used by the
/// VCVT between single and double, where the destination is a double register
/// when `sz == 0`).
fn vd_index(insn: u32) -> usize {
    (((insn >> 12) & 0xf) as usize) | ((d_field(insn) as usize) << 4)
}

/// The single register named by the Vd field: a single register number uses
/// the same `(field << 1) | extension` naming as every other VFP operand.
fn vd_single(insn: u32) -> usize {
    reg_num((insn >> 12) & 0xf, d_field(insn))
}

/// Raw bits of the single register named by the Vm field.
fn vm_single_bits(cpu: &Cpu, insn: u32) -> u32 {
    let reg = reg_num(insn & 0xf, (insn >> 5) & 1);
    cpu.vfp.s_bits(reg)
}

/// `VCVT` between floating point and integer uses the FPSCR rounding mode.
fn round_to_int(vfp: &Vfp, value: f64, unsigned: bool) -> i64 {
    let rounded = match vfp.fpscr & FPSCR_RMODE_MASK {
        FPSCR_RMODE_PLUS_INF => value.ceil(),
        FPSCR_RMODE_MINUS_INF => value.floor(),
        FPSCR_RMODE_ZERO => value.trunc(),
        _ => {
            // Round to nearest, ties to even.
            let r = value.round();
            if (value - value.trunc()).abs() == 0.5 {
                let even = 2.0 * (value / 2.0).round();
                even
            } else {
                r
            }
        }
    };
    if rounded.is_nan() {
        return 0;
    }
    let clamped = if unsigned {
        rounded.clamp(0.0, u32::MAX as f64) as i64
    } else {
        rounded.clamp(i32::MIN as f64, i32::MAX as f64) as i64
    };
    clamped
}

/// `VFPExpandImm` (ARM ARM A2.7.3): an 8-bit immediate expanded into a float.
///
/// ```text
/// sign     = imm8<7>
/// exponent = NOT(imm8<6>) : Replicate(imm8<6>, e) : imm8<5:4>
/// fraction = imm8<3:0> : Zeros(f)
/// ```
///
/// with e = 5, f = 19 for single precision and e = 8, f = 48 for double.
/// Checked against Keystone: `vmov.f32 s0, #1.0` (imm8 = 0x70) gives
/// 0x3f800000 and `#0.5` (imm8 = 0x60) gives 0x3f000000.
pub fn expand_imm(imm8: u32, single: bool) -> f64 {
    let sign = (imm8 >> 7) & 1;
    let repeating = (imm8 >> 6) & 1;
    let top = (imm8 >> 4) & 0x3;
    let fraction = imm8 & 0xf;
    if single {
        let exponent = ((1 - repeating) << 7)
            | (if repeating == 1 { 0b11111 } else { 0 } << 2)
            | top;
        let mantissa = fraction << 19;
        f32::from_bits((sign << 31) | (exponent << 23) | mantissa) as f64
    } else {
        let exponent = ((1 - repeating) << 10)
            | (if repeating == 1 { 0b1111_1111 } else { 0 } << 2)
            | top;
        let mantissa = (fraction as u64) << 48;
        f64::from_bits(((sign as u64) << 63) | ((exponent as u64) << 52) | mantissa)
    }
}

/// VFP data processing (CDP space): `cond 1110 opc1 Vn Vd 101 sz N opc M 0 Vm`.
pub fn cdp(cpu: &mut Cpu, insn: u32, pc: u32, default: Outcome) -> Outcome {
    cpu.vfp.instructions += 1;
    let opc1 = (insn >> 20) & 0xf;
    let base = opc1 & 0b1011; // drop the D bit
    let bit6 = insn & (1 << 6) != 0;
    let single = !is_double(insn);
    let a = read_vn(cpu, insn);
    let b = read_vm(cpu, insn);

    // Three-register operations.
    match base {
        0b0000 | 0b0001 | 0b0010 | 0b0011 | 0b1000 => {
            let x = read_vd(cpu, insn);
            let result = match (base, bit6) {
                (0b0000, false) => x + a * b,                 // VMLA
                (0b0000, true) => x + (-a * b),                // VMLS
                (0b0001, false) => -x + a * b,                 // VNMLS
                (0b0001, true) => -x + (-a * b),               // VNMLA
                (0b0010, false) => a * b,                      // VMUL
                (0b0010, true) => -(a * b),                    // VNMUL
                (0b0011, false) => a + b,                      // VADD
                (0b0011, true) => a - b,                       // VSUB
                (0b1000, false) => a / b,                      // VDIV
                _ => return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() }),
            };
            // The arithmetic instructions leave the FPSCR condition flags
            // alone (verified against QEMU: FPSCR keeps whatever the caller set,
            // only VCMP/VCMPE write them).
            if single {
                let rounded = single_result(result);
                if (a as f32).is_finite() && (b as f32).is_finite() && !rounded.is_finite() {
                    cpu.vfp.set_flag(FPSCR_OFC, true);
                }
                write_vd(cpu, insn, rounded);
            } else {
                if a.is_finite() && b.is_finite() && !result.is_finite() {
                    cpu.vfp.set_flag(FPSCR_OFC, true);
                }
                write_vd(cpu, insn, result);
            }
            return Outcome::Continue;
        }
        // Two-register operations: opc1 == 0b1011, bit 4 == 0.
        // VMOV immediate: opc1 == 0b1011, bit 4 == 0 and **bit 6 == 0**, with
        // the immediate split as imm4H (bits 19:16), i (bit 7), imm4L (bits 3:0).
        0b1011 if insn & (1 << 4) == 0 && !bit6 => {
            let imm4h = (insn >> 16) & 0xf;
            let i = (insn >> 7) & 1;
            let imm4l = insn & 0xf;
            let imm8 = (imm4h << 4) | (i << 3) | imm4l;
            let value = expand_imm(imm8, single);
            write_vd(cpu, insn, value);
            return Outcome::Continue;
        }
        // The rest of the two-register space: opc1 == 0b1011, bit 6 == 1.
        0b1011 => {
            let opc2 = (insn >> 16) & 0xf;
            let n = (insn >> 7) & 1 != 0;
            match (opc2, n) {
                // VMOV / VABS
                (0x0, false) => {
                    write_vd(cpu, insn, b);
                    Outcome::Continue
                }
                (0x0, true) => {
                    write_vd(cpu, insn, b.abs());
                    Outcome::Continue
                }
                // VNEG / VSQRT
                (0x1, false) => {
                    write_vd(cpu, insn, -b);
                    Outcome::Continue
                }
                (0x1, true) => {
                    if b < 0.0 {
                        cpu.vfp.set_flag(FPSCR_IOC, true);
                    }
                    let result = if single { single_result(b.sqrt()) } else { b.sqrt() };
                    write_vd(cpu, insn, result);
                    Outcome::Continue
                }
                // VCMP / VCMPE (with a register or with #0.0)
                (0x4 | 0x5, _) => {
                    let zero = opc2 == 0x5;
                    let value = read_vd(cpu, insn);
                    let other = if zero { 0.0 } else { b };
                    if n {
                        // VCMPE: any NaN input signals an invalid operation.
                        if value.is_nan() || other.is_nan() {
                            cpu.vfp.set_flag(FPSCR_IOC, true);
                        }
                    }
                    cpu.vfp.compare(value, other);
                    Outcome::Continue
                }
                // VCVT between single and double: sz selects the *source* size
                // (`vcvt.f64.f32 d0, s1` = 0xeeb70ae0 has sz = 0).
                (0x7, true) => {
                    let value = read_vm(cpu, insn);
                    if single {
                        // single source, double destination
                        cpu.vfp.set_d(vd_index(insn), value);
                    } else {
                        // double source, single destination
                        cpu.vfp.set_s(vd_single(insn), single_result(value) as f32);
                    }
                    Outcome::Continue
                }
                // VCVT integer -> float: the source is the raw 32-bit pattern
                // of a single register, the destination is a float.
                (0x8, _) => {
                    let raw = vm_single_bits(cpu, insn);
                    let value = if n { raw as i32 as f64 } else { raw as f64 };
                    let result = if single { single_result(value) } else { value };
                    write_vd(cpu, insn, result);
                    Outcome::Continue
                }
                // VCVT float -> integer: the destination is a single register
                // holding the integer, the source is a float (sz selects its
                // precision).
                (0xc | 0xd, _) => {
                    let value = read_vm(cpu, insn);
                    let unsigned = opc2 == 0xc;
                    let rounded = round_to_int(&cpu.vfp, value, unsigned);
                    cpu.vfp.set_s_bits(vd_single(insn), rounded as u32);
                    Outcome::Continue
                }
                _ => Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() }),
            }
        }
        _ => {
            let _ = default;
            Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() })
        }
    }
}

/// `MCR`/`MRC`: `VMOV` between a core register and a single-precision register,
/// plus `VMRS`/`VMSR`.
pub fn mcr_mrc(cpu: &mut Cpu, insn: u32, pc: u32, _default: Outcome) -> Outcome {
    cpu.vfp.instructions += 1;
    let cp = (insn >> 8) & 0xf;
    if cp != 0b1010 && cp != 0b1011 {
        return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() });
    }
    let load = insn & (1 << 20) != 0; // MRC reads, MCR writes
    let opc1 = (insn >> 21) & 0x7;
    let rt = ((insn >> 12) & 0xf) as usize;
    let crn = (insn >> 16) & 0xf;
    let opc2 = (insn >> 5) & 0x7;
    let crm = insn & 0xf;

    // VMRS/VMSR: opc1 == 0b111.  `vmrs r0, fpscr` = 0xeef10a10 (crn = 1),
    // `vmsr fpscr, r0` = 0xeee10a10.
    if opc1 == 0b111 {
        let value = match crn {
            0 => 0x4103_3000,              // FPSID
            1 => cpu.vfp.fpscr,            // FPSCR
            6 => 0x1211_1111,              // MVFR1 (VFPv3)
            7 => 0x1011_0222,              // MVFR0 (VFPv3, single + double)
            8 => 0x4000_0000,              // FPEXC (always enabled here)
            _ => return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() }),
        };
        if load {
            if rt == 15 {
                // `vmrs apsr_nzcv, fpscr` moves the flags into the CPSR.
                let flags = value & 0xf000_0000;
                cpu.cpsr = (cpu.cpsr & 0x0fff_ffff) | flags;
            } else {
                cpu.write_reg(rt, value);
            }
        } else if crn == 1 {
            cpu.vfp.fpscr = cpu.read_reg(rt);
        }
        // Writes to FPSID/MVFR/FPEXC are ignored: the FPU is always on here.
        return Outcome::Continue;
    }

    // VMOV Rt, Sn / Sn, Rt: MRC/MCR p10 with opc1 == 0b000.  The register is
    // CRn * 2 + opc2<2> (`vmov r0, s1` = 0xee100a90: CRn = 0, opc2 = 0b100).
    if opc1 == 0b000 && cp == 0b1010 {
        let reg = reg_num(crn, (opc2 >> 2) & 1);
        if load {
            cpu.write_reg(rt, cpu.vfp.s_bits(reg));
        } else {
            let value = cpu.read_reg(rt);
            cpu.vfp.set_s_bits(reg, value);
        }
        return Outcome::Continue;
    }
    // VMOV Rt, Dm / Dm, Rt (p11, opc2 == 1 selects the low word, 3 the high):
    // `Dm = CRn<3:0> : CRm<3:0>`, no page-zero mapping is needed here.
    if opc1 == 0b000 && cp == 0b1011 && (opc2 == 1 || opc2 == 3) {
        let reg = (crn | (crm << 4)) as usize;
        let high = opc2 == 3;
        if load {
            let word = if high { (cpu.vfp.d[reg] >> 32) as u32 } else { cpu.vfp.d[reg] as u32 };
            cpu.write_reg(rt, word);
        } else {
            let value = cpu.read_reg(rt) as u64;
            if high {
                cpu.vfp.d[reg] = (cpu.vfp.d[reg] & 0x0000_0000_ffff_ffff) | (value << 32);
            } else {
                cpu.vfp.d[reg] = (cpu.vfp.d[reg] & 0xffff_ffff_0000_0000) | value;
            }
        }
        return Outcome::Continue;
    }
    let _ = crm;
    // VCVT with a fixed-point immediate and the rest of the MCR space: report
    // as undefined rather than silently doing the wrong thing.
    Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() })
}

/// `MCRR`/`MRRC`: `VMOV` between two core registers and a double register.
pub fn mcrr_mrrc(cpu: &mut Cpu, insn: u32, pc: u32, _default: Outcome) -> Outcome {
    cpu.vfp.instructions += 1;
    let load = insn & (1 << 20) != 0;
    let rt2 = ((insn >> 16) & 0xf) as usize;
    let rt = ((insn >> 12) & 0xf) as usize;
    let d = ((insn >> 5) & 0xf) as usize | (((insn >> 8) & 0xf) as usize) << 4;
    if load {
        let hi = cpu.read_reg(rt2);
        let lo = cpu.read_reg(rt);
        cpu.vfp.d[d] = ((hi as u64) << 32) | lo as u64;
    } else {
        let value = cpu.vfp.d[d];
        cpu.write_reg(rt2, (value >> 32) as u32);
        cpu.write_reg(rt, value as u32);
    }
    let _ = pc;
    Outcome::Continue
}

/// `LDC`/`STC`: VLDR/VSTR (single register) and VLDM/VSTM (register lists).
pub fn ldc_stc<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32, _default: Outcome) -> Outcome {
    cpu.vfp.instructions += 1;
    let p = insn & (1 << 24) != 0;
    let u = insn & (1 << 23) != 0;
    let _d = insn & (1 << 22) != 0;
    let w = insn & (1 << 21) != 0;
    let load = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xf) as usize;
    let vd = (insn >> 12) & 0xf;
    let single = insn & (1 << 8) == 0;
    let imm8 = (insn & 0xff) as u32;

    let base = cpu.read_reg(rn);

    let fetch32 = |bus: &mut B, addr: u32| -> Result<u32, Trap> {
        bus.read_u32(addr)
            .map_err(|error| Trap::Memory { error, address: addr, pc, access: Access::Read })
    };
    let store32 = |bus: &mut B, addr: u32, value: u32| -> Result<(), Trap> {
        bus.write_u32(addr, value)
            .map_err(|error| Trap::Memory { error, address: addr, pc, access: Access::Write })
    };

    // VLDR/VSTR: P = 1, W = 0 (the writeback forms are the VLDM/VSTM space).
    // `vldr s2, [r5, #8]` = 0xed951a02: the offset is imm8 * 4.
    if p && !w {
        // VLDR/VSTR (single register, register-relative)
        let offset = imm8 * 4;
        let addr = if u { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let reg = if single { reg_num(vd, _d as u32) } else { (vd | ((_d as u32) << 4)) as usize };
        macro_rules! bail {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(t) => return Outcome::Trap(t),
                }
            };
        }
        if load {
            if single {
                let v = bail!(fetch32(bus, addr));
                cpu.vfp.set_s_bits(reg, v);
            } else {
                let lo = bail!(fetch32(bus, addr));
                let hi = bail!(fetch32(bus, addr.wrapping_add(4)));
                cpu.vfp.d[reg] = ((hi as u64) << 32) | lo as u64;
            }
        } else if single {
            let v = cpu.vfp.s_bits(reg);
            bail!(store32(bus, addr, v));
        } else {
            let value = cpu.vfp.d[reg];
            bail!(store32(bus, addr, value as u32));
            bail!(store32(bus, addr.wrapping_add(4), (value >> 32) as u32));
        }
        return Outcome::Continue;
    }

    // VLDM/VSTM (and the VFP push/pop aliases).
    let count = if single { imm8 } else { imm8 / 2 };
    if count == 0 {
        return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() });
    }
    let bytes = if single { count * 4 } else { count * 8 };
    // Only "increment after" (P = 0, U = 1) and "decrement before" (P = 1,
    // U = 0) exist; the registers always ascend in address order.
    let start = match (p, u) {
        (false, true) => base,
        (true, false) => base.wrapping_sub(bytes),
        _ => return Outcome::Trap(Trap::Undefined { address: pc, insn, thumb: cpu.thumb() }),
    };

    macro_rules! bail {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(t) => return Outcome::Trap(t),
            }
        };
    }

    // The list starts at the register named by the Vd field: a single register
    // number is (Vd * 2 + D) and a double one is (D:Vd) -- verified with
    // Keystone (`vpush {s16-s19}` = 0xed2d8a04, `vpush {d16-d19}` = 0xed6d0b08).
    let first = if single { reg_num(vd, _d as u32) } else { (vd | ((_d as u32) << 4)) as usize };
    let mut addr = start;
    for i in 0..count {
        if single {
            let reg = first.wrapping_add(i as usize);
            if load {
                let v = bail!(fetch32(bus, addr));
                cpu.vfp.set_s_bits(reg, v);
            } else {
                let v = cpu.vfp.s_bits(reg);
                bail!(store32(bus, addr, v));
            }
            addr = addr.wrapping_add(4);
        } else {
            let reg = first + i as usize;
            if load {
                let lo = bail!(fetch32(bus, addr));
                let hi = bail!(fetch32(bus, addr.wrapping_add(4)));
                cpu.vfp.d[reg] = ((hi as u64) << 32) | lo as u64;
            } else {
                let value = cpu.vfp.d[reg];
                bail!(store32(bus, addr, value as u32));
                bail!(store32(bus, addr.wrapping_add(4), (value >> 32) as u32));
            }
            addr = addr.wrapping_add(8);
        }
    }
    if w {
        let new_base = if u { base.wrapping_add(bytes) } else { base.wrapping_sub(bytes) };
        cpu.write_reg(rn, new_base);
    }
    Outcome::Continue
}

/// The `cond == 0b1111` VFP space (VMOV immediate and VCVT fixed point).
pub fn unconditional(cpu: &mut Cpu, insn: u32, pc: u32) -> Outcome {
    cdp(cpu, insn, pc, Outcome::Continue)
}

/// Thumb-2 VFP is encoded with the same 32-bit words as ARM mode.
pub fn execute_thumb<B: Bus>(cpu: &mut Cpu, bus: &mut B, insn: u32, pc: u32) -> Outcome {
    // 1110 110x / 1110 111x with coproc 10/11 covers MCR/MRC, CDP and LDC/STC.
    let op = (insn >> 20) & 0xf;
    if insn & (1 << 4) != 0 {
        // MCR/MRC
        mcr_mrc(cpu, insn, pc, Outcome::Continue)
    } else if op & 0x4 == 0x4 && (insn >> 24) & 0xf == 0xe && insn & (1 << 25) == 0 {
        // CDP space (data processing)
        cdp(cpu, insn, pc, Outcome::Continue)
    } else {
        ldc_stc(cpu, bus, insn, pc, Outcome::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_double_views_share_storage() {
        let mut vfp = Vfp::default();
        vfp.set_s(1, 1.5);
        assert_eq!(vfp.s(1), 1.5);
        // s0/s1 live in d0; s0 must be untouched.
        assert_eq!(vfp.s(0), 0.0);
        vfp.set_d(15, 2.25);
        assert_eq!(vfp.d(15), 2.25);
    }

    #[test]
    fn double_register_halves() {
        let mut vfp = Vfp::default();
        vfp.set_s(0, 3.0);
        vfp.set_s(1, 4.0);
        // d0 = s1:s0, little endian -> low word is s0
        let bits = vfp.d[0];
        assert_eq!(f32::from_bits(bits as u32), 3.0);
        assert_eq!(f32::from_bits((bits >> 32) as u32), 4.0);
    }

    #[test]
    fn comparison_sets_fpscr_flags() {
        let mut vfp = Vfp::default();
        vfp.compare(1.0, 2.0);
        assert!(vfp.flag(FPSCR_N));
        vfp.fpscr = 0;
        vfp.compare(2.0, 2.0);
        assert!(vfp.flag(FPSCR_Z) && vfp.flag(FPSCR_C));
        vfp.fpscr = 0;
        vfp.compare(3.0, 2.0);
        assert!(vfp.flag(FPSCR_C) && !vfp.flag(FPSCR_N));
        vfp.fpscr = 0;
        vfp.compare(f64::NAN, 2.0);
        assert!(vfp.flag(FPSCR_IOC) && vfp.flag(FPSCR_V));
    }

    #[test]
    fn rounding_modes_are_respected() {
        let mut vfp = Vfp::default();
        assert_eq!(round_to_int(&vfp, 2.5, false), 2); // round to nearest, ties to even
        assert_eq!(round_to_int(&vfp, 3.5, false), 4);
        vfp.fpscr = FPSCR_RMODE_ZERO;
        assert_eq!(round_to_int(&vfp, -2.7, false), -2);
        vfp.fpscr = FPSCR_RMODE_MINUS_INF;
        assert_eq!(round_to_int(&vfp, -2.2, false), -3);
        vfp.fpscr = FPSCR_RMODE_PLUS_INF;
        assert_eq!(round_to_int(&vfp, 2.2, false), 3);
    }
}
