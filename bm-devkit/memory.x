/* STM32U575CI: 2 MB flash, 768 KB SRAM1-3 contiguous from 0x20000000.
 *
 * Slot 1 behind bm_protocol's MCUboot bootloader (src/CMakeLists.txt:113-124):
 * the slot starts at 0x0800C000, the image header takes 0x200, and the
 * bootloader takes SP and PC from the vector table that follows it
 * (src/apps/bootloader/app_main.c, boot_port_startup). The length is what
 * `bm-image` accepts for a signed image: 0xF07B0 less the header and the
 * signed TLV area's 144 bytes.
 *
 * The top 512 bytes of RAM are left out, as in bm_protocol's
 * src/bsp/common/linker/bs_stm32u575.ld (`_noinit_size`), where the C
 * firmware keeps the no-init block that carries a DFU across a reset.
 * src/noinit.rs reads and writes it at fixed addresses.
 */
MEMORY
{
  FLASH : ORIGIN = 0x0800C200, LENGTH = 0xF0520
  RAM   : ORIGIN = 0x20000000, LENGTH = 768K - 512
}
