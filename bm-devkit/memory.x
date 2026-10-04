/* STM32U575CI: 2 MB flash, 768 KB SRAM1-3 contiguous from 0x20000000.
 *
 * No bootloader: the image starts at the base of flash, where bm_protocol
 * links an application built without USE_BOOTLOADER (src/CMakeLists.txt,
 * BOOTLOADER_SIZE 0). Flashing this overwrites an MCUboot bootloader if one
 * is installed.
 *
 * The top 512 bytes of RAM are left out, as in bm_protocol's
 * src/bsp/common/linker/bs_stm32u575.ld (`_noinit_size`), where the C
 * firmware keeps the no-init block that carries a DFU across a reset.
 * src/noinit.rs reads and writes it at fixed addresses.
 */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 2048K
  RAM   : ORIGIN = 0x20000000, LENGTH = 768K - 512
}
