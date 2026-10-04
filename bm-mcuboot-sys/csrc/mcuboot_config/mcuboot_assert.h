/*
 * bm_protocol's assert in the bootloader resets the MCU
 * (src/lib/mcuboot/include/mcuboot_config/mcuboot_assert.h:21-33,
 * port_misc.c:22-36). Here it leaves the bm_mcuboot_* call that reached it,
 * which returns BM_MCUBOOT_ASSERTED.
 */
#ifndef BM_MCUBOOT_SYS_MCUBOOT_ASSERT_H
#define BM_MCUBOOT_SYS_MCUBOOT_ASSERT_H

#ifdef assert
#undef assert
#endif

void bm_mcuboot_assert_fail(void) __attribute__((noreturn));

#define assert(exp)                   \
    do {                              \
        if (!(exp)) {                 \
            bm_mcuboot_assert_fail(); \
        }                             \
    } while (0)

#endif
