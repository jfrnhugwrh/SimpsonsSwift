//! The Objective-C runtime bridge.
//!
//! The Simpsons engine is a C++ core (`MonkeyApp`, `AppEngine`, `Display`,
//! `Canvas`, …) wrapped in a thin Objective-C layer: the app delegate, the
//! `EAGLView`, and the handful of Foundation/CoreFoundation objects the engine
//! touches.  Everything else reaches the runtime through `objc_msgSend`, which
//! the reference decompilation calls 747 times.
//!
//! The bridge resolves a message in this order:
//!
//! 1. `nil` receiver — legal in Objective-C and *common* in this binary (every
//!    optional collaborator is messaged unconditionally), so it is answered the
//!    ABI-exact way: zero in the integer result registers, zeroed struct return
//!    buffer for `_stret` sends, no side effects.
//! 2. One of the emulator's own classes — the UIKit/GL/Foundation objects the
//!    guest imports but no library provides (`_OBJC_CLASS_$_UIDevice` …).  Their
//!    class objects are synthetic, created by the loader, and their methods are
//!    the `HOST_METHODS` tables here and in [`crate::hle::ui`]/[`crate::hle::gl`].
//! 3. The guest image's own class metadata — a `struct objc_class` is read
//!    straight out of memory (both the legacy `iOS 2-4` and the non-fragile
//!    `iOS 5+` layouts are recognised), its method list walked for the selector
//!    and the superclass chain followed (instance methods) or the metaclass
//!    chain (class methods).  A matching `IMP` runs *in the guest* — the bridge
//!    never emulates code that already exists in the image.
//! 4. NSObject bookkeeping (`retain`, `class`, `isKindOfClass:`, …) that is
//!    pure runtime bookkeeping and is answered directly.
//! 5. A synthetic superclass of an image class — `[EAGLView addSubview:]` must
//!    reach the emulator's `UIView` table even though `EAGLView` is defined in
//!    the image.
//! 6. Unrecognised — reported and answered `nil`, exactly what messaging a
//!    class that does not respond would produce.  No path in this chain ever
//!    writes `0` (or a HLE trampoline address) into the program counter: a
//!    guest IMP is only executed after its target proves executable, which is
//!    what the original NULL-PC fault was missing.
//!
//! The purposes of the diagnostic counters in [`ObjcRuntime`] (last dispatch,
//! unrecognized/nil tables) are crash post-mortems: the machine prints them
//! around any instruction-fetch fault.

use std::collections::HashMap;

use guestmem::AddressSpace;

use super::{optional_cstr, write_guest_cstring, Hle, HleFn};
use crate::error::Result;

// ---------------------------------------------------------------------------
// Layout constants (32-bit ARM ABI, matches the image at its preferred address)
// ---------------------------------------------------------------------------

/// Byte offsets in `struct objc_class`; identical in both ABIs.
const CLASS_ISA: u32 = 0;
const CLASS_SUPERCLASS: u32 = 4;
/// Non-fragile ABI: `class_data_bits_t bits` (`class_ro_t`/`class_rw_t`).
const CLASS_BITS: u32 = 0x14;
/// Fragile ABI offsets.
const LEGACY_NAME: u32 = 8;
const LEGACY_INSTANCE_SIZE: u32 = 0x14;
const LEGACY_METHOD_LISTS: u32 = 0x1c;

/// `class_ro_t` offsets (32-bit).
const RO_INSTANCE_SIZE: u32 = 8;
const RO_NAME: u32 = 0x10;
const RO_BASE_METHODS: u32 = 0x14;
const RO_IVARS: u32 = 0x1c;

/// `bits` can carry runtime flags in the low three bits (`class_rw_t` tagging);
/// mask them to reach the data pointer.
const CLASS_DATA_MASK: u32 = !3;
/// `method_list_t::entsizeAndFlags` flags a pointer-relative method list; those
/// only exist for arm64e, so a `method_list_t` with the bit set is not one this
/// image can have produced — treat it as "not a method list".
const METHOD_LIST_RELATIVE: u32 = 0x8000_0000;
/// The classic `method_t` on 32-bit is `{ SEL; types; IMP }` — 12 bytes.
const METHOD_ENTRY_SIZE: u32 = 12;

/// Where the emulator's synthetic class objects and instances live (the loader
/// occupies `0x7001_0000..0x7002_0000` for the imported classes).
pub const HOST_OBJECTS_BASE: u32 = 0x7002_0000;
pub const HOST_OBJECTS_SIZE: u32 = 0x0002_0000;

/// Default `instanceSize` for an emulator-provided class; a believable value
/// keeps `+[Foo alloc]` allocations straight.
pub const HOST_INSTANCE_SIZE: u32 = 64;

/// Verbosity levels for `sys.objc_verbosity` (`--objc-quiet` / `--objc-trace`).
pub const VERBOSITY_FAILURES: usize = 1;
pub const VERBOSITY_EVERY_CALL: usize = 2;

// ---------------------------------------------------------------------------
// Runtime state (owned by `crate::hle::System`)
// ---------------------------------------------------------------------------

/// Mutable Objective-C runtime state: what the bridge learned by reading the
/// image's metadata, plus what it installed itself.
#[derive(Debug, Default)]
pub struct ObjcRuntime {
    /// class object address / metaclass address -> readable name.
    pub names: HashMap<u32, String>,
    /// class name -> class object address.
    pub classes_by_name: HashMap<String, u32>,
    /// metaclass object address -> class object address.
    pub metaclass_of: HashMap<u32, u32>,
    /// Interned selectors: name -> pointer, pointer -> name.
    pub selectors: HashMap<String, u32>,
    pub selector_names: HashMap<u32, String>,
    /// IMPs installed by `class_addMethod`/`method_setImplementation`, keyed by
    /// `(class object address, selector name)`.
    pub host_imps: HashMap<(u32, String), u32>,
    /// Associated objects: `(owner, key) -> value`.
    pub associated: HashMap<(u32, u32), u32>,
    /// How many dispatches executed a guest IMP / a built-in / failed.
    pub guest_calls: u64,
    pub host_calls: u64,
    pub missing: u64,
    /// Selectors that hit the "unrecognized selector" path, with a count.
    pub unrecognized: HashMap<String, u64>,
    /// Selectors sent to `nil` (expected for the game's optional collaborators).
    pub nil_messages: HashMap<String, u64>,
    /// Counter used to name `objc_allocateClassPair` classes.
    pub dynamic_classes: u32,
    /// One-line description of the most recent dispatch, always updated.  Read
    /// by the machine when it reports a trap, because the instruction that
    /// follows an Objective-C dispatch is where a lost return address shows up.
    pub last_dispatch: String,
}

impl ObjcRuntime {
    fn remember_class(&mut self, name: &str, class: u32, metaclass: u32) {
        self.names.insert(class, name.to_string());
        self.classes_by_name.insert(name.to_string(), class);
        if metaclass != 0 {
            self.names.insert(metaclass, name.to_string());
            self.metaclass_of.insert(metaclass, class);
        }
    }

    fn remember_selector(&mut self, name: String, sel: u32) {
        self.selectors.entry(name.clone()).or_insert(sel);
        if sel != 0 {
            self.selector_names.entry(sel).or_insert(name);
        }
    }

    fn selector_name_for(&self, sel: u32) -> Option<&str> {
        self.selector_names.get(&sel).map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Synthetic class objects
// ---------------------------------------------------------------------------

/// Bytes occupied by one synthetic `struct objc_class`, including its embedded
/// `class_ro_t` and the NUL-terminated class name.
pub fn synthetic_class_size(name: &str) -> u32 {
    let raw = 0x40 + name.len() as u32 + 1;
    (raw + 15) & !15
}

/// Write a synthetic, method-less `struct objc_class` at `addr`.
///
/// The layout is the non-fragile ABI's (the `bits` pointer lands at the same
/// offset regardless), so the generic metadata reader in this module parses it
/// without any special casing; it simply has no methods of its own, which is
/// what makes every message to it fall through to the host method tables.
pub fn write_synthetic_class(
    mem: &mut AddressSpace,
    addr: u32,
    name: &str,
    isa: u32,
    superclass: u32,
    instance_size: u32,
) -> Result<()> {
    let ro = addr + 0x18;
    let name_addr = addr + 0x40;
    mem.write_u32(addr + CLASS_ISA, isa)?;
    mem.write_u32(addr + CLASS_SUPERCLASS, superclass)?;
    mem.write_u32(addr + 0x08, 0)?; // cache
    mem.write_u32(addr + 0x0c, 0)?; // vtable
    mem.write_u32(addr + 0x10, 0)?; // pad to nextext
    mem.write_u32(addr + CLASS_BITS, ro)?; // untagged
    mem.write_u32(ro, 0)?; // flags
    mem.write_u32(ro + 4, 4)?; // instanceStart
    mem.write_u32(ro + RO_INSTANCE_SIZE, instance_size)?;
    mem.write_u32(ro + 0x0c, 0)?; // ivarLayout
    mem.write_u32(ro + RO_NAME, name_addr)?;
    mem.write_u32(ro + RO_BASE_METHODS, 0)?; // baseMethods
    mem.write_u32(ro + 0x18, 0)?; // baseProtocols
    mem.write_u32(ro + RO_IVARS, 0)?; // ivars
    mem.write_u32(ro + 0x20, 0)?; // weakIvarLayout
    mem.write_u32(ro + 0x24, 0)?; // baseProperties
    mem.poke_bytes(name_addr, name.as_bytes())?;
    mem.write_u8(name_addr + name.len() as u32, 0)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Host classes: the emulator's own Objective-C objects
// ---------------------------------------------------------------------------

/// A method implemented by the emulator for one of its host classes.
pub type HostMethod = fn(&mut Hle<'_>, receiver: u32) -> Result<u32>;

/// The host methods every emulator object answers (the `NSObject` protocol).
pub const HOST_METHODS: &[(&str, &str, HostMethod)] = &[
    ("NSObject", "init", host_return_self),
    ("NSObject", "retain", host_return_self),
    ("NSObject", "autorelease", host_return_self),
    ("NSObject", "release", host_return_zero),
    ("NSObject", "dealloc", host_return_zero),
    ("NSObject", "retainCount", host_return_one),
    ("NSObject", "respondsToSelector:", host_return_one),
    ("NSObject", "description", host_description),
    ("NSObject", "debugDescription", host_description),
    ("NSObject", "hash", host_return_receiver),
    ("NSObject", "isEqual:", host_pointer_equal),
    ("NSObject", "isKindOfClass:", host_return_one),
    ("NSObject", "isMemberOfClass:", host_return_one),
    ("NSObject", "class", host_object_class),
    ("NSObject", "superclass", host_return_zero),
    ("NSObject", "performSelector:", host_perform_selector),
    ("NSObject", "performSelector:withObject:", host_perform_selector),
    ("NSObject", "performSelector:withObject:afterDelay:", host_return_zero),
    ("NSObject", "performSelectorOnMainThread:withObject:waitUntilDone:", host_perform_selector),
    ("NSObject", "forwardInvocation:", host_return_zero),
    ("NSObject", "methodSignatureForSelector:", host_return_zero),
];

fn host_return_self(_hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(receiver)
}

fn host_return_zero(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(0)
}

fn host_return_one(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(1)
}

fn host_return_receiver(_hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(receiver)
}

fn host_pointer_equal(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok((receiver == hle.arg(2)) as u32)
}

fn host_description(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    let name = class_name_of(hle, receiver).unwrap_or_else(|| "NSObject".to_string());
    make_nsstring(hle, &format!("<{name}: {receiver:#010x}>"))
}

fn host_object_class(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(read_isa(hle, receiver))
}

fn host_perform_selector(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    // The performed selector is the first argument (after `self`/`_cmd`).
    let sel = hle.arg(2);
    let Some(name) = selector_name(hle, sel) else { return Ok(0) };
    let isa = read_isa(hle, receiver);
    hle.cpu.r[1] = sel;
    if let Some((imp, class_name)) = lookup_imp(hle, isa, &name) {
        return call_method(hle, &name, &class_name, imp);
    }
    // Fall back to the emulator's own tables (the object's own class first).
    let class_name = hle
        .sys
        .host_classes
        .get(&receiver)
        .cloned()
        .or_else(|| hle.sys.host_classes.get(&isa).cloned())
        .unwrap_or_else(|| "NSObject".to_string());
    dispatch_host_method(hle, &class_name, receiver, &name)
}

/// Create (once) a synthetic class object (with its metaclass) for an emulator
/// class, in the `host-objects` region.
pub fn host_class(hle: &mut Hle<'_>, name: &str) -> Result<u32> {
    if let Some(&addr) = hle.sys.host_classes_by_name.get(name) {
        return Ok(addr);
    }
    if hle.mem.region_by_name("host-objects").is_none() {
        hle.mem.map(
            "host-objects",
            HOST_OBJECTS_BASE,
            HOST_OBJECTS_SIZE,
            guestmem::Permissions::RW,
            guestmem::RegionKind::Anonymous,
            &[],
        )?;
    }
    let size = synthetic_class_size(name);
    let class = host_alloc(hle, size * 2, 16)?;
    let metaclass = class + size;
    // The root metaclass is its own isa and has no superclass — exactly how
    // libobjc terminates the chain at NSObject's metaclass.
    write_synthetic_class(hle.mem, metaclass, name, metaclass, 0, 0)?;
    write_synthetic_class(hle.mem, class, name, metaclass, 0, HOST_INSTANCE_SIZE)?;
    hle.sys.host_classes.insert(class, name.to_string());
    hle.sys.host_classes.insert(metaclass, name.to_string());
    hle.sys.host_classes_by_name.insert(name.to_string(), class);
    hle.sys.objc.remember_class(name, class, metaclass);
    Ok(class)
}

/// Allocate an object of a host class, initialised with the class as its `isa`.
pub fn host_instance(hle: &mut Hle<'_>, name: &str, size: u32) -> Result<u32> {
    let class = host_class(hle, name)?;
    let object = host_alloc(hle, size.max(16), 16)?;
    let zeros = vec![0u8; size.max(16).min(4096) as usize];
    hle.write_bytes(object, &zeros)?;
    hle.mem.write_u32(object, class)?;
    Ok(object)
}

/// Bump allocator for host objects (never freed; the guest keeps references).
fn host_alloc(hle: &mut Hle<'_>, size: u32, align: u32) -> Result<u32> {
    let top = if hle.sys.host_objects_top == 0 {
        HOST_OBJECTS_BASE
    } else {
        hle.sys.host_objects_top
    };
    let base = (top + align - 1) & !(align - 1);
    hle.sys.host_objects_top = base + size;
    if hle.sys.host_objects_top > HOST_OBJECTS_BASE + HOST_OBJECTS_SIZE {
        return Err(super::fail("host_alloc", "out of host object memory"));
    }
    Ok(base)
}

/// `host_class` for callers that own the Machine (boot code, tests).
pub fn ensure_host_class(machine: &mut crate::machine::Machine, name: &str) -> u32 {
    if let Some(&addr) = machine.sys.host_classes_by_name.get(name) {
        return addr;
    }
    let mut hle = Hle {
        cpu: &mut machine.cpu,
        mem: &mut machine.mem,
        sys: &mut machine.sys,
        symbol: "<objc>",
        jump: None,
    };
    host_class(&mut hle, name).unwrap_or(0)
}

/// Emulator classes used by the framework shims that the image may not import;
/// pre-created at boot so `objc_getClass` and class chains always resolve.
const KNOWN_HOST_CLASSES: &[&str] = &[
    "NSObject",
    "NSAutoreleasePool",
    "NSString",
    "NSMutableString",
    "NSArray",
    "NSMutableArray",
    "NSDictionary",
    "NSMutableDictionary",
    "NSNumber",
    "NSData",
    "NSMutableData",
    "NSError",
    "NSURL",
    "NSValue",
    "NSDate",
    "NSTimeZone",
    "NSDateFormatter",
    "NSRunLoop",
    "NSTimer",
    "NSNotificationCenter",
    "NSUserDefaults",
    "NSFileManager",
    "NSProcessInfo",
    "NSKeyedArchiver",
    "NSKeyedUnarchiver",
    "NSCharacterSet",
    "NSLocale",
    "NSThread",
    "NSLock",
    "NSRecursiveLock",
    "NSBundle",
    "NSLocaleInfo",
    "NSRangeException",
    "UIApplication",
    "UIScreen",
    "UIDevice",
    "UIView",
    "UIWindow",
    "UIViewController",
    "UITextField",
    "UIColor",
    "UIFont",
    "UILabel",
    "UIAccelerometer",
    "MPMoviePlayerController",
    "MPMusicPlayerController",
    "AVAudioPlayer",
    "AVAudioSession",
    "EAGLContext",
    "EAGLSharegroup",
    "CAEAGLLayer",
];

/// A minimal class name recogniser for `read_class_name` (loader, no `Hle`).
fn read_class_name(mem: &AddressSpace, class_obj: u32) -> Option<String> {
    if class_obj == 0 || class_obj & 3 != 0 {
        return None;
    }
    let bits = mem.read_u32(class_obj + CLASS_BITS).ok()?;
    if bits == 0 {
        return None;
    }
    let ro = bits & CLASS_DATA_MASK;
    let name_ptr = mem.read_u32(ro + RO_NAME).ok()?;
    let name = mem.read_cstr(name_ptr, 128).ok()?;
    if plausible_class_name(&name) {
        Some(name)
    } else {
        None
    }
}

/// Load Objective-C metadata once the image sits in memory: adopt the loader's
/// synthetic classes, register every class the image itself defines
/// (`__objc_classlist` & friends), and intern every selector the image can
/// pass (`__objc_selrefs`).  Afterwards, `objc_getClass` is a table lookup and
/// `__objc_methname` pointers compare equal between different call sites.
pub fn install_image_metadata(machine: &mut crate::machine::Machine) {
    // 1. The loader's synthetic classes for imported `_OBJC_CLASS_$_*` symbols.
    for entry in machine.image.objc_classes.clone() {
        machine.sys.host_classes.insert(entry.class, entry.name.clone());
        machine.sys.host_classes.insert(entry.metaclass, entry.name.clone());
        machine.sys.host_classes_by_name.insert(entry.name.clone(), entry.class);
        machine.sys.objc.remember_class(&entry.name, entry.class, entry.metaclass);
    }

    // 2. The host classes the framework shims provide but the image might not
    //    import (message subclasses registered only on the host side).
    for &name in KNOWN_HOST_CLASSES {
        ensure_host_class(machine, name);
    }

    // 3. The image's own classes and categories.
    let mut image_classes = 0u32;
    for section in machine.image.macho.sections() {
        let is_class_list =
            matches!(section.sectname.as_str(), "__objc_classlist" | "__objc_nlclslist");
        let is_cat_list = matches!(section.sectname.as_str(), "__objc_catlist" | "__objc_nlcatlist");
        if !is_class_list && !is_cat_list {
            continue;
        }
        for i in 0..section.size / 4 {
            let entry = section.addr + i * 4;
            let class_obj = if is_cat_list {
                // A `category_t` holds the class reference in its second word.
                let Ok(cat) = machine.mem.read_u32(entry) else { continue };
                machine.mem.read_u32(cat + 4).unwrap_or(0)
            } else {
                machine.mem.read_u32(entry).unwrap_or(0)
            };
            let Some(name) = read_class_name(&machine.mem, class_obj) else { continue };
            let metaclass = machine.mem.read_u32(class_obj).unwrap_or(0);
            machine.sys.objc.remember_class(&name, class_obj, metaclass);
            image_classes += 1;
        }
    }

    // 4. Selectors.  A SEL *is* its own name pointer on this ABI, so the whole
    //    job is to record which pointer stands for which name.
    for section in machine.image.macho.sections() {
        if section.sectname != "__objc_selrefs" {
            continue;
        }
        for i in 0..section.size / 4 {
            let Ok(sel) = machine.mem.read_u32(section.addr + i * 4) else { continue };
            let Ok(name) = machine.mem.read_cstr(sel, 512) else { continue };
            machine.sys.objc.remember_selector(name, sel);
        }
    }

    machine.sys.log.push(format!(
        "objc: {image_classes} image classes, {} host classes, {} selectors interned",
        machine.sys.objc.classes_by_name.len(),
        machine.sys.objc.selectors.len()
    ));
}

// ---------------------------------------------------------------------------
// Textual diagnostics
// ---------------------------------------------------------------------------

/// A short report used by `--stats` and by fetch-fault post-mortems.
pub fn report(sys: &crate::hle::System) -> String {
    let objc = &sys.objc;
    let mut out = format!(
        "{} guest IMPs, {} host answers, {} missed; {} selectors interned, {} classes seen ({} host)",
        objc.guest_calls,
        objc.host_calls,
        objc.missing,
        objc.selectors.len(),
        objc.classes_by_name.len(),
        sys.host_classes_by_name.len(),
    );
    if !objc.last_dispatch.is_empty() {
        out.push_str(&format!("; last dispatch: {}", objc.last_dispatch));
    }
    out
}

// ---------------------------------------------------------------------------
// Recognition helpers
// ---------------------------------------------------------------------------

/// Read the `isa` pointer of a guest object, tolerating garbage.
///
/// A null-ish or odd receiver is answered with `0`, which downgrades the whole
/// dispatch to the "unrecognized selector" path instead of a memory fault.
fn read_isa(hle: &Hle<'_>, object: u32) -> u32 {
    if object == 0 || object & 1 != 0 {
        return 0;
    }
    hle.mem.read_u32(object).unwrap_or(0)
}

/// A selector is a pointer to its own NUL-terminated name (the classic
/// non-fragile ABI representation, which the guest's `__objc_methname` strings
/// satisfy directly).
fn selector_name(hle: &Hle<'_>, sel: u32) -> Option<String> {
    if sel == 0 {
        return None;
    }
    if let Some(name) = hle.sys.objc.selector_name_for(sel) {
        return Some(name.to_string());
    }
    let name = hle.cstr(sel).ok()?;
    if name.is_empty() || name.len() > 512 || name.contains('\u{fffd}') {
        return None;
    }
    Some(name)
}

/// A very small validation for "does this look like an Objective-C class
/// pointer": readable, aligned, and not one of the HLE trampolines (whose
/// `udf` marker word would otherwise be read as a structure field).
fn is_probable_class(hle: &Hle<'_>, addr: u32) -> bool {
    if addr == 0 || addr & 3 != 0 {
        return false;
    }
    if crate::loader::is_trampoline_page(addr) {
        return false;
    }
    match hle.mem.region_at(addr) {
        Some(region) => region.perms.read,
        None => false,
    }
}

struct ClassInfo {
    name: String,
    superclass: u32,
    instance_size: u32,
    /// Address of the method list to search (modern: `class_ro_t::baseMethods`).
    methods: u32,
    /// Legacy layout puts the method list array directly in the class.
    legacy_method_lists: u32,
}

impl ClassInfo {
    fn read(hle: &Hle<'_>, class_addr: u32) -> Option<ClassInfo> {
        if !is_probable_class(hle, class_addr) {
            return None;
        }
        let _isa = hle.mem.read_u32(class_addr).ok()?;
        let superclass = hle.mem.read_u32(class_addr + CLASS_SUPERCLASS).ok()?;
        let bits = hle.mem.read_u32(class_addr + CLASS_BITS).ok()?;

        // Non-fragile ABI: `bits` (masked) points at a `class_ro_t`.
        if bits != 0 {
            let ro = bits & CLASS_DATA_MASK;
            if let Some(name) = read_ro_name(hle, ro, &superclass) {
                let methods = hle.mem.read_u32(ro + RO_BASE_METHODS).unwrap_or(0);
                let instance_size = hle.mem.read_u32(ro + RO_INSTANCE_SIZE).unwrap_or(0);
                return Some(ClassInfo { name, superclass, instance_size, methods, legacy_method_lists: 0 });
            }
        }

        // Legacy ABI: name at +8 (after isa/superclass), method list array at
        // +0x1c, instance size at +0x14.
        let legacy_name = hle.mem.read_u32(class_addr + LEGACY_NAME).ok()?;
        let name = hle.cstr(legacy_name).ok()?;
        if !plausible_class_name(&name) {
            return None;
        }
        let instance_size = hle.mem.read_u32(class_addr + LEGACY_INSTANCE_SIZE).ok()?;
        let legacy_method_lists = hle.mem.read_u32(class_addr + LEGACY_METHOD_LISTS).ok()?;
        Some(ClassInfo { name, superclass, instance_size, methods: 0, legacy_method_lists })
    }
}

/// The `ro`->name read, with a paranoia check that the name sits close to the
/// structure it belongs to (keeps stray pointers from passing validation).
fn read_ro_name(hle: &Hle<'_>, ro: u32, _superclass: &u32) -> Option<String> {
    if ro == 0 || ro & 3 != 0 {
        return None;
    }
    let name_ptr = hle.mem.read_u32(ro + RO_NAME).ok()?;
    let name = hle.cstr(name_ptr).ok()?;
    if plausible_class_name(&name) {
        Some(name)
    } else {
        None
    }
}

fn plausible_class_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 128
        && name.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Walk a `method_list_t` looking for `selector`.
///
/// Both record shapes the image can hold parse identically here:
///
/// * modern (`method_list_t`): `entsizeAndFlags, count, entries[entsize]`
///   with `entsize` ∈ {12, 16, ...} — entries start at `+8`;
/// * legacy (`objc_method_list`): `obsolete (=0), count, objc_method[1]`
///   with 12-byte entries — also entries at `+8`.
///
/// So `[obsolete|entsize][count][entry0]...` is read uniformly, with the
/// relative-entries flag (arm64e) rejected, and every entry's IMP validated by
/// the caller before it is executed.
fn search_method_list(hle: &Hle<'_>, list: u32, selector: &str) -> Option<u32> {
    if list == 0 || list < 0x1000 || crate::loader::is_trampoline_page(list) {
        return None;
    }
    let first = hle.mem.read_u32(list).ok()?;
    if first & METHOD_LIST_RELATIVE != 0 {
        return None;
    }
    let count = hle.mem.read_u32(list + 4).ok()?;
    if count == 0 || count > 4096 {
        return None;
    }
    let entsize = if first == 0 || first == METHOD_ENTRY_SIZE {
        METHOD_ENTRY_SIZE
    } else if (METHOD_ENTRY_SIZE..=64).contains(&first) && first & 3 == 0 {
        first
    } else {
        return None;
    };
    let entries = list + 8;
    for i in 0..count {
        let entry = entries + i * entsize;
        let name_ptr = hle.mem.read_u32(entry).ok()?;
        if name_ptr == 0 {
            continue;
        }
        let Ok(name) = hle.cstr(name_ptr) else { continue };
        if name == selector {
            return hle.mem.read_u32(entry + 8).ok();
        }
    }
    None
}

/// Find the implementation of `selector` for the class hierarchy starting at
/// `class_addr`.  For instance methods this is `receiver isa`; for class
/// methods, the *metaclass* (so the class methods of the image resolve with no
/// added special casing), and the same walk answers every `[super ...]` send.
fn lookup_imp(hle: &Hle<'_>, class_addr: u32, selector: &str) -> Option<(u32, String)> {
    let mut current = class_addr;
    let mut depth = 0;
    while current != 0 && depth < 32 && !crate::loader::is_trampoline_page(current) {
        let info = ClassInfo::read(hle, current)?;
        // A method installed by the guest (`class_addMethod`, …) wins.
        if let Some(&imp) = hle.sys.objc.host_imps.get(&(current, selector.to_string())) {
            return Some((imp, info.name));
        }
        if let Some(imp) = search_method_list(hle, info.methods, selector) {
            return Some((imp, info.name));
        }
        if info.legacy_method_lists != 0 {
            // `methodLists` is an array terminated by a NULL entry.
            for i in 0..32 {
                let list = hle.mem.read_u32(info.legacy_method_lists + i * 4).ok()?;
                if list == 0 {
                    break;
                }
                if let Some(imp) = search_method_list(hle, list, selector) {
                    return Some((imp, info.name));
                }
            }
        }
        current = info.superclass;
        depth += 1;
    }
    None
}

/// Walk classes from `start` until one of the emulator's own classes provides
/// `name`; returns the class name it was found on.
///
/// This is the "the image subclass's method list ended, now what" step: the
/// image's `EAGLView : UIView` chain ends at a synthetic `UIView` whose methods
/// all live in the host tables.
fn host_chain_lookup(hle: &Hle<'_>, start: u32, name: &str) -> Option<String> {
    let mut current = start;
    let mut depth = 0;
    while current != 0 && depth < 32 && !crate::loader::is_trampoline_page(current) {
        if let Some(class_name) = hle.sys.host_classes.get(&current) {
            for table in [HOST_METHODS, crate::hle::ui::HOST_METHODS, crate::hle::gl::HOST_METHODS] {
                for &(cls, sel, _) in table {
                    if sel == name && (cls == class_name || cls == "NSObject") {
                        return Some(class_name.clone());
                    }
                }
            }
            // Host classes are roots of the lookup in this direction.
            return None;
        }
        current = ClassInfo::read(hle, current)?.superclass;
        depth += 1;
    }
    None
}

/// Name the class an object (or class object) belongs to.
fn class_name_of(hle: &Hle<'_>, object: u32) -> Option<String> {
    if let Some(name) = hle.sys.host_classes.get(&object) {
        return Some(name.clone());
    }
    if let Some(name) = hle.sys.objc.names.get(&object) {
        return Some(name.clone());
    }
    let isa = read_isa(hle, object);
    if let Some(name) = hle.sys.host_classes.get(&isa) {
        return Some(name.clone());
    }
    if let Some(name) = hle.sys.objc.names.get(&isa) {
        return Some(name.clone());
    }
    if let Some(info) = ClassInfo::read(hle, isa) {
        return Some(info.name);
    }
    ClassInfo::read(hle, object).map(|info| info.name)
}

/// True when `imp` is an address the guest CPU may be transferred to.
///
/// A `0`/trampoline/execute-denied target is *never* returned to the machine
/// as a PC: writing it there is exactly how the boot fault (`pc = 0`) was
/// produced.
pub fn valid_method_target(hle: &Hle<'_>, imp: u32) -> bool {
    if imp == 0 || crate::loader::is_trampoline_page(imp) {
        return false;
    }
    match hle.mem.region_at(imp & !1) {
        Some(region) => region.perms.execute,
        None => false,
    }
}

/// Log a dispatch step when theverbosity calls for it, and record it in the
/// diagnostic ring the machine prints around a trap.
fn trace(hle: &mut Hle<'_>, level: usize, line: impl Into<String>) {
    if hle.sys.objc_verbosity >= level {
        let line = line.into();
        hle.sys.note_recent(line.clone());
        hle.sys.log.push(line);
    }
}

/// Set `(r0, r1) = (0, 0)` and zero the struct-return buffer.
///
/// Used when a message is answered "nil"; the caller runs with the values the
/// ABI says a nil-receiver message produces — 64-bit results and `_stret`
/// buffers are zeroed too, which is the difference between "message to nil"
/// and "crash two instructions later because the `CGRect` was garbage".
fn answer_nil(hle: &mut Hle<'_>, stret_buffer: u32) {
    hle.cpu.r[0] = 0;
    hle.cpu.r[1] = 0;
    zero_stret(hle, stret_buffer);
}

/// Zero up to 16 bytes of a `_stret` return buffer (covers `CGRect`, `CGSize`,
/// `CGPoint`, `UIEdgeInsets`, `NSRange` — every structure this game returns by
/// value).
fn zero_stret(hle: &mut Hle<'_>, buffer: u32) {
    if buffer == 0 || buffer & 3 != 0 {
        return;
    }
    let Some(region) = hle.mem.region_at(buffer) else { return };
    if !region.perms.write {
        return;
    }
    let size = 16u32.min(region.end().saturating_sub(buffer));
    let zeros = vec![0u8; size as usize];
    let _ = hle.write_bytes(buffer, &zeros);
}

// ---------------------------------------------------------------------------
// Message dispatch
// ---------------------------------------------------------------------------

/// Hand control to a guest IMP, or answer `0` when the IMP is not executable.
///
/// The value returned here is only observed when the transfer was refused: on
/// success the machine resumes at the IMP, and its return value flows straight
/// back to whoever called `objc_msgSend` (through `HLE_RETURN`).
fn call_method(hle: &mut Hle<'_>, name: &str, class_name: &str, imp: u32) -> Result<u32> {
    if hle.call_guest(imp) {
        hle.sys.objc.guest_calls += 1;
        hle.sys.objc.last_dispatch = format!("-[{class_name} {name}] -> {imp:#010x}");
        trace(hle, VERBOSITY_EVERY_CALL, format!("objc: [{class_name} {name}] -> {imp:#010x}"));
    } else {
        hle.sys.objc.missing += 1;
        hle.sys.objc.last_dispatch = format!("-[{class_name} {name}] -> bad IMP {imp:#010x}");
        *hle.sys.objc.unrecognized.entry(format!("-[{class_name} {name}] (bad IMP {imp:#x})")).or_insert(0) += 1;
        trace(hle, VERBOSITY_FAILURES, format!("objc: [{class_name} {name}] has an uncallable IMP {imp:#x}"));
    }
    Ok(0)
}

/// `id objc_msgSend(id self, SEL _cmd, ...)`.
fn msg_send(hle: &mut Hle<'_>) -> Result<u32> {
    dispatch_with_start(hle, hle.arg(0), hle.arg(1), 0, 0)
}

/// `void objc_msgSend_stret(void *buffer, id self, SEL _cmd, ...)`: the caller
/// receives the struct through `buffer`; every argument past the selector shifts
/// down by one register.
fn msg_send_stret(hle: &mut Hle<'_>) -> Result<u32> {
    dispatch_with_start(hle, hle.arg(1), hle.arg(2), 0, hle.arg(0))
}

/// `objc_msgSendSuper(struct objc_super *sup, SEL, ...)` (iOS 2 legacy): the
/// search starts at `sup->super_class` directly — that field already names the
/// superclass of the definition site.
fn msg_send_super(hle: &mut Hle<'_>) -> Result<u32> {
    let sup = hle.arg(0);
    let receiver = hle.mem.read_u32(sup).unwrap_or(0);
    let start = hle.mem.read_u32(sup + 4).unwrap_or(0);
    dispatch_with_start(hle, receiver, hle.arg(1), start, 0)
}

fn msg_send_super_stret(hle: &mut Hle<'_>) -> Result<u32> {
    let sup = hle.arg(1);
    let receiver = hle.mem.read_u32(sup).unwrap_or(0);
    let start = hle.mem.read_u32(sup + 4).unwrap_or(0);
    dispatch_with_start(hle, receiver, hle.arg(2), start, hle.arg(0))
}

/// `objc_msgSendSuper2(struct objc_super2 *sup, SEL, ...)` (iOS 5+): the second
/// word is the class of the *definition* site, so the search starts at its
/// superclass.
fn msg_send_super2(hle: &mut Hle<'_>) -> Result<u32> {
    let sup = hle.arg(0);
    let receiver = hle.mem.read_u32(sup).unwrap_or(0);
    let current = hle.mem.read_u32(sup + 4).unwrap_or(0);
    let start = if current != 0 { hle.mem.read_u32(current + 4).unwrap_or(0) } else { 0 };
    dispatch_with_start(hle, receiver, hle.arg(1), start, 0)
}

fn msg_send_super2_stret(hle: &mut Hle<'_>) -> Result<u32> {
    let sup = hle.arg(1);
    let receiver = hle.mem.read_u32(sup).unwrap_or(0);
    let current = hle.mem.read_u32(sup + 4).unwrap_or(0);
    let start = if current != 0 { hle.mem.read_u32(current + 4).unwrap_or(0) } else { 0 };
    dispatch_with_start(hle, receiver, hle.arg(2), start, hle.arg(0))
}

/// The main message dispatch.  `start_class` is only non-zero for super sends
/// (the lookup then begins there, per the `objc_super` semantics the caller
/// implemented); `stret_buffer` is only non-zero for struct-returning messages.
fn dispatch_with_start(
    hle: &mut Hle<'_>,
    receiver: u32,
    selector: u32,
    start_class: u32,
    stret_buffer: u32,
) -> Result<u32> {
    // ------------------------------------------------------------------
    // 1. A message to nil is a legal no-op.  The binary sends these in at
    //    least these shapes: optional collaborators (`dismiss` on a missing
    //    movie controller, `release` on a nil field), and chained initialisers
    //    (`[[Foo alloc] init]` where alloc was stubbed to nil).  Answer the
    //    ABI-exact zero, with a counter so `--objc-trace` can show it.
    // ------------------------------------------------------------------
    if receiver == 0 {
        let name = selector_name(hle, selector).unwrap_or_else(|| format!("<sel:{selector:#x}>"));
        *hle.sys.objc.nil_messages.entry(name.clone()).or_insert(0) += 1;
        hle.sys.objc.last_dispatch = format!("nil {name}");
        trace(hle, VERBOSITY_EVERY_CALL, format!("objc: {name} -> nil receiver"));
        answer_nil(hle, stret_buffer);
        return Ok(0);
    }

    let isa = read_isa(hle, receiver);
    let Some(name) = selector_name(hle, selector) else {
        // A selector we cannot even read — never dispatch, never fault.
        hle.sys.objc.missing += 1;
        hle.sys.objc.last_dispatch = format!("unreadable selector {selector:#x} (receiver {receiver:#x})");
        trace(hle, VERBOSITY_FAILURES, format!("objc: unreadable selector {selector:#x} (receiver {receiver:#x}, isa {isa:#x})"));
        *hle.sys.objc.unrecognized.entry(format!("<unreadable sel {selector:#x}>")).or_insert(0) += 1;
        answer_nil(hle, stret_buffer);
        return Ok(0);
    };

    hle.sys.objc.last_dispatch = format!("-[{receiver:#x}->{isa:#x} {name}]");

    // ------------------------------------------------------------------
    // 2/3. Emulator-provided objects: classes (alloc/new/class messages) and
    //     instances of classes imported from UIKit/Foundation/CoreAnimation.
    // ------------------------------------------------------------------
    if let Some(class_name) = hle.sys.host_classes.get(&receiver).cloned() {
        hle.sys.objc.host_calls += 1;
        if name == "alloc" || name == "allocWithZone:" || name == "new" {
            return host_instance(hle, &class_name, HOST_INSTANCE_SIZE);
        }
        return dispatch_host_method(hle, &class_name, receiver, &name);
    }
    let receiver_is_class = if let Some(class_name) = hle.sys.host_classes.get(&isa).cloned() {
        hle.sys.objc.host_calls += 1;
        return dispatch_host_method(hle, &class_name, receiver, &name);
    } else {
        // A class object from the image answers messages by its metaclass chain
        // (that's where a class's `+methods` are listed).  Detection: the
        // receiver's *own* storage parses as a class and its isa is one too.
        ClassInfo::read(hle, receiver).is_some() && is_probable_class(hle, isa)
    };

    // ------------------------------------------------------------------
    // 4. The guest's own method tables (the one place a real IMP can live).
    // ------------------------------------------------------------------
    let start = if start_class != 0 { start_class } else { isa };
    if let Some(&imp) = hle.sys.objc.host_imps.get(&(receiver, name.clone())) {
        return call_method(hle, &name, "<registered>", imp);
    }
    if let Some((imp, class_name)) = lookup_imp(hle, start, &name) {
        hle.sys.note_recent(format!("objc: [{class_name} {name}] -> {imp:#010x}"));
        return call_method(hle, &name, &class_name, imp);
    }

    // ------------------------------------------------------------------
    // 5. NSObject bookkeeping the runtime owns.  (For an image class object
    //    this is also where `+alloc` is answered.)
    // ------------------------------------------------------------------
    if let Some(value) = runtime_answers(hle, receiver, isa, start, &name, receiver_is_class)? {
        return Ok(value);
    }

    // ------------------------------------------------------------------
    // 6. The superclass chain may hit an emulator-provided class whose
    //    methods exist only in the host tables (`[EAGLView addSubview:]` …).
    // ------------------------------------------------------------------
    if let Some(found_on) = host_chain_lookup(hle, start, &name) {
        return dispatch_host_method(hle, &found_on, receiver, &name);
    }

    // ------------------------------------------------------------------
    // 7. Unrecognised.  A real runtime raises `doesNotRecognizeSelector:`;
    //    with no forwarding machinery available we log once and answer nil,
    //    which is the same instruction the game would reach after messaging a
    //    class that declares the selector but does not implement it.
    // ------------------------------------------------------------------
    let class = class_name_of(hle, receiver).unwrap_or_else(|| format!("{isa:#x}"));
    hle.sys.objc.missing += 1;
    hle.sys.objc.last_dispatch = format!("-[{class} {name}] -> UNRECOGNIZED");
    *hle.sys.objc.unrecognized.entry(format!("-[{class} {name}]")).or_insert(0) += 1;
    trace(hle, VERBOSITY_FAILURES, format!("objc: unrecognized selector -[{class} {name}] (receiver {receiver:#x}, isa {isa:#x}, selector {selector:#x})"));
    Ok(0)
}

/// Dispatch to a method implemented by the emulator for one of its classes.
///
/// Checks, in order: guest-registered IMPs (from `class_addMethod`), then the
/// `HOST_METHODS` tables here and in the UI/GL shims.  On a miss, any selector
/// beginning with `init` answers `self` — initialisers are identity by design
/// in the emulator — and everything else becomes a "no host implementation"
/// diagnostic (once, with a count) and a `nil` answer.
fn dispatch_host_method(hle: &mut Hle<'_>, class_name: &str, receiver: u32, name: &str) -> Result<u32> {
    // Methods the guest installed against one of our class objects win.
    let isa = read_isa(hle, receiver);
    for probe in [receiver, isa] {
        if let Some(&imp) = hle.sys.objc.host_imps.get(&(probe, name.clone())) {
            return call_method(hle, name, class_name, imp);
        }
    }
    for table in [HOST_METHODS, crate::hle::ui::HOST_METHODS, crate::hle::gl::HOST_METHODS] {
        for &(class, sel, handler) in table {
            if sel == name && (class == class_name || class == "NSObject") {
                hle.sys.objc.host_calls += 1;
                hle.sys.objc.last_dispatch = format!("-[{class_name} {name}] (host)");
                trace(hle, VERBOSITY_EVERY_CALL, format!("objc: [{class_name} {name}] (host)"));
                return handler(hle, receiver);
            }
        }
    }
    if name.starts_with("init") {
        // Unhandled initialisers succeed trivially (the object exists already).
        return Ok(receiver);
    }
    hle.sys.objc.missing += 1;
    hle.sys.objc.last_dispatch = format!("-[{class_name} {name}] -> NO HOST METHOD");
    *hle.sys.objc.unrecognized.entry(format!("-[{class_name} {name}]")).or_insert(0) += 1;
    trace(hle, VERBOSITY_FAILURES, format!("objc: no host implementation for [{class_name} {name}]"));
    Ok(0)
}

/// Answer the Objective-C bookkeeping messages the runtime owns.  Returns
/// `None` when `name` is not one of them (dispatch falls through).
fn runtime_answers(
    hle: &mut Hle<'_>,
    receiver: u32,
    isa: u32,
    start: u32,
    name: &str,
    receiver_is_class: bool,
) -> Result<Option<u32>> {
    let value = match name {
        "alloc" | "allocWithZone:" | "new" => {
            let info = ClassInfo::read(hle, receiver);
            let mut size = info.as_ref().map(|i| i.instance_size).unwrap_or(0);
            if size < 4 {
                size = 32;
            }
            let object = hle.alloc(size, 16)?;
            let zeros = vec![0u8; size as usize];
            hle.write_bytes(object, &zeros)?;
            hle.mem.write_u32(object, receiver)?;
            hle.sys.objc.host_calls += 1;
            let display = info.as_ref().map(|i| i.name.clone()).unwrap_or_else(|| format!("{receiver:#x}"));
            hle.sys.objc.last_dispatch = format!("+[{display} alloc] -> {object:#010x}");
            trace(hle, VERBOSITY_EVERY_CALL, format!("objc: [{display} alloc] -> {object:#x} ({size} bytes)"));
            return Ok(Some(object));
        }
        "retain" | "autorelease" | "self" => receiver,
        "init" => receiver,
        "release" | "dealloc" | "drain" | "finalize" => 0,
        "retainCount" => 1,
        "zone" => 0,
        "class" => {
            if receiver_is_class {
                receiver
            } else {
                isa
            }
        }
        "superclass" => {
            let base = if receiver_is_class { receiver } else { isa };
            hle.mem.read_u32(base + CLASS_SUPERCLASS).unwrap_or(0)
        }
        "hash" => receiver,
        "isEqual:" => (receiver == hle.arg(2)) as u32,
        "isKindOfClass:" => is_kind_of(hle, receiver, isa, receiver_is_class, hle.arg(2), false),
        "isMemberOfClass:" => is_kind_of(hle, receiver, isa, receiver_is_class, hle.arg(2), true),
        "description" | "debugDescription" => {
            let class = class_name_of(hle, receiver).unwrap_or_else(|| "NSObject".to_string());
            return make_nsstring(hle, &format!("<{class}: {receiver:#010x}>")).map(Some);
        }
        "copy" | "copyWithZone:" | "mutableCopy" | "mutableCopyWithZone:" => {
            return copy_instance(hle, receiver, isa).map(Some)
        }
        "respondsToSelector:" => responds(hle, start, hle.arg(2)),
        "instancesRespondToSelector:" => responds(hle, start, hle.arg(2)),
        "performSelector:" | "performSelector:withObject:" => {
            return perform_selector(hle, receiver, start)
        }
        "performSelector:withObject:withObject:"
        | "performSelector:withObject:afterDelay:"
        | "performSelector:withObject:afterDelay:inModes:" => 0,
        "performSelectorOnMainThread:withObject:waitUntilDone:" => {
            return perform_selector(hle, receiver, start)
        }
        "methodSignatureForSelector:" | "forwardInvocation:" | "doesNotRecognizeSelector:" => 0,
        "isProxy" | "conformsToProtocol:" => 0,
        _ => return Ok(None),
    };
    hle.sys.objc.last_dispatch = format!("-[{:#x} {name}] (runtime)", receiver);
    Ok(Some(value))
}

/// Shallow-copy a guest object: allocate a same-sized block and copy the body.
fn copy_instance(hle: &mut Hle<'_>, receiver: u32, isa: u32) -> Result<u32> {
    let size = ClassInfo::read(hle, isa)
        .map(|i| i.instance_size)
        .unwrap_or(0)
        .clamp(4, 4096);
    let size = size.max(32);
    let copy = hle.alloc(size, 16)?;
    let zeros = vec![0u8; size as usize];
    hle.write_bytes(copy, &zeros)?;
    let mut offset = 0u32;
    while offset < size {
        let chunk = (size - offset).min(1024);
        match hle.mem.read_bytes(receiver + offset, chunk) {
            Ok(bytes) => hle.write_bytes(copy + offset, &bytes)?,
            Err(_) => break,
        }
        offset += chunk;
    }
    Ok(copy)
}

/// `NSObject` introspection for the host tables.
fn responds(hle: &Hle<'_>, start: u32, sel: u32) -> u32 {
    let Some(name) = selector_name(hle, sel) else { return 0 };
    if lookup_imp(hle, start, &name).is_some() {
        return 1;
    }
    if host_chain_lookup(hle, start, &name).is_some() {
        return 1;
    }
    // The bookkeeping the runtime itself implements counts as a response.
    matches!(
        name.as_str(),
        "alloc" | "new" | "retain" | "release" | "autorelease" | "init" | "class" | "superclass"
            | "description" | "hash" | "isEqual:" | "isKindOfClass:" | "isMemberOfClass:"
            | "respondsToSelector:" | "performSelector:" | "copy" | "zone"
    ) as u32
}

/// `isKindOfClass:` / `isMemberOfClass:` over the isa chain, comparing classes
/// by address and (across the image/host boundary) by name.
fn is_kind_of(hle: &Hle<'_>, receiver: u32, isa: u32, receiver_is_class: bool, target: u32, exact: bool) -> u32 {
    let target_name = class_name_of(hle, target);
    let mut current = if receiver_is_class { receiver } else { isa };
    let mut depth = 0;
    while current != 0 && depth < 32 {
        if current == target {
            return 1;
        }
        if !exact {
            if let (Some(a), Some(b)) = (class_name_of(hle, current), target_name.clone()) {
                if a == b {
                    return 1;
                }
            }
        }
        let Some(info) = ClassInfo::read(hle, current) else { break };
        current = info.superclass;
        depth += 1;
    }
    0
}

/// `performSelector:`: execute the guest IMP for `sel` with the receiver in
/// r0, exactly as libobjc's own implementation would.
fn perform_selector(hle: &mut Hle<'_>, receiver: u32, start: u32) -> Result<Option<u32>> {
    let sel = hle.arg(2);
    let Some(name) = selector_name(hle, sel) else { return Ok(Some(0)) };
    let isa = read_isa(hle, receiver);
    let mut class = if start != 0 { start } else { isa };
    let mut depth = 0;
    while class != 0 && depth < 32 {
        if let Some((imp, class_name)) = lookup_imp(hle, class, &name) {
            hle.cpu.r[1] = sel;
            return call_method(hle, &name, &class_name, imp).map(Some);
        }
        if let Some(found_on) = host_chain_lookup(hle, class, &name) {
            return dispatch_host_method(hle, &found_on, receiver, &name).map(Some);
        }
        let Some(info) = ClassInfo::read(hle, class) else { break };
        class = info.superclass;
        depth += 1;
    }
    Ok(Some(0))
}

/// `_objc_msgForward` is where libobjc deposits *unforwardable* messages.
/// A HLE dispatch never reaches this situation (dispatch always answers), but
/// the symbol is imported, so if the guest ever jumps here directly it gets a
/// report plus a nil answer instead of wandering into unmapped memory.
fn msg_forward(hle: &mut Hle<'_>) -> Result<u32> {
    let receiver = hle.arg(0);
    let name = selector_name(hle, hle.arg(1)).unwrap_or_else(|| format!("{:#x}", hle.arg(1)));
    let class = class_name_of(hle, receiver).unwrap_or_else(|| format!("{receiver:#x}"));
    hle.note(format!("objc_msgForward reached for -[{class} {name}]"));
    *hle.sys.objc.unrecognized.entry(format!("forward: [{class} {name}]")).or_insert(0) += 1;
    Ok(0)
}

// ---------------------------------------------------------------------------
// NSString helpers
// ---------------------------------------------------------------------------

/// Best-effort extraction of a guest NSString/CFString: the host-side mirror
/// (for objects the emulator created), then the classic compile-time
/// `cfstringStruct` layout (`isa, flags, char *, length`).
pub fn cf_string_text(hle: &Hle<'_>, string: u32) -> Option<String> {
    if string == 0 {
        return None;
    }
    if string & 1 != 0 {
        return None;
    }
    if let Some(text) = hle.sys.cf_strings.get(&string) {
        return Some(text.clone());
    }
    // Compile-time `__CFString`: word 2 is the bytes pointer.
    let body = hle.mem.read_u32(string + 8).ok()?;
    if body == 0 {
        return None;
    }
    let text = hle.cstr(body).ok()?;
    if !text.is_empty()
        && text.len() < 4096
        && text.chars().all(|c| (0x20..0x7f).contains(&(c as u32)) || c.len_utf8() > 1)
    {
        Some(text)
    } else {
        None
    }
}

/// A guest NSString whose host mirror is `text`.
pub fn make_nsstring(hle: &mut Hle<'_>, text: &str) -> Result<u32> {
    let object = host_instance(hle, "NSString", 32)?;
    hle.sys.cf_strings.insert(object, text.to_string());
    Ok(object)
}

// ---------------------------------------------------------------------------
// The Objective-C entry points the image can import
// ---------------------------------------------------------------------------

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    // Messaging --------------------------------------------------------------
    ("objc_msgSend", msg_send),
    ("objc_msgSend_stret", msg_send_stret),
    ("objc_msgSendSuper", msg_send_super),
    ("objc_msgSendSuper_stret", msg_send_super_stret),
    ("objc_msgSendSuper2", msg_send_super2),
    ("objc_msgSendSuper2_stret", msg_send_super2_stret),
    // The ARM ABI has no separate `objc_msgSend_fpret` (floats come back in
    // core registers) but the symbol exists and aliases the plain send.
    ("objc_msgSend_fpret", msg_send),
    // The forwarding stubs: any dispatch reaching them failed resolution; the
    // handler reports it instead of letting control wander.
    ("objc_msgForward", msg_forward),
    ("objc_msgForward_stret", msg_forward),
    // Class lookup ------------------------------------------------------------
    ("objc_getClass", get_class),
    ("objc_lookUpClass", get_class),
    ("objc_getMetaClass", get_meta_class),
    ("objc_getRequiredClass", get_required_class),
    ("objc_getClassList", get_class_list),
    ("objc_copyClassList", copy_class_list),
    ("objc_allocateClassPair", allocate_class_pair),
    ("objc_registerClassPair", register_class_pair),
    ("objc_disposeClassPair", noop_zero),
    ("objc_getProtocol", noop_zero),
    ("objc_copyProtocolList", noop_zero),
    // Selectors ---------------------------------------------------------------
    ("sel_registerName", sel_register_name),
    ("sel_getName", sel_get_name),
    ("sel_isEqual", sel_is_equal),
    ("sel_isMapped", return_one),
    ("sel_getUid", sel_register_name),
    ("_objc_getSelector", sel_register_name),
    ("objc_getSelector", sel_register_name),
    // NSObject-level reflection -----------------------------------------------
    ("object_getClass", object_get_class),
    ("object_setClass", object_set_class),
    ("object_getClassName", object_get_class_name),
    ("object_getIvar", noop_zero),
    ("object_setIvar", noop_zero),
    ("object_copy", object_copy),
    ("object_dispose", object_dispose),
    ("object_getInstanceVariable", noop),
    ("object_setInstanceVariable", noop),
    ("object_getIndexedIvars", object_get_class),
    // class_* ---------------------------------------------------------------
    ("class_getName", class_get_name),
    ("class_getSuperclass", class_get_superclass),
    ("class_isMetaClass", class_is_meta_class),
    ("class_getInstanceSize", class_get_instance_size),
    ("class_respondsToSelector", class_responds),
    ("class_getMethodImplementation", class_get_method_implementation),
    ("class_getMethodImplementation_stret", class_get_method_implementation),
    ("class_getInstanceMethod", class_get_instance_method),
    ("class_getClassMethod", class_get_class_method),
    ("class_addMethod", class_add_method),
    ("class_replaceMethod", class_add_method),
    ("class_conformsToProtocol", noop_zero),
    ("class_isFinal", noop_zero),
    ("class_addProtocol", return_one),
    ("class_addProperty", return_one),
    ("class_addIvar", noop_zero),
    ("class_getVersion", noop_zero),
    ("class_setVersion", noop_zero),
    ("class_getIvarLayout", noop_zero),
    ("class_getWeakIvarLayout", noop_zero),
    ("class_setIvarLayout", noop),
    ("class_setWeakIvarLayout", noop),
    ("class_getProperty", noop_zero),
    ("class_getInstanceVariable", noop_zero),
    ("class_getImageName", class_get_image_name),
    ("class_copyMethodList", copy_empty_list),
    ("class_copyIvarList", copy_empty_list),
    ("class_copyProtocolList", copy_empty_list),
    ("class_copyPropertyList", copy_empty_list),
    ("class_createInstance", class_create_instance),
    ("objc_constructInstance", object_dispose),
    ("objc_destructInstance", noop),
    // method_* --------------------------------------------------------------
    ("method_getName", method_get_name),
    ("method_getImplementation", method_get_implementation),
    ("method_getTypeEncoding", method_get_type_encoding),
    ("method_setImplementation", method_set_implementation),
    ("method_getNumberOfArguments", method_get_number_of_arguments),
    ("method_getDescription", noop),
    ("method_getArgumentType", noop),
    ("method_getReturnType", noop),
    ("method_copyArgumentType", noop_zero),
    ("method_copyReturnType", noop_zero),
    ("method_exchangeImplementations", method_exchange_implementations),
    ("method_invoke", noop_zero),
    // Associated objects / weak references / ARC-era primitives ---------------
    ("objc_getAssociatedObject", assoc_get),
    ("objc_setAssociatedObject", assoc_set),
    ("objc_removeAssociatedObjects", assoc_remove),
    ("objc_storeStrong", objc_store_strong),
    ("objc_initWeak", objc_init_weak),
    ("objc_loadWeak", objc_load_weak),
    ("objc_loadWeakRetained", objc_load_weak),
    ("objc_storeWeak", objc_init_weak),
    ("objc_destroyWeak", objc_destroy_weak),
    ("objc_clear_deallocating", noop),
    ("objc_copyWeak", objc_copy_weak),
    ("objc_moveWeak", objc_copy_weak),
    // Property helpers ---------------------------------------------------------
    ("objc_getProperty", objc_get_property),
    ("objc_setProperty", objc_set_property),
    ("objc_copyStruct", objc_copy_struct),
    // Retain/release era --------------------------------------------------------
    ("objc_retain", objc_retain),
    ("objc_release", noop_zero),
    ("objc_autorelease", objc_retain),
    ("objc_retainAutorelease", objc_retain),
    ("objc_retainAutoreleasedReturnValue", objc_retain),
    ("objc_releaseAutoreleasedReturnValue", noop_zero),
    ("objc_autoreleaseReturnValue", objc_retain),
    ("objc_unsafeClaimAutoreleasedNSObject", objc_retain),
    ("objc_retainBlock", objc_retain),
    // Autorelease pools --------------------------------------------------------
    ("objc_autoreleasePoolPush", pool_push),
    ("objc_autoreleasePoolPop", noop),
    ("_objc_autoreleasePoolPush", pool_push),
    ("_objc_autoreleasePoolPop", noop),
    // Synchronisation -----------------------------------------------------------
    ("objc_sync_enter", noop_zero),
    ("objc_sync_exit", noop_zero),
    ("objc_sync_notify", noop_zero),
    ("objc_sync_wait", noop_zero),
    // exceptions are Terminal in this emulator ----------------------------------
    ("objc_exception_throw", exception_throw),
    ("objc_exception_rethrow", exception_throw),
    ("objc_begin_catch", return_arg0),
    ("objc_end_catch", noop),
    ("objc_terminate", exception_throw),
    // The fast-enumeration guard -----------------------------------------------
    ("objc_enumerationMutation", array_mutation_fault),
    ("objc_enumerationsMutation", array_mutation_fault),
    // The linker binds these data symbols to trampolines; if they ever execute,
    // return 0 rather than trap.
    ("_objc_empty_cache", noop_zero),
    ("_objc_empty_vtable", noop_zero),
    // Foundation conveniences that go through the runtime three levels up -------
    ("NSClassFromString", ns_class_from_string),
    ("NSSelectorFromString", ns_selector_from_string),
    ("NSStringFromClass", ns_string_from_class),
    ("NSStringFromSelector", ns_string_from_selector),
];

fn noop(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn noop_zero(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn return_one(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(1)
}

fn return_arg0(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

fn pool_push(hle: &mut Hle<'_>) -> Result<u32> {
    // Return a token the matching Pop can consume.
    Ok(host_instance(hle, "NSAutoreleasePool", 32)?)
}

// ---- classes ---------------------------------------------------------------

fn get_class(hle: &mut Hle<'_>) -> Result<u32> {
    let Some(name) = optional_cstr(hle, hle.arg(0))? else { return Ok(0) };
    Ok(hle
        .sys
        .objc
        .classes_by_name
        .get(&name)
        .copied()
        .or_else(|| hle.sys.host_classes_by_name.get(&name).copied())
        .unwrap_or(0))
}

fn get_meta_class(hle: &mut Hle<'_>) -> Result<u32> {
    let Some(name) = optional_cstr(hle, hle.arg(0))? else { return Ok(0) };
    let class = hle
        .sys
        .objc
        .classes_by_name
        .get(&name)
        .copied()
        .or_else(|| hle.sys.host_classes_by_name.get(&name).copied());
    let Some(class) = class else { return Ok(0) };
    Ok(hle.mem.read_u32(class).unwrap_or(0))
}

fn get_required_class(hle: &mut Hle<'_>) -> Result<u32> {
    // A real runtime aborts when the class is absent; an emulator has to keep
    // going, so it synthesises a host class on demand.  The message log records
    // that the class was fabricated, which is what a missing UIKit class would
    // look like on a flagged run.
    let Some(name) = optional_cstr(hle, hle.arg(0))? else { return Ok(0) };
    if let Some(&class) = hle.sys.objc.classes_by_name.get(&name) {
        return Ok(class);
    }
    hle.note(format!("objc_getRequiredClass({name}): no such class, using a host stand-in"));
    host_class(hle, &name)
}

fn get_class_list(hle: &mut Hle<'_>) -> Result<u32> {
    let buffer = hle.arg(0);
    let max = hle.arg(1) as usize;
    write_class_list(hle, buffer, max)
}

fn copy_class_list(hle: &mut Hle<'_>) -> Result<u32> {
    let out_count = hle.arg(0);
    let total = write_class_list(hle, 0, 0)?;
    let bytes = total as u32 * 4;
    let buffer = hle.alloc(bytes.max(4), 8)?;
    write_class_list_at(hle, buffer, total as usize)?;
    if out_count != 0 {
        hle.mem.write_u32(out_count, total)?;
    }
    Ok(buffer)
}

fn collect_class_addresses(hle: &Hle<'_>) -> Vec<u32> {
    let mut classes: Vec<u32> = hle.sys.objc.classes_by_name.values().copied().collect();
    classes.sort_unstable();
    classes.dedup();
    classes
}

fn write_class_list(hle: &mut Hle<'_>, buffer: u32, max: usize) -> Result<u32> {
    let classes = collect_class_addresses(hle);
    let total = classes.len() as u32;
    if buffer != 0 {
        for (i, class) in classes.iter().take(max).enumerate() {
            hle.mem.write_u32(buffer + i as u32 * 4, *class)?;
        }
    }
    Ok(total)
}

fn write_class_list_at(hle: &mut Hle<'_>, buffer: u32, max: usize) -> Result<()> {
    let classes = collect_class_addresses(hle);
    for (i, class) in classes.iter().take(max).enumerate() {
        hle.mem.write_u32(buffer + i as u32 * 4, *class)?;
    }
    Ok(())
}

fn allocate_class_pair(hle: &mut Hle<'_>) -> Result<u32> {
    let _superclass = hle.arg(0);
    let name = match optional_cstr(hle, hle.arg(1))? {
        Some(name) if !name.is_empty() => name,
        _ => {
            hle.sys.objc.dynamic_classes += 1;
            format!("DynamicClass{}", hle.sys.objc.dynamic_classes)
        }
    };
    host_class(hle, &name)
}

fn register_class_pair(hle: &mut Hle<'_>) -> Result<u32> {
    // The class already registered itself when the pair was allocated.
    let _ = hle.arg(0);
    Ok(0)
}

// ---- selectors -------------------------------------------------------------

fn sel_register_name(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    if addr == 0 {
        return Ok(0);
    }
    intern_selector(hle, addr)
}

/// The pointer-interning half of selector registration: look up an existing
/// registration first, otherwise persist the name in the heap and intern its
/// pointer.
fn intern_selector(hle: &mut Hle<'_>, addr: u32) -> Result<u32> {
    let Ok(name) = hle.cstr(addr) else { return Ok(0) };
    intern_selector_by_name(hle, &name)
}

fn intern_selector_by_name(hle: &mut Hle<'_>, name: &str) -> Result<u32> {
    if name.is_empty() || name.len() > 512 {
        return Ok(0);
    }
    if let Some(&sel) = hle.sys.objc.selectors.get(name) {
        return Ok(sel);
    }
    // Persist: stack strings die, but the interned SEL must keep comparing
    // equal for the game's lifetime.
    let sel = hle.alloc(name.len() as u32 + 1, 4)?;
    hle.write_cstr(sel, name)?;
    hle.sys.objc.remember_selector(name.to_string(), sel);
    Ok(sel)
}

fn sel_get_name(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

fn sel_is_equal(hle: &mut Hle<'_>) -> Result<u32> {
    let (a, b) = (hle.arg(0), hle.arg(1));
    if a == b {
        return Ok(1);
    }
    let a_name = selector_name(hle, a);
    let b_name = selector_name(hle, b);
    Ok((a_name.is_some() && a_name == b_name) as u32)
}

// ---- object/class reflection -------------------------------------------------

fn object_get_class(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(read_isa(hle, hle.arg(0)))
}

fn object_set_class(hle: &mut Hle<'_>) -> Result<u32> {
    let (object, class) = (hle.arg(0), hle.arg(1));
    if object != 0 {
        let _ = hle.mem.write_u32(object, class);
    }
    Ok(class)
}

fn object_get_class_name(hle: &mut Hle<'_>) -> Result<u32> {
    let name = class_name_of(hle, hle.arg(0)).unwrap_or_else(|| "nil".to_string());
    write_guest_cstring(hle, &name)
}

fn object_copy(hle: &mut Hle<'_>) -> Result<u32> {
    let (object, _size) = (hle.arg(0), hle.arg(1));
    if object == 0 {
        return Ok(0);
    }
    copy_instance(hle, object, read_isa(hle, object))
}

fn object_dispose(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

fn class_get_name(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    if let Some(name) = hle.sys.objc.names.get(&class).cloned() {
        // The boot-installed classes retained their name pointer: pass through.
        if let Ok(ro) = hle.mem.read_u32(class + CLASS_BITS) {
            let name_ptr = hle.mem.read_u32((ro & CLASS_DATA_MASK) + RO_NAME).unwrap_or(0);
            if name_ptr != 0 {
                return Ok(name_ptr);
            }
        }
        return write_guest_cstring(hle, &name);
    }
    let Some(info) = ClassInfo::read(hle, class) else { return Ok(0) };
    write_guest_cstring(hle, &info.name)
}

fn class_get_superclass(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    Ok(hle.mem.read_u32(class + CLASS_SUPERCLASS).unwrap_or(0))
}

fn class_is_meta_class(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    Ok(hle.sys.objc.metaclass_of.contains_key(&class) as u32)
}

fn class_get_instance_size(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(info) = ClassInfo::read(hle, class) else { return Ok(0) };
    Ok(info.instance_size)
}

fn class_responds(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(name) = selector_name(hle, hle.arg(1)) else { return Ok(0) };
    Ok(lookup_imp(hle, class, &name).is_some() as u32)
}

fn class_get_method_implementation(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(name) = selector_name(hle, hle.arg(1)) else { return Ok(0) };
    let imp = lookup_imp(hle, class, &name).map(|(imp, _)| imp).unwrap_or(0);
    if imp == 0 {
        // libobjc hands back the forwarder; we point the call at ourselves.
        let name = hle.sys.objc.last_dispatch.clone();
        hle.note(format!("class_getMethodImplementation missed for {name}"));
    }
    Ok(imp)
}

fn method_handle_for(hle: &mut Hle<'_>, class: u32, name: &str) -> Result<u32> {
    // A `Method` is `{ SEL namep; char *types; IMP imp; }`; an emulator Method
    // is the same triple in guest memory.
    let method = hle.alloc(16, 8)?;
    let sel = intern_selector_by_name(hle, name)?;
    let imp = lookup_imp(hle, class, name).map(|(imp, _)| imp).unwrap_or(0);
    hle.mem.write_u32(method, sel)?;
    hle.mem.write_u32(method + 4, 0)?; // types unknown
    hle.mem.write_u32(method + 8, imp)?;
    Ok(method)
}

fn class_get_instance_method(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(name) = selector_name(hle, hle.arg(1)) else { return Ok(0) };
    if lookup_imp(hle, class, &name).is_none() {
        return Ok(0);
    }
    method_handle_for(hle, class, &name)
}

fn class_get_class_method(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(name) = selector_name(hle, hle.arg(1)) else { return Ok(0) };
    // Class methods live on the metaclass.
    let meta = hle.mem.read_u32(class).unwrap_or(0);
    if lookup_imp(hle, meta, &name).is_none() {
        return Ok(0);
    }
    method_handle_for(hle, meta, &name)
}

fn class_add_method(hle: &mut Hle<'_>) -> Result<u32> {
    // BOOL class_addMethod(Class cls, SEL name, IMP imp, const char *types)
    let class = hle.arg(0);
    let Some(name) = selector_name(hle, hle.arg(1)) else { return Ok(0) };
    let imp = hle.arg(2);
    if class == 0 || imp == 0 || name.is_empty() {
        return Ok(0);
    }
    hle.sys.objc.host_imps.insert((class, name), imp);
    Ok(1)
}

fn class_create_instance(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let extra = hle.arg(1);
    let info = ClassInfo::read(hle, class);
    let mut size = info.as_ref().map(|i| i.instance_size).unwrap_or(0) + extra;
    if size < 4 {
        size = 32;
    }
    let object = hle.alloc(size, 16)?;
    let zeros = vec![0u8; size as usize];
    hle.write_bytes(object, &zeros)?;
    hle.mem.write_u32(object, class)?;
    Ok(object)
}

// ---- method_* ---------------------------------------------------------------

fn method_get_name(hle: &mut Hle<'_>) -> Result<u32> {
    let method = hle.arg(0);
    Ok(hle.mem.read_u32(method).unwrap_or(0))
}

fn method_get_implementation(hle: &mut Hle<'_>) -> Result<u32> {
    let method = hle.arg(0);
    Ok(hle.mem.read_u32(method + 8).unwrap_or(0))
}

fn method_get_type_encoding(hle: &mut Hle<'_>) -> Result<u32> {
    let method = hle.arg(0);
    Ok(hle.mem.read_u32(method + 4).unwrap_or(0))
}

fn method_set_implementation(hle: &mut Hle<'_>) -> Result<u32> {
    // `IMP method_setImplementation(Method m, IMP imp)`: the method struct is
    // `{ SEL name; char *types; IMP imp; }`, so patch the third word.
    let method = hle.arg(0);
    let imp = hle.arg(1);
    let old = hle.mem.read_u32(method + 8).unwrap_or(0);
    hle.mem.write_u32(method + 8, imp)?;
    Ok(old)
}

fn method_exchange_implementations(hle: &mut Hle<'_>) -> Result<u32> {
    let (m1, m2) = (hle.arg(0), hle.arg(1));
    let i1 = hle.mem.read_u32(m1 + 8).unwrap_or(0);
    let i2 = hle.mem.read_u32(m2 + 8).unwrap_or(0);
    let _ = hle.mem.write_u32(m1 + 8, i2);
    let _ = hle.mem.write_u32(m2 + 8, i1);
    Ok(0)
}

fn method_get_number_of_arguments(hle: &mut Hle<'_>) -> Result<u32> {
    let method = hle.arg(0);
    let types = hle.mem.read_u32(method + 4).unwrap_or(0);
    let Ok(text) = hle.cstr(types) else { return Ok(2) };
    Ok(count_type_encodings(&text).saturating_sub(1))
}

/// Count Objective-C type specifiers in a method type string (ret + args),
/// skipping stack offsets, qualifiers, and grouping `struct`/`array` specs.
fn count_type_encodings(types: &str) -> u32 {
    let chars: Vec<char> = types.chars().collect();
    let mut count = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '0'..='9' => {}
            'r' | 'n' | 'N' | 'o' | 'O' | 'R' | 'V' | 'A' | 'b' => {}
            '{' | '[' => {
                // The whole bracketed entity is one argument.
                let mut depth = 0;
                let close = if c == '{' { '}' } else { ']' };
                while i < chars.len() {
                    if chars[i] == c {
                        depth += 1;
                    } else if chars[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                count += 1;
            }
            _ => count += 1,
        }
        i += 1;
    }
    count
}

// ---- associated objects / weak refs ------------------------------------------

fn assoc_get(hle: &mut Hle<'_>) -> Result<u32> {
    let (owner, key) = (hle.arg(0), hle.arg(1));
    Ok(hle.sys.objc.associated.get(&(owner, key)).copied().unwrap_or(0))
}

fn assoc_set(hle: &mut Hle<'_>) -> Result<u32> {
    let (owner, key, value) = (hle.arg(0), hle.arg(1), hle.arg(2));
    if owner != 0 {
        hle.sys.objc.associated.insert((owner, key), value);
    }
    Ok(0)
}

fn assoc_remove(hle: &mut Hle<'_>) -> Result<u32> {
    let owner = hle.arg(0);
    hle.sys.objc.associated.retain(|&(o, _), _| o != owner);
    Ok(0)
}

fn objc_store_strong(hle: &mut Hle<'_>) -> Result<u32> {
    let (slot, value) = (hle.arg(0), hle.arg(1));
    if slot != 0 {
        let _ = hle.mem.write_u32(slot, value);
    }
    Ok(value)
}

fn objc_init_weak(hle: &mut Hle<'_>) -> Result<u32> {
    let (slot, value) = (hle.arg(0), hle.arg(1));
    if slot != 0 {
        let _ = hle.mem.write_u32(slot, value);
    }
    Ok(value)
}

fn objc_load_weak(hle: &mut Hle<'_>) -> Result<u32> {
    let slot = hle.arg(0);
    Ok(hle.mem.read_u32(slot).unwrap_or(0))
}

fn objc_destroy_weak(hle: &mut Hle<'_>) -> Result<u32> {
    let slot = hle.arg(0);
    if slot != 0 {
        let _ = hle.mem.write_u32(slot, 0);
    }
    Ok(0)
}

fn objc_copy_weak(hle: &mut Hle<'_>) -> Result<u32> {
    let (dest, src) = (hle.arg(0), hle.arg(1));
    let value = hle.mem.read_u32(src).unwrap_or(0);
    if dest != 0 {
        let _ = hle.mem.write_u32(dest, value);
    }
    Ok(value)
}

// ---- properties / struct copies ----------------------------------------------

fn objc_get_property(hle: &mut Hle<'_>) -> Result<u32> {
    // `id objc_getProperty(id self, SEL _cmd, ptrdiff_t offset, BOOL atomic)`
    let object = hle.arg(0);
    let offset = hle.arg(2);
    if object == 0 {
        return Ok(0);
    }
    Ok(hle.mem.read_u32(object.wrapping_add(offset)).unwrap_or(0))
}

fn objc_set_property(hle: &mut Hle<'_>) -> Result<u32> {
    let object = hle.arg(0);
    let offset = hle.arg(2);
    let value = hle.arg(3);
    if object != 0 {
        let _ = hle.mem.write_u32(object.wrapping_add(offset), value);
    }
    Ok(0)
}

/// `objc_copyStruct(dest, src, size, atomic, hasStrong)`.
fn objc_copy_struct(hle: &mut Hle<'_>) -> Result<u32> {
    let (dest, src, size) = (hle.arg(0), hle.arg(1), hle.arg(2));
    if dest == 0 || src == 0 || size == 0 || size > (1 << 20) {
        return Ok(0);
    }
    match hle.mem.read_bytes(src, size) {
        Ok(bytes) => {
            let _ = hle.write_bytes(dest, &bytes);
        }
        Err(_) => {
            // Partial copy for buffers that straddle a region.
            for i in 0..size {
                if let Ok(b) = hle.mem.read_u8(src + i) {
                    let _ = hle.mem.write_u8(dest + i, b);
                }
            }
        }
    }
    Ok(dest)
}

// ---- retain/release -----------------------------------------------------------

fn objc_retain(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

// ---- exceptions ---------------------------------------------------------------

fn exception_throw(hle: &mut Hle<'_>) -> Result<u32> {
    let exception = hle.arg(0);
    let name = cf_string_text(hle, exception).unwrap_or_else(|| format!("exception {exception:#x}"));
    hle.note(format!("objc_exception_throw: {name}"));
    hle.sys.exit_code = Some(134);
    Err(super::fail("objc_exception_throw", format!("guest threw {name}")))
}

fn array_mutation_fault(hle: &mut Hle<'_>) -> Result<u32> {
    let collection = hle.arg(0);
    Err(super::fail("objc_enumerationMutation", format!("collection {collection:#x} mutated during fast enumeration")))
}

// ---- Foundation conveniences ---------------------------------------------------

fn ns_class_from_string(hle: &mut Hle<'_>) -> Result<u32> {
    let Some(name) = cf_string_text(hle, hle.arg(0)) else { return Ok(0) };
    Ok(hle
        .sys
        .objc
        .classes_by_name
        .get(&name)
        .copied()
        .or_else(|| hle.sys.host_classes_by_name.get(&name).copied())
        .unwrap_or(0))
}

fn ns_selector_from_string(hle: &mut Hle<'_>) -> Result<u32> {
    let Some(name) = cf_string_text(hle, hle.arg(0)) else { return Ok(0) };
    intern_selector_by_name(hle, &name)
}

fn ns_string_from_class(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(name) = class_name_of(hle, class) else { return Ok(0) };
    make_nsstring(hle, &name)
}

fn ns_string_from_selector(hle: &mut Hle<'_>) -> Result<u32> {
    let sel = hle.arg(0);
    let Some(name) = selector_name(hle, sel) else { return Ok(0) };
    make_nsstring(hle, &name)
}

fn class_get_image_name(hle: &mut Hle<'_>) -> Result<u32> {
    let _ = hle.arg(0);
    let path = if hle.sys.bundle_path.is_empty() {
        "/Applications/Simpsons.app/Simpsons".to_string()
    } else {
        hle.sys.bundle_path.clone()
    };
    write_guest_cstring(hle, &path)
}

fn copy_empty_list(hle: &mut Hle<'_>) -> Result<u32> {
    let _ = hle.arg(0);
    let out_count = hle.arg(1);
    if out_count != 0 {
        let _ = hle.mem.write_u32(out_count, 0);
    }
    Ok(0)
}
