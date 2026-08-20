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

// --- T3: the wire path ---
#include "bm_ip.h"
#include "network_device.h"
#include "l2.h"
#include "bcmp.h"
#include "messages.h"
#include "messages/heartbeat.h"
#include "messages/info.h"
#include "messages/neighbors.h"
#include "messages/ping.h"
#include "messages/time.h"
#include "messages/resource_discovery.h"
#include "messages/config.h"
#include "middleware.h"
#include "pubsub.h"
#include "bm_service.h"
#include "bm_service_request.h"
#include "bm_service_common.h"
#include "service.h"
#include "echo_service.h"
#include "sys_info_service.h"
#include "power_info_service.h"
#include "metrics_service.h"
#include "config_cbor_map_service.h"
#include "topology.h"
#include "spotter.h"
#include "file_ops.h"

// --- T4: DFU and the top-level entry point ---
#include "dfu.h"
#include "dfu_client.h"
#include "dfu_host.h"
#include "dfu_message_structs.h"
#include "bm_dfu_generic.h"
#include "bm_mavlink.h"

// --- the shim's own control surface ---
#include "bm_shim.h"
