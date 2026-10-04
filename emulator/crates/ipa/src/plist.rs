//! A reader for the two property-list dialects an `Info.plist` can be written
//! in: the XML form Xcode emits and the compact binary form (`bplist00`) that
//! `plutil -convert binary1` and some re-packaging tools produce.
//!
//! The importer only needs a handful of keys out of it (`CFBundleExecutable`,
//! `CFBundleIdentifier`, the version strings, `MinimumOSVersion`), but parsing
//! the whole document is barely more work than scraping for those keys and it
//! means a malformed plist is reported as malformed instead of silently
//! producing a bundle with no executable.

use crate::error::{IpaError, Result};

/// A property-list value.
#[derive(Debug, Clone, PartialEq)]
pub enum Plist {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Data(Vec<u8>),
    Str(String),
    Array(Vec<Plist>),
    /// Ordered rather than hashed: an `Info.plist` has a few dozen keys and the
    /// order is the one the user sees in `plutil -p`.
    Dict(Vec<(String, Plist)>),
}

/// Binary plists nest arbitrarily in principle; nothing real does, and a cycle
/// in a hostile file must not blow the stack.
const MAX_DEPTH: usize = 64;

impl Plist {
    /// Parse either dialect, chosen by the file's first bytes.
    pub fn parse(bytes: &[u8]) -> Result<Plist> {
        if bytes.starts_with(b"bplist00") {
            return Binary::new(bytes)?.top();
        }
        let text = String::from_utf8_lossy(bytes);
        if !text.contains("<plist") {
            return Err(IpaError::Corrupt {
                what: "not a property list (no <plist> element and no bplist00 header)".to_string(),
            });
        }
        Xml::new(&text).document()
    }

    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(entries) => entries.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Plist::Int(value) => Some(*value),
            Plist::Real(value) => Some(*value as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Plist::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Plist]> {
        match self {
            Plist::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The value of `key` as a string, for the diagnostic output.
    pub fn string_at(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            Plist::Str(text) => Some(text.clone()),
            Plist::Int(value) => Some(value.to_string()),
            Plist::Real(value) => Some(value.to_string()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// XML dialect
// ---------------------------------------------------------------------------

/// The document is walked as `Vec<char>` so that multi-byte UTF-8 in
/// `CFBundleDisplayName` (these games are localised) can never land a slice on a
/// character boundary.
struct Xml {
    chars: Vec<char>,
    pos: usize,
}

impl Xml {
    fn new(text: &str) -> Self {
        Xml { chars: text.chars().collect(), pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn starts_with(&self, needle: &str) -> bool {
        let needle: Vec<char> = needle.chars().collect();
        if self.chars.len() - self.pos < needle.len() {
            return false;
        }
        self.chars[self.pos..self.pos + needle.len()] == needle[..]
    }

    fn skip_misc(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.pos += 1,
                Some('<') if self.starts_with("<!--") => {
                    self.pos += 4;
                    while self.pos < self.chars.len() && !self.starts_with("-->") {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 3).min(self.chars.len());
                }
                Some('<') if self.starts_with("<?") => {
                    while self.pos < self.chars.len() && !self.starts_with("?>") {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.chars.len());
                }
                Some('<') if self.starts_with("<!") => {
                    while self.pos < self.chars.len() && self.peek() != Some('>') {
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
                _ => return,
            }
        }
    }

    fn document(&mut self) -> Result<Plist> {
        // Everything before `<plist` is the XML declaration, an optional DOCTYPE
        // and comments; the root element is the first `<plist>`.
        while self.pos < self.chars.len() && !self.starts_with("<plist") {
            self.pos += 1;
        }
        if self.pos >= self.chars.len() {
            return Err(IpaError::Corrupt { what: "the plist has no <plist> root element".to_string() });
        }
        self.pos += "<plist".chars().count();
        self.skip_attributes()?;
        self.skip_misc();
        self.value()
    }

    /// Consume to the end of the current tag, honouring quoted attribute values.
    /// Returns true when the tag was self-closing.
    fn skip_attributes(&mut self) -> Result<bool> {
        loop {
            match self.peek() {
                None => {
                    return Err(IpaError::Corrupt {
                        what: "the plist ends inside an XML tag".to_string(),
                    })
                }
                Some('>') => {
                    self.pos += 1;
                    return Ok(false);
                }
                Some('/') => {
                    self.pos += 1;
                    if self.peek() == Some('>') {
                        self.pos += 1;
                        return Ok(true);
                    }
                }
                Some('"') | Some('\'') => {
                    let quote = self.chars[self.pos];
                    self.pos += 1;
                    while self.peek().is_some_and(|c| c != quote) {
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Collect character data up to the next `<`, decoding entities.
    fn text(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '<' {
                break;
            }
            if c == '&' {
                let start = self.pos;
                self.pos += 1;
                let mut name = String::new();
                while let Some(ch) = self.peek() {
                    if ch == ';' {
                        self.pos += 1;
                        break;
                    }
                    if ch == '<' || name.len() > 10 {
                        break;
                    }
                    name.push(ch);
                    self.pos += 1;
                }
                match decode_entity(&name) {
                    Some(decoded) => out.push_str(&decoded),
                    None => {
                        // Not an entity we recognise: keep it verbatim.
                        out.extend(self.chars[start..self.pos].iter());
                    }
                }
                continue;
            }
            out.push(c);
            self.pos += 1;
        }
        out
    }

    /// Consume `</name>`.
    fn expect_close(&mut self, name: &str) -> Result<()> {
        self.skip_misc();
        let expected = format!("</{name}");
        if !self.starts_with(&expected) {
            return Err(IpaError::Corrupt {
                what: format!("expected the closing tag `</{name}>` in the plist"),
            });
        }
        self.pos += expected.chars().count();
        while self.peek().is_some_and(|c| c != '>') {
            self.pos += 1;
        }
        self.pos += 1;
        Ok(())
    }

    fn value(&mut self) -> Result<Plist> {
        self.skip_misc();
        if self.peek() != Some('<') || self.chars.get(self.pos + 1) == Some(&'/') {
            return Err(IpaError::Corrupt {
                what: "the plist has a value where an element was expected".to_string(),
            });
        }
        self.pos += 1;
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '>' || c == '/' {
                break;
            }
            name.push(c);
            self.pos += 1;
        }
        let empty = self.skip_attributes()?;
        let mut children: Vec<(String, Plist)> = Vec::new();
        let mut text = String::new();
        if empty {
            return Ok(Self::leaf(&name, &text, &children));
        }
        match name.as_str() {
            "string" | "key" | "integer" | "real" | "date" | "data" => text = self.text(),
            "true" | "false" => text.clear(),
            "dict" | "array" => loop {
                self.skip_misc();
                if self.peek().is_none() {
                    return Err(IpaError::Corrupt {
                        what: format!("the plist ends inside a <{name}> element"),
                    });
                }
                if self.starts_with("</") {
                    break;
                }
                if name == "dict" {
                    // `<key>` then a value element, repeatedly.
                    let key = self.value()?;
                    let key = match key {
                        Plist::Str(key) => key,
                        _ => {
                            return Err(IpaError::Corrupt {
                                what: "a <dict> entry in the plist is not keyed by a <key> element"
                                    .to_string(),
                            })
                        }
                    };
                    let value = self.value()?;
                    children.push((key, value));
                } else {
                    children.push((String::new(), self.value()?));
                }
            },
            other => {
                return Err(IpaError::Corrupt {
                    what: format!("unknown plist element <{other}>"),
                })
            }
        }
        self.expect_close(&name)?;
        Ok(Self::leaf(&name, &text, &children))
    }

    fn leaf(name: &str, text: &str, children: &[(String, Plist)]) -> Plist {
        let trimmed = text.trim();
        match name {
            "dict" => Plist::Dict(children.to_vec()),
            "array" => Plist::Array(children.iter().map(|(_, value)| value.clone()).collect()),
            "string" | "key" | "date" => Plist::Str(text.to_string()),
            "data" => Plist::Data(base64_decode(trimmed)),
            "integer" => Plist::Int(
                trimmed
                    .parse::<i64>()
                    .or_else(|_| i64::from_str_radix(trimmed.trim_start_matches("0x"), 16))
                    .unwrap_or(0),
            ),
            "real" => Plist::Real(trimmed.parse::<f64>().unwrap_or(0.0)),
            "true" => Plist::Bool(true),
            "false" => Plist::Bool(false),
            _ => Plist::Null,
        }
    }
}

fn decode_entity(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        other => {
            let rest = other.strip_prefix('#')?;
            let code = match rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => rest.parse::<u32>().ok()?,
            };
            Some(char::from_u32(code)?.to_string())
        }
    }
}

fn base64_decode(text: &str) -> Vec<u8> {
    fn value(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in text.as_bytes() {
        if byte == b'=' {
            break;
        }
        let Some(six) = value(byte) else { continue };
        buffer = (buffer << 6) | six as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Binary dialect (bplist00)
// ---------------------------------------------------------------------------

struct Binary<'a> {
    data: &'a [u8],
    offsets: Vec<usize>,
    object_size: usize,
    reference_size: usize,
    top: usize,
}

impl<'a> Binary<'a> {
    fn new(data: &'a [u8]) -> Result<Self> {
        if data.len() < 40 {
            return Err(IpaError::Corrupt {
                what: format!("binary plist is only {} bytes", data.len()),
            });
        }
        let trailer = data.len() - 32;
        let object_size = data[trailer + 6] as usize;
        let reference_size = data[trailer + 7] as usize;
        let count = be64(data, trailer + 8)?;
        let top = be64(data, trailer + 16)? as usize;
        let table = be64(data, trailer + 24)? as usize;
        if object_size == 0 || object_size > 8 || reference_size == 0 || reference_size > 8 {
            return Err(IpaError::Corrupt {
                what: format!("binary plist trailer has implausible sizes (offset {object_size}, reference {reference_size})"),
            });
        }
        if count > 1 << 20 || table + count as usize * object_size > data.len() {
            return Err(IpaError::Corrupt {
                what: "binary plist offset table runs past the end of the file".to_string(),
            });
        }
        let mut offsets = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            offsets.push(be_uint(data, table + index * object_size, object_size)? as usize);
        }
        if top >= offsets.len() {
            return Err(IpaError::Corrupt {
                what: format!("binary plist top object {top} is out of range ({} objects)", offsets.len()),
            });
        }
        Ok(Binary { data, offsets, object_size, reference_size, top })
    }

    fn top(&self) -> Result<Plist> {
        self.object(self.top, 0)
    }

    fn object(&self, index: usize, depth: usize) -> Result<Plist> {
        if depth > MAX_DEPTH {
            return Err(IpaError::Corrupt {
                what: "binary plist nests more than 64 levels deep".to_string(),
            });
        }
        let offset = *self.offsets.get(index).ok_or_else(|| IpaError::Corrupt {
            what: format!("binary plist references object {index}, which does not exist"),
        })?;
        let marker = *self.data.get(offset).ok_or_else(|| IpaError::Corrupt {
            what: format!("binary plist object {index} is past the end of the file"),
        })?;
        let kind = marker & 0xf0;
        let info = (marker & 0x0f) as usize;
        let mut cursor = offset + 1;
        // A size nibble of 0xf means "the real length follows as an int object".
        let length = if info == 0x0f && matches!(kind, 0x40 | 0x50 | 0x60 | 0xa0 | 0xd0) {
            let size_marker = *self.data.get(cursor).ok_or_else(|| IpaError::Corrupt {
                what: "binary plist length prefix is past the end of the file".to_string(),
            })?;
            if size_marker & 0xf0 != 0x10 {
                return Err(IpaError::Corrupt {
                    what: format!("binary plist length prefix {size_marker:#04x} is not an int object"),
                });
            }
            cursor += 1;
            let size = be_uint(self.data, cursor, 1usize << (size_marker & 0x0f))? as usize;
            cursor += 1usize << (size_marker & 0x0f);
            size
        } else {
            info
        };

        match kind {
            0x00 => Ok(match info {
                0x00 => Plist::Null,
                0x08 => Plist::Bool(false),
                0x09 => Plist::Bool(true),
                _ => Plist::Null,
            }),
            0x10 => {
                let size = 1usize << info.min(3);
                let value = be_uint(self.data, cursor, size)?;
                Ok(Plist::Int(value as i64))
            }
            0x20 => {
                let size = 1usize << info.min(3);
                let raw = be_uint(self.data, cursor, size)?;
                let real = match size {
                    4 => f32::from_bits(raw as u32) as f64,
                    _ => f64::from_bits(raw),
                };
                Ok(Plist::Real(real))
            }
            0x30 => {
                // Seconds since 2001-01-01, as a big-endian double.
                let raw = be_uint(self.data, cursor, 8)?;
                Ok(Plist::Real(f64::from_bits(raw)))
            }
            0x40 => {
                let bytes = self.slice(cursor, length, "binary plist data")?;
                Ok(Plist::Data(bytes.to_vec()))
            }
            0x50 => {
                let bytes = self.slice(cursor, length, "binary plist ASCII string")?;
                Ok(Plist::Str(String::from_utf8_lossy(bytes).into_owned()))
            }
            0x60 => {
                let bytes = self.slice(cursor, length * 2, "binary plist UTF-16 string")?;
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect();
                Ok(Plist::Str(String::from_utf16_lossy(&units)))
            }
            0x80 => {
                let value = be_uint(self.data, cursor, length)?;
                Ok(Plist::Int(value as i64))
            }
            0xa0 => {
                let mut items = Vec::with_capacity(length);
                for slot in 0..length {
                    let reference = self.reference(cursor + slot * self.reference_size)?;
                    items.push(self.object(reference, depth + 1)?);
                }
                Ok(Plist::Array(items))
            }
            0xd0 => {
                let mut entries = Vec::with_capacity(length);
                for slot in 0..length {
                    let key = self.reference(cursor + slot * self.reference_size)?;
                    let value = self.reference(cursor + (length + slot) * self.reference_size)?;
                    let name = match self.object(key, depth + 1)? {
                        Plist::Str(name) => name,
                        other => {
                            return Err(IpaError::Corrupt {
                                what: format!("binary plist dictionary key is a {other:?}, not a string"),
                            })
                        }
                    };
                    entries.push((name, self.object(value, depth + 1)?));
                }
                Ok(Plist::Dict(entries))
            }
            other => Err(IpaError::Corrupt {
                what: format!("unknown binary plist object type {other:#04x}"),
            }),
        }
    }

    fn reference(&self, at: usize) -> Result<usize> {
        Ok(be_uint(self.data, at, self.reference_size)? as usize)
    }

    fn slice(&self, at: usize, length: usize, what: &'static str) -> Result<&'a [u8]> {
        let end = at.checked_add(length).ok_or_else(|| IpaError::Corrupt {
            what: format!("{what} has an absurd length"),
        })?;
        if end > self.data.len() {
            return Err(IpaError::Corrupt {
                what: format!("{what} at {at:#x} runs past the end of the file"),
            });
        }
        Ok(&self.data[at..end])
    }
}

fn be_uint(data: &[u8], at: usize, size: usize) -> Result<u64> {
    if size == 0 || at + size > data.len() {
        return Err(IpaError::Corrupt {
            what: format!("binary plist integer at {at:#x} is past the end of the file"),
        });
    }
    let mut value = 0u64;
    for byte in &data[at..at + size] {
        value = (value << 8) | *byte as u64;
    }
    Ok(value)
}

fn be64(data: &[u8], at: usize) -> Result<u64> {
    be_uint(data, at, 8)
}
