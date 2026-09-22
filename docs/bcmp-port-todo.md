# BCMP porting todo

What of BCMP is still unported, in dependency order, as task cards sized for
one agent each.

`bm-wire` carries six of BCMP's exchanges — heartbeat (`0x01`), echo
(`0x02`/`0x03`, both halves), device info (`0x04`/`0x05`, both halves),
neighbour table (`0x08`/`0x09`, both halves), resource discovery
(`0x0A`/`0x0B`, both halves) and system time (`0x10`–`0x12`, both halves) —
plus the wire engine under them (`bcmp::tx::serialize`, `bcmp::rx::accept`, L2
egress stamping, the link-local RX policy, the two forwarding paths in
`bcmp::forward`) and three state machines, `bm-wire/src/neighbor.rs`,
`bm-wire/src/bcmp/registry.rs` and `bm-wire/src/bcmp/resource.rs`.
`MessageType` names all 45 of bm_core's constants; fourteen body structs have a
codec. CBOR, which the config chain needs, is the `cbor2` crate rather than a
port; `bm-wire/src/cbor.rs` holds only the one deviation from its defaults
that bm_core requires.

`bm_stack::Node` drives that registry: `Node::register` is `packet_add`,
`Node::request` is `bcmp_tx`, and `bm_stack::Event` is where a reply, a timeout
or an unsolicited message arrives.

Everything below is absent from Rust — no files, no stubs, no `TODO` markers.

## Status at a glance

| Area | C source | LoC | Card |
|---|---|---|---|
| Local config store | `bcmp/configuration.c` | 845 | C2 |
| Config over BCMP `0xA0`–`0xA9` | `bcmp/config.c` | 857 | C3 |
| DFU message codecs `0xD0`–`0xD9` | `bcmp/dfu_message_structs.h` | — | D1 |
| DFU core HFSM | `bcmp/dfu_core.c` | 689 | D2 |
| DFU client | `bcmp/dfu_client.c` | 661 | D3 |
| DFU host | `bcmp/dfu_host.c` | 482 | D4 |

C2 and D1 are unblocked and may run in parallel. C3 needs C2. D2 needs D1; D3
and D4 each need D2.

---

## The shared contract

Read this before starting any card.

### Bring the oracle up first

```
git submodule update --init --recursive
```

Without this, `bm-wire-sys` does not build and every differential test fails
for the wrong reason. A sandbox whose stable toolchain predates the 1.97 MSRV
will not build at all. `.claude/hooks/session-start.sh` handles both and runs
before the session starts; see `CLAUDE.md`. Neither is a finding — do not write
it up in the card's report.

### `bm-wire` rules

`no_std`, no `alloc`, `forbid(unsafe_code)`, one dependency (`cbor2`, at
`default-features = false`), and it may never depend on `bm-wire-sys` in any
configuration. Use explicit little-endian
codecs, never `repr(packed)` mirrors. The `std` feature is for tests and
fuzzing only.

### The recipe

`CLAUDE.md`'s "Porting a function to bm-wire" is the procedure. Step 4 — assert
bm_core's own gtest value — is unavailable for almost every card here.

### Step 4 is mostly unavailable, including for DFU

bm_core ships **no on-wire gold vectors for any BCMP message type**. Its
message-level tests (`packet_test.cpp`, `ping_test.cpp`, `info_test.cpp`,
`neighbors_test.cpp`, `time_test.cpp`, `config_test.cpp`,
`resource_discovery_test.cpp`) build randomized C structs and compare against C
structs, so there is nothing to lift.

Literal byte arrays exist in five test files, mostly as inputs:

| Test file | What the hex is | Useful as a gold vector? |
|---|---|---|
| `bm_linux_test.cpp` | Real captured frames with a checksum a live node agreed on | **Yes** — harvested into `bm-wire-diff/src/gold_vectors.rs` |
| `l2_policy_test.cpp` | 16-byte IPv6 address constants | Partly; the l2_policy port uses them |
| `configuration_test.cpp` | A 10-byte test payload being stored | No |
| `cbor_service_helper_test.cpp` | Small CBOR fragments | Yes — the 91-byte map is pinned by `the_cbor_service_helper_gold_map` |
| `pcap_test.cpp` | A pcap file header | Not BCMP |

`dfu_test.cpp`'s `client_golden` and `host_golden` are state-machine transition
traces, not byte vectors: they build `BcmpDfuStart` structs in memory and assert
state enums. Valuable for D2 as the reference event/state sequence, but they
prove nothing about the wire encoding.

Ground truth for every encoding below comes from running the compiled oracle.
Say so in the card's writeup rather than quietly skipping step 4.

### Harness constraints

- **Nothing in `bm-wire-diff` may call `bm_shim_reset`.** `packet_init` hands
  `PACKET` a shim mutex and a shim timer, and `packet.c` has no deinit, so a
  reset frees objects the C still points at.
- C state is process-global. Tests touching the shim take the `SHIM` lock. A
  comparator needing shim state brings the oracle up once per process behind a
  `OnceLock` and serialises every C call behind a `Mutex`;
  `bm-wire-diff/src/bcmp.rs` is the pattern.
- A comparator that brings the **whole stack** up needs a process to itself.
  Build on `bm-wire-diff/src/stack.rs`, drive it from its own file under
  `bm-wire-diff/tests/`, and register its seeds in `replay::STACK_TARGETS`
  rather than `replay::TARGETS`. A test asserts every `seeds/` directory is in
  exactly one of the two lists, and that every `STACK_TARGETS` entry has a
  matching test binary.
- After injecting, use `stack::pump_until_quiet`, not a bare `bm_shim_pump`.
- Everything in `bm-wire-sys/csrc/` stays deterministic.
  `check_symbols.sh --check` omits `rand` and `time` from its libc allowlist so
  a non-deterministic reach trips CI.

### The sequence-list hazard

`bm-wire-diff/src/bcmp.rs` registers its 25 message types as `sequenced_reply`
so that the C's `sequence_list` never grows, which lets the `bcmp` fuzz target
run in-process instead of in fork mode.

`bcmp/config.c` is the only module in bm_core that sets
`BcmpPacketCfg::sequenced_request`. Its ten registrations at `config.c:789-835`
use positional initializers `{sequenced_reply, sequenced_request, process}`.
**Registering any of them in `bcmp.rs` would break the in-process property**, so
card C3 must live in a stack-target binary of its own.

One sequenced request already exists outside it:
`our_sequenced_request_carries_the_number_the_c_would_have_given_it` in
`bm-wire-diff/tests/node_frames.rs` sends three `BcmpConfigGetMessage`s through
`bcmp_tx` to pin the sequence number a request carries. `message_count` is a
function-level `static` with nothing that resets it, so that test assumes it is
the only sequenced sender in its binary. A card adding another to
`node_frames.rs` must say where the counter had got to, or use its own binary.

### Verifying

Per `CLAUDE.md`. At minimum, before claiming a card done:

```
cargo test
cargo build -p bm-wire --target thumbv7em-none-eabihf
cargo build -p bm-wire --target thumbv8m.main-none-eabihf
cargo build -p bm-stack --target thumbv8m.main-none-eabihf
cargo +1.97 check --workspace --all-targets
cargo tree -p bm-wire                         # only cbor2, serde, serde_core
./bm-wire-sys/scripts/check_symbols.sh --check
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features \
  -p bm-wire -p bm-stack -p bm-wire-diff      # a bare `cargo doc` passes where CI fails
cd bm-wire/fuzz && mkdir -p corpus/<target>
cd bm-wire/fuzz && cargo fuzz run <target> corpus/<target> seeds/<target>
```

On a crash: `cargo fuzz tmin <target> <artifact>`, then drop the minimized file
into `bm-wire/fuzz/seeds/<target>/`.

### What the landed cards (M1, M2, M3, M4, M5, C1) left for the rest

- **Forwarding machinery is built.** `bm_stack::Owed::forward` is the decision,
  as a `Reflood` — a byte range within the received frame plus the ingress port,
  not a frame, because `bcmp_ll_forward` builds one new frame per port and a
  node has one transmit buffer. `bm_stack::Node::reflood` is the loop;
  `Node::run` drives it after `deliver`, and a caller driving the synchronous
  half by hand reads `owed.forward` out before handing the rest to `deliver`.
  `bcmp/config.c` and `bcmp/dfu_core.c` forward exactly as `bcmp/time.c` does,
  so C3 and D2 inherit this.
- **The forwarding test is not the acting test.** `SystemTimeHeader::is_local`
  decides forwarding; `SystemTimeRequest::is_for` and friends decide whether to
  act. Divergence #27 is what happens when those are assumed to agree.
- **`bm-wire-diff::forward::Message::SystemTimeForAnother`** is the input shape
  that makes `check_relay` compare the whole receive path, forwarding decision
  included. A card adding another forwarding exchange wants the same shape.
- **Expect divergence #12 constantly.** A body carrying caller-chosen bytes
  reaches the egress-checksum double-carry case within minutes of fuzzing.
  C3 and D2 will meet it on every run. Do not "fix" it on the Rust side.
- **A comparator that reads a frame bm_core stamped must not require it to
  validate.** Some of those frames correctly do not. Read the type out of the
  BCMP header rather than classifying with `rx::accept`.
- **Assert what the comparator believes, not just what the bytes are.** Both of
  M2's divergences (#27, #28) came from a comparator assertion that modelled
  *why* the C does something, next to the byte comparison saying *what* it does.
  The model was wrong twice and the fuzzer said so within minutes each time.
- **Integrator-hook seams have no oracle.** `bm_rtc_get`, `bm_rtc_set` and
  `bm_rtc_get_micro_seconds` are declared in `bcmp/bm_rtc.h` and defined nowhere
  in bm_core; the only implementation is ours, in
  `bm-wire-sys/csrc/bm_generic_shim.c`. `RtcTimeAndDate::to_utc_micros` matches
  it by construction and `stack::set_both_clocks` makes it a shared input rather
  than a comparison. Everything downstream is compared. Say the same thing at
  the same volume for any new seam.
- **Compare the request a step provokes, not only the state it leaves.**
  Divergence #34 is invisible in the info cache and obvious in the frame.
  `bm-wire-diff/src/info.rs` reads the C's `0x04` transmissions out of the
  capture ring and compares them byte for byte against `Owed::reply`. Every
  card with a requester side wants the same.
- **A string bm_core keeps as a `char *` is only readable to its first NUL**,
  since no length is kept beside it. `info.rs`'s `STRINGS` are NUL-free and say
  why at the constant.
- **A module's own statics have to be normalised between seeds.**
  `INFO_REQUEST_LIST` never expires an entry (divergence #19), so an unanswered
  request outlives the seed that made it and would satisfy a later one.
  `info::check` tracks the list from the C's own transmissions and answers
  everything outstanding before returning; `stack::clear_neighbor_table` resets
  the other half. Only `bm_l2_deinit` exists upstream, so every card has this
  problem for whatever statics its module keeps.
- **Divergence #20 is no longer theoretical, and `bm-wire-diff/src/ll.rs` is
  where to stay out of it.** `cargo fuzz run info` reached `ll_item_add`'s
  heap-use-after-free within four minutes, from three frames any node can send.
  `LinkModel` is shared by `registry.rs`, `info.rs` and `resource.rs`; any card
  whose module removes from an `LL` out of insertion order — C3 on
  `sequence_list` — needs it too.
- **Size the port's ceilings out of reach, and assert they stayed there.** The
  port bounds what bm_core leaves unbounded, and a full fixed-capacity
  structure compares as a divergence rather than as a limit. `info::check`
  asserts both the request list and the info cache have a slot to spare, the
  way `neighbor::check` asserts `!outcome.table_full`. Both of those assertions
  fired on the fuzzer's first two runs.
- **BCMP's node-id-keyed lists hold 32 bits of a 64-bit id** (divergence #33);
  `INFO_REQUEST_LIST` and `RESOURCE_REQUEST_LIST` both do. Keeping two ids that
  share their low 32 bits in every comparator's id pool is what separates a
  32-bit key from a 64-bit compare without any extra machinery.
- **Read each module's own correlation; do not assume the last card's.**
  `bcmp/info.c` keeps an unbounded list keyed on half an id; `bcmp/neighbors.c`
  keeps one slot matched on the whole of one; `bcmp/ping.c` keeps four statics
  and matches on sixteen bits and a payload; `bcmp/resource_discovery.c` keeps
  an unbounded list keyed on half an id and, alone in BCMP, requires the reply's
  body to name the address it arrived from. Only `bcmp/config.c` uses
  `packet.c`'s machinery at all.
- **bm_core puts a real timer on exactly one exchange, and it expires
  nothing.** `NEIGHBOR_TIMER` is a one-shot armed by the request, so its
  deadline *is* its deadline — unlike `packet.c`'s auto-reload sweep
  (divergence #22). A card adding a timed exchange keeps the deadline in its
  sans-io state (`TableRequests::remaining_ms`) and gets its own arm in
  `Node::run_with` rather than being folded onto a ticker.
- **`bm_timer_*` is the second integrator seam with no oracle**, after
  `bm_rtc_*`. `csrc/bm_os_shim.c` fires a timer once
  `(int32_t)(tick - due) >= 0`, which is `time_remaining` restated, and
  `TableRequests` compares with `time_remaining` — so the instant agrees by
  construction and what is compared is whether the callback ran.
- **A module's statics can usually be reset through its own front door.**
  `neighbor_table::reset_requester` makes a request naming node zero and
  answers it, which is the only route back to the state a process starts in.
  It runs at the **start** of every seed rather than the end, so a seed that
  panics does not poison the next one — prefer that to `info::check`'s order.
- **A module with no deinit forces a bounded input pool.**
  `bcmp/resource_discovery.c`'s `PUB_LIST` and `SUB_LIST` have no remove, no
  clear and no deinit, so they are process-global and monotone: the only reset
  is `bcmp_resource_discovery_init`, which drops the chain and leaks two
  semaphores. `bm-wire-diff/src/resource.rs` answers that with a fixed name
  pool, a process-global `Model` that the port's table is rebuilt from at the
  start of each seed, and a comparison that asserts *agreement* rather than any
  particular state — so every test in `tests/resource.rs` is order-independent,
  and has to be. C2's config store is the next card with state that outlives a
  seed.
- **The oracle's stack is not a blank slate.** `bm_shim_stack_init` brings
  `metrics_service_init` up, which calls `bm_sub` and so leaves
  `<node id>/metrics/req` in `SUB_LIST` before any comparator runs. Read a
  module's state out of the C on first use rather than assuming it starts
  empty; `resource::oracle_local_resources` is that, through
  `bcmp_resource_discovery_get_local_resources`.
- **`stack::captured_message_type` is how to classify a captured frame**, and
  `stack::Captured` is what `drain` returns. `rx::accept` verifies the
  checksum, so filtering on it silently drops the frames divergence #12 broke —
  exactly the ones worth comparing. M5 added the helper and moved `info.rs` and
  `neighbor_table.rs` onto it.
- **Tell the oracle's own frames from the ones it relayed.**
  `resource::is_ours` compares the node id in the source address, which the
  egress-port stamp does not touch. It is also why that comparator's peer ids
  exclude the node's own: L2 keeps the sender's source address on a relay, so a
  peer calling itself by the oracle's id would be indistinguishable from
  something the oracle built.
- **Every module orders `bcmp_tx` against its own bookkeeping differently.**
  `bcmp_request_info` records first and `ll_remove`s on failure;
  `bcmp_request_neighbor_table` records and keeps it;
  `bcmp_resource_discovery_send_request` transmits first and records only on
  success. All three are observable, and none is a pattern for the next.
- **CBOR is the `cbor2` crate, not a port.** `bm-wire` depends on it at
  `default-features = false`, which is `no_std` and alloc-free; `cbor2::core`
  is the layer to use (`Encoder::push`/`write_all`, `Decoder::pull`), because
  everything above it — `Value`, `from_slice`, `RawValue`, the serde
  derives — needs `alloc`. Bodies come back through `Decoder::read_exact`
  into a caller buffer.
- **Floats must go through `bm_wire::cbor::push_f32_wide`** (divergence #43).
  `cbor2::core::Header::Float` applies RFC 8949 preferred serialization and
  narrows `1.0` to `f9 3c00`; `cbor_value_is_float` accepts only `0xfa`, and
  `cbor_type_to_config` refuses to classify anything else, so a C node rejects
  the whole `ConfigSet` rather than misreading it. This is the one place
  cbor2's defaults are wrong for this wire.
- **cbor2 does not count container items and does not report a shortfall.**
  tinycbor's `close_container` checks the declared length and its encoder
  keeps counting past the end of the buffer so `cbor_encoder_get_extra_bytes_needed`
  can size a retry; cbor2's slice writer simply fails, and there is no close.
  So the `services_cbor_as_map` retry loop has no direct analogue — size the
  buffer with `cbor2::ser::serialized_size` (available without `alloc`), and
  count map pairs yourself or the header will lie.
- **Indefinite-length string reassembly is the caller's job.**
  `Decoder::bytes_body`/`text_body` need `alloc`. A `ConfigSet` body can carry
  a chunked string, so C2 needs a loop over `pull()` that accumulates into a
  fixed buffer, and it is the first thing worth a comparator of its own —
  `bm-wire-diff/src/cbor.rs` proves head agreement and definite-length bodies,
  not reassembly.
- **What the comparator proves is byte agreement, not behaviour agreement.**
  cbor2 and tinycbor are different libraries and are not expected to make the
  same judgements; `bm-wire-diff/src/cbor.rs` asserts they emit identical
  bytes for every value shape `bcmp/configuration.c` stores, read the same
  item head off arbitrary bytes, and disagree in exactly three enumerated
  places (#40, #41, #43). A fourth kind of disagreement fails the fuzzer.
- **Two tinycbor defects the port simply does not have** (#40, #41): a
  failed `cbor_parser_init` that still reports a type and still reports valid,
  and `cbor_value_get_int64` overflowing on `-(2^63) - 1`. Both are `c-only`
  now. They still matter for reading bm_core's source: a C node behaves this
  way.
- **`services_cbor_as_map` cannot publish a partition holding an `ARRAY`**
  (divergence #42), and reads an uninitialised `CborValue` when a key's value
  cannot be read. C3 inherits both; do not port the map builder assuming it
  round-trips.
- **Compare the callbacks a module hands the application, not only its state.**
  `bcmp/neighbors.c` exposes none of its three statics, so the whole of M4's
  comparison is three observable effects: the request frame, the reply callback
  and the timeout callback. `Model` in `bm-wire-diff/src/neighbor_table.rs` is
  the comparator's belief about the statics behind them, asserted against both
  sides on every step.

---

## Card format

Each card gives: the C source, the Rust to create, the comparator and fuzz
target, any new port seam, the C quirks to reproduce, what blocks it, and what
"done" means.

---

# The config chain

## C2 — the local config store

**Blocked by:** nothing; C1 has landed.

**C source.** `bcmp/configuration.c` (845 LoC) — not a wire module. Typed
get/set for `UINT32`, `INT32`, `FLOAT`, `STR`, `BYTES`, `ARRAY`; a partition
header with a CRC32; commit/save; `remove_key`; `clear_partition`. Limits from
`configuration.h`: `MAX_NUM_KV` 50, `MAX_KEY_LEN_BYTES` 32,
`MAX_CONFIG_BUFFER_SIZE_BYTES` 50, `CONFIG_VERSION` 0.

**New port seam.** Config storage in `bm-stack/src/port.rs`, matching
`bm_configs_generic.h`'s `bm_config_read`/`bm_config_write`/`bm_config_reset`.

**Gold vectors.** `configuration_test.cpp`'s literal hex is a 10-byte payload
being stored, not an expected encoding. Pin the CRC32 and partition-header
layout instead, with expected bytes derived from the oracle.

**Done when.** A partition written by the Rust store is byte-identical to one
written by the C for the same key sequence, CRC included, and both reject the
same corrupt images.

## C3 — config over BCMP, `0xA0`–`0xA9`

**Blocked by:** C2. The re-flood these messages need is
`bm_stack::Node::forward_link_local`, which exists.

**C source.** `bcmp/config.c` (857 LoC), one handler
(`bcmp_process_config_message`) for all ten types.

**Read the sequence-list hazard above before starting.** C3 gets its own binary
under `bm-wire-diff/tests/` and its seeds go in `replay::STACK_TARGETS`.

Flags, from the positional initializers at `config.c:789-835`:

| Type | Value | `sequenced_reply` | `sequenced_request` |
|---|---|---|---|
| ConfigGet | `0xA0` | false | **true** |
| ConfigValue | `0xA1` | **true** | false |
| ConfigSet | `0xA2` | false | **true** |
| ConfigCommit | `0xA3` | false | false |
| ConfigStatusRequest | `0xA4` | false | **true** |
| ConfigStatusResponse | `0xA5` | **true** | false |
| ConfigDeleteRequest | `0xA6` | false | **true** |
| ConfigDeleteResponse | `0xA7` | **true** | false |
| ConfigClearRequest | `0xA8` | false | **true** |
| ConfigClearResponse | `0xA9` | **true** | false |

**Quirks to reproduce.**

- `config.c:741` does `key += key->key_length + sizeof(BmConfigStatusKeyData)`
  on a **typed** `BmConfigStatusKeyData *`, so the advance is scaled by the
  struct size rather than being a byte offset. Any status response carrying
  more than one key walks off the end. Confirm the exact consequence against
  the oracle and record it.
- Messages not addressed to this node are forwarded.
- Divergence #22 applies here first and hardest: a sequenced request's real
  timeout is anywhere from 25 ms to 174 ms, and these ten messages are the only
  ones in bm_core that use it.

**Test coverage to expect.** `config_test.cpp` has two cases — `decode` and
`ClearPartitionRequest`. Eight of the ten messages have no C test at all, so
the comparator is the only thing standing between this port and a silent
divergence.

---

# DFU

1832 LoC across three files, the heaviest state in bm_core. Split four ways.

## D1 — DFU message codecs, `0xD0`–`0xD9`

**Blocked by:** nothing.

**C source.** `bcmp/dfu_message_structs.h` (73 LoC) — `BmDfuImgInfo`,
`BmDfuFrameHeader`/`BmDfuFrame`, `BmDfuEventAddress`, `BmDfuEventChunkRequest`,
`BmDfuEventImageChunk`, `BmDfuEventResult`, `BmDfuEventImgInfo` — plus the ten
`BcmpDfu*` wrappers in `bcmp/messages.h`. Pure wire format, no state.

**Quirk to record.** `dfu_core.c:621` and `dfu_core.c:623` both call
`packet_add` for type `0xD9`, once as `BcmpDFUBootCompleteMessage` and once as
its alias `BcmpDFULastMessageMessage`, leaving a duplicate entry in the registry
list. Check what the duplicate does to dispatch.

**Done when.** All ten codecs round-trip against the oracle under a `dfu_codec`
fuzz target. `bm_dfu_max_chunk_size` is 1024, so the comparator's body bound
must accommodate it.

## D2 — the DFU core state machine

**Blocked by:** D1.

**C source.** `dfu_core.c` (689 LoC): `LibSmContext` over the 10 states of
`BmDfuHfsmStates`, 15 `BmDfuEvtType` event types, a 5-deep event queue and its
own task, `BmDfuErr`'s 15 values, and the pending-state-change mechanism.

**Port it sans-io.** Entry points take the current time and an event and return
what the caller owes the network. No task, no queue thread.

**This is where the DFU goldens pay off.** `dfu_test.cpp`'s `client_golden`,
`client_golden_image_has_updated` and `host_golden` are reference event and
state sequences — they drive `bm_dfu_test_set_dfu_event_and_run_sm` and assert
`get_current_state_enum`. Replicate those as `bm-wire` unit tests asserting the
same state enums. This is the one card where step 4 applies, in that form.

**Note.** `dfu_copy_and_process_message` copies the payload to the heap and
hands ownership to the event thread; if the message is not addressed to us and
the destination is link-local multicast, it forwards instead. The sans-io core
does not need the forwarding half, so it can be left to the `bm-stack`
integration.

## D3 — the DFU client

**Blocked by:** D2.

**C source.** `dfu_client.c` (661 LoC). State in `DfuClientCtx`: `image_size`,
`num_chunks`, `crc16` and `running_crc16`, a 2048-byte `img_page_buf` with
`img_page_byte_counter` and `img_flash_offset`, `chunk_retry_num` (max 5,
`bm_dfu_max_chunk_retries`), `current_chunk`, a 2000 ms `chunk_timer`
(`bm_dfu_client_chunk_timeout_ms`), the flash area handle and the host node id.
It persists `dfu_confirm` through the config store and keeps
`client_update_reboot_info` in no-init RAM behind magic `0xBADC0FFE`.

**New port seam.** The DFU flash slot in `bm-stack/src/port.rs`, matching
`bm_dfu_generic.h`'s flash-area open/write/erase/close. `bm-wire`'s `crc.rs`
already has `crc16_ccitt` and is currently unused — this is its first consumer.

**Note.** The no-init-RAM reboot handshake is a hardware seam the mock cannot
fully model. Express it as a trait with an explicit "survives reboot" contract
and document what the mock does instead.

## D4 — the DFU host

**Blocked by:** D2.

**C source.** `dfu_host.c` (482 LoC). `dfu_host_ctx_t`: a 10 s `ack_timer`
(`bm_dfu_host_ack_timeout_ms`) with `ack_retry_num` capped at 2
(`bm_dfu_max_ack_retries`), a 1 s `heartbeat_timer`, a 5 min `update_timer`
(`bm_dfu_update_default_timeout_ms` = 300000), `bytes_remaining`, a 3-slot data
queue and the finish callback.

**Done when.** A scripted host/client pair — Rust host against C client and the
reverse — completes an image transfer with identical frames at every step.

---

# Explicitly out of scope

- **The 15 message types bm_core declares but never handles**: `0x00` (Ack),
  `0x06`/`0x07` (protocol caps), `0x0C`/`0x0D` (neighbour proto), `0xB0`/`0xB1`
  (net state), `0xB2`/`0xB3` (power state), `0xC0`/`0xC1` (reboot), `0xC2`
  (net-assert-quiet). They have structs in `messages.h` and cases in
  `check_endianness`, but no `packet_add` call anywhere, so there is no handler
  to diff against and no deployed node that answers them. Revisit if bm_core
  implements them upstream.
- **`integrations/topology.c`** (674 LoC) — the network topology walk. A
  consumer of M4 rather than part of BCMP, with its own thread, timer and
  doubly-linked cursor. Unblocked now that M4 has landed, and it is where
  divergences #14, #35 and #36 all bite: it is the only caller of
  `bcmp_request_neighbor_table` in bm_core.
- **`middleware/pubsub.c`, `bm_service*.c` and the built-in services** — above
  the wire.
- **`bm-phy-adin2111`** — embassy#7024 is merged; `docs/embassy-port-tracking-prompt.md`
  keeps the design record.

---

# Keeping this current

When a card lands, delete it — git history is the record — and move anything
the next cards need into "What the landed cards left for the rest". When a card
turns up a C defect, the finding goes in `docs/c-divergences.md` and the card
references the number. If a card turns out to be two cards, split it here
before starting.
