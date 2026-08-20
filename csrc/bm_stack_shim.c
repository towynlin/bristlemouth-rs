// Bring the whole stack up on the capture device.
//
// middleware/bristlemouth.c is bm_core's own version of this, but it calls
// adin2111_network_device() directly, so it only works against the real PHY.
// This mirrors its sequence -- and bm_sbc's runtime, which does the same thing
// for the same reason -- against bm_shim_network_device().

#include "bm_shim.h"

#include "bcmp.h"
#include "bm_config.h"
#include "bm_ip.h"
#include "bm_service.h"
#include "l2.h"
#include "metrics_service.h"
#include "middleware.h"
#include "pubsub.h"
#include "topology.h"

BmErr bm_shim_stack_init(void) {
  NetworkDevice network_device = bm_shim_network_device();

  // Order matters and is taken verbatim from bristlemouth_init: bcmp_init
  // registers a link-change callback that L2 replays immediately, so L2 and
  // the IP layer have to be up before it runs.
  BmErr err = BmOK;
  bm_err_check(err, bm_l2_init(network_device));
  bm_err_check(err, bm_ip_init());
  bm_err_check(err, bcmp_init(network_device));
  bm_err_check(err, topology_init(network_device.trait->num_ports()));
  bm_err_check(err, bm_service_init());
  bm_err_check(err, bm_pubsub_init());
  bm_err_check(err, bm_middleware_init());
#if (bm_metrics_enabled != 0)
  bm_err_check(err, metrics_service_init());
#endif
  return err;
}
