/*
 * Logging off, as bm_protocol's bootloader builds it: MCUBOOT_LOG_ENABLE is
 * commented out in src/apps/bootloader/CMakeLists.txt:127-129, so
 * mcuboot_logging.h's level is MCUBOOT_LOG_LEVEL_OFF and every macro is empty.
 */
#ifndef BM_MCUBOOT_SYS_MCUBOOT_LOGGING_H
#define BM_MCUBOOT_SYS_MCUBOOT_LOGGING_H

#define MCUBOOT_LOG_LEVEL_OFF 0
#define MCUBOOT_LOG_LEVEL_ERROR 1
#define MCUBOOT_LOG_LEVEL_WARNING 2
#define MCUBOOT_LOG_LEVEL_INFO 3
#define MCUBOOT_LOG_LEVEL_DEBUG 4

#define MCUBOOT_LOG_LEVEL MCUBOOT_LOG_LEVEL_OFF

#define MCUBOOT_LOG_ERR(...)
#define MCUBOOT_LOG_WRN(...)
#define MCUBOOT_LOG_INF(...)
#define MCUBOOT_LOG_DBG(...)

#define MCUBOOT_LOG_MODULE_DECLARE(...)
#define MCUBOOT_LOG_MODULE_REGISTER(...)

#endif
