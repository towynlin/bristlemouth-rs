/* The addresses of bm_protocol's bs_stm32u575.ld with a bootloader. */
MEMORY
{
  FLASH (rx) : ORIGIN = 0x0800C000, LENGTH = 0xF2000
  RAM (rwx)  : ORIGIN = 0x20000000, LENGTH = 0xC0000
}
SECTIONS
{
  .mcuboot_sig (NOLOAD) : { . = . + 0x200; } > FLASH
  .isr_vector : { KEEP(*(.isr_vector)) } > FLASH
  .note.sofar.version 0x0800C438 : { KEEP(*(.note.sofar.version)) } > FLASH
  .text : { . = ALIGN(16); *(.text*) } > FLASH
  .data : { *(.data*) } > RAM AT > FLASH
  .bss (NOLOAD) : { *(.bss*) } > RAM
  .noinit 0x200BFE00 (NOLOAD) : { *(.noinit) } > RAM
  /DISCARD/ : { *(.ARM.attributes) *(.comment) }
}
