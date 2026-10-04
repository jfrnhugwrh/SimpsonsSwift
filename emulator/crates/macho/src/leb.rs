//! LEB128 readers used by the `LC_DYLD_INFO` opcode streams and the export trie.

use crate::error::{MachOError, Result};

/// Cursor over a byte slice with bounds-checked primitives.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    pub bytes: &'a [u8],
    pub pos: usize,
    stream: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], stream: &'static str) -> Self {
        Reader { bytes, pos: 0, stream }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub fn u8(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.pos).ok_or(MachOError::BadLeb {
            stream: self.stream,
            offset: self.pos,
        })?;
        self.pos += 1;
        Ok(b)
    }

    /// `cstring` as stored in the bind streams: NUL terminated, not aligned.
    pub fn cstr(&mut self) -> Result<String> {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.bytes.len() {
            // Streams are sometimes zero padded and lack a final NUL.
            return Err(MachOError::BadLeb { stream: self.stream, offset: start });
        }
        let s = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
        self.pos += 1; // consume NUL
        Ok(s)
    }

    pub fn uleb(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            if shift >= 64 {
                return Err(MachOError::BadLeb { stream: self.stream, offset: self.pos });
            }
            result |= ((byte & 0x7f) as u64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    pub fn sleb(&mut self) -> Result<i64> {
        let mut result: i64 = 0;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            if shift >= 64 {
                return Err(MachOError::BadLeb { stream: self.stream, offset: self.pos });
            }
            result |= ((byte & 0x7f) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && (byte & 0x40) != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uleb_roundtrip() {
        let mut r = Reader::new(&[0xe5, 0x8e, 0x26], "t");
        assert_eq!(r.uleb().unwrap(), 624485);
        assert!(r.is_empty());
    }

    #[test]
    fn sleb_negative() {
        let mut r = Reader::new(&[0x7f], "t");
        assert_eq!(r.sleb().unwrap(), -1);
    }

    #[test]
    fn strings_are_nul_terminated() {
        let mut r = Reader::new(b"_printf\0_puts\0", "t");
        assert_eq!(r.cstr().unwrap(), "_printf");
        assert_eq!(r.cstr().unwrap(), "_puts");
    }
}
