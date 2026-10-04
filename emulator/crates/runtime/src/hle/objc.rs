//! The Objective-C runtime bridge.
//!
//! The Simpsons engine is a C++ core (`MonkeyApp`, `AppEngine`, `Display`,
//! `Canvas`, …) wrapped in a thin Objective-C layer: the app delegate, the
//! `EAGLView`, and the handful of Foundation/CoreFoundation objects the engine
//! touches.  Everything else reaches the runtime through `objc_msgSend`, which
//! the reference decompilation calls 747 times.
//!
//! Rather than reimplementing libobjc's metadata tables wholesale, the bridge
//! resolves a message *against the guest's own memory*:
//!
//! * a class object is read straight out of the image (`isa`, `superclass`,
//!   `data`), with both the legacy (`iOS 2-4`) and modern (`iOS 5+`) layouts
//!   recognised heuristically;
//! * its method list is walked for the selector, following the superclass chain
//!   for instance methods and the metaclass chain for class methods;
//! * a matching implementation is executed *in the guest* by asking the machine
//!   to jump to the IMP — a HLE handler never needs to emulate C++/ObjC code
//!   that is already in the image.
//!
//! A small set of Foundation behaviours that are pure runtime bookkeeping
//! (`alloc`, `retain`, `respondsToSelector:`, …) is answered directly.

use std::collections::HashMap;

use super::{Hle, HleFn};
use crate::error::Result;

/// Objects of the emulator's own UIKit/GL classes carry this word as their
/// class `data`, so a message send can tell them apart from image classes.
pub const HOST_CLASS_MAGIC: u32 = 0x484f_5354; // "HOST"

/// Where the emulator's synthetic class objects and instances live.
pub const HOST_OBJECTS_BASE: u32 = 0x7002_0000;
pub const HOST_OBJECTS_SIZE: u32 = 0x0002_0000;

/// A method implemented by the emulator for one of its host classes.
pub type HostMethod = fn(&mut Hle<'_>, receiver: u32) -> Result<u32>;

/// The host method table: `(class, selector, implementation)`.
pub const HOST_METHODS: &[(&str, &str, HostMethod)] = &[
    ("NSObject", "init", host_return_self),
    ("NSObject", "retain", host_return_self),
    ("NSObject", "autorelease", host_return_self),
    ("NSObject", "release", host_return_zero),
    ("NSObject", "dealloc", host_return_zero),
    ("NSObject", "retainCount", host_return_one),
    ("NSObject", "respondsToSelector:", host_return_one),
    ("NSObject", "description", host_description),
    ("NSObject", "hash", host_return_receiver),
    ("NSObject", "isEqual:", host_pointer_equal),
    ("NSObject", "isKindOfClass:", host_return_one),
    ("NSObject", "isMemberOfClass:", host_return_one),
    ("NSObject", "class", host_object_class),
    ("NSObject", "superclass", host_return_zero),
    ("NSObject", "performSelector:", host_return_zero),
    ("NSObject", "performSelector:withObject:", host_return_zero),
    ("NSObject", "performSelector:withObject:afterDelay:", host_return_zero),
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
    let name = hle
        .sys
        .host_classes
        .get(&receiver)
        .cloned()
        .or_else(|| hle.mem.read_u32(receiver).ok().and_then(|isa| hle.sys.host_classes.get(&isa).cloned()))
        .unwrap_or_else(|| "NSObject".to_string());
    let text = format!("<{name}: {receiver:#010x}>");
    crate::hle::write_guest_cstring(hle, &text)
}

fn host_object_class(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(hle.mem.read_u32(receiver).unwrap_or(0))
}

/// Create (once) a synthetic class object for one of the emulator's classes.
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
    let addr = host_alloc(hle, 32, 16)?;
    hle.mem.write_u32(addr, 0)?;
    hle.mem.write_u32(addr + 4, 0)?;
    hle.mem.write_u32(addr + 8, 0)?;
    hle.mem.write_u32(addr + 12, 0)?;
    hle.mem.write_u32(addr + 16, HOST_CLASS_MAGIC)?;
    hle.sys.host_classes.insert(addr, name.to_string());
    hle.sys.host_classes_by_name.insert(name.to_string(), addr);
    Ok(addr)
}

/// Allocate an object of a host class, initialised with the class as its `isa`.
pub fn host_instance(hle: &mut Hle<'_>, name: &str, size: u32) -> Result<u32> {
    let class = host_class(hle, name)?;
    let object = host_alloc(hle, size.max(32), 16)?;
    let zeros = vec![0u8; size.max(32) as usize];
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

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    ("objc_msgSend", msg_send),
    ("objc_msgSendSuper2", msg_send_super),
    ("objc_msgSend_stret", msg_send),
    ("objc_getClass", get_class),
    ("objc_getMetaClass", get_class),
    ("object_getClass", object_get_class),
    ("objc_lookUpClass", get_class),
    ("sel_registerName", sel_register_name),
    ("sel_getName", sel_get_name),
    ("class_getName", class_get_name),
    ("class_getSuperclass", class_get_superclass),
    ("class_respondsToSelector", class_responds),
    ("method_setImplementation", method_set_implementation),
    ("objc_enumerationMutation", noop),
    ("objc_getProperty", objc_get_property),
    ("objc_setProperty", objc_set_property),
    ("objc_retain", objc_retain),
    ("objc_release", noop_zero),
    ("objc_autorelease", objc_retain),
    ("objc_autoreleasePoolPush", noop_zero),
    ("objc_autoreleasePoolPop", noop),
    ("NSClassFromString", ns_class_from_string),
];

fn noop(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn noop_zero(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

/// A selector is a pointer to its own NUL-terminated name (the classic
/// non-fragile ABI representation, which the guest's `__objc_methname` strings
/// satisfy directly).
fn selector_name(hle: &Hle<'_>, sel: u32) -> Option<String> {
    if sel == 0 {
        return None;
    }
    let name = hle.cstr(sel).ok()?;
    if name.is_empty() || name.len() > 512 || name.contains('\u{fffd}') {
        return None;
    }
    Some(name)
}

/// A very small validation for "does this look like an Objective-C class
/// pointer": readable, with a name somewhere in the first 32 bytes of its data.
fn is_probable_class(hle: &Hle<'_>, addr: u32) -> bool {
    if addr == 0 || addr & 3 != 0 {
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
    /// Whether the class uses the modern (`class_ro_t`) layout.  Recorded for
    /// diagnostics: the method search in this module handles both.
    #[allow(dead_code)]
    modern: bool,
}

impl ClassInfo {
    fn read(hle: &Hle<'_>, class_addr: u32) -> Option<ClassInfo> {
        if !is_probable_class(hle, class_addr) {
            return None;
        }
        let _isa = hle.mem.read_u32(class_addr).ok()?;
        let superclass = hle.mem.read_u32(class_addr + 4).ok()?;
        let data = hle.mem.read_u32(class_addr + 16).ok()?;

        // Modern (non-fragile) ABI: `data & 3 == 3` and points at a `class_ro_t`.
        if data & 3 == 3 {
            let ro = data & !3;
            let name_ptr = hle.mem.read_u32(ro + 0x10).ok()?;
            let name = hle.cstr(name_ptr).ok()?;
            if plausible_class_name(&name) {
                let methods = hle.mem.read_u32(ro + 0x14).ok()?;
                let instance_size = hle.mem.read_u32(ro + 8).ok()?;
                if methods != 0 {
                    return Some(ClassInfo {
                        name,
                        superclass,
                        instance_size,
                        methods,
                        legacy_method_lists: 0,
                        modern: true,
                    });
                }
            }
        }

        // Legacy ABI: name at +8 (after isa/superclass), method list array at +0x24.
        let legacy_name = hle.mem.read_u32(class_addr + 8).ok()?;
        let name = hle.cstr(legacy_name).ok()?;
        if !plausible_class_name(&name) {
            // The last resort: the `data` word may itself be a class_ro_t even
            // without the tag bits (some older runtimes store it untagged).
            if let Ok(name_ptr) = hle.mem.read_u32(data + 0x10) {
                if let Ok(name) = hle.cstr(name_ptr) {
                    if plausible_class_name(&name) {
                        let methods = hle.mem.read_u32(data + 0x14).unwrap_or(0);
                        let instance_size = hle.mem.read_u32(data + 8).unwrap_or(0);
                        return Some(ClassInfo {
                            name,
                            superclass,
                            instance_size,
                            methods,
                            legacy_method_lists: 0,
                            modern: true,
                        });
                    }
                }
            }
            return None;
        }
        let instance_size = hle.mem.read_u32(class_addr + 0x14).ok()?;
        let legacy_method_lists = hle.mem.read_u32(class_addr + 0x24).ok()?;
        Some(ClassInfo {
            name,
            superclass,
            instance_size,
            methods: 0,
            legacy_method_lists,
            modern: false,
        })
    }
}

fn plausible_class_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 128
        && name.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Walk a `method_list_t` (v2: `entsize` word then `entsize/sizeof(entry)` entries;
/// legacy: a `count` word then entries) looking for `selector`.
fn search_method_list(hle: &Hle<'_>, list: u32, selector: &str) -> Option<u32> {
    if list == 0 || list < 0x1000 {
        return None;
    }
    let first = hle.mem.read_u32(list).ok()?;
    // v2 list: the low bits encode the entry size (12 or 16) and the top bits
    // are flags.  A legacy list starts with a small entry count.
    let (entsize, count, entries) = if first & 0xffff_0000 != 0 || first & 0x3 != 0 {
        let count = hle.mem.read_u32(list + 4).ok()?;
        if count > 4096 {
            return None;
        }
        let size = first & 0xffff;
        if size < 12 || size > 64 {
            return None;
        }
        (size, count, list + 8)
    } else if first < 4096 {
        (12, first, list + 4)
    } else {
        return None;
    };
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

/// Find the implementation of `selector` for instance (`class_method = false`)
/// or class (`true`) messages, walking the inheritance chain.
fn lookup_imp(hle: &Hle<'_>, class_addr: u32, selector: &str, class_method: bool) -> Option<(u32, String)> {
    let mut current = class_addr;
    let mut depth = 0;
    while current != 0 && depth < 16 {
        if let Some(info) = ClassInfo::read(hle, current) {
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
            // The metaclass (isa of a class object) carries the class methods.
            if class_method {
                let isa = hle.mem.read_u32(current).ok()?;
                if isa != 0 && isa != current {
                    if let Some(meta) = ClassInfo::read(hle, isa) {
                        if let Some(imp) = search_method_list(hle, meta.methods, selector) {
                            return Some((imp, meta.name));
                        }
                    }
                }
            }
            current = info.superclass;
        } else {
            break;
        }
        depth += 1;
    }
    None
}

/// Name the class an object (or class object) belongs to.
fn class_name_of(hle: &Hle<'_>, object: u32) -> Option<String> {
    let isa = hle.mem.read_u32(object).ok()?;
    if let Some(info) = ClassInfo::read(hle, isa) {
        return Some(info.name);
    }
    // The object may already be a class object.
    ClassInfo::read(hle, object).map(|info| info.name)
}

/// `id objc_msgSend(id self, SEL _cmd, ...)`.
///
/// The runtime answers the bookkeeping selectors itself and otherwise transfers
/// control to the guest's implementation, arranging the return to come back to
/// the caller through the machine's shadow return stack.
fn msg_send(hle: &mut Hle<'_>) -> Result<u32> {
    dispatch_message(hle, hle.arg(0), hle.arg(1), false)
}

/// `id objc_msgSendSuper2(struct objc_super *super, SEL _cmd, ...)`: the struct
/// holds `{ id receiver; Class currentClass; }`, and the search starts at
/// `currentClass`'s *superclass*.
fn msg_send_super(hle: &mut Hle<'_>) -> Result<u32> {
    let sup = hle.arg(0);
    let selector = hle.arg(1);
    let receiver = hle.mem.read_u32(sup)?;
    // `currentClass` at +4 is the class whose superclass the message goes to.
    let this_class = hle.mem.read_u32(sup + 4)?;
    let superclass = if this_class != 0 { hle.mem.read_u32(this_class + 4).unwrap_or(0) } else { 0 };
    let _ = selector;
    // Shift the arguments down: msgSend's arg0 is the receiver, and the
    // superclass becomes the class to start searching from.  We keep the
    // receiver in r0 and hand the search a fake "self class".
    dispatch_with_start(hle, receiver, selector, superclass)
}

fn dispatch_message(hle: &mut Hle<'_>, receiver: u32, selector: u32, class_method: bool) -> Result<u32> {
    dispatch_with_start(hle, receiver, selector, 0).map(|value| {
        let _ = class_method;
        value
    })
}

fn dispatch_with_start(hle: &mut Hle<'_>, receiver: u32, selector: u32, mut start_class: u32) -> Result<u32> {
    let Some(name) = selector_name(hle, selector) else {
        hle.note(format!("objc_msgSend: bad selector {selector:#x}"));
        return Ok(0);
    };

    // Class objects answer class methods; asking for `class` on a class object
    // is also extremely common, so identify the receiver first.
    let receiver_isa = hle.mem.read_u32(receiver).unwrap_or(0);
    let receiver_is_class = ClassInfo::read(hle, receiver).is_some() && receiver_isa != 0;

    // Objects (and classes) the emulator itself provides.
    if let Some(class_name) = hle.sys.host_classes.get(&receiver_isa).cloned() {
        return dispatch_host_method(hle, &class_name, receiver, &name);
    }
    if let Some(class_name) = hle.sys.host_classes.get(&receiver).cloned() {
        // A class object: `alloc`/`new` and the class-method protocol.
        let instance = match name.as_str() {
            "alloc" | "allocWithZone:" | "new" => super::objc::host_instance(hle, &class_name, 64),
            _ => Ok(0),
        }?;
        if instance != 0 {
            return Ok(instance);
        }
        return dispatch_host_method(hle, &class_name, receiver, &name);
    }
    let _ = &receiver_isa;

    if start_class == 0 {
        start_class = receiver_isa;
    }

    // Foundation bookkeeping the runtime owns.
    match name.as_str() {
        "alloc" | "allocWithZone:" => {
            let info = ClassInfo::read(hle, receiver);
            let size = info.as_ref().map(|i| i.instance_size).unwrap_or(0);
            let size = if size == 0 { 32 } else { size };
            let object = hle.alloc(size, 16)?;
            let zeros = vec![0u8; size as usize];
            hle.write_bytes(object, &zeros)?;
            hle.mem.write_u32(object, receiver)?;
            if let Some(info) = &info {
                hle.note(format!("objc: [{}, alloc] -> {object:#x} ({size} bytes)", info.name));
            }
            return Ok(object);
        }
        "retain" | "autorelease" | "self" | "init" => {
            if name == "init" {
                if let Some((imp, _)) = lookup_imp(hle, start_class, &name, receiver_is_class) {
                    hle.call_guest(imp);
                    return Ok(0);
                }
            }
            return Ok(receiver);
        }
        "release" | "dealloc" => return Ok(0),
        "retainCount" => return Ok(1),
        "class" => {
            if receiver_is_class {
                return Ok(receiver);
            }
            return Ok(receiver_isa);
        }
        "superclass" => {
            let base = if receiver_is_class { receiver_isa } else { receiver_isa };
            return Ok(hle.mem.read_u32(base + 4).unwrap_or(0));
        }
        "respondsToSelector:" => {
            let sel = hle.arg(2);
            let probe = selector_name(hle, sel).unwrap_or_default();
            return Ok(lookup_imp(hle, start_class, &probe, false).is_some() as u32);
        }
        "isKindOfClass:" | "isMemberOfClass:" => {
            let target = hle.arg(2);
            let target_name = ClassInfo::read(hle, target).map(|i| i.name);
            let mut current = if receiver_is_class { receiver } else { receiver_isa };
            let mut depth = 0;
            while current != 0 && depth < 32 {
                if name == "isMemberOfClass:" && current == target {
                    return Ok(1);
                }
                let Some(info) = ClassInfo::read(hle, current) else { break };
                if name == "isKindOfClass:" && Some(info.name.clone()) == target_name {
                    return Ok(1);
                }
                current = info.superclass;
                depth += 1;
            }
            return Ok(0);
        }
        "hash" => return Ok(receiver),
        "isEqual:" => return Ok((receiver == hle.arg(2)) as u32),
        "description" => {
            let class = class_name_of(hle, receiver).unwrap_or_else(|| "NSObject".to_string());
            let text = format!("<{class}: {receiver:#010x}>");
            return super::write_guest_cstring(hle, &text);
        }
        "performSelector:" | "performSelector:withObject:" | "performSelector:withObject:withObject:" => {
            let sel = hle.arg(2);
            let probe = selector_name(hle, sel).unwrap_or_default();
            if let Some((imp, _)) = lookup_imp(hle, start_class, &probe, receiver_is_class) {
                hle.call_guest(imp);
                return Ok(0);
            }
            return Ok(0);
        }
        _ => {}
    }

    if let Some((imp, class_name)) = lookup_imp(hle, start_class, &name, receiver_is_class) {
        hle.note(format!("objc: [{class_name} {name}] -> {imp:#x}"));
        hle.call_guest(imp);
        return Ok(0);
    }

    // No implementation: log once and answer nil, exactly like sending to a
    // class that does not respond (which is what a missing UIKit would do).
    let class = class_name_of(hle, receiver).unwrap_or_else(|| format!("{receiver:#x}"));
    hle.note(format!("objc: no implementation for [{class} {name}]"));
    *hle.sys.unimplemented.entry(format!("-[{class} {name}]")).or_insert(0) += 1;
    Ok(0)
}

/// Dispatch to a method implemented by the emulator for one of its own classes.
fn dispatch_host_method(hle: &mut Hle<'_>, class_name: &str, receiver: u32, selector: &str) -> Result<u32> {
    for (class, sel, handler) in HOST_METHODS {
        if (*class == class_name || *class == "NSObject") && *sel == selector {
            return handler(hle, receiver);
        }
    }
    for (class, sel, handler) in crate::hle::ui::HOST_METHODS {
        if (*class == class_name || *class == "NSObject") && *sel == selector {
            return handler(hle, receiver);
        }
    }
    for (class, sel, handler) in crate::hle::gl::HOST_METHODS {
        if (*class == class_name || *class == "NSObject") && *sel == selector {
            return handler(hle, receiver);
        }
    }
    hle.note(format!("objc: no host implementation for [{class_name} {selector}]"));
    *hle.sys.unimplemented.entry(format!("-[{class_name} {selector}]")).or_insert(0) += 1;
    Ok(0)
}

fn get_class(hle: &mut Hle<'_>) -> Result<u32> {
    let name = hle.cstr(hle.arg(0))?;
    hle.note(format!("objc_getClass({name})"));
    Ok(0)
}

fn object_get_class(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.mem.read_u32(hle.arg(0)).unwrap_or(0))
}

fn sel_register_name(hle: &mut Hle<'_>) -> Result<u32> {
    // Selectors are their own names in the guest image, so the pointer passes
    // straight through.
    Ok(hle.arg(0))
}

fn sel_get_name(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

fn class_get_name(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(info) = ClassInfo::read(hle, class) else { return Ok(0) };
    super::write_guest_cstring(hle, &info.name)
}

fn class_get_superclass(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    Ok(hle.mem.read_u32(class + 4).unwrap_or(0))
}

fn class_responds(hle: &mut Hle<'_>) -> Result<u32> {
    let class = hle.arg(0);
    let Some(name) = selector_name(hle, hle.arg(1)) else { return Ok(0) };
    Ok(lookup_imp(hle, class, &name, true).is_some() as u32)
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

fn objc_get_property(hle: &mut Hle<'_>) -> Result<u32> {
    // `id objc_getProperty(id self, SEL _cmd, ptrdiff_t offset, BOOL atomic)`
    let object = hle.arg(0);
    let offset = hle.arg(2);
    Ok(hle.mem.read_u32(object + offset).unwrap_or(0))
}

fn objc_set_property(hle: &mut Hle<'_>) -> Result<u32> {
    let object = hle.arg(0);
    let offset = hle.arg(2);
    let value = hle.arg(3);
    hle.mem.write_u32(object + offset, value)?;
    Ok(0)
}

fn objc_retain(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(_hle.arg(0))
}

fn ns_class_from_string(hle: &mut Hle<'_>) -> Result<u32> {
    let name = hle.cstr(hle.arg(0))?;
    hle.note(format!("NSClassFromString({name})"));
    Ok(0)
}

/// Convenience used by the UIKit stubs: look up a Foundation constant.
pub fn constant_map() -> HashMap<&'static str, u32> {
    HashMap::new()
}
