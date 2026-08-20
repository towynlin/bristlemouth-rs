// Every bm_core header bound by this crate, plus the shim control surface.
//
// Several bm_core headers have no include guard (network/bm_ip.h,
// bcmp/bm_rtc.h, common/device.h, common/timer_callback_handler.h, ...), so
// each must appear exactly once here and never be reachable twice.

// --- T0: pure ---
#include "crc.h"
#include "util.h"
#include "lib_state_machine.h"
#include "device.h"
#include "l2_policy.h"
#include "network_frames.h"

// --- T1: needs the bm_os shim ---
#include "bm_os.h"
#include "aligned_malloc.h"
#include "ll.h"
#include "q.h"
#include "pcap.h"
#include "cb_queue.h"
#include "timer_callback_handler.h"
#include "packet.h"

// --- T2: needs tinycbor ---
#include "configuration.h"
#include "cbor_service_helper.h"
#include "bm_messages_helper.h"
#include "config_cbor_map_srv_reply_msg.h"
#include "config_cbor_map_srv_request_msg.h"
#include "metrics_reply_msg.h"
#include "sensor_header_msg.h"
#include "sys_info_svc_reply_msg.h"
#include "power_info_reply_msg.h"
#include "bm_configs_generic.h"
#include "bm_rtc.h"

// --- the shim's own control surface ---
#include "bm_shim.h"
