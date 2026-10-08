# Services todo

> **Complete and closed. There is no work here.** Every card has landed and
> on a bench a Bridge's samplers listed `hello_world` with its correct
> `sys_config_crc`. Do not add cards, pick work from this file, or edit it as
> part of a card. It is kept as documentation: the sections below record what
> was built, the API shapes, and the reasons for decisions. "Working a card"
> and the "Order" section describe the process the plan followed and are
> historical.

The port of bm_core's service layer and built-in services, as
dependency-ordered cards sized for one agent each. Same card format as
`docs/history/bcmp-port-todo.md`, whose "The shared contract" applies in full; the
"Services contract" below adds to it.

| In scope | C source |
|---|---|
| Service table and dispatch | `middleware/bm_service.c` |
| Service requests | `middleware/bm_service_request.c` |
| Config map builder | `middleware/cbor_service_helper.c` |
| echo, sys_info, config_map, power_info, metrics | `middleware/*_service.c`, `common/cb_queue.c` |
| Their bodies | `bm_common_messages/{sys_info_svc_reply,config_cbor_map_srv_request,config_cbor_map_srv_reply,power_info_reply,metrics_reply}_msg.c`, `bm_messages_helper.c` |

Out of scope: `middleware/bm_mavlink.c` (no pub/sub; not a service);
`drivers/adin2111/adin2111_network_metrics.c` (needs per-port counters
`embassy-net-adin1110` does not expose; add a card when it does).

Why: a Bridge's `topology_sampler.cpp` and `sensorController.cpp` request
`sys_info` and `config_map` from every node, and `metrics_sampler.cpp`
requests `metrics` (bm_protocol `src/apps/bridge/`). A Rust node now answers
all three.

## Working a card

Before starting:

1. Pick a card whose **Blocked by** is "nothing" and that has no
   **Taken** line. Add `**Taken:** <branch>` under its title as the first
   commit of the branch, so parallel agents do not collide. If the card turns
   out to be two, split it here in that commit.
2. Read "Services contract" and "What the landed cards left for the rest".

One branch and one PR per card; open the PR without being asked
(`CLAUDE.md`, "Pull requests"). When the code is done and verified, edit
this file in a separate commit, the last of the branch:

| Section | Edit |
|---|---|
| The card | Delete it. Git history is the record. |
| What the landed cards left for the rest | Add what a remaining card needs: API shape, a limit or gap left open, and **the reason for any decision between options**. Delete entries no remaining card needs. Keep the heading's card list current. |
| Other cards | Remove the card from every **Blocks** and **Blocked by**; write "nothing" where none remain. |
| Order | Remove it from the graph. |
| Suspected C defects | Confirmed: add to `docs/c-divergences.md` with a number, cite it in the commit message, delete the row. Discarded: delete the row, say why in the commit message. |

New files, crates or verify commands go into `CLAUDE.md`'s layout and
"Verifying" sections in the card's code commits.

The PR description reports, per `CLAUDE.md` "Writing style": what changed,
each verify command run, fuzz minutes per target, and what was not verified.

When the last card lands, mark this file "Complete and closed" as the other
two plans are.

## Services contract

1. **Wire shapes.** Topics are `"%016" PRIx64 "<suffix>"` plus `/req` or
   `/rep`; `power_info`'s is `bus_power_controller/timing`, with no node id.
   A request body is `BmServiceRequestDataHeader` (`id` u32, `data_size` u32;
   8 bytes), a reply `BmServiceReplyDataHeader` (`target_node_id` u64, `id`
   u32, `data_size` u32; 16 bytes), both `bm_service_common.h`, packed,
   little-endian. A handler's buffer is `MAX_BM_SERVICE_DATA_SIZE` − 16 =
   1008 bytes.
2. **Services are UDP.** Divergence #70 applies. Compare against
   `stack::drain`'s normalised frames.
3. **Where it lives.** Codecs and tables are sans-io in `bm_wire::service`.
   Dispatch is in `bm_stack::Node`, not an `App`: the C builds the reply
   inside the subscriber callback, so it is owed from `on_frame` in the
   node's one transmit buffer, and registration subscribes in an order
   resource discovery exposes. Application handlers, the power-stats
   callback and metrics components come through **one** new `Node` generic,
   `S: Services = NoServices`, as `C = NoConfig` and `D = NoDfu` do. Later
   cards add trait methods with defaults, not generics.
4. **Registration order is observable.** `bristlemouth_init` registers
   `metrics` (`bm_metrics_enabled`, 1 in `bm-wire-sys/csrc/bm_config.h`); a
   dev kit's bm_protocol `src/apps/bm_devkit/bmdk_common/app_main.cpp:413-415`
   then registers echo, sys_info, config_map. Each registration adds a
   `SUB_LIST` entry. Each request adds `<svc>/rep` to subscriptions and
   `<svc>/req` to publications; neither is removed. Compare
   `resource::oracle_local_resources` after every step.
5. **Request expiry is a 500 ms auto-reload sweep** (`ExpiryTimerPeriodMs`),
   started by `bm_service_request_init`. It is `packet.c`'s 150 ms sweep
   again (divergence #22): keep its phase from init, give it its own arm in
   `Node::run_with`, and do not fold it onto another timer.
6. **Statics with no reset**: `BM_SERVICE_CONTEXT.service_list` (no dedupe;
   registering twice adds two entries), `CTX.service_request_list`,
   `CTX.request_count` (monotone, so ids differ per seed) and
   `power_info_service.c`'s `service_queue`. Follow
   `bm-wire-diff/src/pubsub.rs`: one Rust node mirrors the oracle for the
   life of the process, service inits run once behind a `OnceLock`, and
   outstanding requests are expired at the **start** of each seed.
7. **Seams with no oracle**, made shared inputs: `bm_app_name`
   (`csrc/bm_config.h`, `"bm_wire_sys"`), `git_sha()` (`common/device.c`),
   uptime, the power-stats callback, metrics component values. `app_name`
   and `git_sha` extend `bm_stack::Identity`, with defaults.
8. **CBOR.** `cbor2::core` only; floats through
   `bm_wire::cbor::push_f32_wide` (#43). `config_map`'s `success` is encoded
   as a uint. tinycbor keeps encoding past the buffer and the handler then
   returns false, so a reply over 1008 bytes is **no reply**: reproduce the
   outcome, not the byte counting.
9. **Gold vectors (recipe step 4).** None on the wire. Available: the
   91-byte map in `the_cbor_service_helper_gold_map`; round-trip values in
   `test/src/metrics_reply_msg_test.cpp` and
   `bm_common_messages/test/power_info_ut.cpp`. No capture of a C dev kit
   answering a Bridge was recorded; E6 closed on the bench check alone.
10. **Compare notifications**: handler calls and `BmServiceReplyCb`
    arguments (`ack`, `msg_id`, data), not only frames.

## Suspected C defects to confirm

None open.

## Cards

None remain.

## Order

No cards remain.

## What the landed cards left for the rest (M1, M2, C1, S1, S2, E1, E2, E3, E4, E5, E6)

- **Bench result (E6).** With a Bridge, a C dev kit on bm_protocol's
  `bm_devkit/hello_world` and `bm-devkit`'s `hello_world`: `bmsrv req
  sysinfo` returned the Rust node's id, git SHA, `sys_config_crc` and app
  name `hello_world`; the topology sampler reported the logged
  `sys_config_crc`; a metrics request returned `{"version": 1, "node_id":
  ..., "uptime_ms": ..., "data": {}}`. Three checklist items of PR #60 were
  not run, among them the sysinfo control against the C dev kit and the
  capture. No pcap was recorded, so there is no `capture_services.rs`.

- **The Rust node on the bench is `bm-devkit`'s `hello_world`.** It lists
  `<id>/metrics` at construction, then echo, sys_info and config_map after
  its `spotter/*` and `spotter/utc-time` subscriptions, as `app_main.cpp`
  does after `bm_sub(APP_PUB_SUB_UTC_TOPIC, ...)`. At start it logs over
  defmt each listed service and `sys_config_crc`, the CRC-32 of the system
  partition as a CBOR map (`ConfigPartition::cbor_map_crc32`): the value a
  Bridge's topology sampler should report for it.
- **`Identity::app_name` on a dev kit is the binary's name**
  (`CARGO_BIN_NAME`, passed to `bm_devkit::node`), so `hello_world`, as
  bm_protocol's `APP_NAME` for `src/apps/bm_devkit/hello_world` (the app
  directory's name, `src/CMakeLists.txt`). Decided over a constant per
  binary because it is the rule CMake applies.
- **Known differences from a C dev kit's replies:** `git_sha` is this repo's `HEAD`, not bm_protocol's; the
  metrics reply has no components, where a C dev kit's has `memory_metrics.c`'s
  `memory`; the device-info version string is `bm-devkit@v<version>+<sha>`.
- **`bm_devkit::RESOURCES` is 16**, not `RESOURCES_DEFAULT` (9):
  `hello_world` advertises 11 topics. Past the ceiling a service reply is
  still sent, but its `/rep` topic is not advertised, where a C node's
  `PUB_LIST` lists it.
- **`bm_stack::channel` carries requests.** `Command::{ServiceRequest,
  SysInfoRequest, ConfigMapRequest, MetricsRequest, PowerInfoRequest}`; each
  reports `Notification::ServiceRequested` with the id or the
  `ServiceRequestError`, then `ServiceReply`, `ServiceTimeout` or
  `PowerInfoReply`. `ServiceRequested` exists because a channel
  application cannot see the id `Node::service_request` returns, and a
  refused request otherwise reports nothing. `ServiceReply` holds up to
  `REPLY_BYTES` (`REPLY_DATA_LEN`, 1008), so every reply a C handler can
  write fits; decided over `DATA_BYTES` (256) because a config_map reply of
  a real system partition can exceed it, at the cost of a larger
  `Notification`.
- **`sys_config_crc` with `NoConfig`** is the empty map's CRC
  (`crc32_ieee(&[0xa0])`), what a C node with an empty system partition
  sends, rather than 0, which the C sends only when the map fails.
- **Decoders** for service bodies: `DecodedSysInfoReply::decode_into`,
  `DecodedConfigMapReply::decode_into`, `metrics::decode`,
  `PowerInfoReply::decode_into`; headers `bm_wire::service::{RequestHeader,
  ReplyHeader}`. `bm-wire-diff/src/pcap.rs` reads captures.
- **The oracle's codecs are a release build** (`T2_RELEASE`, #82): a C dev
  kit built for release behaves as the oracle does.
