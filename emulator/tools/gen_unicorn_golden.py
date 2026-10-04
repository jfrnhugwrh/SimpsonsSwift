#!/usr/bin/env python3
"""Generate `crates/arm/src/unicorn_golden.rs` from QEMU (via Unicorn).

The interpreter must agree with Unicorn on the *observable* result of every
instruction in the table below: registers r0-r12, the link register, the NZCV
flags, the program counter, and the contents of a small data area.  The table is
deliberately made of the instruction forms the Simpsons executable uses most.

Usage:
    pip3 download --no-deps -d /tmp/armtools unicorn keystone-engine
    python3 -m zipfile -e /tmp/armtools/unicorn-*.whl /home/user/.cache/pyarm/
    python3 -m zipfile -e /tmp/armtools/keystone_engine-*.whl /home/user/.cache/pyarm/
    PYTHONPATH=/home/user/.cache/pyarm python3 tools/gen_unicorn_golden.py

Writes the Rust file next to the crate's other sources; `cargo test -p arm`
then checks the interpreter against it.
"""

import io
import sys
import pathlib

sys.path.insert(0, "/home/user/.cache/pyarm")

from keystone import Ks, KS_ARCH_ARM, KS_MODE_ARM, KS_MODE_THUMB  # noqa: E402
from capstone import Cs, CS_ARCH_ARM, CS_MODE_ARM, CS_MODE_THUMB  # noqa: E402
from unicorn import Uc, UC_ARCH_ARM, UC_MODE_ARM, UC_MODE_THUMB, UcError  # noqa: E402
from unicorn.arm_const import (  # noqa: E402
    UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2, UC_ARM_REG_R3, UC_ARM_REG_R4,
    UC_ARM_REG_R5, UC_ARM_REG_R6, UC_ARM_REG_R7, UC_ARM_REG_R8, UC_ARM_REG_R9,
    UC_ARM_REG_R10, UC_ARM_REG_R11, UC_ARM_REG_R12, UC_ARM_REG_SP, UC_ARM_REG_LR,
    UC_ARM_REG_PC, UC_ARM_REG_CPSR, UC_ARM_REG_S0, UC_ARM_REG_FPSCR,
    UC_ARM_REG_FPEXC,
)

BASE = 0x1000          # where the instruction under test is placed
REGION = 0x0010_0000   # 1 MiB, the same window the Rust test maps
DATA = 0x1040          # pre-filled data the loads read
OUT = 0x1080           # spare area stores write into

# Initial state, mirrored by `initial_state()` in the generated test.
INIT_REGS = {
    UC_ARM_REG_R0: 0x0000_0005,
    UC_ARM_REG_R1: 0x0000_0007,
    UC_ARM_REG_R2: 0x0000_0010,
    UC_ARM_REG_R3: 0x0000_0020,
    UC_ARM_REG_R4: 0xFFFF_FFFB,          # -5
    UC_ARM_REG_R5: DATA,                 # pointer
    UC_ARM_REG_R6: DATA + 8,             # pointer
    UC_ARM_REG_R7: OUT,                  # pointer
    UC_ARM_REG_R8: 0x1234_5678,
    UC_ARM_REG_R9: 0xFFFF_FFFF,
    UC_ARM_REG_R10: 0x8000_0000,
    UC_ARM_REG_R11: 0x0000_1000,
    UC_ARM_REG_R12: BASE + 0x60,        # a valid branch target
    UC_ARM_REG_SP: 0x2000,               # inside the mapped window
    UC_ARM_REG_LR: BASE + 0x800,         # inside the mapped window
    UC_ARM_REG_PC: BASE,
}
INIT_CPSR = 0xA000_0010              # N=1, Z=0, C=1, V=0, mode = user
MEM_INIT = {
    DATA + 0x00: 0x1122_3344,
    DATA + 0x04: 0x5566_7788,
    DATA + 0x08: 0x99AA_BBCC,
    DATA + 0x0C: 0xDDEE_FF00,
    DATA + 0x10: 0x0000_0001,
    DATA + 0x14: 0x8000_0000,
}

# s0-s7 start out holding these values (as raw bits) so that float results are
# reproducible; the sign, exponent and mantissa ranges are chosen to make most
# arithmetic exact in single precision.
FP_INIT_BITS = [
    0x3FC0_0000,  # 1.5
    0x4010_0000,  # 2.25
    0xC070_0000,  # -3.75
    0x3F00_0000,  # 0.5
    0x42C8_0000,  # 100.0
    0x3A83_126F,  # ~1e-3
    0x4228_0000,  # 42.0
    0xBF40_0000,  # -0.75
]

MEM_WORDS = 8                        # words compared, starting at DATA

# A tiny jump table for TBB/TBH, reached with r2 = 1 so that the lookup address
# is OUT + 1.  The halfword there selects a target 8 bytes past the instruction.
JUMP_TABLE = {OUT: 0x0000_0004, OUT + 4: 0x0000_0004}


def arm_cases():
    """(name, assembly, thumb) triples."""
    cases = []
    # --- data processing, immediate ------------------------------------
    for text in [
        "and r0, r1, #0xff", "orr r2, r3, #0x10", "eor r4, r5, #0xf0",
        "sub r6, r7, #1", "rsb r0, r1, #0", "add r0, r0, #0x20",
        "adc r1, r2, #0", "sbc r2, r3, #1", "rsc r3, r4, #0",
        "tst r1, #0x3", "teq r2, #0x10", "cmp r0, #5", "cmn r4, #5",
        "mov r5, #0x8000", "mvn r6, #0", "bic r7, r8, #0xf",
        "mov r0, #0xff", "mov r1, #0xff000000", "sub r0, r1, r2, lsl #2",
        "add r0, r1, r2, lsr #1", "and r0, r1, r2, asr #3",
        "orr r0, r1, r2, ror #4", "adc r0, r1, r2, rrx",
    ]:
        cases.append(("arm " + text, text, False))
    # flag-setting forms
    for text in [
        "adds r0, r1, r2", "subs r0, r9, #1", "adds r9, r9, #1",
        "adds r0, r10, r10", "subs r0, r0, r1", "muls r0, r1, r2",
        "ands r0, r9, r8", "orrs r0, r8, r9", "eors r0, r8, r9",
        "adds r0, r0, r1, lsl #2",     ]:
        cases.append(("arm " + text, text, False))
    # --- movw/movt, mrs ------------------------------------------------
    for text in ["movw r0, #0x1234", "movt r0, #0xabcd", "movw r7, #0xffff",
                 "mrs r0, cpsr"]:
        cases.append(("arm " + text, text, False))
    # --- multiplies -----------------------------------------------------
    for text in ["mul r0, r1, r2", "mla r0, r1, r2, r3", "umull r0, r1, r2, r3",
                 "smull r0, r1, r8, r9", "umlal r0, r1, r2, r3",
                 "smlal r0, r1, r2, r3"]:
        cases.append(("arm " + text, text, False))
    # --- loads and stores ------------------------------------------------
    for text in [
        "ldr r0, [r5]", "ldr r1, [r5, #4]", "ldrb r2, [r5, #1]",
        "ldrh r3, [r5, #2]", "ldrsb r4, [r5, #4]", "ldrsh r0, [r5, #6]",
        "ldr r1, [r5, r2]", "str r0, [r7]", "str r1, [r7, #4]",
        "strb r2, [r7, #2]", "strh r3, [r7, #6]", "ldrd r0, r1, [r5]",
        "strd r8, r9, [r7, #8]", "ldr r0, [r5], #4", "str r1, [r7], #4",
        "ldr r0, [r5, #-4]", "ldr r0, [pc, #0x20]",
        "ldm r5, {r0, r1, r2, r3}", "stm r7, {r0, r1, r2, r3}",
        "push {r0, r1, r4}", "pop {r0, r1, r2}",
    ]:
        cases.append(("arm " + text, text, False))
    # --- branches (targets inside the mapped window) ----------------------
    for text in ["b #0x1020", "bl #0x1030", "bx r12", "blx r12", "bx lr"]:
        cases.append(("arm " + text, text, False))
    # --- System / bitfield instructions an optimized iOS binary uses -------
    for text in [
        "dmb sy", "dsb sy", "isb sy", "clrex", "pld [r1]",
        "ubfx r0, r1, #4, #8", "sbfx r2, r3, #2, #10", "bfi r0, r1, #4, #8",
        "bfc r0, #4, #8", "ubfx r4, r5, #0, #16", "sbfx r4, r5, #0, #16",
        "smmla r0, r1, r2, r3", "usad8 r0, r1, r2", "usada8 r0, r1, r2, r3",
        "rbit r0, r1", "rev16 r0, r1", "pkhbt r0, r1, r2, lsl #8",
        "ssat r0, #8, r1, asr #5", "usat r0, #8, r1, lsl #5", "sxtb16 r0, r1",
        "usub8 r0, r1, r2", "uadd8 r0, r1, r2", "sel r0, r1, r2",
    ]:
        cases.append(("arm " + text, text, False))
    # --- VFP --------------------------------------------------------------
    for text in [
        "vadd.f32 s0, s1, s2", "vsub.f32 s3, s4, s5", "vmul.f32 s6, s1, s2",
        "vdiv.f32 s0, s4, s3", "vabs.f32 s1, s2", "vneg.f32 s2, s1",
        "vsqrt.f32 s3, s4", "vmov.f32 s4, s5", "vmov.f32 s0, #1.0",
        "vmov.f32 s1, #0.5", "vmov.f32 s2, #-2.0", "vmov.f32 s3, #16.0",
        "vmla.f32 s0, s1, s2", "vmls.f32 s0, s1, s2", "vnmul.f32 s0, s1, s2",
        "vnmla.f32 s0, s1, s2", "vcmp.f32 s1, s2", "vcmpe.f32 s1, s3",
        "vcmp.f32 s1, #0.0", "vcmpe.f32 s1, #0.0",
        "vcvt.f32.s32 s0, s1", "vcvt.s32.f32 s0, s1", "vcvt.u32.f32 s2, s3",
        "vcvt.f32.u32 s4, s5", "vldr s0, [r5]", "vstr s0, [r7]",
        "vldr s2, [r5, #8]", "vstr s4, [r7, #4]", "vmov r0, s1",
        "vmov r1, s2", "vmov s5, r2", "vmrs r0, fpscr", "vmrs apsr_nzcv, fpscr",
        "vpush {s0-s3}", "vpop {s0-s3}", "vldmia r5, {s0-s3}", "vstmia r7, {s0-s3}",
        "vmrs apsr_nzcv, fpscr", "vmov.f64 d0, d1", "vadd.f64 d0, d1, d2",
        "vmul.f64 d4, d5, d6", "vcvt.f64.f32 d0, s2", "vcvt.f32.f64 s4, d3",
    ]:
        cases.append(("arm " + text, text, False))
    # --- misc ------------------------------------------------------------
    for text in ["clz r0, r1", "sxtb r0, r1", "sxth r2, r3", "uxtb r4, r5",
                 "uxth r6, r7", "rev r0, r8", "rev16 r1, r8", "revsh r2, r8",
                 "sxtb r3, r4, ror #8", "sxth r3, r4, ror #16",
                 "ssat r0, #8, r1", "usat r0, #8, r1", "ssat r0, #16, r9",
                 "usat r0, #5, r2", "ssat r2, #8, r3, lsl #4",
                 "ssat16 r0, #8, r1", "usat16 r0, #8, r1",
                 "sel r0, r1, r2", "qadd r0, r1, r2",
                 "qsub r0, r1, r2", "swp r0, r1, [r5]", "swpb r0, r1, [r5]",
                 "smulbb r0, r1, r2", "smulbt r0, r1, r2", "smultb r0, r1, r2",
                 "smultt r0, r1, r2", "smlabb r0, r1, r2, r3", "smlabt r0, r1, r2, r3",
                 "smulwb r0, r1, r2", "smulwt r0, r1, r2", "smlawb r0, r1, r2, r3",
                 "smlalbb r0, r1, r2, r3", "mrs r0, cpsr", "msr cpsr_f, r1",
                 "adc r0, r1, r2, rrx", "rrx r0, r1"]:
        cases.append(("arm " + text, text, False))
    return cases


def thumb_cases():
    cases = []
    for text in [
        "movs r0, #5", "movs r1, #0xff", "adds r0, r0, r1", "subs r0, r0, r1",
        "adds r2, r3, r4", "subs r2, r3, #7", "adds r5, #0x10", "subs r5, #0x10",
        "lsls r0, r1, #2", "lsrs r2, r3, #1", "asrs r4, r5, #3",
        "ands r0, r1", "orrs r0, r1", "eors r0, r1", "adcs r0, r1", "sbcs r0, r1",
        "rors r0, r1", "tst r0, r1", "cmp r0, r1", "cmn r0, r1", "rsbs r0, r1, #0",
        "muls r0, r1, r0", "bics r0, r1", "mvns r0, r1",
        "mov r8, r1", "mov r12, r1", "adds r0, r0, r8", "cmp r0, r8",
        "ldr r0, [r5]", "str r0, [r7]", "ldrb r2, [r5, #3]", "strb r2, [r7, #1]",
        "ldrh r3, [r5, #4]", "strh r3, [r7, #2]", "ldrsb r4, [r5, r2]", "ldrsh r0, [r5, r2]", "ldr r1, [r5, r2]", "str r1, [r7, r2]",
        "ldrsb.w r4, [r5, #4]", "ldrsh.w r0, [r5, #6]", "ldr.w r0, [r5, #8]", "str.w r0, [r7, #12]",
        "ldr.w r0, [r5, r2, lsl #2]", "ldrb.w r0, [r5, r2]", "ldrh.w r0, [r5, r2]",
        "ldr r0, [r5, #4]!", "str r0, [r7, #4]!", "ldr r0, [r5], #4", "str r0, [r7], #4",
        "ldr.w r0, [pc, #0x20]", "strb.w r0, [r7, #3]", "ldrsh.w r0, [r5, #2]",
        "ldrd r4, r6, [r5, #8]", "strd r4, r6, [r7, #8]", "ldrd r4, r6, [r5], #8",
        "strd r4, r6, [r7, #8]!", "ldrd r4, r6, [r5, #-8]", "strd r4, r6, [r7, #-8]",
        "stmia.w r7!, {r0, r1}", "ldmia.w r5!, {r0, r1}", "stmdb r7!, {r0, r1}",
        "ldmdb r5!, {r0, r1}", "ldrex r0, [r5]", "strex r0, r2, [r7]", "tbb [r5, r2]", "tbh [r5, r2, lsl #1]",
        "ldr r0, [pc, #8]", "ldr r0, [sp, #4]", "str r0, [sp, #8]",
        "add r0, sp, #16", "push {r0, r1, lr}", "pop {r0, r1, r3}",
        "ldmia r5!, {r0, r1}", "stmia r7!, {r0, r1}", "b #0x1020", "bx lr",
        "movw r0, #0x1234", "movt r0, #0x5678", "add.w r2, r3, #0x40",
        "sub.w r2, r3, #0x10", "cmp.w r0, #0x100", "lsl.w r0, r1, #4",
        "mvn.w r0, r1", "eor.w r0, r1, r2", "tst.w r0, r1", "add.w r1, r2, r3, lsl #4",
        "ubfx r0, r1, #4, #8", "sbfx r2, r3, #2, #10", "bfi r0, r1, #4, #8",
        "bfc r0, #4, #8", "uxtb.w r0, r1", "sxtb.w r0, r1", "rev.w r0, r1",
        "clz r0, r1", "dmb sy", "dsb sy", "isb sy", "clrex", "mrs r0, cpsr",
        "ssat r0, #8, r1", "usat r0, #8, r1", "ssat16 r0, #8, r1", "usat16 r0, #8, r1",
        "mul r1, r2, r3", "mla r1, r2, r3, r4", "mls r1, r2, r3, r4", "smulbb r1, r2, r3",
        "smulbt r1, r2, r3", "smlabb r1, r2, r3, r4", "smulwb r1, r2, r3", "smlawb r1, r2, r3, r4",
        "smull r1, r5, r2, r3", "umull r1, r5, r2, r3", "smlal r1, r5, r2, r3", "umlal r1, r5, r2, r3",
        "smlalbb r1, r5, r2, r3", "smmul r1, r2, r3", "smmla r1, r2, r3, r4", "smuad r1, r2, r3",
        "smlad r1, r2, r3, r4", "smusd r1, r2, r3", "sel r1, r2, r3",
        "ssat r0, #8, r1, asr #5", "usat r0, #8, r1, lsl #5",
        "sxtb16 r0, r1", "uxtb16 r0, r1", "pkhbt r0, r1, r2, lsl #8",
        "pkhtb r0, r1, r2, asr #8", "rbit r0, r1", "uadd8 r0, r1, r2",
        "usub8 r0, r1, r2", "sadd16 r0, r1, r2", "ssub16 r0, r1, r2",
        "uadd16 r0, r1, r2", "usub16 r0, r1, r2", "smmla r0, r1, r2, r3",
        "usad8 r0, r1, r2", "usada8 r0, r1, r2, r3", "addw r2, r3, #0x400",
        "subw r2, r3, #0x400", "bl #0x1040", "blx #0x1040",
        "lsls r0, r1", "lsrs r0, r1",
    ]:
        cases.append(("thumb " + text, text, True))
    return cases


def round_trip_ok(asm_text, code, thumb, addr=BASE):
    """True when a disassembler reads back what the assembler was asked for.

    Keystone silently mis-encodes some operand combinations (e.g.
    `usat r2, #8, r3, asr #4` came back as `usat r2, #7, r3, asr #2`), and a
    golden entry derived from a mis-assembly would enshrine the wrong
    behaviour.
    """
    md = Cs(CS_ARCH_ARM, CS_MODE_THUMB if thumb else CS_MODE_ARM)
    text = " ".join(i.mnemonic + " " + i.op_str for i in md.disasm(code, addr))
    return tokens(text) == tokens(asm_text)


REGISTER_ALIASES = {
    "ip": "r12", "sp": "r13", "lr": "r14", "pc": "r15", "sl": "r10",
    "fp": "r11", "sb": "r9", "a1": "r0", "a2": "r1", "a3": "r2", "a4": "r3",
    "v1": "r4", "v2": "r5", "v3": "r6", "v4": "r7", "v5": "r8", "v6": "r9",
    "v7": "r10", "v8": "r11",
    "apsr_nzcv": "fpscr_nzcv", "apsr": "cpsr", "apsr_nzcvq": "cpsr_f",
}
MNEMONIC_ALIASES = {
    "stmia": "stm", "ldmia": "ldm", "stmib": "stmib", "stmda": "stmda",
    "ldmib": "ldmib", "ldmda": "ldmda", "ldmfd": "ldm", "stmfd": "stmdb",
    "ldmfa": "ldmda", "stmfa": "stmib", "push": "stmdb", "pop": "ldm",
}


def tokens(text):
    """Normalised multiset of the meaningful tokens of an instruction."""
    out = []
    cleaned = ""
    for ch in text.lower():
        cleaned += " " if ch in ",{}[]#!" else ch
    for raw in cleaned.split():
        item = raw.replace(".w", "").replace(".n", "")
        # `{s0-s3}` and `{s0, s1, s2, s3}` are the same register list.
        if len(item) > 3 and item[1] == "-" and item[0] == "s" or (
            item[:2] == "s0" and "-" in item
        ) or (item[:2] in ("r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r1") and "-" in item):
            prefix, sep, suffix = item.partition("-")
            if sep and prefix[:1] == suffix[:1] and prefix[1:].isdigit() and suffix[1:].isdigit():
                for index in range(int(prefix[1:]), int(suffix[1:]) + 1):
                    out.append(f"{prefix[0]}{index}")
                continue
        number = item.lstrip("-")
        if number.startswith("0x") or number.isdigit():
            try:
                out.append(str(int(item, 0)))
                continue
            except ValueError:
                pass
        try:
            value = float(item)
        except ValueError:
            pass
        else:
            out.append(str(int(value)) if value.is_integer() else repr(value))
            continue
        item = REGISTER_ALIASES.get(item, item)
        item = MNEMONIC_ALIASES.get(item, item)
        out.append(item)
    return sorted(out)


def run_program(code, thumb, steps=1):
    """Execute `code` (placed at `BASE`) with Unicorn and return the outcome."""
    mode = UC_MODE_THUMB if thumb else UC_MODE_ARM
    uc = Uc(UC_ARCH_ARM, mode)
    # Map from zero so that `push`/`pop`, `[sp, #n]` and a branch to a low
    # address all land in mapped memory, exactly like the Rust test's window.
    uc.mem_map(0, REGION)
    uc.mem_write(BASE, code)
    for addr, word in MEM_INIT.items():
        uc.mem_write(addr, word.to_bytes(4, "little"))
    for addr, word in JUMP_TABLE.items():
        uc.mem_write(addr, word.to_bytes(4, "little"))
    # The status register first: switching mode selects a different banked
    # register file, so writing it after the general-purpose registers would
    # discard them (which is how `push` first hit an unmapped stack).
    uc.reg_write(UC_ARM_REG_CPSR, INIT_CPSR)
    for reg, value in INIT_REGS.items():
        uc.reg_write(reg, value)
    for i, bits in enumerate(FP_INIT_BITS):
        uc.reg_write(UC_ARM_REG_S0 + i, bits)
    # QEMU only decodes the VFP instruction set when FPEXC.EN is set; without
    # this every `vadd`/`vldr`/`vpush` is reported as an invalid instruction.
    uc.reg_write(UC_ARM_REG_FPEXC, 0x4000_0000)
    try:
        uc.emu_start(BASE | (1 if thumb else 0), 0x0004_0000, count=steps)
    except UcError as error:
        return ("trap", str(error))
    regs = [uc.reg_read(r) for r in (
        UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2, UC_ARM_REG_R3,
        UC_ARM_REG_R4, UC_ARM_REG_R5, UC_ARM_REG_R6, UC_ARM_REG_R7,
        UC_ARM_REG_R8, UC_ARM_REG_R9, UC_ARM_REG_R10, UC_ARM_REG_R11,
        UC_ARM_REG_R12, UC_ARM_REG_SP, UC_ARM_REG_LR, UC_ARM_REG_PC)]
    fp = [uc.reg_read(UC_ARM_REG_S0 + i) for i in range(8)]
    fpscr = uc.reg_read(UC_ARM_REG_FPSCR)
    cpsr = uc.reg_read(UC_ARM_REG_CPSR)
    flags = (cpsr >> 28) & 0xF
    thumb_after = 1 if (cpsr & 0x20) else 0
    mem = [int.from_bytes(uc.mem_read(DATA + i * 4, 4), "little") for i in range(MEM_WORDS)]
    return ("ok", regs, flags, thumb_after, mem, fp, fpscr)


def assemble_program(texts, thumb):
    """Assemble each instruction at its own address, returning the bytes."""
    code = b""
    for text in texts:
        ks = Ks(KS_ARCH_ARM, KS_MODE_THUMB if thumb else KS_MODE_ARM)
        encoded = bytes(ks.asm(text, addr=BASE + len(code))[0])
        if not round_trip_ok(text, encoded, thumb, BASE + len(code)):
            md = Cs(CS_ARCH_ARM, CS_MODE_THUMB if thumb else CS_MODE_ARM)
            back = "; ".join(i.mnemonic + " " + i.op_str for i in md.disasm(encoded, BASE + len(code)))
            raise ValueError(f"{text!r} assembled as {back}")
        code += encoded
    return code


def sequence_cases():
    """Programs of two or three instructions, as (name, [assembly], thumb, steps).

    These cover the parts of the architecture that only show up across
    instructions: conditional execution, the Thumb `IT` block, flag carry
    propagation and the writeback performed by the *next* instruction.
    """
    cases = []
    for texts in [
        # Conditional execution: the flags set by the first instruction decide
        # whether the second runs.
        ["movs r0, #0", "addeq r1, r1, #1"],
        ["movs r0, #0", "addne r1, r1, #1"],
        ["cmp r0, #5", "subne r1, r1, #1"],
        ["cmp r0, #0", "mvneq r2, #7"],
        ["adds r0, r9, #1", "adc r3, r3, #0"],
        ["subs r0, r9, #1", "sbc r2, r2, r2"],
        ["cmp r4, #0", "movgt r5, #1"],
        ["movs r0, #0", "strne r0, [r7]"],
        ["movs r0, #0", "ldreq r0, [r5]"],
    ]:
        cases.append(("; ".join(texts), texts, False, 2, False))
    # Thumb IT blocks: the IT instruction carries the condition for the
    # instructions after it.  They have to be assembled as one block, because
    # Keystone will not assemble a conditional Thumb instruction on its own.
    for text, steps in [
        ("it eq\n  addeq r0, r1, r2", 2),
        ("it ne\n  addne r0, r1, r2", 2),
        ("it eq\n  moveq r3, #1", 2),
        ("ite eq\n  moveq r3, #1\n  movne r4, #2", 3),
        ("itt eq\n  moveq r3, #1\n  moveq r4, #2", 3),
        ("it eq\n  ldreq r0, [r5]", 2),
        ("it ne\n  cmpne r0, r1", 2),
        ("it eq\n  strbeq r2, [r7, #1]", 2),
    ]:
        cases.append((text.replace("\n", "; "), [text], True, steps, True))
    # The GE flags: `uadd8` sets one GE bit per byte and `sel` picks bytes from
    # Rn or Rm according to them.
    for texts in [
        ["uadd8 r0, r1, r2", "sel r3, r1, r2"],
        ["usub8 r0, r1, r2", "sel r3, r1, r2"],
        ["sadd16 r0, r1, r2", "sel r3, r1, r2"],
    ]:
        cases.append(("; ".join(texts), texts, False, 2, False))
    # A conditional branch that must not be taken.
    for texts in [
        ["movs r0, #0", "bne #0x1080"],
        ["cmp r0, #5", "bne #0x1080"],
        ["movs r0, #1", "beq #0x1080"],
    ]:
        cases.append(("; ".join(texts), texts, True, 2, False))
    return cases


def main():
    out = io.StringIO()
    out.write('''//! Differential tests against QEMU.
//!
//! Generated by `tools/gen_unicorn_golden.py`; do not edit by hand.  Every
//! entry is one instruction executed by Unicorn from a fixed initial state
//! (see `initial_state`) with the *expected* result recorded.
//!
//! The interpreter is not a cycle-accurate model: the table only compares the
//! architecturally visible outcome (r0-r12, sp, lr, pc, the NZCV flags, the
//! Thumb state and a few words of memory), which is exactly what the guest can
//! observe.

#![cfg(test)]

use crate::test_support::{space, BASE};
use crate::{Cpu, Outcome};

/// Words the loads read and the stores write into; `DATA` matches the generator.
const DATA: u32 = 0x1040;
const OUT: u32 = 0x1080;

pub struct Case {
    pub name: &'static str,
    pub thumb: bool,
    /// The instruction bytes, written at `BASE`.
    pub code: [u8; 16],
    /// How many bytes of `code` are meaningful.
    pub len: u8,
    /// How many instructions to execute (one, unless the case needs a sequence).
    pub steps: u8,
    /// r0-r12, sp, lr, pc.
    pub after: [u32; 16],
    /// NZCV in bits 3:0.
    pub flags: u32,
    /// Set when the processor ends in Thumb state.
    pub thumb_after: bool,
    /// Words at `DATA`.
    pub memory: [u32; 8],
    /// Raw bits of s0-s7.
    pub fp: [u32; 8],
    /// The whole FPSCR; only the condition flags and QC are compared.
    pub fpscr: u32,
}

const FP_INIT_BITS: [u32; 8] = [
    0x3FC0_0000, 0x4010_0000, 0xC070_0000, 0x3F00_0000,
    0x42C8_0000, 0x3A83_126F, 0x4228_0000, 0xBF40_0000,
];

fn initial_state(cpu: &mut Cpu) {
    const REGS: [u32; 13] = [
        0x0000_0005, 0x0000_0007, 0x0000_0010, 0x0000_0020,
        0xFFFF_FFFB, DATA, DATA + 8, OUT,
        0x1234_5678, 0xFFFF_FFFF, 0x8000_0000, 0x0000_1000, BASE + 0x60,
    ];
    cpu.r[..13].copy_from_slice(&REGS);
    cpu.r[13] = 0x2000;
    cpu.r[14] = BASE + 0x800;
    // N=1, Z=0, C=1, V=0, user mode -- keeping the Thumb bit, which
    // `reset` has just set for the Thumb cases (writing CPSR wholesale here
    // silently turned every Thumb case into an ARM one).
    cpu.cpsr = 0xA000_0010 | (cpu.cpsr & crate::FLAG_T);
    for (i, bits) in FP_INIT_BITS.iter().enumerate() {
        cpu.vfp.set_s(i, f32::from_bits(*bits));
    }
}

fn memory_setup(space: &mut guestmem::AddressSpace) {
    const WORDS: [u32; 6] = [
        0x1122_3344, 0x5566_7788, 0x99AA_BBCC, 0xDDEE_FF00, 0x0000_0001, 0x8000_0000,
    ];
    for (i, word) in WORDS.iter().enumerate() {
        space.write_u32(DATA + i as u32 * 4, *word).unwrap();
    }
    // The lookup table `tbb`/`tbh` read from.
    space.write_u32(OUT, 0x0000_0004).unwrap();
    space.write_u32(OUT + 4, 0x0000_0004).unwrap();
}

#[test]
fn interpreter_matches_qemu() {
    let mut failures = Vec::new();
    for case in CASES {
        let mut space = space();
        let mut cpu = Cpu::new();
        cpu.reset(BASE, 0x2000, case.thumb);
        initial_state(&mut cpu);
        memory_setup(&mut space);
        for (i, byte) in case.code[..case.len as usize].iter().enumerate() {
            space.write_u8(BASE + i as u32, *byte).unwrap();
        }
        let mut trapped = None;
        for _ in 0..case.steps {
            let outcome = cpu.step(&mut space);
            if !matches!(outcome, Outcome::Continue) {
                trapped = Some(outcome);
                break;
            }
        }
        if let Some(outcome) = trapped {
            failures.push(format!("{}: interpreter trapped ({outcome:?})", case.name));
            continue;
        }
        let mut after = [0u32; 16];
        after[..13].copy_from_slice(&cpu.r[..13]);
        after[13] = cpu.r[13];
        after[14] = cpu.r[14];
        after[15] = cpu.pc();
        let flags = (cpu.cpsr >> 28) & 0xf;
        if after != case.after {
            let mut diffs = Vec::new();
            for (i, (got, want)) in after.iter().zip(case.after.iter()).enumerate() {
                if got != want {
                    diffs.push(format!("r{i}: got {got:#010x} want {want:#010x}"));
                }
            }
            failures.push(format!("{}: {}", case.name, diffs.join(", ")));
        }
        if flags != case.flags {
            failures.push(format!("{}: flags got {flags:#x} want {:#x}", case.name, case.flags));
        }
        if cpu.thumb() != case.thumb_after {
            failures.push(format!("{}: thumb state got {} want {}", case.name, cpu.thumb(), case.thumb_after));
        }
        for (i, want) in case.fp.iter().enumerate() {
            let got = cpu.vfp.s_bits(i);
            if got != *want {
                failures.push(format!("{}: s{i} got {got:#010x} want {want:#010x}", case.name));
            }
        }
        if cpu.vfp.fpscr & 0xf800_0000 != case.fpscr & 0xf800_0000 {
            failures.push(format!(
                "{}: fpscr got {:#010x} want {:#010x}",
                case.name, cpu.vfp.fpscr, case.fpscr
            ));
        }
        for (i, want) in case.memory.iter().enumerate() {
            let got = space.read_u32(DATA + i as u32 * 4).unwrap();
            if got != *want {
                failures.push(format!("{}: memory[{i}] got {got:#010x} want {want:#010x}", case.name));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ from QEMU:\\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\\n")
    );
}

const CASES: &[Case] = &[
''')
    failures = []
    ok = 0
    programs = []
    for name, text, thumb in arm_cases() + thumb_cases():
        programs.append((name, [text], thumb, 1, False))
    programs.extend(sequence_cases())
    for name, texts, thumb, steps, search in programs:
        try:
            code = assemble_program(texts, thumb)
            assert len(code) <= 16, f"{name}: program is {len(code)} bytes"
            # Unicorn's `count` argument does not always mean "instructions":
            # an `it` block and the instruction it guards are executed by a
            # single unit of work.  Pick the smallest count that reaches the
            # end of the program, which is the same execution either way.
            if not search:
                result = run_program(code, thumb, steps)
            else:
                result = run_program(code, thumb, steps)
                for count in range(1, steps + 1):
                    candidate = run_program(code, thumb, count)
                    if candidate[0] != "ok":
                        result = candidate
                        break
                    if candidate[1][15] == BASE + len(code):
                        result = candidate
                        break
        except Exception as error:  # encoding, round trip or mapping problem
            failures.append((name, repr(error)))
            continue
        if result[0] != "ok":
            failures.append((name, result[1]))
            continue
        _, regs, flags, thumb_after, mem, fp, fpscr = result
        ok += 1
        out.write("    Case {\n")
        out.write(f'        name: "{name}",\n')
        out.write(f"        thumb: {'true' if thumb else 'false'},\n")
        out.write("        code: [" + ", ".join(f"{b:#04x}" for b in code) +
                  ", 0x00" * (16 - len(code)) + "],\n")
        out.write(f"        len: {len(code)},\n")
        out.write(f"        steps: {steps},\n")
        out.write("        after: [\n            " +
                  ", ".join(f"{r:#010x}" for r in regs) + ",\n        ],\n")
        out.write(f"        flags: {flags:#x},\n")
        out.write(f"        thumb_after: {'true' if thumb_after else 'false'},\n")
        out.write("        memory: [" + ", ".join(f"{m:#010x}" for m in mem) + "],\n")
        out.write("        fp: [" + ", ".join(f"{v:#010x}" for v in fp) + "],\n")
        out.write(f"        fpscr: {fpscr:#010x},\n")
        out.write("    },\n")
    out.write("];\n")
    target = pathlib.Path(__file__).resolve().parent.parent / "crates/arm/src/unicorn_golden.rs"
    target.write_text(out.getvalue())
    print(f"wrote {target}: {ok} cases")
    if failures:
        print("skipped:")
        for name, reason in failures:
            print(f"  {name}: {reason}")


_ENCODER_CACHE = {}


def _encode(text, thumb):
    key = (text, thumb)
    if key not in _ENCODER_CACHE:
        ks = Ks(KS_ARCH_ARM, KS_MODE_THUMB if thumb else KS_MODE_ARM)
        _ENCODER_CACHE[key] = bytes(ks.asm(text, addr=BASE)[0])
    return _ENCODER_CACHE[key]


if __name__ == "__main__":
    main()
