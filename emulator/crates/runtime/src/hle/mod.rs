//! High level emulation of the iOS libraries the guest imports.
//!
//! The executable has no dyld and no system libraries next to it: every one of
//! its ~340 imported symbols must be *provided*.  Rather than shipping stub
//! dylibs, the loader binds each import to a trampoline whose slot index maps
//! back to a symbol name, and the [`crate::Machine`] dispatches to a Rust
//! implementation here.
//!
//! Handlers are plain `fn(&mut Hle) -> Result<u32>`; the returned value lands in
//! `r0` (a handler that must produce a 64-bit result writes `r1` itself).  The
//! calling convention is AAPCS, so arguments arrive in `r0-r3` and, beyond that,
//! on the stack — for *variadic* calls, floating point arguments also live there
//! (AAPCS base standard, §5.4), which is what [`VaList`] walks.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Write as _;

use arm::{Cpu, Trap};
use guestmem::AddressSpace;

use crate::error::{Result, RuntimeError};

pub mod audio;
pub mod cf;
pub mod gl;
pub mod libc;
pub mod objc;
pub mod ui;

/// Mutable state the HLE layer keeps between calls.
#[derive(Debug)]
pub struct System {
    /// Open host files: guest fd -> contents (the game only reads its assets).
    pub files: HashMap<i32, FileHandle>,
    /// Next descriptor to hand out.
    pub next_fd: i32,
    /// Allocator bookkeeping for `malloc`/`free`.
    pub heap: Heap,
    /// `mark_time`-style clock: everything is derived from this counter.
    pub nanoseconds: u64,
    /// `arc4random`/`rand` state.
    pub rng: u64,
    /// Standard output captured from the guest (printf, NSLog, write(1)).
    pub stdout: Vec<u8>,
    /// Number of times each unimplemented symbol was called.
    pub unimplemented: HashMap<String, u64>,
    /// Free-form diagnostics for `--verbose`.
    pub log: Vec<String>,
    /// A small ring of the most recent host-side events (HLE calls, dispatches).
    /// Printed around a trap — the instruction preceding a fetch fault at 0x0
    /// is almost always the call whose return address went missing.
    pub recent_calls: VecDeque<String>,
    /// Mutable state of the Objective-C bridge (`objc_msgSend` & friends).
    pub objc: crate::hle::objc::ObjcRuntime,
    /// Verbosity of Objective-C dispatch logging: 0 silent, 1 failures,
    /// 2 every dispatch (`--objc-quiet` .. `--objc-trace`).
    pub objc_verbosity: usize,
    /// Argument vector, made available to `NSProcessInfo`-style calls.
    pub args: Vec<String>,
    /// Set when the guest calls `exit`/`abort`.
    pub exit_code: Option<i32>,
    /// `gettid`-style identity used by pthread stubs.
    pub thread_id: u64,
    /// OpenAL/OpenGL handles handed to the guest.
    pub next_handle: u32,
    /// Framebuffer the GL layer draws into (RGBA8), if a renderer is attached.
    pub framebuffer: Option<crate::hle::gl::Framebuffer>,
    /// Shadow return stack: the guest's `lr` while a HLE function transferred
    /// control into a guest method (see [`Hle::call_guest`]).
    pub return_stack: Vec<u32>,
    /// Synthetic Objective-C classes the emulator provides (UIKit, EAGL, …):
    /// class object address -> class name, and the reverse.
    pub host_classes: HashMap<u32, String>,
    pub host_classes_by_name: HashMap<String, u32>,
    /// Bump pointer for host object allocation.
    pub host_objects_top: u32,
    /// Host-side string for every CFString object handed out.
    pub cf_strings: HashMap<u32, String>,
    /// Host-side arrays for CFArray.
    pub cf_arrays: HashMap<u32, Vec<u32>>,
    /// Host-side identity for CFURL/CFBundle objects.
    pub cf_objects: HashMap<u32, crate::hle::cf::CfObject>,
    /// OpenGL ES state.
    pub gl: crate::hle::gl::Gl,
    /// Directory the game's assets are read from (`CFBundleCopyResourceURL`).
    pub bundle_path: String,
    /// The app's window size, which is what EAGL hands to the renderbuffer.
    pub window_width: u32,
    pub window_height: u32,
    /// The EAGL context the guest made current.
    pub current_eagl: u32,
    /// Frames presented / frames that were not entirely black (boot diagnostics).
    pub frames_presented: u64,
    pub frames_with_content: u64,
    /// The BSD `errno` value the guest reads through `__error`.
    pub errno: i32,
    /// Where the next anonymous `mmap` goes.
    pub anonymous_top: u32,
    /// OpenAL / AudioToolbox bookkeeping (see [`crate::hle::audio`]).
    pub audio_context: u32,
    pub audio_sample_rate: u32,
    pub audio_queues: u64,
    pub audio_buffers_queued: u64,
    pub openal_sources: u64,
    pub openal_buffers: u64,
    /// The delegate class `UIApplicationMain` was given.
    pub app_delegate_class: String,
    /// Set once `_main` has returned or the guest called `exit`.
    pub finished: bool,
}

impl Default for System {
    fn default() -> Self {
        System {
            files: HashMap::new(),
            next_fd: 3,
            heap: Heap::default(),
            nanoseconds: 0,
            rng: 0x9e37_79b9_7f4a_7c15,
            stdout: Vec::new(),
            unimplemented: HashMap::new(),
            log: Vec::new(),
            recent_calls: VecDeque::new(),
            objc: crate::hle::objc::ObjcRuntime::default(),
            objc_verbosity: 1,
            args: Vec::new(),
            exit_code: None,
            thread_id: 1,
            next_handle: 1,
            framebuffer: None,
            return_stack: Vec::new(),
            host_classes: HashMap::new(),
            host_classes_by_name: HashMap::new(),
            host_objects_top: 0,
            cf_strings: HashMap::new(),
            cf_arrays: HashMap::new(),
            cf_objects: HashMap::new(),
            gl: crate::hle::gl::Gl::default(),
            bundle_path: String::new(),
            window_width: 480,
            window_height: 320,
            current_eagl: 0,
            errno: 0,
            anonymous_top: 0x6000_0000,
            audio_context: 0,
            audio_sample_rate: 44100,
            audio_queues: 0,
            audio_buffers_queued: 0,
            openal_sources: 0,
            openal_buffers: 0,
            app_delegate_class: String::new(),
            frames_presented: 0,
            frames_with_content: 0,
            finished: false,
        }
    }
}

impl System {
    /// Record one line in the crash-diagnostics ring, at a fixed small size.
    pub fn note_recent(&mut self, line: String) {
        const RECENT: usize = 32;
        self.recent_calls.push_back(line);
        while self.recent_calls.len() > RECENT {
            self.recent_calls.pop_front();
        }
    }
}

#[derive(Debug, Default)]
pub struct Heap {
    /// Base/size of the guest heap region.
    pub base: u32,
    pub size: u32,
    /// Blocks handed out, by address.
    pub blocks: Vec<(u32, u32)>,
    /// Freed blocks, reusable.
    pub free: Vec<(u32, u32)>,
    pub top: u32,
    pub total_allocated: u64,
    pub total_freed: u64,
}

#[derive(Debug)]
pub struct FileHandle {
    pub name: String,
    pub data: Vec<u8>,
    pub pos: usize,
}

/// Variadic-argument cursor for the AAPCS base calling convention.
pub struct VaList<'a> {
    pub cpu: &'a Cpu,
    pub mem: &'a AddressSpace,
    /// Next core register (r0-r3 are consumed from index 0).
    core: usize,
    /// Next stack word, relative to the caller's stack pointer at entry.
    stack: u32,
    /// `sp` the callee saw on entry (i.e. sp at the call site).
    sp: u32,
}

impl<'a> VaList<'a> {
    /// `fixed` arguments have already been consumed from the register list.
    pub fn new(cpu: &'a Cpu, mem: &'a AddressSpace, sp: u32, fixed: usize) -> Self {
        VaList { cpu, mem, core: fixed, stack: 0, sp }
    }

    fn next_core(&mut self) -> Result<u32> {
        if self.core < 4 {
            let value = self.cpu.r[self.core];
            self.core += 1;
            Ok(value)
        } else {
            let addr = self.sp.wrapping_add(self.stack);
            self.stack += 4;
            Ok(self.mem.read_u32(addr)?)
        }
    }

    pub fn u32(&mut self) -> Result<u32> {
        self.next_core()
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.next_core()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64> {
        // The AAPCS keeps 8-byte values in an even/odd register pair, so a
        // value that would start in r3 is bumped to the stack instead, and stack
        // slots are 8-byte aligned.
        if self.core == 3 {
            self.core = 4;
            self.stack = 0;
        } else if self.core >= 4 {
            self.stack = (self.stack + 7) & !7;
        }
        let lo = self.next_core()?;
        let hi = self.next_core()?;
        Ok((lo as u64) | ((hi as u64) << 32))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    pub fn f32(&mut self) -> Result<f32> {
        // In a variadic call a `float` is promoted to `double`.
        Ok(self.f64()? as f32)
    }

    pub fn pointer(&mut self) -> Result<u32> {
        self.next_core()
    }
}

/// Everything a HLE handler is allowed to touch.
pub struct Hle<'a> {
    pub cpu: &'a mut Cpu,
    pub mem: &'a mut AddressSpace,
    pub sys: &'a mut System,
    /// Symbol currently being dispatched (diagnostics and unsupported logging).
    pub symbol: &'a str,
    /// When a handler sets this, the machine jumps there instead of returning to
    /// the caller with `r0`.  Used by `objc_msgSend`, which must transfer control
    /// to a guest method implementation.
    pub jump: Option<u32>,
}

impl<'a> Hle<'a> {
    /// Transfer control to guest code at `address`, arranging for its return to
    /// come back to whoever called this HLE function.
    ///
    /// Returns `false` — with no transfer performed — when `address` is not a
    /// place the guest can execute.  The caller must then answer the call
    /// itself, because jumping there anyway is precisely how a NULL IMP (or a
    /// HLE trampoline used as a function pointer) turns into a fetch fault at a
    /// stray address.
    pub fn call_guest(&mut self, address: u32) -> bool {
        if !crate::hle::objc::valid_method_target(self, address) {
            return false;
        }
        let lr = self.cpu.r[14];
        self.sys.return_stack.push(lr);
        self.cpu.r[14] = crate::loader::HLE_RETURN;
        self.jump = Some(address);
        true
    }
    pub fn arg(&self, index: usize) -> u32 {
        self.cpu.r[index]
    }

    pub fn arg32(&self, index: usize) -> i32 {
        self.cpu.r[index] as i32
    }

    pub fn set_r0(&mut self, value: u32) {
        self.cpu.r[0] = value;
    }

    pub fn sp(&self) -> u32 {
        self.cpu.r[13]
    }

    pub fn lp(&self) -> u32 {
        self.cpu.r[14]
    }

    /// The pointer a caller would pass as `&x`/`x`.
    pub fn read_u32(&self, addr: u32) -> Result<u32> {
        Ok(self.mem.read_u32(addr)?)
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) -> Result<()> {
        self.mem.write_u32(addr, value)?;
        Ok(())
    }

    /// Read a NUL-terminated C string, with a sanity limit.
    pub fn cstr(&self, addr: u32) -> Result<String> {
        Ok(self.mem.read_cstr(addr, 1 << 16)?)
    }

    pub fn bytes(&self, addr: u32, len: u32) -> Result<Vec<u8>> {
        Ok(self.mem.read_bytes(addr, len)?)
    }

    pub fn write_bytes(&mut self, addr: u32, bytes: &[u8]) -> Result<()> {
        self.mem.poke_bytes(addr, bytes)?;
        Ok(())
    }

    /// Write a NUL-terminated string, returning its address.
    pub fn write_cstr(&mut self, addr: u32, text: &str) -> Result<()> {
        self.write_bytes(addr, text.as_bytes())?;
        self.mem.write_u8(addr + text.len() as u32, 0)?;
        Ok(())
    }

    /// Malloc in guest memory (see [`crate::hle::libc::malloc`]).
    pub fn alloc(&mut self, size: u32, align: u32) -> Result<u32> {
        libc::guest_alloc(self, size, align)
    }

    pub fn note(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.sys.log.push(message);
    }

    /// Called for a symbol with no handler: log it once, return zero.
    pub fn unsupported(&mut self) -> u32 {
        *self.sys.unimplemented.entry(self.symbol.to_string()).or_insert(0) += 1;
        0
    }

    pub fn err(&self, code: i32) -> Result<u32> {
        Ok(code as u32)
    }
}

/// A handler: reads arguments, returns the value for `r0`.
pub type HleFn = fn(&mut Hle<'_>) -> Result<u32>;

/// The symbol table.  Names are matched with and without the Mach-O leading
/// underscore, and the `__imp_`/`dyld_stub_` decorations are stripped.
pub fn lookup(symbol: &str) -> Option<HleFn> {
    let name = normalize(symbol);
    for (candidate, handler) in libc::FUNCTIONS
        .iter()
        .chain(objc::FUNCTIONS.iter())
        .chain(cf::FUNCTIONS.iter())
        .chain(gl::FUNCTIONS.iter())
        .chain(audio::FUNCTIONS.iter())
        .chain(ui::FUNCTIONS.iter())
    {
        if *candidate == name {
            return Some(*handler);
        }
    }
    None
}

/// Strip the decorations the guest's symbols carry.
pub fn normalize(symbol: &str) -> &str {
    let name = symbol.strip_prefix("__imp_").unwrap_or(symbol);
    // Mach-O C symbols carry one leading underscore; C++ mangled names carry two
    // (`__Znwm`), so only remove one.
    name.strip_prefix('_').unwrap_or(name)
}

/// True for symbols this emulator knows it is deliberately stubbing.
pub fn is_known_stub(symbol: &str) -> bool {
    let name = normalize(symbol);
    name.starts_with("gl")
        || name.starts_with("al")
        || name.starts_with("Audio")
        || name.starts_with("CF")
        || name.starts_with("CG")
        || name.starts_with("NS")
        || name.starts_with("UI")
        || name.starts_with("objc_")
}

/// Format the `printf` family's output.
///
/// Only the conversions the engine's own logging uses are implemented, but the
/// common set is complete enough to read any string the game prints:
/// `%d %i %u %x %X %o %c %s %p %f %g %e %%`, with `-+ #0` flags, a width, a
/// precision and the `h`/`hh`/`l`/`ll`/`z`/`j`/`t` length modifiers.
pub fn format_string(fmt: &str, args: &mut VaList<'_>) -> Result<String> {
    let mut out = String::new();
    let bytes: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            out.push('%');
            break;
        }
        // Flags.
        let (mut left, mut plus, mut space, mut hash, mut zero) = (false, false, false, false, false);
        loop {
            match bytes.get(i) {
                Some('-') => left = true,
                Some('+') => plus = true,
                Some(' ') => space = true,
                Some('#') => hash = true,
                Some('0') => zero = true,
                _ => break,
            }
            i += 1;
        }
        // Width.
        let mut width = 0usize;
        while let Some(d) = bytes.get(i) {
            if let Some(v) = d.to_digit(10) {
                width = width * 10 + v as usize;
                i += 1;
            } else {
                break;
            }
        }
        // Precision.
        let mut precision: Option<usize> = None;
        if bytes.get(i) == Some(&'.') {
            i += 1;
            let mut p = 0usize;
            while let Some(d) = bytes.get(i) {
                if let Some(v) = d.to_digit(10) {
                    p = p * 10 + v as usize;
                    i += 1;
                } else {
                    break;
                }
            }
            precision = Some(p);
        }
        // Length modifier.
        let mut long_count = 0;
        while let Some(&c) = bytes.get(i) {
            match c {
                'l' => {
                    long_count += 1;
                    i += 1;
                }
                'h' | 'z' | 'j' | 't' | 'L' | 'q' => {
                    i += 1;
                }
                _ => break,
            }
        }
        let Some(&conv) = bytes.get(i) else { break };
        i += 1;

        let text = match conv {
            '%' => "%".to_string(),
            'd' | 'i' => {
                let value = if long_count >= 2 { args.u64()? as i64 } else { args.i32()? as i64 };
                let negative = value < 0;
                let magnitude = value.unsigned_abs();
                let mut digits = magnitude.to_string();
                if let Some(p) = precision {
                    while digits.len() < p {
                        digits.insert(0, '0');
                    }
                }
                let sign = if negative {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                format!("{sign}{digits}")
            }
            'u' => {
                let value = if long_count >= 2 { args.u64()? } else { args.u32()? as u64 };
                value.to_string()
            }
            'x' | 'X' => {
                let value = if long_count >= 2 { args.u64()? } else { args.u32()? as u64 };
                let mut s = if conv == 'x' { format!("{value:x}") } else { format!("{value:X}") };
                if hash && value != 0 {
                    s.insert_str(0, if conv == 'x' { "0x" } else { "0X" });
                }
                if let Some(p) = precision {
                    let prefix = if hash && value != 0 { 2 } else { 0 };
                    while s.len() - prefix < p {
                        s.insert(prefix, '0');
                    }
                }
                s
            }
            'o' => {
                let value = if long_count >= 2 { args.u64()? } else { args.u32()? as u64 };
                let s = format!("{value:o}");
                if hash {
                    format!("0{s}")
                } else {
                    s
                }
            }
            'c' => char::from_u32(args.u32()? & 0xff).unwrap_or('?').to_string(),
            's' => {
                let addr = args.pointer()?;
                if addr == 0 {
                    "(null)".to_string()
                } else {
                    let s = args.mem.read_cstr(addr, 1 << 20)?;
                    match precision {
                        Some(p) if s.len() > p => s[..p].to_string(),
                        _ => s,
                    }
                }
            }
            'p' => {
                let value = args.pointer()?;
                format!("0x{value:x}")
            }
            'f' | 'F' => {
                let value = args.f64()?;
                let p = precision.unwrap_or(6);
                let mut s = format!("{value:.p$}");
                if plus && value >= 0.0 {
                    s.insert(0, '+');
                }
                s
            }
            'e' | 'E' => {
                let value = args.f64()?;
                let p = precision.unwrap_or(6);
                let s = format!("{value:.p$e}");
                if conv == 'E' {
                    s.to_uppercase()
                } else {
                    s
                }
            }
            'g' | 'G' => {
                let value = args.f64()?;
                let p = precision.unwrap_or(6).max(1);
                let mut s = format!("{value:.p$}").trim_end_matches('0').trim_end_matches('.').to_string();
                if s.is_empty() {
                    s.push('0');
                }
                s
            }
            other => format!("%{other}"),
        };

        // Width and zero padding apply to the rendered text.
        let text = if text.len() < width {
            let pad = width - text.len();
            if left {
                format!("{text}{}", " ".repeat(pad))
            } else if zero && matches!(conv, 'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G') {
                if let Some(rest) = text.strip_prefix('-') {
                    format!("-{}{rest}", "0".repeat(pad))
                } else {
                    format!("{}{text}", "0".repeat(pad))
                }
            } else {
                format!("{}{text}", " ".repeat(pad))
            }
        } else {
            text
        };
        let _ = write!(out, "{text}");
    }
    Ok(out)
}

/// Convenience for handlers that want to build a guest string in one shot.
pub fn write_guest_cstring(hle: &mut Hle<'_>, text: &str) -> Result<u32> {
    let size = text.len() as u32 + 1;
    let addr = hle.alloc(size, 4)?;
    hle.write_cstr(addr, text)?;
    Ok(addr)
}

/// Turn a guest pointer into an optional string (0 -> `None`).
pub fn optional_cstr(hle: &Hle<'_>, addr: u32) -> Result<Option<String>> {
    if addr == 0 {
        Ok(None)
    } else {
        Ok(Some(hle.cstr(addr)?))
    }
}

/// A handler that is known but does nothing yet.
pub fn unimplemented_logged(hle: &mut Hle<'_>) -> Result<u32> {
    let name = hle.symbol.to_string();
    hle.note(format!("unimplemented: {name}"));
    Ok(hle.unsupported())
}

/// Trap helper: the guest called something that must not silently succeed.
pub fn fail(function: &str, message: impl Into<String>) -> RuntimeError {
    RuntimeError::Hle { function: function.to_string(), message: message.into() }
}

/// Instruction the HLE layer uses to stop the machine when a handler decides
/// the guest cannot continue (e.g. `abort`).
pub const HLE_ABORT_TRAP: Trap = Trap::Breakpoint { address: 0, imm: 0xff };
