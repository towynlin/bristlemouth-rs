// A NetworkDevice that is a capture buffer rather than an Ethernet PHY.
//
// This is the wire boundary: everything bm_core transmits arrives at send()
// and is queued for Rust to read, and bm_shim_rx_inject pushes bytes back up
// the same path the ADIN2111 driver would. A differential fuzz target feeds
// the same input to this and to the Rust port and compares what comes out.

#include "bm_shim.h"

#include "bm_config.h"
#include "network_device.h"

#include <stdlib.h>
#include <string.h>

// Bristlemouth spec: egress ports are 1-15, port 0 means "all ports".
#define SHIM_NUM_PORTS 2
#define MAX_CAPTURED_FRAMES 64

typedef struct {
  uint8_t port;
  uint32_t len;
  uint8_t *data;
} CapturedFrame;

static struct {
  bool enabled;
  bool powered;
  uint16_t enabled_ports_mask; // bit n set => port n+1 enabled

  CapturedFrame frames[MAX_CAPTURED_FRAMES];
  uint32_t frame_count;
  uint32_t frame_head;
  uint32_t dropped; // frames the capture ring had no room for

  NetworkDeviceCallbacks callbacks;
} CTX;

// ---------------------------------------------------------------------------
// NetworkDeviceTrait
// ---------------------------------------------------------------------------

static BmErr shim_send(void *self, uint8_t *data, size_t length, uint8_t port) {
  (void)self;
  if (!data || length == 0) {
    return BmEINVAL;
  }
  if (CTX.frame_count == MAX_CAPTURED_FRAMES) {
    // Losing frames silently would read as "nothing was transmitted", so
    // count it; bm_shim_tx_dropped surfaces it to the test or fuzz target.
    CTX.dropped++;
    return BmENOMEM;
  }
  uint32_t slot = (CTX.frame_head + CTX.frame_count) % MAX_CAPTURED_FRAMES;
  CapturedFrame *frame = &CTX.frames[slot];
  frame->data = (uint8_t *)malloc(length);
  if (!frame->data) {
    return BmENOMEM;
  }
  memcpy(frame->data, data, length);
  frame->len = (uint32_t)length;
  frame->port = port;
  CTX.frame_count++;
  return BmOK;
}

static BmErr shim_enable(void *self) {
  (void)self;
  CTX.enabled = true;
  return BmOK;
}

static BmErr shim_disable(void *self) {
  (void)self;
  CTX.enabled = false;
  return BmOK;
}

static BmErr shim_enable_port(void *self, uint8_t port_num) {
  (void)self;
  if (port_num == 0 || port_num > SHIM_NUM_PORTS) {
    return BmEINVAL;
  }
  CTX.enabled_ports_mask |= (uint16_t)(1U << (port_num - 1));
  return BmOK;
}

static BmErr shim_disable_port(void *self, uint8_t port_num) {
  (void)self;
  if (port_num == 0 || port_num > SHIM_NUM_PORTS) {
    return BmEINVAL;
  }
  CTX.enabled_ports_mask &= (uint16_t)~(1U << (port_num - 1));
  return BmOK;
}

static BmErr shim_retry_negotiation(void *self, uint8_t port_index,
                                    bool *renegotiated) {
  (void)self;
  (void)port_index;
  if (renegotiated) {
    // Never renegotiate: a link that flaps on its own would make a fuzz
    // input's outcome depend on how many ticks the harness happened to run.
    *renegotiated = false;
  }
  return BmOK;
}

static uint8_t shim_num_ports(void) { return SHIM_NUM_PORTS; }

static BmErr shim_port_stats(void *self, uint8_t port_index, void *stats) {
  (void)self;
  (void)port_index;
  (void)stats;
  return BmENODEV;
}

static BmErr shim_handle_interrupt(void *self) {
  (void)self;
  return BmOK;
}

static const NetworkDeviceTrait SHIM_TRAIT = {
    .send = shim_send,
    .enable = shim_enable,
    .disable = shim_disable,
    .enable_port = shim_enable_port,
    .disable_port = shim_disable_port,
    .retry_negotiation = shim_retry_negotiation,
    .num_ports = shim_num_ports,
    .port_stats = shim_port_stats,
    .handle_interrupt = shim_handle_interrupt,
};

NetworkDevice bm_shim_network_device(void) {
  // bm_l2_init writes its own receive and link_change handlers into
  // callbacks, so the struct has to outlive the call.
  NetworkDevice device = {
      .self = NULL,
      .trait = &SHIM_TRAIT,
      .callbacks = &CTX.callbacks,
  };
  return device;
}

// ---------------------------------------------------------------------------
// Rust-facing capture and injection
// ---------------------------------------------------------------------------

BmErr bm_shim_rx_inject(uint8_t port, const uint8_t *data, uint32_t len) {
  if (!data || len == 0) {
    return BmEINVAL;
  }
  if (!CTX.callbacks.receive) {
    return BmENODEV; // bm_l2_init has not run
  }
  // The driver hands L2 a mutable buffer it is free to rewrite in place (see
  // bm_l2_policy_rx_apply), so never pass the caller's memory straight down.
  uint8_t *copy = (uint8_t *)malloc(len);
  if (!copy) {
    return BmENOMEM;
  }
  memcpy(copy, data, len);
  CTX.callbacks.receive(port, copy, len);
  free(copy);
  return BmOK;
}

void bm_shim_link_change(uint8_t port, bool up) {
  if (CTX.callbacks.link_change) {
    CTX.callbacks.link_change(port, up);
  }
}

uint32_t bm_shim_tx_count(void) { return CTX.frame_count; }
uint32_t bm_shim_tx_dropped(void) { return CTX.dropped; }

int32_t bm_shim_tx_pop(uint8_t *buf, uint32_t buf_len, uint8_t *port) {
  if (CTX.frame_count == 0) {
    return -1;
  }
  CapturedFrame *frame = &CTX.frames[CTX.frame_head];
  int32_t len = (int32_t)frame->len;
  if (buf && buf_len) {
    uint32_t n = frame->len < buf_len ? frame->len : buf_len;
    memcpy(buf, frame->data, n);
  }
  if (port) {
    *port = frame->port;
  }
  free(frame->data);
  memset(frame, 0, sizeof(*frame));
  CTX.frame_head = (CTX.frame_head + 1) % MAX_CAPTURED_FRAMES;
  CTX.frame_count--;
  // The full length, even when truncated, so a caller can tell it needs more
  // room rather than silently accepting a short frame.
  return len;
}

void bm_shim_network_device_reset(void) {
  while (CTX.frame_count > 0) {
    bm_shim_tx_pop(NULL, 0, NULL);
  }
  memset(&CTX, 0, sizeof(CTX));
}
