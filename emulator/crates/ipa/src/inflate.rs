//! A raw DEFLATE (RFC 1951) decompressor.
//!
//! An `.ipa` is a ZIP archive and essentially everything inside a real App Store
//! package — `Info.plist`, the PNGs, the audio, the Mach-O itself — is deflate
//! compressed, so the importer needs an inflater.  The workspace has no external
//! dependencies, so this module is the whole of it: a bit reader, canonical
//! Huffman tables and the copy loop.
//!
//! It is deliberately strict, in the same spirit as the `macho` crate: an
//! over-subscribed code, a distance that reaches before the start of the output
//! or a stream that stops mid-field is an [`IpaError::Corrupt`], never a wrong
//! byte handed back to the caller.

use crate::error::{IpaError, Result};

/// Base match lengths for length codes 257..=285.
pub(crate) const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits carried by each length code.
pub(crate) const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base match distances for distance codes 0..=29.
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits carried by each distance code.
pub(crate) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Refuse to inflate into more than this: a 1 KB entry claiming 4 GB is a
/// zip bomb, not a game asset.  Callers pass their own (much smaller) limit;
/// this is the backstop.
pub const MAX_INFLATED: usize = 1 << 31;

/// LSB-first bit reader.  DEFLATE packs ordinary fields least-significant bit
/// first, but Huffman codes are packed most-significant bit first — hence
/// [`Huffman::decode`] pulls one bit at a time instead of a whole field.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, acc: 0, bits: 0 }
    }

    /// Read `n` bits (`n <= 25`) as a little-endian field.
    fn take(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 25, "inflate never reads more than 25 bits at a time");
        while self.bits < n {
            if self.pos >= self.data.len() {
                return Err(IpaError::Corrupt {
                    what: "deflate stream ended in the middle of a field".to_string(),
                });
            }
            self.acc |= (self.data[self.pos] as u32) << self.bits;
            self.pos += 1;
            self.bits += 8;
        }
        let value = self.acc & ((1u32 << n) - 1);
        self.acc >>= n;
        self.bits -= n;
        Ok(value)
    }

    /// Discard the rest of the current byte (before a stored block).
    fn align(&mut self) {
        let drop = self.bits % 8;
        self.acc >>= drop;
        self.bits -= drop;
    }

    /// Append `n` literal bytes of the stream to `out`.
    fn copy(&mut self, out: &mut Vec<u8>, n: usize) -> Result<()> {
        if n > self.data.len() - self.pos {
            return Err(IpaError::Corrupt {
                what: format!("stored deflate block wants {n} bytes, {} left", self.data.len() - self.pos),
            });
        }
        out.extend_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(())
    }
}

/// A canonical Huffman code, decoded with the "first/index" walk from zlib's
/// `puff.c`: no lookup tables, no allocation beyond the sorted symbol list, and
/// an over-subscribed code is rejected when the table is built rather than when
/// it is used.
pub(crate) struct Huffman {
    /// `counts[len]` = number of symbols with that code length.
    counts: [u16; 16],
    /// Symbols ordered by (code length, symbol value) — canonical order.
    symbols: Vec<u16>,
}

impl Huffman {
    pub fn from_lengths(lengths: &[u8]) -> Result<Self> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            if length > 15 {
                return Err(IpaError::Corrupt {
                    what: format!("deflate code length {length} is longer than 15 bits"),
                });
            }
            counts[length as usize] += 1;
        }
        // An all-zero tree is legal for the distance alphabet of a block that
        // never back-references (zlib builds a dummy entry for it); decoding
        // through one is an error, which `decode` produces naturally because
        // `symbols` is empty.
        let mut symbols = Vec::new();
        if counts[0] as usize == lengths.len() {
            return Ok(Huffman { counts, symbols });
        }

        // Over-subscription check: at every length the codes used so far must
        // leave room in the tree.  Incomplete codes are allowed (that is how a
        // single-symbol alphabet is written) and simply fail to decode.
        let mut left: i32 = 1;
        for length in 1..=15 {
            left <<= 1;
            left -= counts[length] as i32;
            if left < 0 {
                return Err(IpaError::Corrupt {
                    what: format!("over-subscribed deflate Huffman code at {length} bits"),
                });
            }
        }

        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        symbols.resize(lengths.len(), 0);
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[offsets[length as usize] as usize] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader) -> Result<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for length in 1..15 {
            code |= br.take(1)? as i32;
            let count = self.counts[length] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(IpaError::Corrupt {
            what: "invalid Huffman code in deflate stream".to_string(),
        })
    }
}

fn fixed_trees() -> (Huffman, Huffman) {
    // RFC 1951 §3.2.6: the fixed literal/length tree is 8 bits for 0..=143,
    // 9 for 144..=255, 7 for 256..=279 and 8 for 280..=287; every distance is
    // 5 bits.
    let mut literal = [0u8; 288];
    for (symbol, length) in literal.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let distance = [5u8; 30];
    (
        Huffman::from_lengths(&literal).expect("the fixed literal tree is valid by construction"),
        Huffman::from_lengths(&distance).expect("the fixed distance tree is valid by construction"),
    )
}

fn dynamic_trees(br: &mut BitReader) -> Result<(Huffman, Huffman)> {
    /// Order in which the code-length alphabet's own lengths are written.
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

    let hlit = br.take(5)? as usize + 257;
    let hdist = br.take(5)? as usize + 1;
    let hclen = br.take(4)? as usize + 4;

    let mut code_lengths = [0u8; 19];
    for index in 0..hclen {
        code_lengths[ORDER[index]] = br.take(3)? as u8;
    }
    let meta = Huffman::from_lengths(&code_lengths)?;

    let total = hlit + hdist;
    let mut lengths = vec![0u8; total];
    let mut filled = 0usize;
    let mut previous = 0u8;
    while filled < total {
        let symbol = meta.decode(br)?;
        match symbol {
            0..=15 => {
                lengths[filled] = symbol as u8;
                previous = symbol as u8;
                filled += 1;
            }
            16 => {
                if filled == 0 {
                    return Err(IpaError::Corrupt {
                        what: "deflate repeat-previous code (16) with nothing to repeat".to_string(),
                    });
                }
                fill(&mut lengths, &mut filled, total, 3 + br.take(2)? as usize, previous)?;
            }
            17 => fill(&mut lengths, &mut filled, total, 3 + br.take(3)? as usize, 0)?,
            18 => fill(&mut lengths, &mut filled, total, 11 + br.take(7)? as usize, 0)?,
            _ => {
                return Err(IpaError::Corrupt {
                    what: format!("invalid deflate code-length symbol {symbol}"),
                })
            }
        }
    }
    Ok((Huffman::from_lengths(&lengths[..hlit])?, Huffman::from_lengths(&lengths[hlit..])?))
}

fn fill(lengths: &mut [u8], filled: &mut usize, total: usize, count: usize, value: u8) -> Result<()> {
    for _ in 0..count {
        if *filled >= total {
            return Err(IpaError::Corrupt {
                what: "deflate code-length run overflows the alphabet".to_string(),
            });
        }
        lengths[*filled] = value;
        *filled += 1;
    }
    Ok(())
}

fn decode_block(
    br: &mut BitReader,
    literal: &Huffman,
    distance: &Huffman,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<()> {
    loop {
        let symbol = literal.decode(br)?;
        match symbol {
            0..=255 => {
                if out.len() >= limit {
                    return Err(IpaError::TooLarge {
                        what: "decompressed archive entry".to_string(),
                        limit: limit as u64,
                    });
                }
                out.push(symbol as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let index = (symbol - 257) as usize;
                let length =
                    LENGTH_BASE[index] as usize + br.take(LENGTH_EXTRA[index] as u32)? as usize;
                let code = distance.decode(br)?;
                if code as usize >= DIST_BASE.len() {
                    return Err(IpaError::Corrupt {
                        what: format!("invalid deflate distance code {code}"),
                    });
                }
                let back =
                    DIST_BASE[code as usize] as usize + br.take(DIST_EXTRA[code as usize] as u32)? as usize;
                if back > out.len() {
                    return Err(IpaError::Corrupt {
                        what: format!(
                            "deflate match distance {back} reaches before the start of the output ({} bytes)",
                            out.len()
                        ),
                    });
                }
                if out.len() + length > limit {
                    return Err(IpaError::TooLarge {
                        what: "decompressed archive entry".to_string(),
                        limit: limit as u64,
                    });
                }
                let start = out.len() - back;
                if back >= length {
                    out.extend_from_within(start..start + length);
                } else {
                    // Overlapping copy (`distance < length`) has to be
                    // byte-at-a-time: that is how RLE-style runs are written.
                    for offset in 0..length {
                        let byte = out[start + offset];
                        out.push(byte);
                    }
                }
            }
            _ => {
                return Err(IpaError::Corrupt {
                    what: format!("invalid deflate literal/length symbol {symbol}"),
                })
            }
        }
    }
}

/// Decompress a raw DEFLATE stream (no zlib header, no gzip wrapper — that is
/// what ZIP stores).  `expected_len` is only a capacity hint from the ZIP
/// central directory; `limit` is the hard cap on the output.
pub fn inflate_limited(data: &[u8], expected_len: usize, limit: usize) -> Result<Vec<u8>> {
    let limit = limit.min(MAX_INFLATED);
    let mut br = BitReader::new(data);
    let mut out = Vec::with_capacity(expected_len.min(limit).min(1 << 20));
    loop {
        let last = br.take(1)?;
        match br.take(2)? {
            0 => {
                br.align();
                let len = br.take(16)? as usize;
                let nlen = br.take(16)? as usize;
                if len ^ 0xffff != nlen {
                    return Err(IpaError::Corrupt {
                        what: format!(
                            "stored deflate block length {len:#06x} does not match its complement {nlen:#06x}"
                        ),
                    });
                }
                if out.len() + len > limit {
                    return Err(IpaError::TooLarge {
                        what: "decompressed archive entry".to_string(),
                        limit: limit as u64,
                    });
                }
                br.copy(&mut out, len)?;
            }
            1 => {
                let (literal, distance) = fixed_trees();
                decode_block(&mut br, &literal, &distance, &mut out, limit)?;
            }
            2 => {
                let (literal, distance) = dynamic_trees(&mut br)?;
                decode_block(&mut br, &literal, &distance, &mut out, limit)?;
            }
            other => {
                return Err(IpaError::Corrupt {
                    what: format!("deflate block type {other} is reserved"),
                })
            }
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}

/// Decompress a raw DEFLATE stream with the default cap.
pub fn inflate(data: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    inflate_limited(data, expected_len, MAX_INFLATED)
}
