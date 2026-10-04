/*
 * MCUboot settings, as bm_protocol's bootloader builds it. Line numbers are
 * bm_protocol's src/lib/mcuboot/include/mcuboot_config/mcuboot_config.h.
 *
 * Not set, as there: an upgrade mode (so swap with scratch), encryption,
 * a FIH profile, serial recovery, downgrade prevention.
 */
#ifndef BM_MCUBOOT_SYS_MCUBOOT_CONFIG_H
#define BM_MCUBOOT_SYS_MCUBOOT_CONFIG_H

/* :30-32. build.rs defines CONFIG_BOOT_SIGN_ED25519 for the signing build,
 * as src/apps/bootloader/CMakeLists.txt:132-134 does under SIGN_IMAGES. */
#ifdef CONFIG_BOOT_SIGN_ED25519
#define MCUBOOT_SIGN_ED25519
#endif

/* :79 */
#define MCUBOOT_USE_TINYCRYPT
/* :86 */
#define MCUBOOT_VALIDATE_PRIMARY_SLOT
/* :94 */
#define MCUBOOT_USE_FLASH_AREA_GET_SECTORS
/* :98 */
#define MCUBOOT_MAX_IMG_SECTORS 121
/* :102 */
#define MCUBOOT_IMAGE_NUMBER 1
/* :133 */
#define CONFIG_MCUBOOT 1
/* :143. mcuboot_assert.h here leaves boot_go rather than resetting. */
#define MCUBOOT_HAVE_ASSERT_H 1
/* :156. The bootloader reloads the IWDG; there is none here. */
#define MCUBOOT_WATCHDOG_FEED() \
    do {                        \
    } while (0)
/* :175 */
#define MCUBOOT_PERUSER_MGMT_GROUP_ENABLED 0
/* :180 */
#define MCUBOOT_CPU_IDLE() \
    do {                   \
    } while (0)

#endif
