/*
 * The flash map API bootutil calls. The declarations are MCUboot's porting
 * contract (docs/PORTING.md), with the struct layout of bm_protocol's
 * src/lib/mcuboot/include/flash_map_backend/flash_map_backend.h:36-79.
 * bm_mcuboot.c implements it over RAM.
 */
#ifndef BM_MCUBOOT_SYS_FLASH_MAP_BACKEND_H
#define BM_MCUBOOT_SYS_FLASH_MAP_BACKEND_H

#include <stddef.h>
#include <stdint.h>

struct flash_area {
    uint8_t fa_id;
    uint8_t fa_device_id;
    uint16_t pad16;
    /* From the start of the device, not of the address space. */
    uint32_t fa_off;
    uint32_t fa_size;
};

struct flash_sector {
    /* From the start of the area. */
    uint32_t fs_off;
    uint32_t fs_size;
};

int flash_area_open(uint8_t id, const struct flash_area **area);
void flash_area_close(const struct flash_area *area);
int flash_area_read(const struct flash_area *area, uint32_t off, void *dst,
                    uint32_t len);
int flash_area_write(const struct flash_area *area, uint32_t off,
                     const void *src, uint32_t len);
int flash_area_erase(const struct flash_area *area, uint32_t off,
                     uint32_t len);
size_t flash_area_align(const struct flash_area *area);
uint8_t flash_area_erased_val(const struct flash_area *area);
int flash_area_get_sectors(int fa_id, uint32_t *count,
                           struct flash_sector *sectors);
int flash_area_id_from_multi_image_slot(int image_index, int slot);
int flash_area_id_to_multi_image_slot(int image_index, int area_id);
int flash_area_id_from_image_slot(int slot);
uint8_t flash_area_get_device_id(const struct flash_area *area);

static inline uint32_t flash_area_get_off(const struct flash_area *area) {
    return area->fa_off;
}

static inline uint32_t flash_area_get_size(const struct flash_area *area) {
    return area->fa_size;
}

static inline uint8_t flash_area_get_id(const struct flash_area *area) {
    return area->fa_id;
}

static inline uint32_t flash_sector_get_off(const struct flash_sector *sector) {
    return sector->fs_off;
}

static inline uint32_t flash_sector_get_size(const struct flash_sector *sector) {
    return sector->fs_size;
}

#endif
