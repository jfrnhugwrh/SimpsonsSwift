//! Tests for the importer.
//!
//! The archives under test are built in memory by [`crate::test_support`]: a
//! real ZIP, deflate-compressed, holding a real ARMv7 Mach-O and an `Info.plist`
//! written by Apple's `plistlib`.  Nothing copyrighted is involved — the fixtures
//! are a Mach-O synthesised by `macho::test_support` and the *metadata* of the
//! release this emulator targets.

use std::path::{Path, PathBuf};

use crate::error::IpaError;
use crate::inflate::inflate;
use crate::json;
use crate::library::{self, ImportedGame, Manifest};
use crate::plist::Plist;
use crate::test_support::*;
use crate::zip::{crc32, Zip};
use crate::{
    app_bundle_dirs, import_bytes, inspect_bytes, looks_like_ipa, slug, ImportOptions, TitleMatch,
};

// ---------------------------------------------------------------------------
// scratch directories
// ---------------------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("simpsons-ipa-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn options_in(root: &Path) -> ImportOptions {
    ImportOptions { root: Some(root.to_path_buf()), ..Default::default() }
}

// ---------------------------------------------------------------------------
// DEFLATE
// ---------------------------------------------------------------------------

fn corpora() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("one byte", b"a".to_vec()),
        ("ascii", b"the quick brown fox jumps over the lazy dog".to_vec()),
        (
            "repetitive",
            b"ABCDEFGH".iter().cycle().take(50_000).copied().collect::<Vec<u8>>(),
        ),
        (
            "long match",
            std::iter::repeat(b'x').take(70_000).collect::<Vec<u8>>(),
        ),
        (
            "binary",
            (0..40_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect::<Vec<u8>>(),
        ),
        (
            "text with runs",
            "Simpsons ".repeat(9_000).into_bytes(),
        ),
    ]
}

#[test]
fn inflate_round_trips_every_block_type() {
    for (name, data) in corpora() {
        for (encoder, kind) in [
            (deflate_stored as fn(&[u8]) -> Vec<u8>, "stored"),
            (deflate_fixed, "fixed"),
            (deflate_dynamic, "dynamic"),
        ] {
            let compressed = encoder(&data);
            let inflated = inflate(&compressed, data.len())
                .unwrap_or_else(|e| panic!("{name}/{kind}: {e}"));
            assert_eq!(inflated, data, "{name} did not survive {kind} Huffman coding");
        }
    }
}

#[test]
fn a_damaged_stream_never_decodes_to_the_original_bytes() {
    let data = "Simpsons ".repeat(500).into_bytes();
    let mut compressed = deflate_dynamic(&data);
    // Flip a bit in the middle of the block: the decode must fail, or at the
    // very least produce something other than the original (in which case the
    // ZIP layer's CRC check is what catches it — see `zip_detects_a_damaged_entry`).
    let middle = compressed.len() / 2;
    compressed[middle] ^= 0x80;
    match inflate(&compressed, data.len()) {
        Err(error) => assert!(matches!(error, IpaError::Corrupt { .. } | IpaError::TooLarge { .. }), "{error}"),
        Ok(bytes) => assert_ne!(bytes, data, "a corrupted stream decoded to the original bytes"),
    }
}

#[test]
fn inflate_rejects_a_stored_block_with_a_bad_complement() {
    let mut compressed = deflate_stored(b"hello");
    // `deflate_stored` emits one header byte (BFINAL/BTYPE plus padding to the
    // byte boundary) then LEN and NLEN as little-endian words: byte 3 is the low
    // byte of NLEN, whose complement must match LEN.
    assert_eq!(&compressed[..3], &[0x01, 0x05, 0x00]);
    compressed[3] ^= 0xff;
    let error = inflate(&compressed, 5).unwrap_err();
    assert!(matches!(error, IpaError::Corrupt { .. }), "{error}");
    assert!(error.to_string().contains("complement"), "{error}");
}

#[test]
fn inflate_rejects_a_truncated_stream() {
    let compressed = deflate_fixed(b"the quick brown fox");
    let error = inflate(&compressed[..compressed.len() - 1], 19).unwrap_err();
    assert!(matches!(error, IpaError::Corrupt { .. }), "{error}");
}

/// A tiny bit writer for hand-built deflate headers.
struct Bits {
    out: Vec<u8>,
    acc: u32,
    bits: u32,
}

impl Bits {
    fn new() -> Self {
        Bits { out: Vec::new(), acc: 0, bits: 0 }
    }

    fn push(&mut self, value: u32, count: u32) {
        self.acc |= (value & ((1 << count) - 1)) << self.bits;
        self.bits += count;
        while self.bits >= 8 {
            self.out.push((self.acc & 0xff) as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push((self.acc & 0xff) as u8);
        }
        self.out
    }
}

#[test]
fn inflate_rejects_an_over_subscribed_code() {
    // A dynamic block whose code-length alphabet claims four 1-bit codes, which
    // the Kraft inequality forbids.
    let mut bits = Bits::new();
    bits.push(1, 1); // BFINAL
    bits.push(2, 2); // BTYPE = dynamic
    bits.push(0, 5); // HLIT = 257
    bits.push(0, 5); // HDIST = 1
    bits.push(0, 4); // HCLEN = 4
    for _ in 0..4 {
        bits.push(1, 3);
    }
    let error = inflate(&bits.finish(), 0).unwrap_err();
    assert!(matches!(error, IpaError::Corrupt { .. }), "{error}");
    assert!(error.to_string().contains("over-subscribed"), "{error}");
}

#[test]
fn inflate_decodes_15_bit_huffman_codes() {
    // Construct a canonical Huffman tree with 15-bit codes.
    // Lengths: symbols 0 and 1 have length 15; symbols 2..=15 have lengths 14 down to 1.
    // Sum of 2^-len: 2*(2^-15) + sum_{i=1..14} 2^-i = 2^-14 + (1 - 2^-14) = 1 (complete tree).
    let mut lengths = vec![0u8; 288];
    lengths[0] = 15;
    lengths[1] = 15;
    for i in 2..=15 {
        lengths[i] = (16 - i) as u8;
    }
    // Symbol 256 (end-of-block) needs a code: let symbol 256 be symbol 1 with length 15.
    // Specifically, let literal 0 be len 15, and EOB (256) be len 15.
    let mut lit_lengths = vec![0u8; 288];
    lit_lengths[0] = 15;
    lit_lengths[256] = 15; // EOB
    for i in 1..=14 {
        lit_lengths[i] = (15 - i) as u8; // lengths 14 down to 1 for symbols 1..14
    }

    // Dynamic block with these literal lengths and 1 dummy distance code.
    let mut bits = Bits::new();
    bits.push(1, 1); // BFINAL = 1
    bits.push(2, 2); // BTYPE = dynamic
    bits.push(288 - 257, 5); // HLIT: 288 codes -> value 31
    bits.push(0, 5); // HDIST: 1 code -> value 0
    // We can write literal lengths directly or with uncompressed code lengths.
    // In test_support we have dynamic deflate helpers.
    // Testing the Huffman struct directly:
    let huffman = crate::inflate::Huffman::from_lengths(&lit_lengths).expect("valid complete tree");
    // Symbol 256 with length 15 has canonical code 111111111111111 (15 ones).
    let mut br_data = vec![0xff; 4];
    let mut br = crate::inflate::BitReader::new(&br_data);
    let decoded = huffman.decode(&mut br).expect("decodes 15-bit code");
    assert_eq!(decoded, 256);
}

// ---------------------------------------------------------------------------
// ZIP
// ---------------------------------------------------------------------------

#[test]
fn crc32_matches_the_known_check_value() {
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b"TheSimpsons"), 0xf768_7bbd);
}

#[test]
fn zip_reads_stored_and_deflated_entries() {
    let mut builder = ZipBuilder::new();
    builder.stored("a.txt", b"stored bytes");
    builder.file("b.bin", &(0..9_000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
    builder.directory("Payload");
    let archive = builder.build();

    let zip = Zip::open(&archive).unwrap();
    assert_eq!(zip.entries().len(), 3);
    assert_eq!(zip.read(zip.find("a.txt").unwrap()).unwrap(), b"stored bytes");
    assert_eq!(zip.read(zip.find("b.bin").unwrap()).unwrap().len(), 9_000);
    assert!(zip.find("Payload/").unwrap().is_directory());
    assert!(!zip.find("b.bin").unwrap().is_executable());
}

#[test]
fn zip_detects_a_damaged_entry() {
    let mut builder = ZipBuilder::new();
    builder.file("Payload/App.app/Info.plist", INFO_PLIST.as_bytes());
    let mut archive = builder.build();
    // Damage the compressed bytes: they live between the local header and the
    // central directory, so the middle of the file is a safe place to poke.
    let middle = archive.len() / 3;
    archive[middle] ^= 0xff;
    let zip = Zip::open(&archive).unwrap();
    let error = zip.read(zip.find("Payload/App.app/Info.plist").unwrap()).unwrap_err();
    assert!(matches!(error, IpaError::Corrupt { .. }), "{error}");
}

#[test]
fn zip_rejects_files_that_are_not_archives() {
    let not_a_zip = b"Mach-O\x00\x00not a zip at all, just bytes".to_vec();
    assert!(matches!(Zip::open(&not_a_zip).unwrap_err(), IpaError::NotAZip(_)));

    // A ZIP signature with no end-of-central-directory record: truncated.
    let mut truncated = ZipBuilder::new();
    truncated.file("Payload/App.app/Info.plist", INFO_PLIST.as_bytes());
    let archive = truncated.build();
    let error = Zip::open(&archive[..archive.len() - 30]).unwrap_err();
    assert!(matches!(error, IpaError::NotAZip(_)), "{error}");

    assert!(matches!(Zip::open(b"PK\x03\x04").unwrap_err(), IpaError::NotAZip(_)));
}

#[test]
fn zip_refuses_compression_methods_it_cannot_do() {
    let mut builder = ZipBuilder::new();
    builder.entry("Payload/App.app/Info.plist", b"bzip2 bytes", 12, 0o644);
    let archive = builder.build();
    let zip = Zip::open(&archive).unwrap();
    let error = zip.read(zip.find("Payload/App.app/Info.plist").unwrap()).unwrap_err();
    assert!(matches!(error, IpaError::Unsupported { .. }), "{error}");
}

#[test]
fn zip_reads_a_zip64_archive() {
    // One stored entry, with 0xffff_ffff in the central directory and the real
    // sizes in the ZIP64 extra field, plus a ZIP64 end-of-directory record.
    let name = b"Payload/App.app/Info.plist";
    let data = b"zip64 payload";
    let mut out: Vec<u8> = Vec::new();

    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // stored
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x2821u16.to_le_bytes());
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // extra length
    out.extend_from_slice(name);
    out.extend_from_slice(&1u16.to_le_bytes()); // zip64 extra id
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(data);

    let cd_offset = out.len() as u64;
    out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    out.extend_from_slice(&0x032du16.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x2821u16.to_le_bytes());
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&28u16.to_le_bytes()); // extra length
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&((0o10_0644u32) << 16).to_le_bytes());
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // local header offset
    let cd_size = out.len() as u64 - cd_offset;

    let zip64_offset = out.len() as u64;
    out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
    out.extend_from_slice(&44u64.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());

    out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&zip64_offset.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());

    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0xffffu16.to_le_bytes());
    out.extend_from_slice(&0xffffu16.to_le_bytes());
    out.extend_from_slice(&0xffffu16.to_le_bytes());
    out.extend_from_slice(&0xffffu16.to_le_bytes());
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    let zip = Zip::open(&out).expect("the ZIP64 archive opens");
    assert!(zip.zip64, "the archive should be reported as ZIP64");
    assert_eq!(zip.entries().len(), 1);
    assert_eq!(zip.read(&zip.entries()[0]).unwrap(), data);
}

// ---------------------------------------------------------------------------
// plist
// ---------------------------------------------------------------------------

#[test]
fn xml_plist_reads_the_reference_document() {
    let plist = Plist::parse(INFO_PLIST.as_bytes()).expect("plistlib XML parses");
    assert_eq!(plist.get("CFBundleExecutable").and_then(|v| v.as_str()), Some("TheSimpsons"));
    assert_eq!(
        plist.get("CFBundleIdentifier").and_then(|v| v.as_str()),
        Some("com.ea.simpsonsarcade.bv")
    );
    assert_eq!(plist.get("CFBundleShortVersionString").and_then(|v| v.as_str()), Some("1.1.43"));
    // The `&amp;` entity and the non-ASCII copyright have to survive.
    assert_eq!(
        plist.get("NSHumanReadableCopyright").and_then(|v| v.as_str()),
        Some("Tom & Jerry © EA")
    );
    assert_eq!(
        plist.get("UIDeviceFamily").and_then(|v| v.as_array()).map(|a| a.len()),
        Some(2)
    );
    assert_eq!(plist.string_at("DTPlatformName").as_deref(), Some("iphoneos"));
}

#[test]
fn binary_plist_reads_the_same_document() {
    let plist = Plist::parse(BINARY_INFO_PLIST).expect("bplist00 parses");
    assert_eq!(plist.get("CFBundleExecutable").and_then(|v| v.as_str()), Some("TheSimpsons"));
    assert_eq!(
        plist.get("CFBundleIdentifier").and_then(|v| v.as_str()),
        Some("com.ea.simpsonsarcade.bv")
    );
    assert_eq!(plist.get("CFBundleShortVersionString").and_then(|v| v.as_str()), Some("1.1.43"));
    assert_eq!(
        plist.get("NSHumanReadableCopyright").and_then(|v| v.as_str()),
        Some("Tom & Jerry © EA"),
        "the UTF-16 string object has to decode"
    );
    let families = plist.get("UIDeviceFamily").and_then(|v| v.as_array()).expect("array");
    assert_eq!(families.iter().filter_map(|v| v.as_int()).collect::<Vec<_>>(), vec![1, 2]);
}

#[test]
fn plist_reports_documents_it_cannot_read() {
    assert!(Plist::parse(b"not a plist").is_err());
    assert!(Plist::parse(b"<plist><dict><key>a</dict></plist>").is_err());
    assert!(Plist::parse(&BINARY_INFO_PLIST[..40]).is_err());
}

// ---------------------------------------------------------------------------
// inspection
// ---------------------------------------------------------------------------

#[test]
fn inspection_recognises_the_game() {
    let archive = simpsons_ipa();
    let bundle = inspect_bytes(&archive, None).expect("the fixture IPA validates");
    assert_eq!(bundle.app_dir, "Payload/TheSimpsons.app");
    assert_eq!(bundle.app_name, "TheSimpsons.app");
    assert_eq!(bundle.executable_name, "TheSimpsons");
    assert_eq!(bundle.executable_entry, "Payload/TheSimpsons.app/TheSimpsons");
    assert_eq!(bundle.info.bundle_id.as_deref(), Some("com.ea.simpsonsarcade.bv"));
    assert_eq!(bundle.info.display_name.as_deref(), Some("The Simpsons Arcade"));
    assert_eq!(bundle.version().as_deref(), Some("1.1.43"));
    assert_eq!(bundle.info.minimum_os.as_deref(), Some("2.2.1"));
    assert_eq!(bundle.info.device_families, vec![1, 2]);
    assert_eq!(bundle.architectures.iter().map(|a| a.name.clone()).collect::<Vec<_>>(), vec!["armv7"]);
    assert!(bundle.has_arm);
    assert!(!bundle.encrypted);
    assert_eq!(bundle.title, "The Simpsons Arcade");
    assert_eq!(bundle.title_match, TitleMatch::Exact);
    assert!(bundle.summary().contains("armv7"), "{}", bundle.summary());
}

#[test]
fn inspection_finds_the_bundle_without_directory_entries() {
    // Xcode writes `Payload/` and `Payload/App.app/` records; `zip -r` on some
    // systems does not.  The bundle must be found either way.
    let mut builder = ZipBuilder::new();
    builder.file("Payload/App.app/Info.plist", INFO_PLIST.as_bytes());
    builder.entry("Payload/App.app/TheSimpsons", &armv7_executable(), METHOD_DEFLATE, 0o755);
    let archive = builder.build();
    let bundle = inspect_bytes(&archive, None).expect("found without directory entries");
    assert_eq!(bundle.app_dir, "Payload/App.app");
    assert_eq!(app_bundle_dirs(&Zip::open(&archive).unwrap()), vec!["Payload/App.app".to_string()]);
}

#[test]
fn inspection_picks_the_armv7_slice_of_a_universal_binary() {
    let archive = build_ipa(&FakeIpa { fat: true, ..Default::default() });
    let bundle = inspect_bytes(&archive, None).expect("a fat binary still has an armv7 slice");
    let names: Vec<&str> = bundle.architectures.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["armv7", "arm64"]);
    assert!(bundle.has_arm);
    assert_eq!(bundle.title_match, TitleMatch::Exact);
}

#[test]
fn inspection_refuses_an_arm64_only_build() {
    let archive = build_ipa(&FakeIpa { arm64_only: true, ..Default::default() });
    let error = inspect_bytes(&archive, None).unwrap_err();
    match error {
        IpaError::NoArmSlice { ref architectures, .. } => assert_eq!(architectures, &vec!["arm64".to_string()]),
        other => panic!("expected NoArmSlice, got {other}"),
    }
    assert!(error.to_string().contains("ARMv7"), "{error}");
}

#[test]
fn inspection_refuses_an_encrypted_binary() {
    let archive = build_ipa(&FakeIpa { encrypted: true, ..Default::default() });
    let error = inspect_bytes(&archive, None).unwrap_err();
    assert!(matches!(error, IpaError::Encrypted { cryptid: 1, .. }), "{error}");
    assert!(error.to_string().contains("FairPlay"), "{error}");
}

#[test]
fn inspection_refuses_archives_with_no_app_bundle() {
    let mut builder = ZipBuilder::new();
    builder.file("readme.txt", b"not an app");
    builder.file("Pictures/photo.png", b"nope");
    let archive = builder.build();
    let error = inspect_bytes(&archive, None).unwrap_err();
    assert!(matches!(error, IpaError::NoAppBundle { .. }), "{error}");
}

#[test]
fn inspection_reports_a_bundle_with_no_info_plist() {
    let archive = build_ipa(&FakeIpa { omit_info_plist: true, ..Default::default() });
    let error = inspect_bytes(&archive, None).unwrap_err();
    assert!(matches!(error, IpaError::InfoPlist { .. }), "{error}");
}

#[test]
fn inspection_reports_a_bundle_with_no_executable() {
    let mut builder = ZipBuilder::new();
    builder.file("Payload/App.app/Info.plist", INFO_PLIST.as_bytes());
    builder.file("Payload/App.app/Icon.png", b"png");
    let error = inspect_bytes(&builder.build(), None).unwrap_err();
    match error {
        IpaError::NoExecutable { tried, .. } => assert!(tried.contains(&"TheSimpsons".to_string())),
        other => panic!("expected NoExecutable, got {other}"),
    }
}

#[test]
fn inspection_needs_a_name_when_two_bundles_are_present() {
    let mut builder = ZipBuilder::new();
    for app in ["TheSimpsons.app", "SimpsonsPad.app"] {
        builder.file(&format!("Payload/{app}/Info.plist"), INFO_PLIST.as_bytes());
        builder.entry(&format!("Payload/{app}/TheSimpsons"), &armv7_executable(), METHOD_DEFLATE, 0o755);
    }
    let archive = builder.build();
    let error = inspect_bytes(&archive, None).unwrap_err();
    match &error {
        IpaError::AmbiguousBundles(bundles) => assert_eq!(bundles.len(), 2),
        other => panic!("expected AmbiguousBundles, got {other}"),
    }
    // ...and works when the bundle is named.
    let bundle = inspect_bytes(&archive, Some("SimpsonsPad")).expect("named bundle");
    assert_eq!(bundle.app_name, "SimpsonsPad.app");
}

#[test]
fn inspection_flags_the_wrong_version_and_the_wrong_app() {
    let other_version = build_ipa(&FakeIpa { version: "1.0.0".to_string(), ..Default::default() });
    let bundle = inspect_bytes(&other_version, None).expect("still the right title");
    assert_eq!(bundle.title_match, TitleMatch::OtherVersion);
    assert_eq!(bundle.version().as_deref(), Some("1.0.0"));

    let mut builder = ZipBuilder::new();
    let plist = INFO_PLIST
        .replace("com.ea.simpsonsarcade.bv", "com.example.other")
        .replace("TheSimpsons", "OtherApp")
        .replace("The Simpsons Arcade", "Other App");
    builder.file("Payload/OtherApp.app/Info.plist", plist.as_bytes());
    builder.entry("Payload/OtherApp.app/OtherApp", &armv7_executable(), METHOD_DEFLATE, 0o755);
    let bundle = inspect_bytes(&builder.build(), None).expect("a valid IPA, wrong game");
    assert_eq!(bundle.title_match, TitleMatch::Unknown);
}

#[test]
fn inspection_reads_a_binary_info_plist() {
    let archive = build_ipa(&FakeIpa { binary_plist: true, ..Default::default() });
    let bundle = inspect_bytes(&archive, None).expect("bplist00 Info.plist");
    assert_eq!(bundle.info.bundle_id.as_deref(), Some("com.ea.simpsonsarcade.bv"));
    assert_eq!(bundle.executable_name, "TheSimpsons");
    assert_eq!(bundle.title_match, TitleMatch::Exact);
}

// ---------------------------------------------------------------------------
// importing
// ---------------------------------------------------------------------------

#[test]
fn import_extracts_a_playable_bundle() {
    let root = temp_dir("import");
    let archive = simpsons_ipa();
    let report = import_bytes(&archive, "The Simpsons Arcade v1.1.43.ipa", &options_in(&root))
        .expect("the fixture IPA imports");

    assert!(!report.reused);
    assert_eq!(report.root, root);
    assert_eq!(report.report.files, 5, "Info.plist + executable + 3 resources");
    assert!(report.report.bytes > 0);

    let game = &report.game;
    assert_eq!(game.dir, root.join("com.ea.simpsonsarcade.bv"));
    assert_eq!(game.label(), "The Simpsons Arcade 1.1.43");
    assert!(game.is_usable(), "{} is missing", game.executable().display());
    assert!(game.bundle_dir().join("Info.plist").is_file());
    assert!(game.bundle_dir().join("en.lproj/Localizable.strings").is_file());

    // The Mach-O has to be executable and still be the one that was packaged.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(game.executable()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the executable bit should survive the import");
    }
    let manifest = Manifest::read(&game.dir).expect("a manifest was written");
    assert_eq!(manifest.bundle_id.as_deref(), Some("com.ea.simpsonsarcade.bv"));
    assert_eq!(manifest.version.as_deref(), Some("1.1.43"));
    assert_eq!(manifest.source_file, "The Simpsons Arcade v1.1.43.ipa");
    assert_eq!(manifest.architectures, vec!["armv7".to_string()]);
    assert_eq!(manifest.title_match, "exact");

    // The library can find it again.
    let games = library::list(&root);
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].executable(), game.executable());
    assert!(crate::find_game(&root, "com.ea.simpsonsarcade.bv").is_some());
    assert!(crate::find_game(&root, "TheSimpsons.app").is_some());
    assert!(crate::find_game(&root, "nope").is_none());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn import_refuses_to_silently_replace_an_import() {
    let root = temp_dir("replace");
    let archive = simpsons_ipa();
    import_bytes(&archive, "game.ipa", &options_in(&root)).unwrap();

    let error = import_bytes(&archive, "game.ipa", &options_in(&root)).unwrap_err();
    assert!(matches!(error, IpaError::AlreadyImported { .. }), "{error}");
    assert!(error.to_string().contains("--force"), "{error}");

    // --force re-extracts, reuse accepts what is there.
    let mut forced = options_in(&root);
    forced.force = true;
    assert!(!import_bytes(&archive, "game.ipa", &forced).unwrap().reused);
    let mut reuse = options_in(&root);
    reuse.reuse = true;
    assert!(import_bytes(&archive, "game.ipa", &reuse).unwrap().reused);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn import_rejects_an_ipa_that_is_not_the_game() {
    let root = temp_dir("wrong-game");
    let mut builder = ZipBuilder::new();
    let plist = INFO_PLIST
        .replace("com.ea.simpsonsarcade.bv", "com.example.other")
        .replace("TheSimpsons", "OtherApp")
        .replace("The Simpsons Arcade", "Other App");
    builder.file("Payload/OtherApp.app/Info.plist", plist.as_bytes());
    builder.entry("Payload/OtherApp.app/OtherApp", &armv7_executable(), METHOD_DEFLATE, 0o755);
    let archive = builder.build();

    let error = import_bytes(&archive, "other.ipa", &options_in(&root)).unwrap_err();
    assert!(matches!(error, IpaError::UnsupportedApp { .. }), "{error}");
    assert!(error.to_string().contains("--allow-other-app"), "{error}");

    let mut allowed = options_in(&root);
    allowed.allow_other_app = true;
    let report = import_bytes(&archive, "other.ipa", &allowed).expect("allowed explicitly");
    assert_eq!(report.game.manifest.title, "unknown");
    assert_eq!(report.game.dir.file_name().unwrap(), "com.example.other");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn import_refuses_entries_that_escape_the_destination() {
    let root = temp_dir("zip-slip");
    let mut builder = ZipBuilder::new();
    builder.file("Payload/App.app/Info.plist", INFO_PLIST.as_bytes());
    builder.entry("Payload/App.app/TheSimpsons", &armv7_executable(), METHOD_DEFLATE, 0o755);
    builder.file("Payload/App.app/../../../../etc/evil", b"nope");
    let archive = builder.build();

    let error = import_bytes(&archive, "evil.ipa", &options_in(&root)).unwrap_err();
    assert!(matches!(error, IpaError::UnsafePath(_)), "{error}");
    assert!(!root.join("etc/evil").exists());

    // A symlink entry is skipped rather than created (a fresh root: the
    // extraction above left a partial import behind).
    let root = temp_dir("zip-slip-symlink");
    let mut with_symlink = ZipBuilder::new();
    with_symlink.file("Payload/App.app/Info.plist", INFO_PLIST.as_bytes());
    with_symlink.entry("Payload/App.app/TheSimpsons", &armv7_executable(), METHOD_DEFLATE, 0o755);
    with_symlink.entry("Payload/App.app/link", b"/etc/passwd", METHOD_STORED, 0o12_0777);
    let report = import_bytes(&with_symlink.build(), "link.ipa", &options_in(&root)).unwrap();
    assert_eq!(report.report.skipped, vec!["Payload/App.app/link".to_string()]);
    assert!(!report.game.bundle_dir().join("link").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn import_reports_damaged_archives_without_panicking() {
    let root = temp_dir("damaged");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("garbage", vec![0u8; 64]),
        ("text", b"this is not an ipa, it is a text file\n".to_vec()),
    ];
    for (name, bytes) in cases {
        let error = import_bytes(&bytes, name, &options_in(&root))
            .err()
            .unwrap_or_else(|| panic!("{name} should not import"));
        assert!(
            matches!(error, IpaError::NotAZip(_) | IpaError::Corrupt { .. } | IpaError::NoAppBundle { .. }),
            "{name}: {error}"
        );
    }
    // A truncated copy of a real IPA.
    let archive = simpsons_ipa();
    for cut in [1, 64, archive.len() / 2, archive.len() - 1] {
        let result = import_bytes(&archive[..cut], "truncated.ipa", &options_in(&root));
        assert!(result.is_err(), "truncating to {cut} bytes should fail");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_imported_bundle_is_an_ordinary_ios_bundle() {
    // The whole point: after importing, the emulator's existing entry point
    // (`MachO::from_path_slice` + `--bundle`) is all that is needed.
    let root = temp_dir("usable");
    let report = import_bytes(&simpsons_ipa(), "game.ipa", &options_in(&root)).unwrap();
    let image = macho::MachO::from_path_slice(report.game.executable(), macho::CPU_TYPE_ARM)
        .expect("the extracted executable parses as a Mach-O");
    assert_eq!(image.header.cpusubtype_name(), "armv7");
    assert!(!image.is_encrypted());
    assert!(image.entry_pc().is_ok());
    // ...and the bundle directory is the one the emulator loads assets from.
    assert!(report.game.bundle_dir().join("assets/strings_generic.bin").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// library helpers
// ---------------------------------------------------------------------------

#[test]
fn slug_is_filesystem_safe() {
    assert_eq!(slug(Some("com.ea.simpsonsarcade.bv"), "TheSimpsons.app"), "com.ea.simpsonsarcade.bv");
    assert_eq!(slug(None, "The Simpsons Arcade.ipa"), "The-Simpsons-Arcade.ipa");
    assert_eq!(slug(None, ""), "imported-game");
    assert_eq!(slug(None, "../.."), "imported-game");
}

#[test]
fn manifest_round_trips_through_json() {
    let manifest = Manifest {
        title: "The Simpsons Arcade".to_string(),
        title_match: "exact".to_string(),
        app_bundle: "TheSimpsons.app".to_string(),
        executable: "TheSimpsons".to_string(),
        bundle_id: Some("com.ea.simpsonsarcade.bv".to_string()),
        display_name: Some("The Simpsons Arcade".to_string()),
        version: Some("1.1.43".to_string()),
        build: Some("1.1.43".to_string()),
        architectures: vec!["armv7".to_string()],
        source_file: "The Simpsons Arcade v1.1.43.ipa".to_string(),
        source_size: 53_379_072,
        files: 1_234,
        bytes: 157_286_400,
        imported_unix: 1_767_225_600,
    };
    let text = manifest.to_json().to_string_pretty();
    assert!(text.contains("\"bundle_id\": \"com.ea.simpsonsarcade.bv\""), "{text}");
    let parsed = Manifest::from_json(&json::parse(&text).unwrap()).unwrap();
    assert_eq!(parsed, manifest);
}

#[test]
fn timestamps_are_formatted_without_a_date_library() {
    assert_eq!(library::format_unix(0), "1970-01-01T00:00:00Z");
    assert_eq!(library::format_unix(1_767_225_599), "2025-12-31T23:59:59Z");
    assert_eq!(library::format_unix(1_767_225_600), "2026-01-01T00:00:00Z");
    // Leap days, including the century rule.
    assert_eq!(library::format_unix(1_709_164_800), "2024-02-29T00:00:00Z");
    assert_eq!(library::format_unix(951_827_415), "2000-02-29T12:30:15Z");
}

#[test]
fn ipa_detection_is_by_extension_and_magic() {
    assert!(looks_like_ipa(Path::new("The Simpsons Arcade v1.1.43.ipa")));
    assert!(looks_like_ipa(Path::new("/tmp/GAME.IPA")));
    assert!(!looks_like_ipa(Path::new("Simpsons.app/Simpsons")));
    assert!(crate::has_zip_magic(b"PK\x03\x04rest"));
    assert!(!crate::has_zip_magic(b"\xcf\xfa\xed\xfe"));
}

#[test]
fn an_imported_game_survives_a_reload_from_disk() {
    let root = temp_dir("reload");
    import_bytes(&simpsons_ipa(), "game.ipa", &options_in(&root)).unwrap();
    let reloaded = ImportedGame::load(&root.join("com.ea.simpsonsarcade.bv")).expect("manifest reads back");
    assert_eq!(reloaded.label(), "The Simpsons Arcade 1.1.43");
    assert!(reloaded.is_usable());
    let _ = std::fs::remove_dir_all(&root);
}
