//! Test fixtures for the importer: a ZIP *writer*, a DEFLATE *compressor* and a
//! synthetic-but-real `.ipa`.
//!
//! The point of putting these here (rather than checking binary fixtures in) is
//! that the tests exercise the importer against archives built the same way the
//! real thing is built: deflate-compressed entries with CRCs, a `Payload/`
//! directory, an `Info.plist` written by Apple's own `plistlib`, and an ARMv7
//! Mach-O produced by `macho::test_support`.  The compressor is deliberately
//! simple — canonical, complete Huffman codes rather than optimal ones — because
//! what the tests need is coverage of every path through [`crate::inflate`], not
//! a small file.

use macho::test_support::{build, Program, Stub};

use crate::zip::crc32;

/// `Info.plist` as Apple's `plistlib` writes it (generated from the metadata of
/// the release this emulator targets, `com.ea.simpsonsarcade.bv` 1.1.43).  It
/// keeps the `&amp;` entity and the non-ASCII copyright so the XML reader is
/// tested against a document a real tool produced.
pub const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDisplayName</key>
	<string>The Simpsons Arcade</string>
	<key>CFBundleExecutable</key>
	<string>TheSimpsons</string>
	<key>CFBundleIdentifier</key>
	<string>com.ea.simpsonsarcade.bv</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>TheSimpsons</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.1.43</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>iPhoneOS</string>
	</array>
	<key>CFBundleVersion</key>
	<string>1.1.43</string>
	<key>DTPlatformName</key>
	<string>iphoneos</string>
	<key>MinimumOSVersion</key>
	<string>2.2.1</string>
	<key>NSHumanReadableCopyright</key>
	<string>Tom &amp; Jerry © EA</string>
	<key>UIDeviceFamily</key>
	<array>
		<integer>1</integer>
		<integer>2</integer>
	</array>
</dict>
</plist>
"#;

/// The same document in the binary (`bplist00`) dialect, as `plistlib` writes
/// it.  Stored verbatim so the binary reader is checked against Apple's layout
/// rather than against a writer in this crate that could share its bugs.
#[rustfmt::skip]
pub const BINARY_INFO_PLIST: &[u8] = &[
    0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xdd, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x0f, 0x12, 0x13, 0x14, 0x13, 0x16,
    0x17, 0x18, 0x19, 0x5f, 0x10, 0x13, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x44, 0x69,
    0x73, 0x70, 0x6c, 0x61, 0x79, 0x4e, 0x61, 0x6d, 0x65, 0x5f, 0x10, 0x12, 0x43, 0x46, 0x42, 0x75,
    0x6e, 0x64, 0x6c, 0x65, 0x45, 0x78, 0x65, 0x63, 0x75, 0x74, 0x61, 0x62, 0x6c, 0x65, 0x5f, 0x10,
    0x12, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x49, 0x64, 0x65, 0x6e, 0x74, 0x69, 0x66,
    0x69, 0x65, 0x72, 0x5f, 0x10, 0x1d, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x49, 0x6e,
    0x66, 0x6f, 0x44, 0x69, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x61, 0x72, 0x79, 0x56, 0x65, 0x72, 0x73,
    0x69, 0x6f, 0x6e, 0x5c, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x4e, 0x61, 0x6d, 0x65,
    0x5f, 0x10, 0x13, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x50, 0x61, 0x63, 0x6b, 0x61,
    0x67, 0x65, 0x54, 0x79, 0x70, 0x65, 0x5f, 0x10, 0x1a, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c,
    0x65, 0x53, 0x68, 0x6f, 0x72, 0x74, 0x56, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x53, 0x74, 0x72,
    0x69, 0x6e, 0x67, 0x5f, 0x10, 0x1a, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x53, 0x75,
    0x70, 0x70, 0x6f, 0x72, 0x74, 0x65, 0x64, 0x50, 0x6c, 0x61, 0x74, 0x66, 0x6f, 0x72, 0x6d, 0x73,
    0x5f, 0x10, 0x0f, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x56, 0x65, 0x72, 0x73, 0x69,
    0x6f, 0x6e, 0x5e, 0x44, 0x54, 0x50, 0x6c, 0x61, 0x74, 0x66, 0x6f, 0x72, 0x6d, 0x4e, 0x61, 0x6d,
    0x65, 0x5f, 0x10, 0x10, 0x4d, 0x69, 0x6e, 0x69, 0x6d, 0x75, 0x6d, 0x4f, 0x53, 0x56, 0x65, 0x72,
    0x73, 0x69, 0x6f, 0x6e, 0x5f, 0x10, 0x18, 0x4e, 0x53, 0x48, 0x75, 0x6d, 0x61, 0x6e, 0x52, 0x65,
    0x61, 0x64, 0x61, 0x62, 0x6c, 0x65, 0x43, 0x6f, 0x70, 0x79, 0x72, 0x69, 0x67, 0x68, 0x74, 0x5e,
    0x55, 0x49, 0x44, 0x65, 0x76, 0x69, 0x63, 0x65, 0x46, 0x61, 0x6d, 0x69, 0x6c, 0x79, 0x5f, 0x10,
    0x13, 0x54, 0x68, 0x65, 0x20, 0x53, 0x69, 0x6d, 0x70, 0x73, 0x6f, 0x6e, 0x73, 0x20, 0x41, 0x72,
    0x63, 0x61, 0x64, 0x65, 0x5b, 0x54, 0x68, 0x65, 0x53, 0x69, 0x6d, 0x70, 0x73, 0x6f, 0x6e, 0x73,
    0x5f, 0x10, 0x18, 0x63, 0x6f, 0x6d, 0x2e, 0x65, 0x61, 0x2e, 0x73, 0x69, 0x6d, 0x70, 0x73, 0x6f,
    0x6e, 0x73, 0x61, 0x72, 0x63, 0x61, 0x64, 0x65, 0x2e, 0x62, 0x76, 0x53, 0x36, 0x2e, 0x30, 0x54,
    0x41, 0x50, 0x50, 0x4c, 0x56, 0x31, 0x2e, 0x31, 0x2e, 0x34, 0x33, 0xa1, 0x15, 0x58, 0x69, 0x50,
    0x68, 0x6f, 0x6e, 0x65, 0x4f, 0x53, 0x58, 0x69, 0x50, 0x68, 0x6f, 0x6e, 0x65, 0x6f, 0x73, 0x55,
    0x32, 0x2e, 0x35, 0x2e, 0x31, 0x6f, 0x10, 0x10, 0x00, 0x54, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x20,
    0x00, 0x26, 0x00, 0x20, 0x00, 0x4a, 0x00, 0x65, 0x00, 0x72, 0x00, 0x72, 0x00, 0x79, 0x00, 0x20,
    0x00, 0xa9, 0x00, 0x20, 0x00, 0x45, 0x00, 0x41, 0xa2, 0x1a, 0x1b, 0x10, 0x01, 0x10, 0x02, 0x00,
    0x08, 0x00, 0x23, 0x00, 0x39, 0x00, 0x4e, 0x00, 0x63, 0x00, 0x83, 0x00, 0x90, 0x00, 0xa6, 0x00,
    0xc3, 0x00, 0xe0, 0x00, 0xf2, 0x01, 0x01, 0x01, 0x14, 0x01, 0x2f, 0x01, 0x3e, 0x01, 0x54, 0x01,
    0x60, 0x01, 0x7b, 0x01, 0x7f, 0x01, 0x84, 0x01, 0x8b, 0x01, 0x8d, 0x01, 0x96, 0x01, 0x9f, 0x01,
    0xa5, 0x01, 0xc8, 0x01, 0xcb, 0x01, 0xcd, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xcf,
];

// ---------------------------------------------------------------------------
// Mach-O fixtures
// ---------------------------------------------------------------------------

/// A small but complete ARMv7 iOS executable, the shape of the game's binary.
pub fn armv7_executable() -> Vec<u8> {
    let program = Program {
        code: vec![0x00, 0x00, 0xa0, 0xe1, 0x1e, 0xff, 0x2f, 0xe1], // mov r0,r0; bx lr
        cstrings: b"TheSimpsons\0strings_generic.bin\0".to_vec(),
        stubs: vec![Stub {
            symbol: "_printf".into(),
            bytes: vec![0x04, 0xc0, 0x9f, 0xe5, 0x1c, 0xff, 0x2f, 0xe1, 0, 0, 0, 0],
        }],
        defines: vec![("_main".into(), 0x1000)],
        dylibs: vec![
            "/usr/lib/libSystem.B.dylib".into(),
            "/usr/lib/libz.1.dylib".into(),
            "/System/Library/Frameworks/UIKit.framework/UIKit".into(),
        ],
        uuid: Some([0x54, 0x48, 0x45, 0x53, 0x49, 0x4d, 0x50, 0x53, 9, 9, 9, 9, 9, 9, 9, 9]),
        ..Default::default()
    };
    build(&program).0
}

/// Rewrite the `LC_UUID` load command as an `LC_ENCRYPTION_INFO` with
/// `cryptid = 1`, which is what an undecrypted App Store binary looks like.
/// Both commands are 24 bytes, so every other offset in the image stays valid.
pub fn mark_encrypted(data: &mut [u8]) -> bool {
    if data.len() < 28 {
        return false;
    }
    let read = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    let ncmds = read(16);
    let sizeofcmds = read(20) as usize;
    let mut at = 28usize;
    let mut target = None;
    for _ in 0..ncmds {
        if at + 8 > 28 + sizeofcmds || at + 8 > data.len() {
            return false;
        }
        let (cmd, size) = (read(at), read(at + 4) as usize);
        if size < 8 {
            return false;
        }
        if cmd == macho::LC_UUID && size == 24 && at + 24 <= data.len() {
            target = Some(at);
            break;
        }
        at += size;
    }
    let Some(at) = target else { return false };
    for (offset, value) in [
        (0usize, macho::LC_ENCRYPTION_INFO),
        (4, 24),
        (8, 0x1000),  // cryptoff
        (12, 0x2000), // cryptsize
        (16, 1),      // cryptid
        (20, 0),      // pad
    ] {
        data[at + offset..at + offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    true
}

/// A thin arm64 Mach-O header: enough for the architecture scan to see that the
/// image is *not* ARMv7.
pub fn arm64_header() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&macho::MH_MAGIC_64.to_le_bytes());
    out.extend_from_slice(&macho::CPU_TYPE_ARM64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&macho::MH_EXECUTE.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// Wrap slices in a universal ("fat") container, the way `lipo` does.
pub fn fat_container(slices: &[(u32, u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&macho::FAT_MAGIC.to_be_bytes());
    out.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    let mut offset = 8 + 20 * slices.len();
    for (cputype, cpusubtype, bytes) in slices {
        offset = (offset + 3) & !3;
        out.extend_from_slice(&cputype.to_be_bytes());
        out.extend_from_slice(&cpusubtype.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes()); // align = 1 << 2
    }
    for (_, _, bytes) in slices {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(bytes);
    }
    out
}

// ---------------------------------------------------------------------------
// DEFLATE compressor
// ---------------------------------------------------------------------------

/// Compression method numbers, as ZIP stores them.
pub const METHOD_STORED: u16 = 0;
pub const METHOD_DEFLATE: u16 = 8;

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter { out: Vec::new(), acc: 0, bits: 0 }
    }

    /// Append `count` bits of `value`, least significant bit first.
    fn field(&mut self, value: u32, count: u32) {
        self.acc |= (value as u64 & ((1u64 << count) - 1)) << self.bits;
        self.bits += count;
        while self.bits >= 8 {
            self.out.push((self.acc & 0xff) as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    /// Append a Huffman code, most significant bit first (DEFLATE's convention).
    fn code(&mut self, code: u32, count: u32) {
        for bit in (0..count).rev() {
            self.field((code >> bit) & 1, 1);
        }
    }

    fn align(&mut self) {
        let remainder = self.bits % 8;
        if remainder != 0 {
            self.field(0, 8 - remainder);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Literal(u8),
    Match { length: u16, distance: u16 },
}

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const WINDOW: usize = 32768;

fn hash3(data: &[u8], at: usize) -> usize {
    ((data[at] as usize) << 10 ^ (data[at + 1] as usize) << 5 ^ data[at + 2] as usize) & 0x7fff
}

/// Greedy LZ77 with a single-slot hash table: finds the last occurrence of each
/// 3-byte prefix, which is plenty for test data and keeps the fixture builder
/// linear.
fn tokenize(data: &[u8]) -> Vec<Token> {
    let mut table = vec![usize::MAX; 0x8000];
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let mut found: Option<(usize, usize)> = None;
        if at + MIN_MATCH <= data.len() {
            let hash = hash3(data, at);
            let candidate = table[hash];
            if candidate != usize::MAX && candidate < at && at - candidate <= WINDOW {
                let mut length = 0;
                while length < MAX_MATCH && at + length < data.len() && data[candidate + length] == data[at + length]
                {
                    length += 1;
                }
                if length >= MIN_MATCH {
                    found = Some((length, at - candidate));
                }
            }
            table[hash] = at;
        }
        match found {
            Some((length, distance)) => {
                tokens.push(Token::Match { length: length as u16, distance: distance as u16 });
                for skipped in 1..length {
                    if at + skipped + MIN_MATCH <= data.len() {
                        table[hash3(data, at + skipped)] = at + skipped;
                    }
                }
                at += length;
            }
            None => {
                tokens.push(Token::Literal(data[at]));
                at += 1;
            }
        }
    }
    tokens
}

/// `(symbol, extra value, extra bits)` for a match length.
fn length_code(length: u16) -> (u16, u16, u8) {
    for (index, base) in crate::inflate::LENGTH_BASE.iter().enumerate().rev() {
        if length >= *base {
            return (257 + index as u16, length - base, crate::inflate::LENGTH_EXTRA[index]);
        }
    }
    unreachable!("lengths of at least {MIN_MATCH} always have a code")
}

/// `(code, extra value, extra bits)` for a match distance.
fn distance_code(distance: u16) -> (u16, u16, u8) {
    for (index, base) in crate::inflate::DIST_BASE.iter().enumerate().rev() {
        if distance >= *base {
            return (index as u16, distance - base, crate::inflate::DIST_EXTRA[index]);
        }
    }
    unreachable!("distances of at least 1 always have a code")
}

/// The fixed literal/length code of RFC 1951 §3.2.6.
fn fixed_code(symbol: u16) -> (u32, u32) {
    match symbol {
        0..=143 => (0x30 + symbol as u32, 8),
        144..=255 => (0x190 + symbol as u32 - 144, 9),
        256..=279 => (symbol as u32 - 256, 7),
        _ => (0xc0 + symbol as u32 - 280, 8),
    }
}

/// A complete canonical code: every symbol gets `floor(log2 n)` or
/// `floor(log2 n) + 1` bits, which always satisfies the Kraft equality.  The
/// tests do not need optimal codes, only valid ones that exercise the decoder.
fn complete_lengths(counts: &[u32]) -> Vec<u8> {
    let mut lengths = vec![0u8; counts.len()];
    let mut used: Vec<u16> =
        counts.iter().enumerate().filter(|(_, count)| **count > 0).map(|(symbol, _)| symbol as u16).collect();
    // Frequent symbols first so the shorter codes go where they are used most.
    used.sort_by(|&a, &b| counts[b as usize].cmp(&counts[a as usize]).then(a.cmp(&b)));
    let count = used.len();
    match count {
        0 => lengths,
        1 => {
            lengths[used[0] as usize] = 1;
            lengths
        }
        _ => {
            let mut bits = 0usize;
            while (1usize << (bits + 1)) <= count {
                bits += 1;
            }
            let short = (1usize << (bits + 1)) - count;
            for (index, symbol) in used.iter().enumerate() {
                lengths[*symbol as usize] = if index < short { bits as u8 } else { bits as u8 + 1 };
            }
            lengths
        }
    }
}

/// Canonical code values for a set of code lengths.
fn canonical_codes(lengths: &[u8]) -> Vec<u32> {
    let longest = lengths.iter().max().copied().unwrap_or(0) as usize;
    let mut counts = vec![0u32; longest + 2];
    for length in lengths {
        counts[*length as usize] += 1;
    }
    let mut next = vec![0u32; longest + 2];
    let mut code = 0u32;
    for bits in 1..=longest {
        code = (code + counts[bits - 1]) << 1;
        next[bits] = code;
    }
    let mut codes = vec![0u32; lengths.len()];
    for (symbol, length) in lengths.iter().enumerate() {
        if *length != 0 {
            codes[symbol] = next[*length as usize];
            next[*length as usize] += 1;
        }
    }
    codes
}

/// Deflate using only stored blocks (BTYPE 00) — exercises the inflater's
/// byte-alignment and length/complement path.
pub fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::new();
    let mut at = 0usize;
    loop {
        let chunk = (data.len() - at).min(0xffff);
        let last = at + chunk >= data.len();
        writer.field(u32::from(last), 1);
        writer.field(0, 2);
        writer.align();
        writer.field(chunk as u32, 16);
        writer.field((chunk as u32) ^ 0xffff, 16);
        for byte in &data[at..at + chunk] {
            writer.field(*byte as u32, 8);
        }
        at += chunk;
        if last {
            break;
        }
    }
    writer.finish()
}

/// Deflate with the fixed Huffman trees (BTYPE 01), including back-references.
pub fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.field(1, 1);
    writer.field(1, 2);
    for token in tokenize(data) {
        match token {
            Token::Literal(byte) => {
                let (code, bits) = fixed_code(byte as u16);
                writer.code(code, bits);
            }
            Token::Match { length, distance } => {
                let (symbol, extra, extra_bits) = length_code(length);
                let (code, bits) = fixed_code(symbol);
                writer.code(code, bits);
                writer.field(extra as u32, extra_bits as u32);
                let (distance_symbol, extra, extra_bits) = distance_code(distance);
                writer.code(distance_symbol as u32, 5);
                writer.field(extra as u32, extra_bits as u32);
            }
        }
    }
    let (code, bits) = fixed_code(256);
    writer.code(code, bits);
    writer.finish()
}

struct LengthSymbol {
    symbol: u16,
    extra: u32,
    bits: u32,
}

/// Write the code-length alphabet the way a real encoder does, including the
/// run-length codes 16/17/18.
fn length_sequence(lengths: &[u8]) -> Vec<LengthSymbol> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < lengths.len() {
        let value = lengths[at];
        let mut run = 1;
        while at + run < lengths.len() && lengths[at + run] == value {
            run += 1;
        }
        let mut remaining = run;
        if value == 0 {
            while remaining > 0 {
                if remaining >= 11 {
                    let count = remaining.min(138);
                    out.push(LengthSymbol { symbol: 18, extra: count as u32 - 11, bits: 7 });
                    remaining -= count;
                } else if remaining >= 3 {
                    let count = remaining.min(10);
                    out.push(LengthSymbol { symbol: 17, extra: count as u32 - 3, bits: 3 });
                    remaining -= count;
                } else {
                    out.push(LengthSymbol { symbol: 0, extra: 0, bits: 0 });
                    remaining -= 1;
                }
            }
        } else {
            out.push(LengthSymbol { symbol: value as u16, extra: 0, bits: 0 });
            remaining -= 1;
            while remaining > 0 {
                if remaining >= 3 {
                    let count = remaining.min(6);
                    out.push(LengthSymbol { symbol: 16, extra: count as u32 - 3, bits: 2 });
                    remaining -= count;
                } else {
                    out.push(LengthSymbol { symbol: value as u16, extra: 0, bits: 0 });
                    remaining -= 1;
                }
            }
        }
        at += run;
    }
    out
}

/// Deflate with dynamic Huffman trees (BTYPE 10) — the header, the code-length
/// alphabet and the canonical decoder all get exercised here.
pub fn deflate_dynamic(data: &[u8]) -> Vec<u8> {
    let tokens = tokenize(data);
    let mut literal_counts = vec![0u32; 288];
    let mut distance_counts = vec![0u32; 30];
    for token in &tokens {
        match *token {
            Token::Literal(byte) => literal_counts[byte as usize] += 1,
            Token::Match { length, distance } => {
                let (symbol, _, _) = length_code(length);
                literal_counts[symbol as usize] += 1;
                let (distance_symbol, _, _) = distance_code(distance);
                distance_counts[distance_symbol as usize] += 1;
            }
        }
    }
    literal_counts[256] += 1; // end of block

    let mut literal_lengths = complete_lengths(&literal_counts);
    literal_lengths.resize(literal_lengths.len().max(257), 0);
    let mut distance_lengths = complete_lengths(&distance_counts);
    distance_lengths.resize(distance_lengths.len().max(1), 0);

    let sequence = length_sequence(
        &literal_lengths
            .iter()
            .chain(distance_lengths.iter())
            .copied()
            .collect::<Vec<u8>>(),
    );

    // Code-length alphabet over the symbols the sequence actually uses.
    let mut code_length_counts = vec![0u32; 19];
    for item in &sequence {
        code_length_counts[item.symbol as usize] += 1;
    }
    let code_length_lengths = complete_lengths(&code_length_counts);
    let code_length_codes = canonical_codes(&code_length_lengths);

    let mut writer = BitWriter::new();
    writer.field(1, 1); // BFINAL
    writer.field(2, 2); // BTYPE = dynamic
    writer.field(literal_lengths.len() as u32 - 257, 5);
    writer.field(distance_lengths.len() as u32 - 1, 5);

    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut needed = 4usize;
    for index in 0..19 {
        if code_length_lengths[ORDER[index]] != 0 {
            needed = index + 1;
        }
    }
    writer.field(needed as u32 - 4, 4);
    for index in 0..needed {
        writer.field(code_length_lengths[ORDER[index]] as u32, 3);
    }
    for item in &sequence {
        writer.code(code_length_codes[item.symbol as usize], code_length_lengths[item.symbol as usize] as u32);
        writer.field(item.extra, item.bits);
    }

    let literal_codes = canonical_codes(&literal_lengths);
    let distance_codes = canonical_codes(&distance_lengths);
    for token in tokens {
        match token {
            Token::Literal(byte) => {
                writer.code(literal_codes[byte as usize], literal_lengths[byte as usize] as u32);
            }
            Token::Match { length, distance } => {
                let (symbol, extra, extra_bits) = length_code(length);
                writer.code(literal_codes[symbol as usize], literal_lengths[symbol as usize] as u32);
                writer.field(extra as u32, extra_bits as u32);
                let (distance_symbol, extra, extra_bits) = distance_code(distance);
                writer.code(
                    distance_codes[distance_symbol as usize],
                    distance_lengths[distance_symbol as usize] as u32,
                );
                writer.field(extra as u32, extra_bits as u32);
            }
        }
    }
    writer.code(literal_codes[256], literal_lengths[256] as u32);
    writer.finish()
}

// ---------------------------------------------------------------------------
// ZIP / IPA writer
// ---------------------------------------------------------------------------

/// One entry of an archive under construction.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub method: u16,
    /// Unix mode, stored in the external attributes as `zip` does.
    pub mode: u16,
}

/// A ZIP writer: local headers, central directory, end-of-directory record.
#[derive(Debug, Clone, Default)]
pub struct ZipBuilder {
    pub entries: Vec<ZipEntry>,
    pub comment: String,
}

impl ZipBuilder {
    pub fn new() -> Self {
        ZipBuilder::default()
    }

    /// A deflate-compressed file.
    pub fn file(&mut self, name: &str, data: &[u8]) -> &mut Self {
        self.entry(name, data, METHOD_DEFLATE, 0o644)
    }

    /// A stored (uncompressed) file.
    pub fn stored(&mut self, name: &str, data: &[u8]) -> &mut Self {
        self.entry(name, data, METHOD_STORED, 0o644)
    }

    /// A file with an explicit compression method and Unix mode.
    pub fn entry(&mut self, name: &str, data: &[u8], method: u16, mode: u16) -> &mut Self {
        self.entries.push(ZipEntry { name: name.to_string(), data: data.to_vec(), method, mode });
        self
    }

    /// An explicit directory entry (optional in ZIP, and Xcode does write them).
    pub fn directory(&mut self, name: &str) -> &mut Self {
        let name = if name.ends_with('/') { name.to_string() } else { format!("{name}/") };
        self.entries.push(ZipEntry { name, data: Vec::new(), method: METHOD_STORED, mode: 0o40755 });
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for entry in &self.entries {
            let offset = out.len() as u32;
            let name = entry.name.as_bytes();
            let compressed = match entry.method {
                METHOD_STORED => entry.data.clone(),
                METHOD_DEFLATE => deflate_dynamic(&entry.data),
                _ => entry.data.clone(),
            };
            let directory = entry.name.ends_with('/');

            // local file header
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&entry.method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0x2821u16.to_le_bytes()); // mod date (1980-01-01)
            out.extend_from_slice(&crc32(&entry.data).to_le_bytes());
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra length
            out.extend_from_slice(name);
            out.extend_from_slice(&compressed);

            // central directory record
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&0x0314u16.to_le_bytes()); // version made by: Unix, v2.0
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&entry.method.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0x2821u16.to_le_bytes());
            central.extend_from_slice(&crc32(&entry.data).to_le_bytes());
            central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            central.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra length
            central.extend_from_slice(&0u16.to_le_bytes()); // comment length
            central.extend_from_slice(&0u16.to_le_bytes()); // disk number
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            let mode = if directory { entry.mode | 0o04_0000 } else { entry.mode | 0o10_0000 };
            central.extend_from_slice(&((mode as u32) << 16).to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
        }

        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        let cd_size = central.len() as u32;

        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&(self.comment.len() as u16).to_le_bytes());
        out.extend_from_slice(self.comment.as_bytes());
        out
    }
}

/// What [`build_ipa`] should put in the archive.
#[derive(Debug, Clone)]
pub struct FakeIpa {
    pub app_name: String,
    pub executable_name: String,
    pub payload_dir: String,
    pub version: String,
    pub bundle_id: String,
    /// Compression for the bundle's files.
    pub method: u16,
    /// Rewrite the executable so it looks FairPlay encrypted.
    pub encrypted: bool,
    /// Make the executable arm64-only.
    pub arm64_only: bool,
    /// Universal binary holding both slices.
    pub fat: bool,
    /// Extra files inside the bundle, as `(relative path, bytes)`.
    pub resources: Vec<(String, Vec<u8>)>,
    /// Extra top-level entries, e.g. `iTunesMetadata.plist`.
    pub extra: Vec<(String, Vec<u8>)>,
    /// Omit `Info.plist` entirely.
    pub omit_info_plist: bool,
    /// Use the binary plist dialect for `Info.plist`.
    pub binary_plist: bool,
}

impl Default for FakeIpa {
    fn default() -> Self {
        FakeIpa {
            app_name: "TheSimpsons.app".to_string(),
            executable_name: "TheSimpsons".to_string(),
            payload_dir: "Payload".to_string(),
            version: "1.1.43".to_string(),
            bundle_id: "com.ea.simpsonsarcade.bv".to_string(),
            method: METHOD_DEFLATE,
            encrypted: false,
            arm64_only: false,
            fat: false,
            resources: Vec::new(),
            extra: Vec::new(),
            omit_info_plist: false,
            binary_plist: false,
        }
    }
}

/// Build an `.ipa` in memory: `Payload/<App>.app/` with an `Info.plist`, an
/// executable and whatever resources the test asks for.
pub fn build_ipa(config: &FakeIpa) -> Vec<u8> {
    let bundle = format!("{}/{}", config.payload_dir, config.app_name);
    let mut archive = ZipBuilder::new();
    archive.comment = "built by crates/ipa/src/test_support.rs".to_string();
    archive.directory(&config.payload_dir);
    archive.directory(&bundle);

    if !config.omit_info_plist {
        let plist = if config.binary_plist {
            BINARY_INFO_PLIST.to_vec()
        } else {
            INFO_PLIST
                .replace("com.ea.simpsonsarcade.bv", &config.bundle_id)
                .replace("1.1.43", &config.version)
                .replace("TheSimpsons", &config.app_name.trim_end_matches(".app"))
                .into_bytes()
        };
        archive.entry(&format!("{bundle}/Info.plist"), &plist, config.method, 0o644);
    }

    let mut executable = if config.arm64_only {
        arm64_header()
    } else {
        armv7_executable()
    };
    if config.encrypted && !config.arm64_only {
        assert!(mark_encrypted(&mut executable), "the fixture executable has an LC_UUID to rewrite");
    }
    if config.fat && !config.arm64_only {
        let arm64 = arm64_header();
        executable = fat_container(&[
            (macho::CPU_TYPE_ARM, macho::CPU_SUBTYPE_ARM_V7, &executable),
            (macho::CPU_TYPE_ARM64, 0, &arm64),
        ]);
    }
    archive.entry(
        &format!("{bundle}/{}", config.executable_name),
        &executable,
        config.method,
        0o755,
    );

    for (name, bytes) in &config.resources {
        archive.entry(&format!("{bundle}/{name}"), bytes, config.method, 0o644);
    }
    for (name, bytes) in &config.extra {
        archive.entry(name, bytes, config.method, 0o644);
    }
    archive.build()
}

/// An `.ipa` for the game this emulator targets, with a couple of resources.
pub fn simpsons_ipa() -> Vec<u8> {
    build_ipa(&FakeIpa {
        resources: vec![
            ("assets/strings_generic.bin".to_string(), vec![0xab; 4096]),
            ("Icon.png".to_string(), b"\x89PNG\r\n\x1a\n fake".to_vec()),
            ("en.lproj/Localizable.strings".to_string(), b"HOMER = \"Homer\";".to_vec()),
        ],
        extra: vec![("iTunesMetadata.plist".to_string(), INFO_PLIST.as_bytes().to_vec())],
        ..Default::default()
    })
}
