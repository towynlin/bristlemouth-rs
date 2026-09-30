//! The config partitions in the W25's first 36 KB, as bm_protocol's
//! `bm_config_wrapper.cpp` maps them through `NvmPartition`.

use bm_stack::ConfigStorage;
use bm_wire::configuration::Partition;
use embedded_hal::delay::DelayNs;
use embedded_hal::spi::SpiDevice;

use crate::w25::W25;

/// Size of each config partition. `*_CONFIG_FLASH_SIZE_BYTES`.
pub const PARTITION_SIZE: u32 = 10 * 1024;

/// A partition's first byte in flash, from
/// `src/lib/common/external_flash_partitions.c`.
#[must_use]
pub const fn partition_offset(partition: Partition) -> u32 {
    match partition {
        Partition::Hardware => 0x0000,
        Partition::System => 0x3000,
        Partition::User => 0x6000,
    }
}

/// [`ConfigStorage`] over a [`W25`].
///
/// `timeout_ms` is not used: the C spends it waiting for the driver's mutex,
/// and this store owns the part.
pub struct FlashConfigStorage<SPI, D> {
    flash: W25<SPI, D>,
}

impl<SPI: SpiDevice, D: DelayNs> FlashConfigStorage<SPI, D> {
    /// The config partitions on `flash`.
    pub const fn new(flash: W25<SPI, D>) -> Self {
        Self { flash }
    }

    /// The part, for reads and writes outside the config partitions.
    pub fn flash(&mut self) -> &mut W25<SPI, D> {
        &mut self.flash
    }

    /// `NvmPartition`'s address: the partition's offset plus `offset`, if
    /// `offset + len < PARTITION_SIZE`, where the C asserts.
    fn address(partition: Partition, offset: u32, len: usize) -> Option<u32> {
        let end = offset.checked_add(u32::try_from(len).ok()?)?;
        (end < PARTITION_SIZE).then(|| partition_offset(partition) + offset)
    }
}

impl<SPI: SpiDevice, D: DelayNs> ConfigStorage for FlashConfigStorage<SPI, D> {
    fn read(
        &mut self,
        partition: Partition,
        offset: u32,
        buf: &mut [u8],
        _timeout_ms: u32,
    ) -> bool {
        let Some(addr) = Self::address(partition, offset, buf.len()) else {
            return false;
        };
        match self.flash.read(addr, buf) {
            Ok(()) => true,
            Err(e) => {
                defmt::warn!(
                    "config read at {=u32:#x}: {}",
                    addr,
                    defmt::Debug2Format(&e)
                );
                false
            }
        }
    }

    fn write(&mut self, partition: Partition, offset: u32, buf: &[u8], _timeout_ms: u32) -> bool {
        let Some(addr) = Self::address(partition, offset, buf.len()) else {
            return false;
        };
        match self.flash.write(addr, buf) {
            Ok(()) => true,
            Err(e) => {
                defmt::warn!(
                    "config write at {=u32:#x}: {}",
                    addr,
                    defmt::Debug2Format(&e)
                );
                false
            }
        }
    }

    /// `bm_config_reset`: `resetSystem(RESET_REASON_CONFIG)`, less the reset
    /// reason, which the C keeps in no-init RAM this crate does not place.
    fn reset(&mut self) {
        cortex_m::peripheral::SCB::sys_reset();
    }
}
