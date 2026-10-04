//! The guest address space.
//!
//! iOS `mmap`s the Mach-O segments at their `vmaddr`, so the emulator needs an
//! address space that mirrors the guest's own view of memory: named regions
//! created from `LC_SEGMENT` file contents, anonymous regions for the stack and
//! the heap, plus protection bits so that a write to `__TEXT` (or a read of a
//! NULL pointer through `__PAGEZERO`) fails loudly instead of silently
//! corrupting the loaded game.

use std::collections::BTreeMap;
use std::fmt;

pub const PAGE_SIZE: u32 = 0x1000;
pub const PAGE_MASK: u32 = PAGE_SIZE - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl Permissions {
    pub const NONE: Permissions = Permissions { read: false, write: false, execute: false };
    pub const R: Permissions = Permissions { read: true, write: false, execute: false };
    pub const RW: Permissions = Permissions { read: true, write: true, execute: false };
    pub const RX: Permissions = Permissions { read: true, write: false, execute: true };
    pub const RWX: Permissions = Permissions { read: true, write: true, execute: true };

    /// Build from `VM_PROT_*` bits.
    pub fn from_vm_prot(bits: u32) -> Self {
        Permissions {
            read: bits & 1 != 0,
            write: bits & 2 != 0,
            execute: bits & 4 != 0,
        }
    }

    pub fn to_vm_prot(self) -> u32 {
        (self.read as u32) | ((self.write as u32) << 1) | ((self.execute as u32) << 2)
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            if self.read { 'r' } else { '-' },
            if self.write { 'w' } else { '-' },
            if self.execute { 'x' } else { '-' }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Backed by an `LC_SEGMENT`.
    Image,
    Stack,
    Heap,
    /// Where HLE entry points live for bound symbols.
    Trampoline,
    /// Loaded dylib images (used once real iOS libraries are mapped).
    Dylib,
    /// Plain `mmap`ped memory.
    Anonymous,
}

#[derive(Debug, Clone)]
pub struct Region {
    pub name: String,
    pub base: u32,
    /// Page-rounded size.
    pub size: u32,
    pub data: Vec<u8>,
    pub perms: Permissions,
    pub kind: RegionKind,
    /// True for regions that were created by `mmap`/`brk` and can be resized.
    pub resizable: bool,
}

impl Region {
    pub fn end(&self) -> u32 {
        self.base.saturating_add(self.size)
    }

    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.base && addr < self.end()
    }

    pub fn offset_of(&self, addr: u32) -> Option<usize> {
        if addr >= self.base && addr < self.end() {
            Some((addr - self.base) as usize)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Execute => "execute",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// Nothing is mapped at the address.
    Unmapped { addr: u32, access: Access, len: u32 },
    /// Mapped, but the region does not allow the access.
    Denied { addr: u32, access: Access, region: String, perms: Permissions },
    /// The address range crosses the end of a region.
    OutOfRegion { addr: u32, len: u32, region: String, region_end: u32 },
    /// Two regions would overlap.
    Overlap { base: u32, size: u32, existing: String },
    /// Not page aligned where alignment is required.
    Misaligned { addr: u32, what: &'static str },
    /// Address arithmetic overflowed the 32-bit guest space.
    AddressOverflow { addr: u32, len: u32 },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Unmapped { addr, access, len } => {
                write!(f, "unmapped {access} of {len} byte(s) at {addr:#010x}")
            }
            MemoryError::Denied { addr, access, region, perms } => write!(
                f,
                "permission denied: {access} at {addr:#010x} in {region} ({perms})"
            ),
            MemoryError::OutOfRegion { addr, len, region, region_end } => write!(
                f,
                "{len} byte(s) at {addr:#010x} run past the end of {region} ({region_end:#010x})"
            ),
            MemoryError::Overlap { base, size, existing } => write!(
                f,
                "mapping {size:#x} bytes at {base:#010x} overlaps {existing}"
            ),
            MemoryError::Misaligned { addr, what } => {
                write!(f, "{what} address {addr:#010x} is not page aligned")
            }
            MemoryError::AddressOverflow { addr, len } => {
                write!(f, "address {addr:#010x} + {len:#x} overflows the 32-bit guest space")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

pub type Result<T> = std::result::Result<T, MemoryError>;

/// Sparse 32-bit address space, `BTreeMap`-indexed by page so lookups stay
/// O(log n) with n = number of mapped pages.
#[derive(Debug, Clone, Default)]
pub struct AddressSpace {
    regions: Vec<Region>,
    page_index: BTreeMap<u32, usize>,
}

impl AddressSpace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    pub fn region_by_name(&self, name: &str) -> Option<&Region> {
        self.regions.iter().find(|r| r.name == name)
    }

    pub fn region_at(&self, addr: u32) -> Option<&Region> {
        self.page_index.get(&(addr & !PAGE_MASK)).map(|&i| &self.regions[i])
    }

    pub fn is_mapped(&self, addr: u32) -> bool {
        self.page_index.contains_key(&(addr & !PAGE_MASK))
    }

    /// Map `size` bytes at `base` (both page aligned) and copy `init` into the
    /// front of it; the remainder is zero filled.
    #[allow(clippy::too_many_arguments)]
    pub fn map(
        &mut self,
        name: impl Into<String>,
        base: u32,
        size: u32,
        perms: Permissions,
        kind: RegionKind,
        init: &[u8],
    ) -> Result<&Region> {
        if base & PAGE_MASK != 0 {
            return Err(MemoryError::Misaligned { addr: base, what: "region base" });
        }
        if size & PAGE_MASK != 0 {
            return Err(MemoryError::Misaligned { addr: size, what: "region size" });
        }
        let end = base.checked_add(size).ok_or(MemoryError::AddressOverflow { addr: base, len: size })?;
        if size == 0 {
            return Err(MemoryError::AddressOverflow { addr: base, len: 0 });
        }
        if init.len() as u32 > size {
            return Err(MemoryError::OutOfRegion {
                addr: base,
                len: init.len() as u32,
                region: name.into(),
                region_end: end,
            });
        }
        let name = name.into();
        for existing in &self.regions {
            if base < existing.end() && existing.base < end {
                return Err(MemoryError::Overlap {
                    base,
                    size,
                    existing: format!("{} ({:#x}..{:#x})", existing.name, existing.base, existing.end()),
                });
            }
        }
        let mut data = vec![0u8; size as usize];
        data[..init.len()].copy_from_slice(init);
        self.regions.push(Region { name, base, size, data, perms, kind, resizable: false });
        self.regions.sort_by_key(|r| r.base);
        self.rebuild_index();
        Ok(self.regions.iter().find(|r| r.base == base).unwrap())
    }

    /// Remove a mapping.  Ranges that only partially overlap a region split it,
    /// exactly like `munmap` does in the kernel.
    pub fn unmap(&mut self, base: u32, size: u32) -> Result<()> {
        let end =
            base.checked_add(size).ok_or(MemoryError::AddressOverflow { addr: base, len: size })?;
        if !self.regions.iter().any(|r| r.base < end && base < r.end()) {
            return Err(MemoryError::Unmapped { addr: base, access: Access::Write, len: size });
        }
        let mut result: Vec<Region> = Vec::with_capacity(self.regions.len() + 1);
        for region in self.regions.drain(..) {
            if region.end() <= base || region.base >= end {
                result.push(region);
                continue;
            }
            if region.base < base {
                let mut left = region.clone();
                left.size = base - region.base;
                left.data.truncate(left.size as usize);
                result.push(left);
            }
            if region.end() > end {
                let mut right = region.clone();
                let delta = (end - region.base) as usize;
                right.base = end;
                right.size = region.end() - end;
                right.data = region.data[delta..].to_vec();
                result.push(right);
            }
        }
        self.regions = result;
        self.regions.sort_by_key(|r| r.base);
        self.rebuild_index();
        Ok(())
    }

    /// Change protection bits; partially covered regions are split.
    pub fn protect(&mut self, base: u32, size: u32, perms: Permissions) -> Result<()> {
        let end =
            base.checked_add(size).ok_or(MemoryError::AddressOverflow { addr: base, len: size })?;
        if !self.regions.iter().any(|r| r.base < end && base < r.end()) {
            return Err(MemoryError::Unmapped { addr: base, access: Access::Write, len: size });
        }
        let mut result: Vec<Region> = Vec::with_capacity(self.regions.len() + 2);
        for region in self.regions.drain(..) {
            if region.end() <= base || region.base >= end {
                result.push(region);
                continue;
            }
            if region.base < base {
                let mut left = region.clone();
                left.size = base - region.base;
                left.data.truncate(left.size as usize);
                result.push(left);
            }
            let mid_base = region.base.max(base);
            let mid_end = region.end().min(end);
            let mut mid = region.clone();
            mid.base = mid_base;
            mid.size = mid_end - mid_base;
            mid.data = region.data[(mid_base - region.base) as usize..(mid_end - region.base) as usize]
                .to_vec();
            mid.perms = perms;
            result.push(mid);
            if region.end() > end {
                let mut right = region.clone();
                let delta = (end - region.base) as usize;
                right.base = end;
                right.size = region.end() - end;
                right.data = region.data[delta..].to_vec();
                result.push(right);
            }
        }
        self.regions = result;
        self.regions.sort_by_key(|r| r.base);
        self.rebuild_index();
        Ok(())
    }

    fn rebuild_index(&mut self) {
        self.page_index.clear();
        for (i, region) in self.regions.iter().enumerate() {
            let mut page = region.base;
            while page < region.end() {
                self.page_index.insert(page, i);
                page += PAGE_SIZE;
            }
        }
    }

    /// Grow a region in place (used by the `brk`/heap implementation).
    pub fn grow_region(&mut self, name: &str, new_end: u32) -> Result<()> {
        let idx = self
            .regions
            .iter()
            .position(|r| r.name == name)
            .ok_or_else(|| MemoryError::Unmapped { addr: 0, access: Access::Write, len: 0 })?;
        let region = &self.regions[idx];
        let new_size = new_end - region.base;
        let new_end_aligned = (region.base + new_size).next_multiple_of(PAGE_SIZE);
        for other in &self.regions {
            if other.base == region.base {
                continue;
            }
            if other.base < new_end_aligned && region.base < other.end() {
                return Err(MemoryError::Overlap {
                    base: region.base,
                    size: new_end_aligned - region.base,
                    existing: other.name.clone(),
                });
            }
        }
        let region = &mut self.regions[idx];
        if new_end_aligned > region.end() {
            region.data.resize((new_end_aligned - region.base) as usize, 0);
            region.size = new_end_aligned - region.base;
        }
        self.rebuild_index();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Accessors
    // ------------------------------------------------------------------

    fn locate(&self, addr: u32, len: u32, access: Access) -> Result<(usize, usize)> {
        let page = addr & !PAGE_MASK;
        let idx = *self.page_index.get(&page).ok_or(MemoryError::Unmapped { addr, access, len })?;
        let region = &self.regions[idx];
        let start = region
            .offset_of(addr)
            .ok_or(MemoryError::Unmapped { addr, access, len })?;
        if start as u64 + len as u64 > region.data.len() as u64 {
            return Err(MemoryError::OutOfRegion {
                addr,
                len,
                region: region.name.clone(),
                region_end: region.end(),
            });
        }
        let allowed = match access {
            Access::Read => region.perms.read,
            Access::Write => region.perms.write,
            Access::Execute => region.perms.execute,
        };
        if !allowed {
            return Err(MemoryError::Denied {
                addr,
                access,
                region: region.name.clone(),
                perms: region.perms,
            });
        }
        Ok((idx, start))
    }

    pub fn read_bytes(&self, addr: u32, len: u32) -> Result<Vec<u8>> {
        let (idx, start) = self.locate(addr, len, Access::Read)?;
        Ok(self.regions[idx].data[start..start + len as usize].to_vec())
    }

    pub fn read_into(&self, addr: u32, out: &mut [u8]) -> Result<()> {
        let len = out.len() as u32;
        let (idx, start) = self.locate(addr, len, Access::Read)?;
        out.copy_from_slice(&self.regions[idx].data[start..start + len as usize]);
        Ok(())
    }

    pub fn write_bytes(&mut self, addr: u32, bytes: &[u8]) -> Result<()> {
        let len = bytes.len() as u32;
        let (idx, start) = self.locate(addr, len, Access::Write)?;
        self.regions[idx].data[start..start + len as usize].copy_from_slice(bytes);
        Ok(())
    }

    /// Write memory with `read-only-image` semantics relaxed: used by the dyld
    /// rebase/bind phases, which must patch `__DATA` before protection applies.
    pub fn poke_bytes(&mut self, addr: u32, bytes: &[u8]) -> Result<()> {
        let len = bytes.len() as u32;
        let (idx, start) = self.locate(addr, len, Access::Read)?;
        self.regions[idx].data[start..start + len as usize].copy_from_slice(bytes);
        Ok(())
    }

    pub fn read_u8(&self, addr: u32) -> Result<u8> {
        Ok(self.read_bytes(addr, 1)?[0])
    }

    pub fn read_u16(&self, addr: u32) -> Result<u16> {
        let b = self.read_bytes(addr, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn read_u32(&self, addr: u32) -> Result<u32> {
        let b = self.read_bytes(addr, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn write_u8(&mut self, addr: u32, v: u8) -> Result<()> {
        self.write_bytes(addr, &[v])
    }

    pub fn write_u16(&mut self, addr: u32, v: u16) -> Result<()> {
        self.write_bytes(addr, &v.to_le_bytes())
    }

    pub fn write_u32(&mut self, addr: u32, v: u32) -> Result<()> {
        self.write_bytes(addr, &v.to_le_bytes())
    }

    /// Read a NUL terminated string, with a hard limit so a missing terminator
    /// cannot run away.
    pub fn read_cstr(&self, addr: u32, max: usize) -> Result<String> {
        let mut out = Vec::new();
        let mut p = addr;
        while out.len() < max {
            let b = match self.read_u8(p) {
                Ok(b) => b,
                Err(_) if !out.is_empty() => break,
                Err(e) => return Err(e),
            };
            if b == 0 {
                break;
            }
            out.push(b);
            p = p.wrapping_add(1);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub fn total_mapped(&self) -> u64 {
        self.regions.iter().map(|r| r.size as u64).sum()
    }

    /// A `xxd`-style dump, handy for `--trace` and post-mortem debugging.
    pub fn hexdump(&self, addr: u32, len: u32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut p = addr;
        while p < addr + len {
            let line_len = (addr + len - p).min(16);
            match self.read_bytes(p, line_len) {
                Ok(bytes) => {
                    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    let ascii: String = bytes
                        .iter()
                        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                        .collect();
                    lines.push(format!("{p:#010x}  {:<47}  {}", hex.join(" "), ascii));
                }
                Err(e) => {
                    lines.push(format!("{p:#010x}  <{e}>"));
                    break;
                }
            }
            p += line_len;
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> AddressSpace {
        let mut m = AddressSpace::new();
        m.map("text", 0x1000, 0x2000, Permissions::RX, RegionKind::Image, &[1, 2, 3, 4]).unwrap();
        m.map("data", 0x4000, 0x1000, Permissions::RW, RegionKind::Image, &[]).unwrap();
        m
    }

    #[test]
    fn read_write_roundtrip() {
        let mut m = space();
        assert_eq!(m.read_u32(0x1000).unwrap(), 0x04030201);
        m.write_u32(0x4000, 0xdead_beef).unwrap();
        assert_eq!(m.read_u32(0x4000).unwrap(), 0xdead_beef);
        assert_eq!(m.read_u16(0x4000).unwrap(), 0xbeef);
    }

    #[test]
    fn permissions_are_enforced() {
        let mut m = space();
        let err = m.write_u32(0x1000, 1).unwrap_err();
        assert!(matches!(err, MemoryError::Denied { .. }), "{err}");
        let mut m2 = space();
        let err = m2.write_u32(0x1004, 1).unwrap_err();
        assert!(err.to_string().contains("permission denied"));
    }

    #[test]
    fn unmapped_access_is_diagnosed_with_the_address() {
        // 0x1234 is inside the test image; the address below is not mapped by
        // `space()` at all.
        let m = space();
        let err = m.read_u32(0x0100_1234).unwrap_err();
        assert!(matches!(err, MemoryError::Unmapped { addr: 0x0100_1234, .. }), "{err}");
        assert!(err.to_string().contains("0x01001234"), "{err}");
    }

    #[test]
    fn pagezero_stays_unmapped() {
        let m = AddressSpace::new();
        assert!(m.read_u32(0).is_err());
    }

    #[test]
    fn cross_region_access_faults() {
        let m = space();
        let err = m.read_bytes(0x2ffc, 8).unwrap_err();
        assert!(matches!(err, MemoryError::OutOfRegion { .. }), "{err}");
    }

    #[test]
    fn overlaps_are_rejected() {
        let mut m = space();
        let err = m
            .map("clash", 0x2000, 0x1000, Permissions::RW, RegionKind::Anonymous, &[])
            .unwrap_err();
        assert!(matches!(err, MemoryError::Overlap { .. }), "{err}");
    }

    #[test]
    fn protect_changes_permissions() {
        let mut m = space();
        assert!(m.read_u32(0x1000).is_ok());
        m.protect(0x1000, 0x2000, Permissions::RW).unwrap();
        assert!(m.write_u32(0x1000, 7).is_ok());
        assert!(m.read_u32(0x1000).unwrap() == 7);
    }

    #[test]
    fn read_cstr_stops_at_nul() {
        let mut m = AddressSpace::new();
        m.map("s", 0x8000, 0x1000, Permissions::RW, RegionKind::Anonymous, b"hi\0there\0").unwrap();
        assert_eq!(m.read_cstr(0x8000, 64).unwrap(), "hi");
        assert_eq!(m.read_cstr(0x8003, 64).unwrap(), "there");
    }

    #[test]
    fn grow_region_extends_into_free_space() {
        let mut m = AddressSpace::new();
        m.map("heap", 0x10_0000, 0x1000, Permissions::RW, RegionKind::Heap, &[]).unwrap();
        m.grow_region("heap", 0x10_0000 + 0x9000).unwrap();
        assert_eq!(m.region_by_name("heap").unwrap().size, 0x9000);
        m.write_u32(0x10_8000, 5).unwrap();
        assert_eq!(m.read_u32(0x10_8000).unwrap(), 5);
    }

    #[test]
    fn hexdump_is_readable() {
        let m = space();
        let lines = m.hexdump(0x1000, 4);
        assert!(lines[0].starts_with("0x00001000  01 02 03 04"));
    }
}
