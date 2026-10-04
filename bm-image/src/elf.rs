//! An ELF32 little-endian file as `objcopy -O binary --gap-fill` reads one:
//! every section that is allocated, has file contents and is not empty, at
//! its load address.

use crate::Error;

const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u32 = 2;
const PT_LOAD: u32 = 1;
const PHDR_SIZE: usize = 32;
const SHDR_SIZE: usize = 40;

/// Whether `file` starts with the ELF magic.
pub fn is_elf(file: &[u8]) -> bool {
    file.starts_with(b"\x7fELF")
}

/// The loaded sections of `elf` from `base` to the end of the last one, with
/// `fill` between them.
///
/// A section's load address is that of the `PT_LOAD` segment holding its
/// file bytes, so `.data` lands where the startup code copies it from.
///
/// Refuses an ELF with no section starting at `base`, one with a section
/// below it, and one whose sections end more than `max_len` bytes after it.
pub fn flat(elf: &[u8], base: u32, fill: u8, max_len: u64) -> Result<Vec<u8>, Error> {
    let sections = sections(elf)?;
    if let Some(&(addr, _)) = sections.iter().find(|(addr, _)| *addr < base) {
        return Err(Error::SectionBelow { addr, base });
    }
    if !sections.iter().any(|(addr, _)| *addr == base) {
        return Err(Error::NothingAt(base));
    }
    let len = sections
        .iter()
        .map(|(addr, data)| u64::from(addr - base) + data.len() as u64)
        .max()
        .unwrap_or(0);
    if len > max_len {
        return Err(Error::TooLong {
            len,
            limit: max_len,
        });
    }
    let mut out = vec![fill; len as usize];
    let mut end = base;
    for (addr, data) in sections {
        if addr < end {
            return Err(Error::Overlap(addr));
        }
        let at = (addr - base) as usize;
        out[at..at + data.len()].copy_from_slice(data);
        end = addr + data.len() as u32;
    }
    Ok(out)
}

/// Each loaded section's load address and bytes, in address order.
fn sections(elf: &[u8]) -> Result<Vec<(u32, &[u8])>, Error> {
    if !is_elf(elf) {
        return Err(Error::Elf("no ELF magic"));
    }
    if elf.get(4..6) != Some(&[1, 1]) {
        return Err(Error::Elf("not 32-bit little-endian"));
    }
    let segments = table(elf, 28, 42, 44, PHDR_SIZE)?;
    let headers = table(elf, 32, 46, 48, SHDR_SIZE)?;

    let mut out = Vec::new();
    for sh in headers {
        let (kind, flags) = (word(sh, 4), word(sh, 8));
        let (addr, offset, size) = (word(sh, 12), word(sh, 16), word(sh, 20));
        if flags & SHF_ALLOC == 0 || kind == SHT_NOBITS || size == 0 {
            continue;
        }
        let end = u64::from(offset) + u64::from(size);
        let data = elf
            .get(offset as usize..end as usize)
            .ok_or(Error::Elf("a section's bytes are outside the file"))?;
        let load = segments
            .clone()
            .filter(|ph| word(ph, 0) == PT_LOAD)
            .find_map(|ph| {
                let (p_offset, p_paddr, p_filesz) = (word(ph, 4), word(ph, 12), word(ph, 16));
                let holds = offset >= p_offset && end <= u64::from(p_offset) + u64::from(p_filesz);
                holds.then(|| p_paddr.wrapping_add(offset - p_offset))
            })
            .unwrap_or(addr);
        if u64::from(load) + u64::from(size) > u64::from(u32::MAX) + 1 {
            return Err(Error::Elf("a section wraps the address space"));
        }
        out.push((load, data));
    }
    out.sort_by_key(|(addr, _)| *addr);
    Ok(out)
}

/// The entries of the table whose file offset, entry size and count are at
/// `off_at`, `size_at` and `count_at` in the ELF header.
fn table(
    elf: &[u8],
    off_at: usize,
    size_at: usize,
    count_at: usize,
    min_size: usize,
) -> Result<std::slice::ChunksExact<'_, u8>, Error> {
    let header = elf.get(..52).ok_or(Error::Elf("shorter than its header"))?;
    let off = word(header, off_at) as usize;
    let size = usize::from(half(header, size_at));
    let count = usize::from(half(header, count_at));
    if count == 0 {
        return Ok(elf[..0].chunks_exact(min_size));
    }
    if size < min_size {
        return Err(Error::Elf("a header table's entries are too short"));
    }
    let bytes = elf
        .get(off..off.saturating_add(size * count))
        .ok_or(Error::Elf("a header table is outside the file"))?;
    Ok(bytes.chunks_exact(size))
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn half(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &[u8] = include_bytes!("../testdata/app.elf");

    #[test]
    fn rejects_what_is_not_elf32_le() {
        assert_eq!(flat(b"", 0, 0xFF, 16), Err(Error::Elf("no ELF magic")));
        assert_eq!(
            flat(b"\x7fELF\x02\x01", 0, 0xFF, 16),
            Err(Error::Elf("not 32-bit little-endian"))
        );
        assert_eq!(
            flat(b"\x7fELF\x01\x01", 0, 0xFF, 16),
            Err(Error::Elf("shorter than its header"))
        );
        // The section table is at the end of the file.
        assert_eq!(
            flat(&APP[..APP.len() - 1], 0x0800_C200, 0xFF, 0x1000),
            Err(Error::Elf("a header table is outside the file"))
        );
    }

    #[test]
    fn rejects_a_wrong_base() {
        assert_eq!(
            flat(APP, 0x0800_C204, 0xFF, 0x1000),
            Err(Error::SectionBelow {
                addr: 0x0800_C200,
                base: 0x0800_C204
            })
        );
        assert_eq!(
            flat(APP, 0x0800_0000, 0xFF, 0x10_0000),
            Err(Error::NothingAt(0x0800_0000))
        );
    }

    #[test]
    fn rejects_more_than_max_len() {
        let len = flat(APP, 0x0800_C200, 0xFF, 0x1000).unwrap().len() as u64;
        assert!(flat(APP, 0x0800_C200, 0xFF, len).is_ok());
        assert_eq!(
            flat(APP, 0x0800_C200, 0xFF, len - 1),
            Err(Error::TooLong {
                len,
                limit: len - 1
            })
        );
    }
}
