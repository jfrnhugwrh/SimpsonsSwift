//! CoreFoundation, at the level the engine uses it: strings, arrays, bundles,
//! URLs and the locale plumbing that its address-book/submission code touches.
//!
//! A `CFStringRef` is a guest allocation whose *host* text lives in
//! [`System::cf_strings`]; that is enough for the guest's opaque use of the
//! handle (it only ever passes the pointer back to us, or to Foundation classes
//! that we also provide).

use super::{Hle, HleFn};
use crate::error::Result;

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    ("CFStringCreateWithCString", string_create_with_cstring),
    ("CFStringCreateWithBytes", string_create_with_bytes),
    ("CFStringCreateWithCStringNoCopy", string_create_with_cstring),
    ("CFStringCreateMutable", string_create_mutable),
    ("CFStringCreateMutableCopy", string_create_mutable_copy),
    ("CFStringCreateCopy", string_copy),
    ("CFStringCreateWithFormat", string_create_with_format),
    ("CFStringCreateWithFormatAndArguments", string_create_with_format),
    ("CFStringCreateWithSubstring", string_create_substring),
    ("CFStringAppend", string_append),
    ("CFStringAppendCString", string_append_cstring),
    ("CFStringAppendCharacters", string_append_characters),
    ("CFStringAppendFormat", string_append_format),
    ("CFStringGetCString", string_get_cstring),
    ("CFStringGetLength", string_get_length),
    ("CFStringGetCharacterAtIndex", string_get_char_at),
    ("CFStringGetCharacters", string_get_characters),
    ("CFStringGetCharactersPtr", string_get_characters_ptr),
    ("CFStringGetBytes", string_get_bytes),
    ("CFStringGetIntValue", string_get_int_value),
    ("CFStringCompare", string_compare),
    ("CFStringCompareWithOptions", string_compare),
    ("CFStringDelete", string_delete),
    ("CFStringUppercase", string_uppercase),
    ("CFArrayGetCount", array_get_count),
    ("CFArrayGetValueAtIndex", array_get_value),
    ("CFArrayCreate", array_create),
    ("CFArrayCreateMutable", array_create),
    ("CFArrayAppendValue", array_append),
    ("CFBundleGetMainBundle", bundle_main),
    ("CFBundleCopyBundleURL", bundle_copy_url),
    ("CFBundleCopyResourceURL", bundle_copy_resource_url),
    ("CFBundleCopyBundleLocalizations", bundle_localizations),
    ("CFURLCreateCopyAppendingPathComponent", url_append_component),
    ("CFURLCreateCopyDeletingLastPathComponent", url_delete_last_component),
    ("CFURLGetFileSystemRepresentation", url_get_fs_representation),
    ("CFLocaleCopyCurrent", locale_copy_current),
    ("CFLocaleCopyPreferredLanguages", locale_preferred_languages),
    ("CFLocaleCreate", locale_copy_current),
    ("CFLocaleGetValue", locale_get_value),
    ("CFCharacterSetGetPredefined", charset_predefined),
    ("CFCharacterSetIsCharacterMember", charset_is_member),
    ("CFRelease", cf_release),
    ("CFRetain", cf_retain),
    ("CFShow", cf_show),
    ("CFGetTypeID", cf_get_type_id),
    ("CFStringGetTypeID", cf_get_type_id),
    ("CFArrayGetTypeID", cf_get_type_id),
    ("CFEqual", cf_equal),
    ("CFHash", cf_retain),
];

/// What a CF object handed to the guest actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfObject {
    /// A string; the text is also mirrored in `System::cf_strings`.
    String,
    Array,
    /// A file system location (bundle, URL).
    Path(String),
    Bundle,
    Locale,
    CharacterSet,
    /// A number/boolean/other scalar, stored inline.
    Scalar(u32),
}

/// Guest `CFStringRef` layout (our own): `{ isa, length, data_pointer }`.
fn make_cf_string(hle: &mut Hle<'_>, text: &str) -> Result<u32> {
    let bytes = text.as_bytes();
    let data = hle.alloc(bytes.len().max(1) as u32 + 1, 4)?;
    hle.write_bytes(data, bytes)?;
    hle.mem.write_u8(data + bytes.len() as u32, 0)?;
    let handle = hle.alloc(16, 8)?;
    let class = super::objc::host_class(hle, "NSString")?;
    hle.mem.write_u32(handle, class)?;
    hle.mem.write_u32(handle + 4, text.chars().count() as u32)?;
    hle.mem.write_u32(handle + 8, data)?;
    hle.sys.cf_strings.insert(handle, text.to_string());
    hle.sys.cf_objects.insert(handle, CfObject::String);
    Ok(handle)
}

fn make_cf_object(hle: &mut Hle<'_>, object: CfObject) -> Result<u32> {
    let handle = hle.alloc(16, 8)?;
    let class = super::objc::host_class(hle, "NSObject")?;
    hle.mem.write_u32(handle, class)?;
    hle.mem.write_u32(handle + 4, 0)?;
    hle.mem.write_u32(handle + 8, 0)?;
    hle.sys.cf_objects.insert(handle, object);
    Ok(handle)
}

fn string_text(hle: &Hle<'_>, handle: u32) -> Result<String> {
    if let Some(text) = hle.sys.cf_strings.get(&handle) {
        return Ok(text.clone());
    }
    // Strings that were created by the guest itself (a literal in `__cstring`)
    // come through as a plain pointer.
    hle.cstr(handle)
}

fn string_create_with_cstring(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let text = if addr == 0 { String::new() } else { hle.cstr(addr)? };
    let handle = make_cf_string(hle, &text)?;
    hle.note(format!("CFStringCreateWithCString(\"{text}\")"));
    Ok(handle)
}

fn string_create_with_bytes(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let len = hle.arg(1);
    let bytes = if addr == 0 { Vec::new() } else { hle.bytes(addr, len)? };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let handle = make_cf_string(hle, &text)?;
    hle.note(format!("CFStringCreateWithBytes({len} bytes)"));
    Ok(handle)
}

fn string_create_mutable(hle: &mut Hle<'_>) -> Result<u32> {
    let _ = (hle.arg(0), hle.arg(1));
    make_cf_string(hle, "")
}

fn string_create_mutable_copy(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    make_cf_string(hle, &text)
}

fn string_copy(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    make_cf_string(hle, &text)
}

fn string_create_with_format(hle: &mut Hle<'_>) -> Result<u32> {
    let fmt = hle.cstr(hle.arg(0))?;
    let sp = hle.sp();
    let text = {
        let mut va = super::VaList::new(&*hle.cpu, &*hle.mem, sp, 1);
        super::format_string(&fmt, &mut va)?
    };
    make_cf_string(hle, &text)
}

fn string_create_substring(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    let start = hle.arg(1) as usize;
    let len = hle.arg(2) as usize;
    let chars: Vec<char> = text.chars().collect();
    let slice: String = chars.iter().skip(start).take(len).collect();
    make_cf_string(hle, &slice)
}

fn string_append(hle: &mut Hle<'_>) -> Result<u32> {
    let target = hle.arg(0);
    let other = string_text(hle, hle.arg(1))?;
    let mut text = string_text(hle, target)?;
    text.push_str(&other);
    update_cf_string(hle, target, &text)
}

fn string_append_cstring(hle: &mut Hle<'_>) -> Result<u32> {
    let target = hle.arg(0);
    let text = hle.cstr(hle.arg(1))?;
    let _encoding = hle.arg(2);
    let mut current = string_text(hle, target)?;
    current.push_str(&text);
    update_cf_string(hle, target, &current)
}

fn string_append_characters(hle: &mut Hle<'_>) -> Result<u32> {
    let target = hle.arg(0);
    let addr = hle.arg(1);
    let count = hle.arg(2);
    let bytes = hle.bytes(addr, count * 2)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text = String::from_utf16_lossy(&units);
    let mut current = string_text(hle, target)?;
    current.push_str(&text);
    update_cf_string(hle, target, &current)
}

fn string_append_format(hle: &mut Hle<'_>) -> Result<u32> {
    let target = hle.arg(0);
    let fmt = hle.cstr(hle.arg(1))?;
    let sp = hle.sp();
    let text = {
        let mut va = super::VaList::new(&*hle.cpu, &*hle.mem, sp, 2);
        super::format_string(&fmt, &mut va)?
    };
    let mut current = string_text(hle, target)?;
    current.push_str(&text);
    update_cf_string(hle, target, &current)
}

fn update_cf_string(hle: &mut Hle<'_>, handle: u32, text: &str) -> Result<u32> {
    let data = hle.alloc(text.len() as u32 + 1, 4)?;
    hle.write_cstr(data, text)?;
    hle.mem.write_u32(handle + 4, text.chars().count() as u32)?;
    hle.mem.write_u32(handle + 8, data)?;
    hle.sys.cf_strings.insert(handle, text.to_string());
    Ok(0)
}

fn string_get_cstring(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    let buffer = hle.arg(1);
    let size = hle.arg(2);
    let _encoding = hle.arg(3);
    if buffer == 0 {
        // `CFStringGetCString(..., NULL, 0, ...)` is the "does it fit?" probe.
        return Ok(1);
    }
    if (text.len() as u32 + 1) > size {
        return Ok(0);
    }
    hle.write_cstr(buffer, &text)?;
    Ok(1)
}

fn string_get_length(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(string_text(hle, hle.arg(0))?.chars().count() as u32)
}

fn string_get_char_at(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    let index = hle.arg(1) as usize;
    Ok(text.chars().nth(index).map(|c| c as u32).unwrap_or(0))
}

fn string_get_characters(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    let range_start = hle.arg(1) as usize;
    let range_len = hle.arg(2) as usize;
    let buffer = hle.arg(3);
    let units: Vec<u16> = text
        .encode_utf16()
        .skip(range_start)
        .take(range_len)
        .collect();
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    hle.write_bytes(buffer, &bytes)?;
    Ok(0)
}

fn string_get_characters_ptr(hle: &mut Hle<'_>) -> Result<u32> {
    // We cannot promise a stable UCS-2 buffer, and Foundation is allowed to get
    // NULL back.
    let _ = hle.arg(0);
    Ok(0)
}

fn string_get_bytes(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    let start = hle.arg(1) as usize;
    let len = hle.arg(2) as usize;
    let encoding = hle.arg(3);
    let buffer = hle.arg(4);
    let used = if hle.arg(6) != 0 { hle.mem.read_u32(hle.arg(6))? } else { 0 };
    let bytes: Vec<u8> = if encoding == 0x0800_0100 {
        // kCFStringEncodingUTF8
        text.chars().skip(start).take(len).collect::<String>().into_bytes()
    } else {
        text.chars().skip(start).take(len).map(|c| c as u8).collect()
    };
    if buffer != 0 && used > 0 {
        let n = used.min(bytes.len() as u32) as usize;
        hle.write_bytes(buffer, &bytes[..n])?;
    }
    if hle.arg(5) != 0 {
        hle.mem.write_u32(hle.arg(5), bytes.len() as u32)?;
    }
    Ok(1)
}

fn string_get_int_value(hle: &mut Hle<'_>) -> Result<u32> {
    let text = string_text(hle, hle.arg(0))?;
    let value: i32 = text.trim().parse().unwrap_or(0);
    Ok(value as u32)
}

fn string_compare(hle: &mut Hle<'_>) -> Result<u32> {
    let a = string_text(hle, hle.arg(0))?;
    let b = string_text(hle, hle.arg(1))?;
    Ok(match a.cmp(&b) {
        std::cmp::Ordering::Less => (-1i32) as u32,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

fn string_delete(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.arg(0);
    let start = hle.arg(1) as usize;
    let len = hle.arg(2) as usize;
    let text = string_text(hle, handle)?;
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    for (index, c) in chars.iter().enumerate() {
        if index < start || index >= start + len {
            result.push(*c);
        }
    }
    update_cf_string(hle, handle, &result)
}

fn string_uppercase(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.arg(0);
    let text = string_text(hle, handle)?.to_uppercase();
    if hle.arg(1) != 0 {
        update_cf_string(hle, handle, &text)?;
        return Ok(0);
    }
    make_cf_string(hle, &text)
}

fn array_get_count(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.arg(0);
    Ok(hle.sys.cf_arrays.get(&handle).map(|a| a.len() as u32).unwrap_or(0))
}

fn array_get_value(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.arg(0);
    let index = hle.arg(1) as usize;
    Ok(hle
        .sys
        .cf_arrays
        .get(&handle)
        .and_then(|a| a.get(index).copied())
        .unwrap_or(0))
}

fn array_create(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = make_cf_object(hle, CfObject::Array)?;
    hle.sys.cf_arrays.insert(handle, Vec::new());
    Ok(handle)
}

fn array_append(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.arg(0);
    let value = hle.arg(1);
    hle.sys.cf_arrays.entry(handle).or_default().push(value);
    Ok(0)
}

fn bundle_main(hle: &mut Hle<'_>) -> Result<u32> {
    make_cf_object(hle, CfObject::Bundle)
}

fn bundle_copy_url(hle: &mut Hle<'_>) -> Result<u32> {
    let path = hle.sys.bundle_path.clone();
    make_cf_object(hle, CfObject::Path(path))
}

/// `CFURLRef CFBundleCopyResourceURL(CFBundleRef, CFStringRef name, CFStringRef ext, CFStringRef dir)`
///
/// The engine uses this to open its data files; the emulator resolves them
/// against the asset directory the guest was started with.
fn bundle_copy_resource_url(hle: &mut Hle<'_>) -> Result<u32> {
    let name = if hle.arg(1) != 0 { string_text(hle, hle.arg(1))? } else { String::new() };
    let extension = if hle.arg(2) != 0 { string_text(hle, hle.arg(2))? } else { String::new() };
    let directory = if hle.arg(3) != 0 { string_text(hle, hle.arg(3))? } else { String::new() };
    let mut file = name;
    if !extension.is_empty() {
        file.push('.');
        file.push_str(&extension);
    }
    let mut path = hle.sys.bundle_path.clone();
    if !directory.is_empty() {
        path.push('/');
        path.push_str(&directory);
    }
    path.push('/');
    path.push_str(&file);
    hle.note(format!("CFBundleCopyResourceURL -> {path}"));
    make_cf_object(hle, CfObject::Path(path))
}

fn bundle_localizations(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = make_cf_object(hle, CfObject::Array)?;
    let english = make_cf_string(hle, "en")?;
    hle.sys.cf_arrays.insert(handle, vec![english]);
    Ok(handle)
}

fn url_append_component(hle: &mut Hle<'_>) -> Result<u32> {
    let base = path_of(hle, hle.arg(0))?;
    let component = string_text(hle, hle.arg(1))?;
    make_cf_object(hle, CfObject::Path(format!("{base}/{component}")))
}

fn url_delete_last_component(hle: &mut Hle<'_>) -> Result<u32> {
    let base = path_of(hle, hle.arg(0))?;
    let parent = base.rsplit_once('/').map(|(head, _)| head.to_string()).unwrap_or(base);
    make_cf_object(hle, CfObject::Path(parent))
}

fn url_get_fs_representation(hle: &mut Hle<'_>) -> Result<u32> {
    let path = path_of(hle, hle.arg(0))?;
    let buffer = hle.arg(1);
    let size = hle.arg(2);
    if path.len() as u32 + 1 > size {
        return Ok(0);
    }
    hle.write_cstr(buffer, &path)?;
    Ok(1)
}

fn path_of(hle: &Hle<'_>, handle: u32) -> Result<String> {
    match hle.sys.cf_objects.get(&handle) {
        Some(CfObject::Path(path)) => Ok(path.clone()),
        Some(CfObject::Bundle) => Ok(hle.sys.bundle_path.clone()),
        _ => Ok(hle.cstr(handle).unwrap_or_default()),
    }
}

fn locale_copy_current(hle: &mut Hle<'_>) -> Result<u32> {
    make_cf_object(hle, CfObject::Locale)
}

fn locale_preferred_languages(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = make_cf_object(hle, CfObject::Array)?;
    let english = make_cf_string(hle, "en")?;
    hle.sys.cf_arrays.insert(handle, vec![english]);
    Ok(handle)
}

fn locale_get_value(hle: &mut Hle<'_>) -> Result<u32> {
    let _ = hle.arg(0);
    make_cf_string(hle, "en_US")
}

fn charset_predefined(hle: &mut Hle<'_>) -> Result<u32> {
    let _ = hle.arg(0);
    make_cf_object(hle, CfObject::CharacterSet)
}

fn charset_is_member(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn cf_release(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn cf_retain(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

fn cf_show(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.arg(0);
    let text = hle
        .sys
        .cf_strings
        .get(&handle)
        .cloned()
        .unwrap_or_else(|| format!("<CFObject {handle:#x}>"));
    hle.note(format!("CFShow: {text}"));
    Ok(0)
}

fn cf_get_type_id(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(1)
}

fn cf_equal(hle: &mut Hle<'_>) -> Result<u32> {
    let a = hle.arg(0);
    let b = hle.arg(1);
    if a == b {
        return Ok(1);
    }
    let (x, y) = (hle.sys.cf_strings.get(&a).cloned(), hle.sys.cf_strings.get(&b).cloned());
    Ok(matches!((x, y), (Some(a), Some(b)) if a == b) as u32)
}
