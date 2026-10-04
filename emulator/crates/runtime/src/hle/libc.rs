//! `libSystem`, `libc++` (just the ABI entry points) and `libgcc`/`compiler-rt`
//! helpers.
//!
//! The game was built with clang against a static-ish iOS 4/5 SDK; the symbols
//! it imports are exactly what a C++ game of that era used: allocation, string
//! and memory routines, `printf`-family logging, `FILE*` IO for its asset files,
//! a few pthread entry points, the C++ runtime's guard/exception helpers and the
//! ARM EABI soft-float helpers (`__addsf3vfp` …).

use super::{format_string, Hle, HleFn, VaList};
use crate::error::Result;
use guestmem::{Permissions, RegionKind};

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    // ---- memory -----------------------------------------------------------
    ("malloc", malloc),
    ("calloc", calloc),
    ("realloc", realloc),
    ("free", free),
    ("memcpy", memcpy),
    ("memmove", memmove),
    ("memset", memset),
    ("memcmp", memcmp),
    ("bcopy", bcopy),
    ("bzero", bzero),
    ("memchr", memchr),
    // ---- strings ----------------------------------------------------------
    ("strlen", strlen),
    ("strcpy", strcpy),
    ("strncpy", strncpy),
    ("strcat", strcat),
    ("strncat", strncat),
    ("strcmp", strcmp),
    ("strncmp", strncmp),
    ("strcasecmp", strcasecmp),
    ("strcspn", strcspn),
    ("strchr", strchr),
    ("strrchr", strrchr),
    ("strstr", strstr),
    ("strdup", strdup),
    ("strtol", strtol),
    ("atoi", atoi),
    ("__maskrune", maskrune),
    ("__tolower", tolower),
    ("tolower", tolower),
    ("toupper", toupper),
    ("__srget", srget),
    // ---- stdio ------------------------------------------------------------
    ("printf", printf),
    ("fprintf", fprintf),
    ("sprintf", sprintf),
    ("snprintf", snprintf),
    ("vsprintf", vsprintf),
    ("vsnprintf", vsnprintf),
    ("puts", puts),
    ("fputs", fputs),
    ("fputc", fputc),
    ("putchar", putchar),
    ("perror", perror),
    ("fopen", fopen),
    ("fclose", fclose),
    ("fread", fread),
    ("fwrite", fwrite),
    ("fseek", fseek),
    ("ftell", ftell),
    ("feof", feof),
    ("ferror", ferror),
    ("fileno", fileno),
    ("fflush", fflush),
    ("flockfile", noop_zero),
    ("funlockfile", noop),
    ("setvbuf", noop_zero),
    // ---- process / time ---------------------------------------------------
    ("exit", exit),
    ("_exit", exit),
    ("abort", abort),
    ("getpid", getpid),
    ("getppid", getpid),
    ("getuid", getuid),
    ("geteuid", getuid),
    ("getgid", getuid),
    ("getegid", getuid),
    ("issetugid", noop_zero),
    ("gettimeofday", gettimeofday),
    ("mach_absolute_time", mach_absolute_time),
    ("usleep", usleep),
    ("nanosleep", usleep),
    ("srand", srand),
    ("rand", rand),
    ("random", rand),
    ("arc4random", arc4random),
    ("sysctl", sysctl),
    ("sysctlbyname", sysctl),
    ("OSAtomicAdd32", osatomic_add32),
    ("OSAtomicAdd32Barrier", osatomic_add32),
    ("sysconf", sysconf),
    // ---- pthread ----------------------------------------------------------
    ("pthread_attr_init", noop_zero),
    ("pthread_attr_destroy", noop_zero),
    ("pthread_attr_setdetachstate", noop_zero),
    ("pthread_attr_setstacksize", noop_zero),
    ("pthread_attr_getschedparam", noop_zero),
    ("pthread_attr_setschedparam", noop_zero),
    ("pthread_create", pthread_create),
    ("pthread_join", noop_zero),
    ("pthread_detach", noop_zero),
    ("pthread_self", pthread_self),
    ("pthread_mutex_init", noop_zero),
    ("pthread_mutex_destroy", noop_zero),
    ("pthread_mutex_lock", noop_zero),
    ("pthread_mutex_unlock", noop_zero),
    ("pthread_cond_init", noop_zero),
    ("pthread_cond_destroy", noop_zero),
    ("pthread_cond_signal", noop_zero),
    ("pthread_cond_broadcast", noop_zero),
    ("pthread_cond_wait", noop_zero),
    ("pthread_cond_timedwait", noop_zero),
    ("pthread_key_create", pthread_key_create),
    ("pthread_getspecific", noop_zero),
    ("pthread_setspecific", noop_zero),
    ("pthread_mutexattr_init", noop_zero),
    ("pthread_mutexattr_settype", noop_zero),
    ("pthread_mutexattr_destroy", noop_zero),
    ("pthread_once", pthread_once),
    // ---- C++ runtime ------------------------------------------------------
    ("__cxa_atexit", cxa_atexit),
    ("__cxa_guard_acquire", cxa_guard_acquire),
    ("__cxa_guard_release", noop_zero),
    ("__cxa_guard_abort", noop_zero),
    ("__cxa_pure_virtual", abort),
    ("__cxa_bad_cast", abort),
    ("__cxa_throw", cxa_throw),
    ("__cxa_begin_catch", noop_zero),
    ("__cxa_end_catch", noop),
    ("__cxa_rethrow", cxa_throw),
    ("__cxa_allocate_exception", cxa_allocate_exception),
    ("__cxa_free_exception", free),
    ("__gxx_personality_v0", noop_zero),
    ("_Unwind_SjLj_Register", noop_zero),
    ("_Unwind_SjLj_Unregister", noop),
    ("_Unwind_SjLj_Resume", abort),
    ("_Unwind_Resume", abort),
    ("__dynamic_cast", dynamic_cast),
    ("_Znwm", operator_new),
    ("_Znam", operator_new),
    ("_ZdlPv", operator_delete),
    ("_ZdaPv", operator_delete),
    ("_Znwj", operator_new),
    ("_Znaj", operator_new),
    // ---- compiler-rt soft float (VFP ABI: operands in s0/s1, result in s0) --
    ("__addsf3vfp", addsf3),
    ("__subsf3vfp", subsf3),
    ("__mulsf3vfp", mulsf3),
    ("__divsf3vfp", divsf3),
    ("__adddf3vfp", adddf3),
    ("__subdf3vfp", subdf3),
    ("__muldf3vfp", muldf3),
    ("__divdf3vfp", divdf3),
    ("__eqsf2vfp", eqsf2),
    ("__nesf2vfp", nesf2),
    ("__ltsf2vfp", ltsf2),
    ("__lesf2vfp", lesf2),
    ("__gtsf2vfp", gtsf2),
    ("__gesf2vfp", gesf2),
    ("__eqdf2vfp", eqdf2),
    ("__nedf2vfp", nedf2),
    ("__ltdf2vfp", ltdf2),
    ("__ledf2vfp", ledf2),
    ("__gtdf2vfp", gtdf2),
    ("__gedf2vfp", gedf2),
    ("__unordsf2vfp", unordsf2),
    ("__unorddf2vfp", unorddf2),
    ("__fixsfsivfp", fixsfsi),
    ("__fixunssfsivfp", fixunssfsi),
    ("__floatsisfvfp", floatsisf),
    ("__floatunssisfvfp", floatunssisf),
    ("__fixdfsivfp", fixsfsi),
    ("__fixunsdfsivfp", fixunssfsi),
    ("__floatsidfvfp", floatsidf),
    ("__floatunssidfvfp", floatunssidf),
    ("__extendsfdf2vfp", extendsfdf2),
    ("__truncdfsf2vfp", truncdfsf2),
    ("__floatdidf", floatdidf),
    ("__fixdfdi", fixdfdi),
    ("__fixsfdi", fixsfdi),
    ("__floatundisf", floatunssisf),
    ("__divsi3", divsi3),
    ("__udivsi3", udivsi3),
    ("__modsi3", modsi3),
    ("__umodsi3", umodsi3),
    ("__divdi3", divsi3),
];

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

const HEAP_BASE: u32 = 0x7100_0000;
const HEAP_CHUNK: u32 = 8 << 20;
const BLOCK_MAGIC: u32 = 0x5a11_0c00;

fn noop(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn noop_zero(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

/// The heap region is created lazily the first time the guest allocates.
fn ensure_heap(hle: &mut Hle<'_>) -> Result<()> {
    if hle.sys.heap.base != 0 {
        return Ok(());
    }
    let base = HEAP_BASE;
    hle.mem.map("heap", base, HEAP_CHUNK, Permissions::RW, RegionKind::Heap, &[])?;
    let heap = &mut hle.sys.heap;
    heap.base = base;
    heap.size = HEAP_CHUNK;
    heap.top = base;
    Ok(())
}

/// Bump/free-list allocator over the guest heap region.
pub fn guest_alloc(hle: &mut Hle<'_>, size: u32, align: u32) -> Result<u32> {
    ensure_heap(hle)?;
    let align = align.max(4).next_power_of_two();
    let total = size.max(1) + 8;
    // First fit in the free list.
    let free_index = hle
        .sys
        .heap
        .free
        .iter()
        .position(|&(_, block)| block >= total);
    if let Some(index) = free_index {
        let (addr, block) = hle.sys.heap.free.remove(index);
        if block > total + 16 {
            hle.sys.heap.free.push((addr + total, block - total));
        }
        hle.mem.write_u32(addr, size)?;
        hle.mem.write_u32(addr + 4, BLOCK_MAGIC)?;
        return Ok(addr + 8);
    }
    let heap = &mut hle.sys.heap;
    let mut base = heap.top;
    base = (base + align - 1) & !(align - 1);
    let end = base + total;
    if end > heap.base + heap.size {
        return Err(super::fail("malloc", format!("out of guest heap ({size} bytes)")));
    }
    hle.mem.write_u32(base, size)?;
    hle.mem.write_u32(base + 4, BLOCK_MAGIC)?;
    hle.sys.heap.top = end;
    hle.sys.heap.blocks.push((base, total));
    hle.sys.heap.total_allocated += total as u64;
    Ok(base + 8)
}

fn malloc(hle: &mut Hle<'_>) -> Result<u32> {
    let size = hle.arg(0);
    guest_alloc(hle, size, 8)
}

fn calloc(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let size = hle.arg(1);
    let total = count.saturating_mul(size);
    let addr = guest_alloc(hle, total, 8)?;
    let zeros = vec![0u8; total as usize];
    hle.write_bytes(addr, &zeros)?;
    Ok(addr)
}

fn realloc(hle: &mut Hle<'_>) -> Result<u32> {
    let old = hle.arg(0);
    let size = hle.arg(1);
    if old == 0 {
        return guest_alloc(hle, size, 8);
    }
    let old_size = hle.mem.read_u32(old - 8)?;
    let new = guest_alloc(hle, size, 8)?;
    let copy = old_size.min(size);
    let bytes = hle.bytes(old, copy)?;
    hle.write_bytes(new, &bytes)?;
    free_at(hle, old)?;
    Ok(new)
}

fn free_at(hle: &mut Hle<'_>, addr: u32) -> Result<()> {
    if addr == 0 {
        return Ok(());
    }
    // Guard against a pointer that was never handed out by `malloc`.
    let magic = hle.mem.read_u32(addr.wrapping_sub(4))?;
    if magic != BLOCK_MAGIC {
        hle.note(format!("free({addr:#x}): bad block magic"));
        return Ok(());
    }
    let size = hle.mem.read_u32(addr - 8)?;
    let block = size.max(1) + 8;
    hle.sys.heap.blocks.retain(|&(a, _)| a != addr - 8);
    hle.sys.heap.free.push((addr - 8, block));
    hle.sys.heap.total_freed += block as u64;
    Ok(())
}

fn free(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    free_at(hle, addr)?;
    Ok(0)
}

fn memcpy(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let src = hle.arg(1);
    let len = hle.arg(2);
    if len == 0 {
        return Ok(dst);
    }
    let bytes = hle.bytes(src, len)?;
    hle.write_bytes(dst, &bytes)?;
    Ok(dst)
}

fn memmove(hle: &mut Hle<'_>) -> Result<u32> {
    memcpy(hle)
}

fn memset(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let value = (hle.arg(1) & 0xff) as u8;
    let len = hle.arg(2);
    if len > 0 {
        let bytes = vec![value; len as usize];
        hle.write_bytes(dst, &bytes)?;
    }
    Ok(dst)
}

fn memcmp(hle: &mut Hle<'_>) -> Result<u32> {
    let a = hle.arg(0);
    let b = hle.arg(1);
    let len = hle.arg(2);
    let (x, y) = (hle.bytes(a, len)?, hle.bytes(b, len)?);
    for i in 0..len as usize {
        if x[i] != y[i] {
            return Ok((x[i] as i32 - y[i] as i32) as u32);
        }
    }
    Ok(0)
}

fn bcopy(hle: &mut Hle<'_>) -> Result<u32> {
    memcpy(hle)
}

fn bzero(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let len = hle.arg(1);
    if len > 0 {
        let zeros = vec![0u8; len as usize];
        hle.write_bytes(dst, &zeros)?;
    }
    Ok(0)
}

fn memchr(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let needle = (hle.arg(1) & 0xff) as u8;
    let len = hle.arg(2);
    let bytes = hle.bytes(addr, len)?;
    match bytes.iter().position(|&b| b == needle) {
        Some(index) => Ok(addr + index as u32),
        None => Ok(0),
    }
}

fn strlen(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    Ok(hle.cstr(addr)?.len() as u32)
}

fn strcpy(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let src = hle.arg(1);
    let text = hle.cstr(src)?;
    hle.write_cstr(dst, &text)?;
    Ok(dst)
}

fn strncpy(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let src = hle.arg(1);
    let count = hle.arg(2);
    let text = hle.cstr(src)?;
    let bytes = text.as_bytes();
    let mut out = vec![0u8; count as usize];
    let n = bytes.len().min(count as usize);
    out[..n].copy_from_slice(&bytes[..n]);
    hle.write_bytes(dst, &out)?;
    Ok(dst)
}

fn strcat(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let src = hle.arg(1);
    let head = hle.cstr(dst)?;
    let tail = hle.cstr(src)?;
    hle.write_cstr(dst, &format!("{head}{tail}"))?;
    Ok(dst)
}

fn strncat(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let src = hle.arg(1);
    let count = hle.arg(2);
    let head = hle.cstr(dst)?;
    let tail = hle.cstr(src)?;
    let add = &tail[..tail.len().min(count as usize)];
    hle.write_cstr(dst, &format!("{head}{add}"))?;
    Ok(dst)
}

fn strcmp(hle: &mut Hle<'_>) -> Result<u32> {
    let a = hle.cstr(hle.arg(0))?;
    let b = hle.cstr(hle.arg(1))?;
    Ok(match a.cmp(&b) {
        std::cmp::Ordering::Less => (-1i32) as u32,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

fn strncmp(hle: &mut Hle<'_>) -> Result<u32> {
    let a = hle.cstr(hle.arg(0))?;
    let b = hle.cstr(hle.arg(1))?;
    let n = hle.arg(2) as usize;
    let (x, y) = (a.as_bytes(), b.as_bytes());
    for i in 0..n {
        let (cx, cy) = (*x.get(i).unwrap_or(&0), *y.get(i).unwrap_or(&0));
        if cx != cy {
            return Ok((cx as i32 - cy as i32) as u32);
        }
        if cx == 0 {
            break;
        }
    }
    Ok(0)
}

fn strcasecmp(hle: &mut Hle<'_>) -> Result<u32> {
    let a = hle.cstr(hle.arg(0))?.to_lowercase();
    let b = hle.cstr(hle.arg(1))?.to_lowercase();
    Ok(match a.cmp(&b) {
        std::cmp::Ordering::Less => (-1i32) as u32,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

fn strcspn(hle: &mut Hle<'_>) -> Result<u32> {
    let text = hle.cstr(hle.arg(0))?;
    let reject = hle.cstr(hle.arg(1))?;
    let count = text.chars().take_while(|c| !reject.contains(*c)).count();
    Ok(count as u32)
}

fn strchr(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let needle = (hle.arg(1) & 0xff) as u8;
    let text = hle.cstr(addr)?;
    match text.as_bytes().iter().position(|&b| b == needle) {
        Some(index) => Ok(addr + index as u32),
        None => Ok(0),
    }
}

fn strrchr(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let needle = (hle.arg(1) & 0xff) as u8;
    let text = hle.cstr(addr)?;
    match text.as_bytes().iter().rposition(|&b| b == needle) {
        Some(index) => Ok(addr + index as u32),
        None => Ok(0),
    }
}

fn strstr(hle: &mut Hle<'_>) -> Result<u32> {
    let hay_addr = hle.arg(0);
    let needle_addr = hle.arg(1);
    let hay = hle.cstr(hay_addr)?;
    let needle = hle.cstr(needle_addr)?;
    if needle.is_empty() {
        return Ok(hay_addr);
    }
    match hay.find(&needle) {
        Some(index) => Ok(hay_addr + index as u32),
        None => Ok(0),
    }
}

fn strdup(hle: &mut Hle<'_>) -> Result<u32> {
    let text = hle.cstr(hle.arg(0))?;
    super::write_guest_cstring(hle, &text)
}

fn strtol(hle: &mut Hle<'_>) -> Result<u32> {
    let text = hle.cstr(hle.arg(0))?;
    let endptr = hle.arg(1);
    let base = hle.arg(2) as u32;
    let trimmed = text.trim_start();
    let base = if base == 0 {
        if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
            16
        } else if trimmed.starts_with('0') {
            8
        } else {
            10
        }
    } else {
        base
    };
    let digits: String = trimmed
        .chars()
        .skip(if base == 16 && (trimmed.starts_with("0x") || trimmed.starts_with("0X")) { 2 } else { 0 })
        .take_while(|c| c.is_digit(base))
        .collect();
    let value = i64::from_str_radix(&digits, base).unwrap_or(0);
    if endptr != 0 {
        let consumed = text.len() - trimmed.len() + digits.len();
        hle.write_u32(endptr, hle.arg(0) + consumed as u32)?;
    }
    Ok(value as u32)
}

fn atoi(hle: &mut Hle<'_>) -> Result<u32> {
    let text = hle.cstr(hle.arg(0))?;
    let value: i64 = text.trim().parse().unwrap_or(0);
    Ok(value as u32)
}

fn maskrune(_hle: &mut Hle<'_>) -> Result<u32> {
    // The engine only uses this through `isalpha`-style macros in string code.
    Ok(0)
}

fn tolower(hle: &mut Hle<'_>) -> Result<u32> {
    let c = hle.arg(0);
    Ok((c as u8 as char).to_ascii_lowercase() as u32)
}

fn toupper(hle: &mut Hle<'_>) -> Result<u32> {
    let c = hle.arg(0);
    Ok((c as u8 as char).to_ascii_uppercase() as u32)
}

fn srget(_hle: &mut Hle<'_>) -> Result<u32> {
    // `__srget(FILE *)`: read one character, -1 at EOF.
    Ok(0xffff_ffff)
}

// ---------------------------------------------------------------------------
// stdio
// ---------------------------------------------------------------------------

fn log_line(hle: &mut Hle<'_>, text: &str) {
    hle.sys.stdout.extend_from_slice(text.as_bytes());
    hle.sys.stdout.push(b'\n');
}

fn printf(hle: &mut Hle<'_>) -> Result<u32> {
    let fmt = hle.cstr(hle.arg(0))?;
    let sp = hle.sp();
    let text = {
        let mut va = VaList::new(&*hle.cpu, &*hle.mem, sp, 1);
        format_string(&fmt, &mut va)?
    };
    hle.sys.stdout.extend_from_slice(text.as_bytes());
    Ok(text.len() as u32)
}

fn fprintf(hle: &mut Hle<'_>) -> Result<u32> {
    let fmt = hle.cstr(hle.arg(1))?;
    let sp = hle.sp();
    let text = {
        let mut va = VaList::new(&*hle.cpu, &*hle.mem, sp, 2);
        format_string(&fmt, &mut va)?
    };
    hle.sys.stdout.extend_from_slice(text.as_bytes());
    Ok(text.len() as u32)
}

fn sprintf(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let fmt = hle.cstr(hle.arg(1))?;
    let sp = hle.sp();
    let text = {
        let mut va = VaList::new(&*hle.cpu, &*hle.mem, sp, 2);
        format_string(&fmt, &mut va)?
    };
    hle.write_cstr(dst, &text)?;
    Ok(text.len() as u32)
}

fn vsprintf(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let fmt = hle.cstr(hle.arg(1))?;
    // `va_list` on ARM is a pointer to the argument save area.
    let va_addr = hle.arg(2);
    let text = format_from_memory(hle, &fmt, va_addr)?;
    hle.write_cstr(dst, &text)?;
    Ok(text.len() as u32)
}

fn vsnprintf(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let limit = hle.arg(1);
    let fmt = hle.cstr(hle.arg(2))?;
    let va_addr = hle.arg(3);
    let text = format_from_memory(hle, &fmt, va_addr)?;
    let truncated: String = text.chars().take(limit.saturating_sub(1) as usize).collect();
    hle.write_cstr(dst, &truncated)?;
    Ok(text.len() as u32)
}

fn snprintf(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let limit = hle.arg(1);
    let fmt = hle.cstr(hle.arg(2))?;
    let sp = hle.sp();
    let text = {
        let mut va = VaList::new(&*hle.cpu, &*hle.mem, sp, 3);
        format_string(&fmt, &mut va)?
    };
    let truncated: String = text.chars().take(limit.saturating_sub(1) as usize).collect();
    hle.write_cstr(dst, &truncated)?;
    Ok(text.len() as u32)
}

/// Walk a `va_list` that already lives in guest memory (the compiler spills the
/// register save area and passes a pointer to it).  A `va_list` is a plain
/// pointer to the first anonymous argument, so the cursor starts past the four
/// register slots and walks memory from there.
fn format_from_memory(hle: &mut Hle<'_>, fmt: &str, va_addr: u32) -> Result<String> {
    let mut cpu = arm::Cpu::new();
    // Mark every core register consumed so `VaList` reads the stack.
    cpu.r[0] = va_addr;
    let mem: &guestmem::AddressSpace = &hle.mem;
    let mut va = VaList::new(&cpu, mem, va_addr, 4);
    let text = format_string(fmt, &mut va)?;
    Ok(text)
}

fn puts(hle: &mut Hle<'_>) -> Result<u32> {
    let text = hle.cstr(hle.arg(0))?;
    log_line(hle, &text);
    Ok(0)
}

fn fputs(hle: &mut Hle<'_>) -> Result<u32> {
    let text = hle.cstr(hle.arg(0))?;
    hle.sys.stdout.extend_from_slice(text.as_bytes());
    Ok(0)
}

fn fputc(hle: &mut Hle<'_>) -> Result<u32> {
    let c = hle.arg(0) as u8;
    hle.sys.stdout.push(c);
    Ok(c as u32)
}

fn putchar(hle: &mut Hle<'_>) -> Result<u32> {
    let c = hle.arg(0) as u8;
    hle.sys.stdout.push(c);
    Ok(c as u32)
}

fn perror(hle: &mut Hle<'_>) -> Result<u32> {
    let prefix = if hle.arg(0) == 0 { String::new() } else { hle.cstr(hle.arg(0))? };
    log_line(hle, &format!("{prefix}: error"));
    Ok(0)
}

/// `FILE *` objects are tiny guest allocations holding the descriptor, so the
/// guest's opaque pointers stay valid and `fileno` works.
fn make_file(hle: &mut Hle<'_>, fd: i32) -> Result<u32> {
    let addr = guest_alloc(hle, 4, 4)?;
    hle.mem.write_u32(addr, fd as u32)?;
    Ok(addr)
}

fn fopen(hle: &mut Hle<'_>) -> Result<u32> {
    let path = hle.cstr(hle.arg(0))?;
    let mode = hle.cstr(hle.arg(1))?.replace(['b', 't'], "");
    let fd = hle.sys.next_fd.max(3);
    hle.sys.next_fd = fd + 1;
    match std::fs::read(&path) {
        Ok(data) => {
            hle.sys.files.insert(fd, super::FileHandle { name: path.clone(), data, pos: 0 });
            hle.note(format!("fopen({path}, {mode}) -> {fd}"));
            make_file(hle, fd)
        }
        Err(e) => {
            // Report the failure the way libc does: NULL plus errno.
            hle.note(format!("fopen({path}, {mode}) failed: {e}"));
            Ok(0)
        }
    }
}

fn fclose(hle: &mut Hle<'_>) -> Result<u32> {
    let fp = hle.arg(0);
    if fp != 0 {
        let fd = hle.mem.read_u32(fp)? as i32;
        hle.sys.files.remove(&fd);
    }
    Ok(0)
}

fn fread(hle: &mut Hle<'_>) -> Result<u32> {
    let dst = hle.arg(0);
    let size = hle.arg(1);
    let count = hle.arg(2);
    let fp = hle.arg(3);
    let fd = hle.mem.read_u32(fp)? as i32;
    let Some(file) = hle.sys.files.get(&fd) else { return Ok(0) };
    let want = (size * count) as usize;
    let available = file.data.len().saturating_sub(file.pos);
    let n = want.min(available);
    let chunk = file.data[file.pos..file.pos + n].to_vec();
    hle.sys.files.get_mut(&fd).unwrap().pos += n;
    hle.write_bytes(dst, &chunk)?;
    if size == 0 {
        return Ok(0);
    }
    Ok((n as u32) / size)
}

fn fwrite(hle: &mut Hle<'_>) -> Result<u32> {
    let src = hle.arg(0);
    let size = hle.arg(1);
    let count = hle.arg(2);
    let fp = hle.arg(3);
    let total = size * count;
    let bytes = hle.bytes(src, total)?;
    let fd = if fp == 0 { 1 } else { hle.mem.read_u32(fp)? as i32 };
    if fd <= 2 {
        hle.sys.stdout.extend_from_slice(&bytes);
    }
    Ok(count)
}

fn fseek(hle: &mut Hle<'_>) -> Result<u32> {
    let fp = hle.arg(0);
    let offset = hle.arg(1) as i32;
    let whence = hle.arg(2);
    let fd = hle.mem.read_u32(fp)? as i32;
    let Some(file) = hle.sys.files.get_mut(&fd) else { return Ok(0xffff_ffff) };
    let new_pos = match whence {
        0 => offset as i64,
        1 => file.pos as i64 + offset as i64,
        _ => file.data.len() as i64 + offset as i64,
    }
    .clamp(0, file.data.len() as i64);
    file.pos = new_pos as usize;
    Ok(0)
}

fn ftell(hle: &mut Hle<'_>) -> Result<u32> {
    let fp = hle.arg(0);
    let fd = hle.mem.read_u32(fp)? as i32;
    Ok(match hle.sys.files.get(&fd) {
        Some(file) => file.pos as u32,
        None => 0,
    })
}

fn feof(hle: &mut Hle<'_>) -> Result<u32> {
    let fp = hle.arg(0);
    let fd = hle.mem.read_u32(fp)? as i32;
    Ok(match hle.sys.files.get(&fd) {
        Some(file) => (file.pos >= file.data.len()) as u32,
        None => 1,
    })
}

fn ferror(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn fileno(hle: &mut Hle<'_>) -> Result<u32> {
    let fp = hle.arg(0);
    Ok(hle.mem.read_u32(fp)?)
}

fn fflush(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

// ---------------------------------------------------------------------------
// process / time / randomness
// ---------------------------------------------------------------------------

fn exit(hle: &mut Hle<'_>) -> Result<u32> {
    let code = hle.arg(0) as i32;
    hle.sys.exit_code = Some(code);
    hle.sys.finished = true;
    Ok(0)
}

fn abort(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.exit_code = Some(134);
    hle.sys.finished = true;
    Err(super::fail("abort", "guest called abort()"))
}

fn getpid(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(4242)
}

fn getuid(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(501)
}

/// `gettimeofday(struct timeval *tv, struct timezone *tz)`.
fn gettimeofday(hle: &mut Hle<'_>) -> Result<u32> {
    let tv = hle.arg(0);
    let seconds = (hle.sys.nanoseconds / 1_000_000_000) as u32;
    let micros = ((hle.sys.nanoseconds / 1000) % 1_000_000) as u32;
    if tv != 0 {
        hle.mem.write_u32(tv, 1_300_000_000 + seconds)?;
        hle.mem.write_u32(tv + 4, micros)?;
    }
    Ok(0)
}

/// Mach's monotonic clock, in nanoseconds (that is what the guest's timers
/// assume when they convert with `_timing_factor`).
fn mach_absolute_time(hle: &mut Hle<'_>) -> Result<u32> {
    let now = hle.sys.nanoseconds;
    Ok((now & 0xffff_ffff) as u32)
}

fn usleep(hle: &mut Hle<'_>) -> Result<u32> {
    // Sleeps advance the virtual clock so that timed loops terminate.
    let micros = hle.arg(0) as u64;
    hle.sys.nanoseconds = hle.sys.nanoseconds.saturating_add(micros * 1000);
    Ok(0)
}

fn srand(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.rng = hle.arg(0) as u64;
    Ok(0)
}

fn next_random(hle: &mut Hle<'_>) -> u32 {
    // xorshift64*
    let mut x = hle.sys.rng.max(1);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    hle.sys.rng = x;
    (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
}

fn rand(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(next_random(hle) & 0x7fff_ffff)
}

fn arc4random(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(next_random(hle))
}

fn sysctl(hle: &mut Hle<'_>) -> Result<u32> {
    // `sysctlbyname("hw.machine")` and friends: answer with a plausible iPhone.
    let name_addr = hle.arg(0);
    let oldp = hle.arg(2);
    let oldlenp = hle.arg(3);
    let name = if name_addr == 0 { String::new() } else { hle.cstr(name_addr).unwrap_or_default() };
    let value: &[u8] = match name.as_str() {
        "hw.machine" => b"iPhone3,1\0",
        "hw.model" => b"iPhone\0",
        "hw.ncpu" => &[2, 0, 0, 0],
        "kern.osversion" => b"10B350\0",
        _ => b"",
    };
    if oldp != 0 && oldlenp != 0 && !value.is_empty() {
        let len = hle.mem.read_u32(oldlenp)?;
        let n = len.min(value.len() as u32);
        hle.write_bytes(oldp, &value[..n as usize])?;
        hle.mem.write_u32(oldlenp, value.len() as u32)?;
        return Ok(0);
    }
    Ok(0)
}

const OATOMIC_BASE: u32 = 0x7999_0000;
const OATOMIC_SIZE: u32 = 0x0004_0000;

fn osatomic_add32(hle: &mut Hle<'_>) -> Result<u32> {
    // `int32_t OSAtomicAdd32(int32_t amount, volatile int32_t *address)`
    let amount = hle.arg(0) as i32;
    let address = hle.arg(1);
    if hle.mem.region_at(address).is_none() {
        // Guests sometimes hand us a stack address from a thread that has not
        // been scheduled yet; give the atomic its own scratch cell.
        if hle.mem.region_by_name("atomic-scratch").is_none() {
            hle.mem.map("atomic-scratch", OATOMIC_BASE, OATOMIC_SIZE, Permissions::RW, RegionKind::Anonymous, &[])?;
        }
        let old = hle.mem.read_u32(OATOMIC_BASE)?;
        hle.mem.write_u32(OATOMIC_BASE, (old as i32).wrapping_add(amount) as u32)?;
        return Ok(old);
    }
    let old = hle.mem.read_u32(address)?;
    hle.mem.write_u32(address, (old as i32).wrapping_add(amount) as u32)?;
    Ok(old)
}

fn sysconf(hle: &mut Hle<'_>) -> Result<u32> {
    // _SC_PAGESIZE (29) is what the engine asks for.
    if hle.arg(0) == 29 {
        return Ok(4096);
    }
    Ok(4096)
}

// ---------------------------------------------------------------------------
// pthread (single threaded for now; see `System::threads`)
// ---------------------------------------------------------------------------

fn pthread_create(hle: &mut Hle<'_>) -> Result<u32> {
    let thread_out = hle.arg(0);
    let start = hle.arg(2);
    hle.note(format!("pthread_create(start={start:#x}) -- running on the main thread"));
    if thread_out != 0 {
        let id = crate::hle::libc::guest_alloc(hle, 64, 8)?;
        hle.mem.write_u32(thread_out, id)?;
    }
    // The thread is started lazily by the scheduler (not implemented yet); the
    // engine's worker loop is entered through the main thread instead.
    Ok(0)
}

fn pthread_self(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0x1234_5678)
}

fn pthread_key_create(hle: &mut Hle<'_>) -> Result<u32> {
    let key = hle.arg(0);
    if key != 0 {
        hle.mem.write_u32(key, 1)?;
    }
    Ok(0)
}

fn pthread_once(hle: &mut Hle<'_>) -> Result<u32> {
    // Run the initialiser the first time only.
    let once = hle.arg(0);
    let init = hle.arg(1);
    if once == 0 {
        return Ok(0);
    }
    let done = hle.mem.read_u32(once)?;
    if done == 0 {
        hle.mem.write_u32(once, 1)?;
        hle.call_guest(init);
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// C++ runtime
// ---------------------------------------------------------------------------

fn cxa_atexit(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn cxa_guard_acquire(hle: &mut Hle<'_>) -> Result<u32> {
    // Return 1 the first time the guard is seen so the initialiser runs.
    let guard = hle.arg(0);
    let state = hle.mem.read_u8(guard)?;
    if state & 1 == 0 {
        hle.mem.write_u8(guard, state | 1)?;
        Ok(1)
    } else {
        Ok(0)
    }
}

fn cxa_throw(_hle: &mut Hle<'_>) -> Result<u32> {
    Err(super::fail("__cxa_throw", "C++ exception thrown by the guest"))
}

fn cxa_allocate_exception(hle: &mut Hle<'_>) -> Result<u32> {
    let size = hle.arg(0);
    guest_alloc(hle, size, 16)
}

fn dynamic_cast(hle: &mut Hle<'_>) -> Result<u32> {
    // `void *__dynamic_cast(const void *src, ...)`: without RTTI data modelled,
    // report a failed cast, which the engine handles by falling back.
    let _ = hle.arg(0);
    Ok(0)
}

fn operator_new(hle: &mut Hle<'_>) -> Result<u32> {
    let size = hle.arg(0);
    let addr = guest_alloc(hle, size, 16)?;
    let zeros = vec![0u8; size as usize];
    hle.write_bytes(addr, &zeros)?;
    Ok(addr)
}

fn operator_delete(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    free_at(hle, addr)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// compiler-rt: VFP ABI soft float and integer division
// ---------------------------------------------------------------------------

fn addsf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.s(0), hle.cpu.vfp.s(1));
    hle.cpu.vfp.set_s(0, a + b);
    Ok(0)
}

fn subsf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.s(0), hle.cpu.vfp.s(1));
    hle.cpu.vfp.set_s(0, a - b);
    Ok(0)
}

fn mulsf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.s(0), hle.cpu.vfp.s(1));
    hle.cpu.vfp.set_s(0, a * b);
    Ok(0)
}

fn divsf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.s(0), hle.cpu.vfp.s(1));
    hle.cpu.vfp.set_s(0, a / b);
    Ok(0)
}

fn adddf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.d(0), hle.cpu.vfp.d(1));
    hle.cpu.vfp.set_d(0, a + b);
    Ok(0)
}

fn subdf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.d(0), hle.cpu.vfp.d(1));
    hle.cpu.vfp.set_d(0, a - b);
    Ok(0)
}

fn muldf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.d(0), hle.cpu.vfp.d(1));
    hle.cpu.vfp.set_d(0, a * b);
    Ok(0)
}

fn divdf3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.cpu.vfp.d(0), hle.cpu.vfp.d(1));
    hle.cpu.vfp.set_d(0, a / b);
    Ok(0)
}

fn compare_sf(a: f32, b: f32) -> u32 {
    // ARM EABI: -1 less, 0 equal, 1 greater (as an `int` in r0).
    if a.is_nan() || b.is_nan() {
        return 1;
    }
    match a.partial_cmp(&b) {
        Some(std::cmp::Ordering::Less) => u32::MAX,
        Some(std::cmp::Ordering::Greater) => 1,
        _ => 0,
    }
}

fn compare_df(a: f64, b: f64) -> u32 {
    if a.is_nan() || b.is_nan() {
        return 1;
    }
    match a.partial_cmp(&b) {
        Some(std::cmp::Ordering::Less) => u32::MAX,
        Some(std::cmp::Ordering::Greater) => 1,
        _ => 0,
    }
}

fn eqsf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_sf(hle.cpu.vfp.s(0), hle.cpu.vfp.s(1)))
}
fn nesf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_sf(hle.cpu.vfp.s(0), hle.cpu.vfp.s(1)))
}
fn ltsf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_sf(hle.cpu.vfp.s(0), hle.cpu.vfp.s(1)))
}
fn lesf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_sf(hle.cpu.vfp.s(0), hle.cpu.vfp.s(1)))
}
fn gtsf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_sf(hle.cpu.vfp.s(0), hle.cpu.vfp.s(1)))
}
fn gesf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_sf(hle.cpu.vfp.s(0), hle.cpu.vfp.s(1)))
}
fn eqdf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_df(hle.cpu.vfp.d(0), hle.cpu.vfp.d(1)))
}
fn nedf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_df(hle.cpu.vfp.d(0), hle.cpu.vfp.d(1)))
}
fn ltdf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_df(hle.cpu.vfp.d(0), hle.cpu.vfp.d(1)))
}
fn ledf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_df(hle.cpu.vfp.d(0), hle.cpu.vfp.d(1)))
}
fn gtdf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_df(hle.cpu.vfp.d(0), hle.cpu.vfp.d(1)))
}
fn gedf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(compare_df(hle.cpu.vfp.d(0), hle.cpu.vfp.d(1)))
}
fn unordsf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok((hle.cpu.vfp.s(0).is_nan() || hle.cpu.vfp.s(1).is_nan()) as u32)
}
fn unorddf2(hle: &mut Hle<'_>) -> Result<u32> {
    Ok((hle.cpu.vfp.d(0).is_nan() || hle.cpu.vfp.d(1).is_nan()) as u32)
}

fn fixsfsi(hle: &mut Hle<'_>) -> Result<u32> {
    // `__fixsfsivfp(float)` takes its argument in s0 and returns in r0.
    let value = hle.cpu.vfp.s(0) as i32;
    hle.cpu.vfp.set_s(0, value as f32);
    Ok(value as u32)
}

fn fixunssfsi(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.cpu.vfp.s(0).max(0.0) as u32;
    hle.cpu.vfp.set_s(0, value as f32);
    Ok(value)
}

fn floatsisf(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.arg(0) as i32 as f32;
    hle.cpu.vfp.set_s(0, value);
    Ok(0)
}

fn floatunssisf(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.arg(0) as f32;
    hle.cpu.vfp.set_s(0, value);
    Ok(0)
}

fn floatsidf(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.arg(0) as i32 as f64;
    hle.cpu.vfp.set_d(0, value);
    Ok(0)
}

fn floatunssidf(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.arg(0) as f64;
    hle.cpu.vfp.set_d(0, value);
    Ok(0)
}

fn floatdidf(hle: &mut Hle<'_>) -> Result<u32> {
    let value = ((hle.arg(0) as u64) | ((hle.arg(1) as u64) << 32)) as i64 as f64;
    hle.cpu.vfp.set_d(0, value);
    Ok(0)
}

fn fixdfdi(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.cpu.vfp.d(0) as i64;
    hle.cpu.r[0] = value as u32;
    hle.cpu.r[1] = (value >> 32) as u32;
    Ok(0)
}

fn fixsfdi(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.cpu.vfp.s(0) as i64;
    hle.cpu.r[0] = value as u32;
    hle.cpu.r[1] = (value >> 32) as u32;
    Ok(0)
}

fn extendsfdf2(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.cpu.vfp.s(0) as f64;
    hle.cpu.vfp.set_d(0, value);
    Ok(0)
}

fn truncdfsf2(hle: &mut Hle<'_>) -> Result<u32> {
    let value = hle.cpu.vfp.d(0) as f32;
    hle.cpu.vfp.set_s(0, value);
    Ok(0)
}

fn divsi3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.arg(0) as i32, hle.arg(1) as i32);
    if b == 0 {
        return Ok(0);
    }
    Ok(a.wrapping_div(b) as u32)
}

fn udivsi3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.arg(0), hle.arg(1));
    if b == 0 {
        return Ok(0);
    }
    Ok(a / b)
}

fn modsi3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.arg(0) as i32, hle.arg(1) as i32);
    if b == 0 {
        return Ok(0);
    }
    Ok(a.wrapping_rem(b) as u32)
}

fn umodsi3(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.arg(0), hle.arg(1));
    if b == 0 {
        return Ok(0);
    }
    Ok(a % b)
}
