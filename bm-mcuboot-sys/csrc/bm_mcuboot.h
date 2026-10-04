/*
 * What src/lib.rs calls. build.rs compiles this crate's C twice; the signing
 * build's copy of every symbol carries an `ed25519_` prefix.
 */
#ifndef BM_MCUBOOT_H
#define BM_MCUBOOT_H

#include <stdint.h>

/* The flash map: bm_protocol src/CMakeLists.txt:113-124, as offsets from
 * FLASH_START in the order of src/lib/mcuboot/port_flash.c:25-28. */
#define BM_MCUBOOT_PAGE_SIZE 0x2000u
#define BM_MCUBOOT_BOOTLOADER_SIZE 0xC000u
#define BM_MCUBOOT_APP_SIZE 0xF2000u
#define BM_MCUBOOT_SCRATCH_SIZE 0x10000u
#define BM_MCUBOOT_FLASH_SIZE \
    (BM_MCUBOOT_BOOTLOADER_SIZE + 2 * BM_MCUBOOT_APP_SIZE + BM_MCUBOOT_SCRATCH_SIZE)

/* Returned by a call that reached a failed bootutil assert, where the
 * bootloader resets. */
#define BM_MCUBOOT_ASSERTED INT32_MIN

/* Every byte of the device to 0xFF. */
void bm_mcuboot_flash_reset(void);

/* Whole-area access by area id (sysflash.h), with no flash semantics: `store`
 * overwrites. 0, or -1 for an unknown id or a range outside the area. */
int bm_mcuboot_flash_load(uint8_t id, uint32_t off, uint8_t *dst, uint32_t len);
int bm_mcuboot_flash_store(uint8_t id, uint32_t off, const uint8_t *src,
                           uint32_t len);

/* bootutil's functions, unchanged but for the assert escape. */
int32_t bm_mcuboot_set_pending(int permanent);
int32_t bm_mcuboot_set_confirmed(void);
int32_t bm_mcuboot_swap_type(void);

/* boot_go. On 0, `image_off`, `flash_dev_id` and the 32-byte `header` are
 * boot_rsp's br_image_off, br_flash_dev_id and *br_hdr; otherwise they are
 * left alone. */
int32_t bm_mcuboot_boot_go(uint32_t *image_off, uint8_t *flash_dev_id,
                           uint8_t *header);

/* tinycrypt's SHA-256, the one image_validate.c hashes an image with. */
void bm_mcuboot_sha256(const uint8_t *data, uint32_t len, uint8_t *digest);

#endif
