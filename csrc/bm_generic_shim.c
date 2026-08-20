// The remaining integrator hooks bm_core declares but does not define:
// bm_configs_generic.h (NVM), bm_rtc.h (wall clock), bm_dfu_generic.h (flash).
//
// All three are backed by RAM here, so a fuzz iteration touches no device and
// no real time. See csrc/bm_shim.h for the contract.

#include "bm_shim.h"

#include "bm_config.h"
#include "bm_configs_generic.h"
#include "bm_dfu_generic.h"
#include "bm_rtc.h"

#include <stdlib.h>
#include <string.h>

// Large enough for a full ConfigPartition (see bcmp/configuration.h), rounded
// up so a partition write near the end still fits.
#define PARTITION_BYTES 8192
#define DFU_FLASH_BYTES (256 * 1024)

static struct {
  uint8_t partitions[BM_CFG_PARTITION_COUNT][PARTITION_BYTES];
  RtcTimeAndDate rtc;
  bool rtc_set;
  uint8_t flash[DFU_FLASH_BYTES];
} CTX;

void bm_shim_generic_reset(void) { memset(&CTX, 0, sizeof(CTX)); }

// ---------------------------------------------------------------------------
// Config storage
// ---------------------------------------------------------------------------

static bool partition_range_ok(BmConfigPartition partition, uint32_t offset,
                               size_t length) {
  return partition < BM_CFG_PARTITION_COUNT &&
         (uint64_t)offset + length <= PARTITION_BYTES;
}

bool bm_config_read(BmConfigPartition partition, uint32_t offset,
                    uint8_t *buffer, size_t length, uint32_t timeout_ms) {
  (void)timeout_ms;
  if (!buffer || !partition_range_ok(partition, offset, length)) {
    return false;
  }
  memcpy(buffer, &CTX.partitions[partition][offset], length);
  return true;
}

bool bm_config_write(BmConfigPartition partition, uint32_t offset,
                     uint8_t *buffer, size_t length, uint32_t timeout_ms) {
  (void)timeout_ms;
  if (!buffer || !partition_range_ok(partition, offset, length)) {
    return false;
  }
  memcpy(&CTX.partitions[partition][offset], buffer, length);
  return true;
}

void bm_config_reset(void) { memset(CTX.partitions, 0, sizeof(CTX.partitions)); }

// ---------------------------------------------------------------------------
// RTC — a settable value, never the host clock
// ---------------------------------------------------------------------------

BmErr bm_rtc_set(const RtcTimeAndDate *time_and_date) {
  if (!time_and_date) {
    return BmEINVAL;
  }
  CTX.rtc = *time_and_date;
  CTX.rtc_set = true;
  return BmOK;
}

BmErr bm_rtc_get(RtcTimeAndDate *time_and_date) {
  if (!time_and_date) {
    return BmEINVAL;
  }
  if (!CTX.rtc_set) {
    return BmENODATA;
  }
  *time_and_date = CTX.rtc;
  return BmOK;
}

uint64_t bm_rtc_get_micro_seconds(RtcTimeAndDate *time_and_date) {
  if (!time_and_date) {
    return 0;
  }
  uint64_t seconds = utc_from_date_time(
      time_and_date->year, time_and_date->month, time_and_date->day,
      time_and_date->hour, time_and_date->minute, time_and_date->second);
  return seconds * 1000000ULL + (uint64_t)time_and_date->ms * 1000ULL;
}

// ---------------------------------------------------------------------------
// DFU — a RAM buffer standing in for the update slot
// ---------------------------------------------------------------------------

// The flash "area" handle is just the buffer itself; nothing dereferences it
// except the functions below.
static const void *const FLASH_AREA = (const void *)&CTX.flash;

BmErr bm_dfu_client_set_confirmed(void) { return BmOK; }
BmErr bm_dfu_client_set_pending_and_reset(void) { return BmOK; }
BmErr bm_dfu_client_fail_update_and_reset(void) { return BmOK; }

BmErr bm_dfu_client_flash_area_open(const void **flash_area) {
  if (!flash_area) {
    return BmEINVAL;
  }
  *flash_area = FLASH_AREA;
  return BmOK;
}

BmErr bm_dfu_client_flash_area_close(const void *flash_area) {
  return flash_area == FLASH_AREA ? BmOK : BmEINVAL;
}

BmErr bm_dfu_client_flash_area_write(const void *flash_area, uint32_t off,
                                     const void *src, uint32_t len) {
  if (flash_area != FLASH_AREA || !src ||
      (uint64_t)off + len > DFU_FLASH_BYTES) {
    return BmEINVAL;
  }
  memcpy(&CTX.flash[off], src, len);
  return BmOK;
}

BmErr bm_dfu_client_flash_area_erase(const void *flash_area, uint32_t off,
                                     uint32_t len) {
  if (flash_area != FLASH_AREA || (uint64_t)off + len > DFU_FLASH_BYTES) {
    return BmEINVAL;
  }
  memset(&CTX.flash[off], 0xFF, len); // erased flash reads as ones
  return BmOK;
}

uint32_t bm_dfu_client_flash_area_get_size(const void *flash_area) {
  return flash_area == FLASH_AREA ? DFU_FLASH_BYTES : 0;
}

BmErr bm_dfu_host_get_chunk(uint32_t offset, uint8_t *buffer, size_t len,
                            uint32_t timeouts) {
  (void)timeouts;
  if (!buffer || (uint64_t)offset + len > DFU_FLASH_BYTES) {
    return BmEINVAL;
  }
  memcpy(buffer, &CTX.flash[offset], len);
  return BmOK;
}

void bm_dfu_core_lpm_peripheral_active(void) {}
void bm_dfu_core_lpm_peripheral_inactive(void) {}
