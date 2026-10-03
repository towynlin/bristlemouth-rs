# Services todo

Open. The port of bm_core's service layer and built-in services, as
dependency-ordered cards sized for one agent each. Same card format as
`docs/bcmp-port-todo.md`, whose "The shared contract" applies in full; the
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
requests `metrics` (bm_protocol `src/apps/bridge/`). A Rust node answers none
of them today.

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
   `bm_common_messages/test/power_info_ut.cpp`. E5 records a capture of a C
   dev kit answering a Bridge; that becomes the gold set.
10. **Compare notifications**: handler calls and `BmServiceReplyCb`
    arguments (`ack`, `msg_id`, data), not only frames.

## Suspected C defects to confirm

From reading the source; not yet run.

| Where | Suspicion | Card |
|---|---|---|
| `metrics_service_handler` | Accepts a non-empty request, which sys_info and power_info refuse; `uptime_ms` wraps after ~49.7 days. | E4 |

## Cards


### E4 — metrics

**Taken:** claude/services-e4-metrics

- **C:** `metrics_service.c`.
- **Rust:** registered at construction when enabled (contract 4);
  components through `Services::metrics`; `uptime_ms` from the time the node
  is given.
- **Comparator:** extends `services`.
- **Blocked by:** nothing. **Blocks:** E5.

### E5 — On a bus

- **Rust:** `bm_stack::channel` `Command`/`Notification` variants for
  service requests and replies; `bm-devkit/src/bin/hello_world.rs` registers
  in `app_main.cpp`'s order; `hello_node` makes one request.
- **Capture:** a C dev kit answering a Bridge's samplers, as
  `bm-wire-diff/testdata/services-*.pcap`, asserted by
  `tests/capture_services.rs` (pattern: `capture_h0.rs`).
- **Blocked by:** E4.
- **Done:** on a bench, a Bridge's topology and metrics samplers list the
  Rust node with its correct `sys_config_crc`.

## Order

| Wave | Cards | Each needs |
|---|---|---|
| 1 | E4 | nothing |
| 2 | E5 | E4 |

Cards within a wave can run in parallel.

## What the landed cards left for the rest (M1, M2, C1, S1, S2, E1, E2, E3)

- **The service list is `bm_wire::service::ServiceTable<H, N, NAME>`**, the
  C's walk included (#89): `lookup` returns the first service whose name
  `strncmp`-prefixes the topic and what its checks made of the request;
  `remove` is the prefix removal. `bm_stack::Node` holds one with `H =
  bm_stack::service::ServiceHandler`, `SERVICES` (16) names of up to 48 bytes.
- **Built-ins are `ServiceHandler` variants; the application's are
  `Services::handle`.** Echo is `ServiceHandler::Echo`, sys_info
  `ServiceHandler::SysInfo`, config_map `ServiceHandler::ConfigMap`,
  power_info `ServiceHandler::PowerInfo`, all answered in `Node::serve`,
  which takes `&I` and `&C`. E4 adds a variant and its arm there; its
  uptime is not passed yet. A built-in's handler body is a sans-io function
  beside its codec (`bm_wire::service::sys_info::handle`,
  `config_map::handle`, `power_info::handle`), and its registration a
  `Node::register_<name>_service` building `<id><SUFFIX>`. `Services` has
  `handle` and `power_info` (the stats callback, default `None`, which
  sends no reply); E4 adds `metrics` with a default (contract 3).
- **`Identity::git_sha` and `Identity::app_name`** (contract 7). `git_sha`
  defaults to `device_info().git_sha` and is what the device-info reply
  and DFU now read. `app_name` defaults to empty. `bm-devkit`'s
  `DevkitIdentity` does not set it: bm_protocol's `bm_app_name` is the
  CMake `APP_NAME` of the app built, which E5 should look up there.
- **`sys_config_crc` with `NoConfig`** is the empty map's CRC
  (`crc32_ieee(&[0xa0])`), what a C node with an empty system partition
  sends, rather than 0, which the C sends only when the map fails.
- **Metrics is not registered at construction yet.** E4 registers it in
  `Node::with_services`, first, as `bristlemouth_init` does. Until then
  `bm-wire-diff/src/services.rs` lists a stand-in named
  `<id>/metrics` first, so the walks agree, and never sends it a request
  it would answer.
- **A topic's subscribers are a list.** `bm_wire::pubsub::Subscriptions`
  keeps `Subscriber::Application`, `Subscriber::Service` and
  `Subscriber::Reply` (`_service_request_cb` on `<svc>/rep`) callbacks per
  topic as the C does, #79 included. `Node::deliver_publication` calls each
  service and reply callback once per publication.
- **One reply per received publication**, in `Owed::reply`, built in the
  node's transmit buffer. The C calls the service callback once per listing
  on each matching subscription and replies each time (#89); the comparator
  asserts the C's replies, handler calls and local deliveries are the Rust
  node's one repeated.
- **A node's own publication is not dispatched to its services**, so
  `Node::service_request` to the node's own service times out where the C
  answers it a pump later. Decided in S2: answering would owe a second frame
  (request, then reply) from one call, and the node has one transmit buffer
  and no queue for it; no caller needs it. Local deliveries reach
  application and reply callbacks only (`Node::deliver_locally`). The
  comparator skips such a request.
- **The C's service list cannot be reset.** Registering adds one entry and
  at most one callback; unregistering removes one callback and at most one
  entry, so an entry left without its callback (#79's re-registration, #89's
  prefix removal) stays for the life of the process. The comparator's
  `reset` unregisters what it can at the start of each input, and
  `LEAK_BUDGET` lets four steps per process leave an entry, only for `x`, a
  name sharing a prefix with no other: a stuck entry stops every request it
  prefixes and is removed by unregistering any name that prefixes it, which
  then strands that name too. S2 extends the same mirror to requests.
- **Every resource is advertised once at start-up, longest first** (#38):
  `advertise_everything` subscribes and unsubscribes each request,
  application and asked reply topic, and publishes nothing to each reply and
  asked request topic. A card adding a topic adds it there.
- **`bm_stack::mock::frames::service_request`** and `service_reply` build a
  peer's request and reply frames.
- **Requests: `Node::service_request(now_ms, service, data, timeout_s)`**
  returns `(id, Outbound)`; the answer is `Event::ServiceReply { id,
  service, data }` from `on_frame_with` or `Event::ServiceTimeout { id,
  service }` from `Node::on_service_expiry`. The C's per-request
  `reply_cb` is not carried: `Node::sys_info_request(_with)(now_ms,
  target, timeout_s)` builds `<target>/sys_info` and calls
  `service_request_with` with no data;
  `Node::config_map_request(_with)(now_ms, target, partition_id,
  timeout_s)` does the same with a `ConfigMapRequest`. E4's requester
  follows. The application tells replies apart by `service` or
  by the id it kept, and decodes the data itself
  (`DecodedSysInfoReply::decode_into`, `DecodedConfigMapReply::decode_into`).
  `service` is the request's, not the reply's topic (#92). `data` is
  `data_size` bytes or what arrived if fewer (#92), so a decoder sees a
  short body where the C reads past the publication.
- **power_info's requester is the exception.** `power_info_service_request`
  queues the caller's callback and makes every request's `reply_cb` its
  own, which dequeues the oldest callback (#96).
  `Node::power_info_request(_with)(now_ms, timeout_s)` queues one in
  `bm_wire::service::power_info::Callbacks`, named by the request's id; its
  requests report `Event::PowerInfoReply { id, reply }` with the dequeued
  callback's id, only for a reply that decodes, and never `ServiceReply` or
  `ServiceTimeout`. Decided so the events are the C's callbacks one for
  one. The comparator tells the oracle's callbacks apart as
  `C_POWER_REPLY`, eight functions, since a `BmPowerInfoReplyCb` takes no
  context.
- **Ceilings:** `bm_stack::service::SERVICE_REQUESTS` (8) requests, names of
  `SERVICE_NAME_BYTES`. `bm_wire::service::Requests<N, NAME>` is the list,
  id counter and sweep phase; `resuming(next_id, next_sweep_ms)` lines one up
  with a running C.
- **The request sweep is the third timer**, `Node::on_service_expiry`, with
  its own arm in `Node::run_app` and phased from construction. `on_tick`
  runs it too. An `App` ticker on a multiple of 500 ms now loses its tie to
  that arm, as one on the 150 ms grid already did.
- **The oracle's sweep needs `stack::start_timer_callback_handler`.**
  `bm_service_request.c` hands expiries to `timer_callback_handler.c`'s task,
  which `bm_shim_stack_init` does not start, because `dfu_host.c`'s
  heartbeats go through it too and `dfu_core`'s comparator does not expect
  them. The `services` comparator starts it at bring-up.
- **Time in the `services` comparator** moves only in `Step::Wait`, one sweep
  at a time, and at each input's start, which waits out every request (the
  C's list has no reset). `Timeout` keeps every timeout within seconds.
  `Step::Ask` asks an `ASKED` service; `Step::Reply` injects a peer's reply.
  E4 adds its request as a `Step` variant, as `Step::AskSysInfo`,
  `Step::AskConfigMap` and `Step::AskPowerInfo` do; `Answer` carries reply bytes, which
  `service_codecs` already compares decoded, so it was not extended. A
  service whose request carries CBOR needs a step that sends a well-formed
  one, as `Step::RequestConfigMap` does: in 14 minutes of fuzzing,
  `Step::Request`'s arbitrary data reached no config_map reply;
  `Step::ReplyPowerInfo` is the same for a power_info reply.
  `Step::PowerStats` sets the stats both sides' callbacks return.
  `Summary::sys_info_replies`, `config_map_replies` and
  `config_map_successes` count the node's replies; `sys_info_decoded` and
  `config_map_decoded` the answers to `PEER_SYS_INFO` and `PEER_CONFIG_MAP`
  that decode.
- **The comparator reads `bm_get_subs` under an allocation floor** (#78):
  `bm_shim_alloc_floor` makes `bm_malloc` return at least n zeroed bytes,
  and `pubsub::oracle_subscriptions` sets 4096, so the pools no longer
  budget bytes. E3 found the 256-byte budget could not hold
  `bus_power_controller/timing`'s two topics. A card adding topics raises
  `services::SUBSCRIPTIONS` (28) and `RESOURCES` (48), the Rust node's
  ceilings.
- **The comparator's node keeps a `Config<RamConfigStorage>`**, replaced at
  each input by `config::reset(&[])`, which also empties the oracle's
  `CONFIGS`. `Step::Configure(Seed)` writes one key to both, in any
  partition. Before each sys_info reply the comparator asserts
  `services_cbor_encoded_as_crc32` against `cbor_map_crc32`, and before
  each config_map reply `services_cbor_as_map` against `cbor_map`; it skips
  a request whose partition's map is `MapError::Unreachable`, or whose
  `partition_id` is tagged.

- **`bm_wire::cbor::parser` is tinycbor's parser.** `Value` is `CborValue`
  and each method the C function it names, including error codes, tags not
  counting as items, and `cbor_value_validate_basic` reading only the
  top-level item. M2 added `string_length`, `map_find_value`, `skip_tag`,
  `is_double` and the codes `ImproperValue`, `TooFewItems` and
  `UnsupportedType`; floats are read as `extract()`'s bits. Where tinycbor
  would fail a `cbor_assert`, a method returns `CborError::Unreachable`, and
  a comparator does not call the C on that input.
- **The metrics body: `bm_wire::service::metrics`** (E4). `encode(&Reply,
  &[Component], buf)`; a component is a key and `&[Entry]`, an entry a key
  and a `Field`. Contract 8's "over 1008 bytes is no reply" is
  `Err(CborError::OutOfMemory)` from `encode` into the handler's 1008-byte
  buffer. `Field::String` fails the encode (divergence #85), so
  `Services::metrics` should not offer it. `metrics_reply_msg.c` and
  `bm_messages_helper.c` are not in `T2_RELEASE`: the oracle's metrics codec
  keeps its asserts, which #87 reaches.
- **The oracle's codecs are a release build.** `bm-wire-sys/build.rs`
  `T2_RELEASE` compiles `sys_info_svc_reply_msg.c`,
  `config_cbor_map_srv_{request,reply}_msg.c` and `power_info_reply_msg.c`
  with `NDEBUG`, because a debug build aborts on a non-uint value (#82).
  `config_cbor_map_service.c`, in T3, calls the release decoder, so the
  `services` stack target sees release behaviour too. A debug-built C node
  aborts on a `config_map` request such as `{"p": "ab"}`; the Rust server
  does what a release node does.
- **Decoders write into `&mut self`** (`decode_into`) rather than returning a
  value, because the C writes fields as it reads them.
- **Encoders return `Err(CborError::OutOfMemory)` where the C's handler
  returns false**: no reply (contract 8). `config_map::handle` writes the reply's fields and then the
  map into that buffer, measuring the map first; a reply that does not fit
  is no reply (#94).
- **Decoded strings are `CborString`s**, borrowed from the body, chunked or
  not; `copy_to` and `eq_bytes` read them. No allocation.
- **Allocation is a seam.** `sys_info_reply_decode` and
  `config_cbor_map_reply_decode` `bm_malloc` a size the sender chooses
  (#83, #84). The port assumes the allocation succeeds.
  `bm_shim_heap_watch_begin(limit)` refuses zero bytes or more than `limit`
  on the calling thread and counts what it granted, so a comparator can skip
  heap-dependent inputs and free what the C leaks.
- **A partition as a map: `ConfigPartition::cbor_map(&mut [u8])`** returns
  the map's length, or a `MapError`: `NoMap` where the C returns `NULL`,
  `TooSmall(n)` where the buffer is short (the C allocates), `Unreachable`
  where the C is undefined. `cbor_map_crc32` is 0 for any error; it is
  `sys_config_crc`. A node with `NoConfig` maps every partition as the
  empty map, `a0`.
- **The map is read as a release build reads it** (#88): each value by its
  key's stored type, through `parser::Value`'s unchecked accessors. The same
  decision as #82, for the same reason; `bm-wire-sys/build.rs` compiles
  `cbor_service_helper.c` with `NDEBUG`. A debug-built C node aborts on a
  `sys_info` or `config_map` request where a refused typed set (#46) left a
  key's type over a string head.
