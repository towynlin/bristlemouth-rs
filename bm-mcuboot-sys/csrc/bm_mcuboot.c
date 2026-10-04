/*
 * MCUboot's flash map over a RAM array, and the entry points src/lib.rs calls.
 * The flash functions follow bm_protocol's src/lib/mcuboot/port_flash.c; line
 * numbers below are that file's.
 */
#include <setjmp.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

#include "bm_mcuboot.h"
#include "bootutil/bootutil.h"
#include "bootutil/bootutil_public.h"
#include "bootutil/image.h"
#include "bootutil/sign_key.h"
#include "flash_map_backend/flash_map_backend.h"
#include "mcuboot_config/mcuboot_config.h"
#include "sysflash/sysflash.h"
#include "tinycrypt/sha256.h"

#define ERASED 0xFF

static uint8_t s_flash[BM_MCUBOOT_FLASH_SIZE];

/* :30-56 */
static const struct flash_area s_areas[] = {
    {
        .fa_id = FLASH_AREA_BOOTLOADER,
        .fa_device_id = FLASH_DEVICE_INTERNAL_FLASH,
        .fa_off = 0,
        .fa_size = BM_MCUBOOT_BOOTLOADER_SIZE,
    },
    {
        .fa_id = FLASH_AREA_IMAGE_PRIMARY(0),
        .fa_device_id = FLASH_DEVICE_INTERNAL_FLASH,
        .fa_off = BM_MCUBOOT_BOOTLOADER_SIZE,
        .fa_size = BM_MCUBOOT_APP_SIZE,
    },
    {
        .fa_id = FLASH_AREA_IMAGE_SECONDARY(0),
        .fa_device_id = FLASH_DEVICE_INTERNAL_FLASH,
        .fa_off = BM_MCUBOOT_BOOTLOADER_SIZE + BM_MCUBOOT_APP_SIZE,
        .fa_size = BM_MCUBOOT_APP_SIZE,
    },
    {
        .fa_id = FLASH_AREA_IMAGE_SCRATCH,
        .fa_device_id = FLASH_DEVICE_INTERNAL_FLASH,
        .fa_off = BM_MCUBOOT_BOOTLOADER_SIZE + 2 * BM_MCUBOOT_APP_SIZE,
        .fa_size = BM_MCUBOOT_SCRATCH_SIZE,
    },
};

static const struct flash_area *lookup(int id) {
    for (size_t i = 0; i < sizeof(s_areas) / sizeof(s_areas[0]); i++) {
        if (s_areas[i].fa_id == id) {
            return &s_areas[i];
        }
    }
    return NULL;
}

static bool in_area(const struct flash_area *area, uint32_t off, uint32_t len) {
    return off <= area->fa_size && len <= area->fa_size - off;
}

/* :77-83 */
int flash_area_open(uint8_t id, const struct flash_area **area) {
    *area = lookup(id);
    return *area != NULL ? 0 : -1;
}

void flash_area_close(const struct flash_area *area) { (void)area; }

/* :90-107 */
int flash_area_read(const struct flash_area *area, uint32_t off, void *dst,
                    uint32_t len) {
    if (!in_area(area, off, len)) {
        return -1;
    }
    memcpy(dst, &s_flash[area->fa_off + off], len);
    return 0;
}

/* :110-137. flashWrite is modelled as NOR programming, which only clears
 * bits; the read-back comparison is port_flash.c's MCUBOOT_VERIFY_WE, so a
 * write over bytes that are not erased fails unless it leaves them equal to
 * `src`. */
int flash_area_write(const struct flash_area *area, uint32_t off,
                     const void *src, uint32_t len) {
    if (!in_area(area, off, len)) {
        return -1;
    }
    uint8_t *dst = &s_flash[area->fa_off + off];
    const uint8_t *bytes = src;
    for (uint32_t i = 0; i < len; i++) {
        dst[i] &= bytes[i];
    }
    return memcmp(dst, src, len) == 0 ? 0 : -1;
}

/* :140-170. Whole pages only. The range check is this file's: port_flash.c
 * has none on erase. */
int flash_area_erase(const struct flash_area *area, uint32_t off,
                     uint32_t len) {
    if ((len % BM_MCUBOOT_PAGE_SIZE) != 0 || (off % BM_MCUBOOT_PAGE_SIZE) != 0) {
        return -1;
    }
    if (!in_area(area, off, len)) {
        return -1;
    }
    memset(&s_flash[area->fa_off + off], ERASED, len);
    return 0;
}

/* :173-176 */
size_t flash_area_align(const struct flash_area *area) {
    (void)area;
    return MCUBOOT_BOOT_MAX_ALIGN;
}

/* :179-182 */
uint8_t flash_area_erased_val(const struct flash_area *area) {
    (void)area;
    return ERASED;
}

/* :185-204 */
int flash_area_get_sectors(int fa_id, uint32_t *count,
                           struct flash_sector *sectors) {
    const struct flash_area *area = lookup(fa_id);
    if (area == NULL) {
        return -1;
    }
    uint32_t total = 0;
    for (uint32_t off = 0; off < area->fa_size; off += BM_MCUBOOT_PAGE_SIZE) {
        sectors[total].fs_off = off;
        sectors[total].fs_size = BM_MCUBOOT_PAGE_SIZE;
        total++;
    }
    *count = total;
    return 0;
}

/* :209-221 */
int flash_area_id_from_multi_image_slot(int image_index, int slot) {
    (void)image_index;
    switch (slot) {
    case 0:
        return FLASH_AREA_IMAGE_PRIMARY(image_index);
    case 1:
        return FLASH_AREA_IMAGE_SECONDARY(image_index);
    }
    return -1;
}

/* :226-235 */
int flash_area_id_to_multi_image_slot(int image_index, int area_id) {
    (void)image_index;
    if (area_id == FLASH_AREA_IMAGE_PRIMARY(image_index)) {
        return 0;
    }
    if (area_id == FLASH_AREA_IMAGE_SECONDARY(image_index)) {
        return 1;
    }
    return -1;
}

/* :237-239 */
int flash_area_id_from_image_slot(int slot) {
    return flash_area_id_from_multi_image_slot(0, slot);
}

/* :241-245 */
uint8_t flash_area_get_device_id(const struct flash_area *area) {
    (void)area;
    return 0;
}

/* The key table, as bm_protocol's src/apps/bootloader/keys.c:4-14, over
 * test_ed25519_pub_key.c. */
#if defined(MCUBOOT_SIGN_ED25519)
extern const unsigned char ed25519_pub_key[];
extern const unsigned int ed25519_pub_key_len;
const struct bootutil_key bootutil_keys[] = {
    {
        .key = ed25519_pub_key,
        .len = &ed25519_pub_key_len,
    },
};
const int bootutil_key_cnt = 1;
#endif

/* The assert escape. Only the bm_mcuboot_* entry points below arm it. */
static jmp_buf s_assert_jmp;
static bool s_armed;

void bm_mcuboot_assert_fail(void) {
    if (!s_armed) {
        abort();
    }
    s_armed = false;
    longjmp(s_assert_jmp, 1);
}

#define GUARDED(expr)                   \
    do {                                \
        if (setjmp(s_assert_jmp) != 0) { \
            return BM_MCUBOOT_ASSERTED; \
        }                               \
        s_armed = true;                 \
        int32_t guarded_rc = (expr);    \
        s_armed = false;                \
        return guarded_rc;              \
    } while (0)

void bm_mcuboot_flash_reset(void) { memset(s_flash, ERASED, sizeof(s_flash)); }

int bm_mcuboot_flash_load(uint8_t id, uint32_t off, uint8_t *dst, uint32_t len) {
    const struct flash_area *area = lookup(id);
    if (area == NULL || !in_area(area, off, len)) {
        return -1;
    }
    memcpy(dst, &s_flash[area->fa_off + off], len);
    return 0;
}

int bm_mcuboot_flash_store(uint8_t id, uint32_t off, const uint8_t *src,
                           uint32_t len) {
    const struct flash_area *area = lookup(id);
    if (area == NULL || !in_area(area, off, len)) {
        return -1;
    }
    memcpy(&s_flash[area->fa_off + off], src, len);
    return 0;
}

int32_t bm_mcuboot_set_pending(int permanent) {
    GUARDED(boot_set_pending(permanent));
}

int32_t bm_mcuboot_set_confirmed(void) { GUARDED(boot_set_confirmed()); }

int32_t bm_mcuboot_swap_type(void) { GUARDED(boot_swap_type()); }

static int32_t go(uint32_t *image_off, uint8_t *flash_dev_id, uint8_t *header) {
    _Static_assert(sizeof(struct image_header) == 32, "image_header size");
    struct boot_rsp rsp = {0};
    int32_t rc = fih_int_decode(boot_go(&rsp));
    if (rc == 0) {
        *image_off = rsp.br_image_off;
        *flash_dev_id = rsp.br_flash_dev_id;
        memcpy(header, rsp.br_hdr, sizeof(struct image_header));
    }
    return rc;
}

int32_t bm_mcuboot_boot_go(uint32_t *image_off, uint8_t *flash_dev_id,
                           uint8_t *header) {
    GUARDED(go(image_off, flash_dev_id, header));
}

void bm_mcuboot_sha256(const uint8_t *data, uint32_t len, uint8_t *digest) {
    struct tc_sha256_state_struct state;
    tc_sha256_init(&state);
    tc_sha256_update(&state, data, len);
    tc_sha256_final(digest, &state);
}

/* asn1parse.c's mbedtls_asn1_get_mpi references this and nothing calls
 * either; bm_protocol's link discards them with --gc-sections. Defined so
 * that the link does not depend on that. */
int mbedtls_mpi_read_binary(void *x, const unsigned char *buf, size_t len);
int mbedtls_mpi_read_binary(void *x, const unsigned char *buf, size_t len) {
    (void)x;
    (void)buf;
    (void)len;
    abort();
}
