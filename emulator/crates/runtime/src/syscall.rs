//! The Darwin system call layer.
//!
//! On ARM, `svc #0x80` enters the kernel with the call number in `r12`: a
//! positive number indexes the BSD table, a negative one the Mach trap table.
//! Arguments arrive in `r0-r3` (and beyond, on the stack) and the result is
//! returned in `r0`, with a BSD error signalled by `r0 = -1` plus `errno`.
//!
//! The emulator implements the calls a game of this era makes: process control,
//! file IO, memory mapping, time, and enough Mach traps for the runtime's own
//! timers and semaphores to work.

use crate::error::Result;
use crate::machine::Machine;

/// BSD error codes the guest can observe (`sys/errno.h`).
pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const EBADF: i32 = 9;
pub const EAGAIN: i32 = 35;
pub const EINVAL: i32 = 22;
pub const ENOSYS: i32 = 78;
pub const ENOTTY: i32 = 25;

/// Result of a syscall: `None` continues the machine, `Some(code)` exits it.
pub type SyscallResult = Option<i32>;

/// Set `errno` for the guest and return -1, the way libsyscall's `cerror` does.
fn fail(m: &mut Machine, error: i32) -> Result<SyscallResult> {
    m.sys.errno = error;
    m.cpu.r[0] = 0xffff_ffff;
    Ok(None)
}

fn ok(m: &mut Machine, value: u32) -> Result<SyscallResult> {
    m.cpu.r[0] = value;
    Ok(None)
}

/// Arguments beyond r0-r3 live on the stack; `sp` points at the return address
/// the guest's `svc` sequence never pushed, so index 0 is [sp].
fn stack_arg(m: &Machine, index: usize) -> u32 {
    m.mem.read_u32(m.cpu.r[13] + (index as u32) * 4).unwrap_or(0)
}

pub fn dispatch(m: &mut Machine, number: i32) -> Result<SyscallResult> {
    if number < 0 {
        return mach_trap(m, (-number) as u32);
    }
    match number {
        // ---- process control -------------------------------------------
        1 => {
            let code = m.cpu.r[0] as i32;
            m.log(format!("exit({code})"));
            Ok(Some(code))
        }
        20 => ok(m, 4242),                 // getpid
        24 => ok(m, 501),                  // getuid
        25 => ok(m, 501),                  // geteuid
        39 => ok(m, 1),                    // getppid
        47 => ok(m, 20),                   // getgid
        43 => ok(m, 20),                   // getegid
        147 => ok(m, 0),                   // issetugid
        33 => ok(m, 0),                    // access: allow
        46 | 48 | 49 | 52 => ok(m, 0),     // sigaction / sigprocmask / getlogin / sigpending
        54 => {
            // ioctl: report "not a terminal" for the calls the engine makes.
            fail(m, ENOTTY)
        }
        92 => ok(m, 0),                    // fcntl: F_GETFL etc.
        93 => ok(m, 0),                    // select: no descriptors ready
        // ---- file IO -----------------------------------------------------
        3 => read(m),
        4 => write(m),
        5 => open(m),
        199 => lseek(m),
        188 | 189 | 190 => stat(m),        // stat / fstat / lstat
        202 => sysctl(m),
        116 => gettimeofday(m),
        73 => munmap(m),
        74 => mprotect(m),
        197 => mmap(m),
        6 => ok(m, 0),                     // close
        90 => ok(m, 0),                    // setpriority
        _ => {
            m.log(format!("unimplemented BSD syscall {number}"));
            fail(m, ENOSYS)
        }
    }
}

fn read(m: &mut Machine) -> Result<SyscallResult> {
    // stdin is not connected; report end of file so read loops terminate.
    let _fd = m.cpu.r[0];
    ok(m, 0)
}

fn write(m: &mut Machine) -> Result<SyscallResult> {
    let fd = m.cpu.r[0] as i32;
    let buffer = m.cpu.r[1];
    let count = m.cpu.r[2];
    if fd == 1 || fd == 2 {
        let bytes = m.mem.read_bytes(buffer, count).unwrap_or_default();
        m.sys.stdout.extend_from_slice(&bytes);
    }
    let written = count;
    m.cpu.r[0] = written;
    Ok(None)
}

fn open(m: &mut Machine) -> Result<SyscallResult> {
    let path = m.mem.read_cstr(m.cpu.r[0], 4096).unwrap_or_default();
    match std::fs::read(&path) {
        Ok(data) => {
            let fd = m.sys.next_fd;
            m.sys.next_fd += 1;
            m.sys.files.insert(fd, hle::FileHandle { name: path.clone(), data, pos: 0 });
            m.log(format!("open({path}) -> {fd}"));
            return ok(m, fd as u32);
        }
        Err(_) => {
            m.log(format!("open({path}) failed"));
            return fail(m, ENOENT);
        }
    }
}

fn lseek(m: &mut Machine) -> Result<SyscallResult> {
    let fd = m.cpu.r[0] as i32;
    let offset = m.cpu.r[1] as i32 as i64;
    let whence = m.cpu.r[2];
    if let Some(file) = m.sys.files.get_mut(&fd) {
        let base = match whence {
            1 => file.pos as i64,
            2 => file.data.len() as i64,
            _ => 0,
        };
        file.pos = (base + offset).clamp(0, file.data.len() as i64) as usize;
        let pos = file.pos as u32;
        return ok(m, pos);
    }
    fail(m, EBADF)
}

/// `stat`/`fstat`: the engine only checks whether a file exists and how big it
/// is, so fill a plausible `struct stat` (`st_mode`, `st_size`).
fn stat(m: &mut Machine) -> Result<SyscallResult> {
    let fd = m.cpu.r[0] as i32;
    let out = m.cpu.r[1];
    let size = m.sys.files.get(&fd).map(|f| f.data.len() as u32).unwrap_or(0);
    if out != 0 {
        // Darwin arm `struct stat`: mode at +4, size at +0x30 for the 32-bit ABI.
        m.mem.write_u32(out + 4, 0o100644)?;
        m.mem.write_u32(out + 0x30, size)?;
    }
    m.cpu.r[0] = 0;
    Ok(None)
}

fn sysctl(m: &mut Machine) -> Result<SyscallResult> {
    // `sysctl(name, namelen, oldp, oldlenp, newp, newlen)`.
    let name = m.cpu.r[0];
    let oldp = m.cpu.r[2];
    let oldlenp = m.cpu.r[3];
    let first = m.mem.read_u32(name).unwrap_or(0);
    let second = m.mem.read_u32(name + 4).unwrap_or(0);
    // CTL_HW(6), HW_PAGESIZE(7) is what the C++ runtime asks for.
    let value: &[u8] = match (first, second) {
        (6, 7) => &4096u32.to_le_bytes(),
        (6, 3) => &2u32.to_le_bytes(),
        (1, 1) => b"Darwin\0",
        _ => &0u32.to_le_bytes(),
    };
    if oldp != 0 && oldlenp != 0 {
        let len = m.mem.read_u32(oldlenp)? as usize;
        let n = len.min(value.len());
        m.mem.poke_bytes(oldp, &value[..n])?;
        m.mem.write_u32(oldlenp, value.len() as u32)?;
    }
    m.cpu.r[0] = 0;
    Ok(None)
}

fn gettimeofday(m: &mut Machine) -> Result<SyscallResult> {
    let tv = m.cpu.r[0];
    let seconds = 1_300_000_000u32 + (m.sys.nanoseconds / 1_000_000_000) as u32;
    let micros = ((m.sys.nanoseconds / 1000) % 1_000_000) as u32;
    if tv != 0 {
        m.mem.write_u32(tv, seconds)?;
        m.mem.write_u32(tv + 4, micros)?;
    }
    m.cpu.r[0] = 0;
    Ok(None)
}

fn mmap(m: &mut Machine) -> Result<SyscallResult> {
    // `mmap(addr, len, prot, flags, fd, offset)`
    let address = m.cpu.r[0];
    let length = m.cpu.r[1];
    let prot = m.cpu.r[2];
    let _flags = m.cpu.r[3];
    let fd = stack_arg(m, 0) as i32;
    let offset = stack_arg(m, 1);
    let page = guestmem::PAGE_SIZE;
    let size = (length + page - 1) & !(page - 1);
    let perms = guestmem::Permissions::from_vm_prot(prot & 7);
    let base = if address != 0 {
        address & !(page - 1)
    } else {
        m.sys.anonymous_top
    };
    let name = format!("mmap-{:#x}", base);
    let init = if fd >= 0 {
        m.sys.files.get(&fd).map(|f| {
            let start = offset as usize;
            let end = (start + size as usize).min(f.data.len());
            if start < end {
                f.data[start..end].to_vec()
            } else {
                Vec::new()
            }
        })
    } else {
        None
    };
    match m.mem.map(name, base, size, perms, guestmem::RegionKind::Heap, init.as_deref().unwrap_or(&[])) {
        Ok(_) => {
            if address == 0 {
                m.sys.anonymous_top = base + size;
            }
            m.cpu.r[0] = base;
            return Ok(None);
        }
        Err(e) => {
            m.log(format!("mmap({base:#x}, {size:#x}) failed: {e}"));
            return fail(m, EINVAL);
        }
    }
}

fn munmap(m: &mut Machine) -> Result<SyscallResult> {
    let address = m.cpu.r[0];
    let length = m.cpu.r[1];
    if let Err(e) = m.mem.unmap(address, length.max(guestmem::PAGE_SIZE)) {
        m.log(format!("munmap({address:#x}, {length:#x}): {e}"));
    }
    m.cpu.r[0] = 0;
    Ok(None)
}

fn mprotect(m: &mut Machine) -> Result<SyscallResult> {
    let address = m.cpu.r[0];
    let length = m.cpu.r[1];
    let prot = m.cpu.r[2];
    if let Err(e) = m.mem.protect(address, length, guestmem::Permissions::from_vm_prot(prot & 7)) {
        m.log(format!("mprotect({address:#x}, {length:#x}): {e}"));
    }
    m.cpu.r[0] = 0;
    Ok(None)
}

/// Mach traps: indexed by `-r12`.
fn mach_trap(m: &mut Machine, index: u32) -> Result<SyscallResult> {
    match index {
        // mach_absolute_time
        3 => {
            let now = m.sys.nanoseconds;
            m.cpu.r[0] = now as u32;
            m.cpu.r[1] = (now >> 32) as u32;
        }
        // mach_reply_port / thread_self_trap / task_self_trap
        26 => m.cpu.r[0] = 0x1000_0001,
        27 => m.cpu.r[0] = 0x2000_0001,
        28 => m.cpu.r[0] = 0x3000_0001,
        // mach_msg_trap: no ports exist, so report a receive timeout
        31 => {
            m.cpu.r[0] = 0x1000_4003;
        }
        // semaphore_signal_trap / semaphore_wait_trap
        33 | 34 | 35 | 36 | 37 => m.cpu.r[0] = 0,
        _ => {
            m.log(format!("unimplemented Mach trap {index}"));
            m.cpu.r[0] = 0xffff_ffff;
        }
    }
    Ok(None)
}

use crate::hle;
