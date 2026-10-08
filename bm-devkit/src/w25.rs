//! The W25Q64JV NOR flash on SPI2, as bm_protocol's `spiflash::W25`
//! (`src/lib/drivers/w25.cpp`) drives it.
//!
//! Generic over `embedded-hal`'s blocking [`SpiDevice`] and [`DelayNs`], and
//! free of embassy and `cortex-m`, so it can be exercised against a simulated
//! part on a host.
//!
//! | Operation | Sequence |
//! |---|---|
//! | [`W25::read`] | wait for `BUSY` clear, then `0x03` and a 24-bit address |
//! | [`W25::erase_sector`] | wait for `BUSY` clear, `0x06`, wait for `WEL` set, `0x20` and address, wait for `WEL` clear |
//! | [`W25::write`] | per 4 KB sector: read it, patch it, erase it, then per 256-byte page: wait for `BUSY` clear, `0x06`, wait for `WEL` set, `0x02` with address and page, wait for `WEL` clear |
//!
//! A write interrupted between erase and program loses the sector, as in the
//! C.

use embedded_hal::delay::DelayNs;
use embedded_hal::spi::{Error as _, ErrorKind, Operation, SpiDevice};

/// Erase unit. `W25_SECTOR_SIZE`.
pub const SECTOR_SIZE: usize = 4096;
/// Program unit. `W25_PAGE_SIZE`.
pub const PAGE_SIZE: usize = 256;
/// `W25_MAX_ADDRESS`. The C asserts `addr + len` stays below it.
pub const MAX_ADDRESS: u32 = 0x7F_FFFF;

const READ_DATA: u8 = 0x03;
const PAGE_PROGRAM: u8 = 0x02;
const READ_STATUS_1: u8 = 0x05;
const WRITE_ENABLE: u8 = 0x06;
const SECTOR_ERASE: u8 = 0x20;

const SR1_BUSY: u8 = 1 << 0;
const SR1_WEL: u8 = 1 << 1;

/// `W25_WRITE_TIMEOUT_MS`: `BUSY` before any command, `WEL` after `0x06` and
/// after a page program.
const WRITE_TIMEOUT_MS: u32 = 15;
/// `W25_SECTOR_ERASE_TIMEOUT_MS`: `WEL` clear after a sector erase.
const SECTOR_ERASE_TIMEOUT_MS: u32 = 400;
/// Delay between status polls. A timeout counts polls, so the time actually
/// waited also includes each status read's SPI transfer.
const POLL_US: u32 = 10;

/// Why an operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum Error {
    /// The SPI transfer failed.
    Spi(ErrorKind),
    /// A status bit did not reach its state in time.
    Timeout,
    /// `addr + len` reaches [`MAX_ADDRESS`], where the C asserts.
    OutOfRange,
}

/// The status-register half of the part: the SPI device and a delay.
struct Bus<SPI, D> {
    spi: SPI,
    delay: D,
}

impl<SPI: SpiDevice, D: DelayNs> Bus<SPI, D> {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Error> {
        self.spi.transaction(ops).map_err(|e| Error::Spi(e.kind()))
    }

    fn command(&mut self, command: u8) -> Result<(), Error> {
        self.transaction(&mut [Operation::Write(&[command])])
    }

    fn status(&mut self) -> Result<u8, Error> {
        let mut status = [0];
        self.transaction(&mut [
            Operation::Write(&[READ_STATUS_1]),
            Operation::Read(&mut status),
        ])?;
        Ok(status[0])
    }

    /// Poll status register 1 until `mask` reads as `set`: `readyToWrite`
    /// (`BUSY` clear) and `checkWEL`.
    fn wait(&mut self, mask: u8, set: bool, timeout_ms: u32) -> Result<(), Error> {
        for _ in 0..=timeout_ms * 1000 / POLL_US {
            if (self.status()? & mask != 0) == set {
                return Ok(());
            }
            self.delay.delay_us(POLL_US);
        }
        Err(Error::Timeout)
    }

    /// `_read`.
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Error> {
        self.wait(SR1_BUSY, false, WRITE_TIMEOUT_MS)?;
        self.transaction(&mut [
            Operation::Write(&header(READ_DATA, addr)),
            Operation::Read(buf),
        ])
    }

    /// `_eraseSector`.
    fn erase_sector(&mut self, addr: u32) -> Result<(), Error> {
        self.wait(SR1_BUSY, false, WRITE_TIMEOUT_MS)?;
        self.command(WRITE_ENABLE)?;
        self.wait(SR1_WEL, true, WRITE_TIMEOUT_MS)?;
        self.transaction(&mut [Operation::Write(&header(SECTOR_ERASE, addr))])?;
        self.wait(SR1_WEL, false, SECTOR_ERASE_TIMEOUT_MS)
    }

    /// One iteration of `_write`'s page loop.
    fn program_page(&mut self, addr: u32, page: &[u8]) -> Result<(), Error> {
        self.wait(SR1_BUSY, false, WRITE_TIMEOUT_MS)?;
        self.command(WRITE_ENABLE)?;
        self.wait(SR1_WEL, true, WRITE_TIMEOUT_MS)?;
        self.transaction(&mut [
            Operation::Write(&header(PAGE_PROGRAM, addr)),
            Operation::Write(page),
        ])?;
        self.wait(SR1_WEL, false, WRITE_TIMEOUT_MS)
    }
}

fn header(command: u8, addr: u32) -> [u8; 4] {
    let [_, a2, a1, a0] = addr.to_be_bytes();
    [command, a2, a1, a0]
}

fn check_range(addr: u32, len: usize) -> Result<(), Error> {
    let end = u32::try_from(len)
        .ok()
        .and_then(|len| addr.checked_add(len));
    match end {
        Some(end) if end < MAX_ADDRESS => Ok(()),
        _ => Err(Error::OutOfRange),
    }
}

/// The part, with the 4 KB buffer a sector write needs. The C allocates that
/// buffer per write; here it lives with the driver, off the caller's stack.
pub struct W25<SPI, D> {
    bus: Bus<SPI, D>,
    sector: [u8; SECTOR_SIZE],
}

impl<SPI: SpiDevice, D: DelayNs> W25<SPI, D> {
    /// The part on `spi`, with `delay` pacing status polls.
    pub const fn new(spi: SPI, delay: D) -> Self {
        Self {
            bus: Bus { spi, delay },
            sector: [0xFF; SECTOR_SIZE],
        }
    }

    /// `W25::read`: `buf.len()` bytes from `addr`, in one transfer.
    ///
    /// # Errors
    ///
    /// As [`Error`]; `buf` may then hold part of the read.
    pub fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Error> {
        check_range(addr, buf.len())?;
        self.bus.read(addr, buf)
    }

    /// `W25::eraseSector`: erase the 4 KB sector at `addr`, which must be
    /// sector-aligned.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfRange`] for an unaligned address, where the C asserts;
    /// otherwise as [`Error`].
    pub fn erase_sector(&mut self, addr: u32) -> Result<(), Error> {
        if !addr.is_multiple_of(SECTOR_SIZE as u32) {
            return Err(Error::OutOfRange);
        }
        check_range(addr, 0)?;
        self.bus.erase_sector(addr)
    }

    /// `W25::write`: store `data` at `addr`, read-modify-erase-program per
    /// sector it touches. The rest of each sector is rewritten with what it
    /// held.
    ///
    /// The C's sector count, `ceil(len / 4096)` plus one when the write ends
    /// inside a later sector than it starts in, is one too many for writes
    /// such as a config image (4359 bytes from a partition's start: three
    /// sectors, not two); it rewrites the extra sector unchanged. This
    /// touches only the sectors `data` overlaps.
    ///
    /// # Errors
    ///
    /// As [`Error`]. Sectors before the failing one are written; the failing
    /// one may be erased.
    pub fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), Error> {
        check_range(addr, data.len())?;
        let mut addr = addr;
        let mut data = data;
        while !data.is_empty() {
            let base = addr - addr % SECTOR_SIZE as u32;
            let start = (addr - base) as usize;
            let n = data.len().min(SECTOR_SIZE - start);

            self.bus.read(base, &mut self.sector)?;
            self.sector[start..start + n].copy_from_slice(&data[..n]);
            self.bus.erase_sector(base)?;
            for (i, page) in self.sector.chunks_exact(PAGE_SIZE).enumerate() {
                self.bus.program_page(base + (i * PAGE_SIZE) as u32, page)?;
            }

            addr += n as u32;
            data = &data[n..];
        }
        Ok(())
    }
}
