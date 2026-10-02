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
| `_service_request_received_cb` | Reads `data_size` before checking `data_len >= 8`. | S1 |
| `_service_request_received_cb` | `strncmp` over the service's length, then `break` on a topic-length mismatch: a service whose name prefixes a later one's shadows it. | S1 |
| `_service_list_remove_service` | Prefix match: unregistering `a` removes `ab`. | S1 |
| `echo_service_handler` | `*buffer_len <= MAX_BM_SERVICE_DATA_SIZE` is always true; a request over 1008 bytes overflows the reply buffer. | S1 |
| `bm_service_request` | The inner `node` shadows the outer: after a failed subscribe or send the request stays listed and later times out with `ack = false`; an id is consumed either way. | S2 |
| `_service_request_cb` | No length check on the header or `data_size`; matches id and target only, not topic. | S2 |
| `config_map_service_handler` | An invalid partition still replies, `success = 0`; a map over 1008 bytes gets no reply. | E2 |
| `power_info_reply_cb` | Callbacks are dequeued FIFO, not by id; a failed send leaves its callback queued and shifts every later pairing. | E3 |
| `power_info_service_init` | Stores the callback when registration fails. | E3 |
| `metrics_service_handler` | Accepts a non-empty request, which sys_info and power_info refuse; `uptime_ms` wraps after ~49.7 days. | E4 |
| `sys_info_service_handler` | `sys_config_crc` is 0 whenever `services_cbor_as_map` fails (#42). | C1 |

## Cards

### C1 — Config partition as a CBOR map

**Taken:** claude/services-c1-cbor-map

- **C:** `services_cbor_as_map`, `services_cbor_encoded_as_crc32`.
- **Rust:** `bm_wire::configuration::ConfigPartition::cbor_map` and
  `cbor_map_crc32`, reading the byte image as the C does, #42 included (an
  `ARRAY` copied raw; an unreadable value ends the map).
- **Comparator:** extend `bm-wire-diff/src/configuration.rs` and its
  existing target; reuse its `config_init` reset.
- **Blocked by:** nothing. **Blocks:** E1, E2.
- **Done:** `configuration` fuzz target clean with map and CRC compared.

### S1 — Service table, dispatch, echo

- **C:** `bm_service.c`, `echo_service.c`.
- **Rust:** `bm_wire::service::{RequestHeader, ReplyHeader, topic,
  ServiceTable<N>}` with the C's matching; `Node`'s `S: Services` generic;
  `Node::register_service`, `unregister_service`; the reply published from
  `on_frame`; echo as the first built-in.
- **Comparator:** stack target `services` (`bm-wire-diff/src/services.rs`,
  `tests/services.rs`, `replay::STACK_TARGETS`). A scripted peer publishes
  arbitrary bodies on arbitrary `/req` topics to both nodes; compare reply
  frames, handler calls and resources.
- **Blocked by:** nothing. **Blocks:** S2, E1, E2, E3, E4.
- **Done:** fuzz target `services` clean; `hello_node` unchanged and passing.

### S2 — Service requests

- **C:** `bm_service_request.c`.
- **Rust:** `bm_wire::service::Requests<N>` (id counter, deadlines, the
  500 ms sweep); `Node::service_request(service, data, timeout_s)`,
  `Event::ServiceReply { id, service, data }`, `Event::ServiceTimeout`.
- **Comparator:** extends `services`. Each node requests the other's echo;
  the peer injects replies with arbitrary target, id and length. Compare
  request frames, callbacks, the expiry instant and resources. Assert the
  request table kept a slot free.
- **Blocked by:** S1. **Blocks:** E1, E2, E3, E4.

### E1 — sys_info

- **C:** `sys_info_service.c`.
- **Rust:** the handler, `Node::sys_info_request`; `Identity::app_name`,
  `git_sha`. The CRC is C1's over the system partition.
- **Comparator:** extends `services`.
- **Blocked by:** C1, S2. **Blocks:** E5.

### E2 — config_map

- **C:** `config_cbor_map_service.c`.
- **Rust:** the handler, reading `C: Configuration`;
  `Node::config_map_request`.
- **Comparator:** extends `services`; seed both stores as
  `bm-wire-diff/src/config.rs` does.
- **Blocked by:** C1, S2. **Blocks:** E5.

### E3 — power_info

- **C:** `power_info_service.c`, `common/cb_queue.c`.
- **Rust:** the server through `Services::power_info`; the requester, with
  the FIFO pairing reproduced in what `Event::PowerInfoReply` reports.
- **Comparator:** extends `services`.
- **Blocked by:** S2. **Blocks:** E5.

### E4 — metrics

- **C:** `metrics_service.c`.
- **Rust:** registered at construction when enabled (contract 4);
  components through `Services::metrics`; `uptime_ms` from the time the node
  is given.
- **Comparator:** extends `services`.
- **Blocked by:** S2. **Blocks:** E5.

### E5 — On a bus

- **Rust:** `bm_stack::channel` `Command`/`Notification` variants for
  service requests and replies; `bm-devkit/src/bin/hello_world.rs` registers
  in `app_main.cpp`'s order; `hello_node` makes one request.
- **Capture:** a C dev kit answering a Bridge's samplers, as
  `bm-wire-diff/testdata/services-*.pcap`, asserted by
  `tests/capture_services.rs` (pattern: `capture_h0.rs`).
- **Blocked by:** E1, E2, E3, E4.
- **Done:** on a bench, a Bridge's topology and metrics samplers list the
  Rust node with its correct `sys_config_crc`.

## Order

| Wave | Cards | Each needs |
|---|---|---|
| 1 | C1, S1 | nothing |
| 2 | S2 | S1 |
| 3 | E1, E2, E3, E4 | S2 and its codec cards (see **Blocked by**) |
| 4 | E5 | E1–E4 |

Cards within a wave can run in parallel.

## What the landed cards left for the rest (M1, M2)

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
  aborts on a `config_map` request such as `{"p": "ab"}`; E2's Rust server
  does what a release node does.
- **Decoders write into `&mut self`** (`decode_into`) rather than returning a
  value, because the C writes fields as it reads them and
  `power_info_reply_cb` passes the partly decoded struct to the requester's
  callback whether or not decoding succeeded. E3 reports that struct.
- **Encoders return `Err(CborError::OutOfMemory)` where the C's handler
  returns false**: no reply (contract 8). E1–E3 encode into the handler's
  1008 bytes. `ConfigMapReply::encode` takes the map as a slice and writes
  its length as `cbor_encoded_map_len`; `config_map_service_handler` sends
  an empty one with `success` false. `config_map::PARTITION_ID_*` are the
  request's partition ids.
- **Decoded strings are `CborString`s**, borrowed from the body, chunked or
  not; `copy_to` and `eq_bytes` read them. No allocation.
- **Allocation is a seam.** `sys_info_reply_decode` and
  `config_cbor_map_reply_decode` `bm_malloc` a size the sender chooses
  (#83, #84). The port assumes the allocation succeeds.
  `bm_shim_heap_watch_begin(limit)` refuses zero bytes or more than `limit`
  on the calling thread and counts what it granted, so a comparator can skip
  heap-dependent inputs and free what the C leaks.
