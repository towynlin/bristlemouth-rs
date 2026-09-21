# BCMP porting todo

What of BCMP is still unported, in dependency order, as task cards sized for
one agent each.

`bm-wire` carries five of BCMP's exchanges — heartbeat (`0x01`), echo
(`0x02`/`0x03`, both halves), device info (`0x04`/`0x05`, responder only),
neighbour table (`0x08`/`0x09`, responder only) and system time (`0x10`–`0x12`,
both halves) — plus the wire engine under them (`bcmp::tx::serialize`,
`bcmp::rx::accept`, L2 egress stamping, the link-local RX policy, the two
forwarding paths in `bcmp::forward`) and two state machines,
`bm-wire/src/neighbor.rs` and `bm-wire/src/bcmp/registry.rs`. `MessageType`
names all 45 of bm_core's constants; eleven body structs have a codec.

`bm_stack::Node` drives that registry: `Node::register` is `packet_add`,
`Node::request` is `bcmp_tx`, and `bm_stack::Event` is where a reply, a timeout
or an unsolicited message arrives.

Everything below is absent from Rust — no files, no stubs, no `TODO` markers.

## Status at a glance

| Area | C source | LoC | Card |
|---|---|---|---|
| Device-info reply consumption `0x05` | `bcmp/info.c` | 264 | M3 |
| Neighbour-table reply consumption `0x09` | `bcmp/neighbors.c` | 489 | M4 |
| Resource discovery `0x0A`,`0x0B` | `bcmp/resource_discovery.c` | 444 | M5 |
| CBOR codec | `third_party/tinycbor` | — | C1 |
| Local config store | `bcmp/configuration.c` | 845 | C2 |
| Config over BCMP `0xA0`–`0xA9` | `bcmp/config.c` | 857 | C3 |
| DFU message codecs `0xD0`–`0xD9` | `bcmp/dfu_message_structs.h` | — | D1 |
| DFU core HFSM | `bcmp/dfu_core.c` | 689 | D2 |
| DFU client | `bcmp/dfu_client.c` | 661 | D3 |
| DFU host | `bcmp/dfu_host.c` | 482 | D4 |

M3, M4, M5, C1 and D1 are unblocked and may run in parallel. C2 needs C1. C3
needs C1 and C2. D2 needs D1; D3 and D4 each need D2.

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

`no_std`, no `alloc`, `forbid(unsafe_code)`, zero dependencies, and it may never
depend on `bm-wire-sys` in any configuration. Use explicit little-endian
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
| `cbor_service_helper_test.cpp` | Small CBOR fragments | Marginal; useful for C1 |
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
cargo tree -p bm-wire                         # must show no dependencies
./bm-wire-sys/scripts/check_symbols.sh --check
cd bm-wire/fuzz && mkdir -p corpus/<target>
cd bm-wire/fuzz && cargo fuzz run <target> corpus/<target> seeds/<target>
```

On a crash: `cargo fuzz tmin <target> <artifact>`, then drop the minimized file
into `bm-wire/fuzz/seeds/<target>/`.

### What the landed cards (M1, M2) left for the rest

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

---

## Card format

Each card gives: the C source, the Rust to create, the comparator and fuzz
target, any new port seam, the C quirks to reproduce, what blocks it, and what
"done" means.

---

# Messages bm_core implements

## M3 — device-info reply consumption, `0x05` receive side

**Blocked by:** nothing. `0x05` is registered unsequenced, so the reply arrives
as `Node::Event::Message` and reaches `Node::submit`'s match on message type;
the cache this card adds hangs off that arm.

**The gap.** `bm-wire/src/bcmp/info.rs` already decodes `DeviceInfoReply`, and
`Node` already emits a request when it discovers a neighbour. Nothing consumes
the reply — there is no info cache and no topology table.

**C source.** `bcmp/info.c` (264 LoC), `bcmp_process_info_reply` plus
`INFO_REQUEST_LIST` (node id → callback) and `INFO_EXPECT_NODE_ID`.

**Quirks.** Divergence #19: `INFO_REQUEST_LIST` grows with no de-duplication and
no expiry, which is why the `neighbor` fuzz target needs `-fork=1` and why this
one will too. Divergence #14: the variable-length parse copies per
attacker-declared lengths without consulting `BcmpProcessData.size`; keep the
comparator's malformed inputs away from the C, as `bcmp_messages.rs` does with
`decode_probe`.

**Done when.** A `Node` that requested info from a neighbour caches the reply,
and the cache contents match what the C's callback reports for the same frames.

## M4 — neighbour-table reply consumption, `0x09` receive side

**Blocked by:** nothing, on the same terms as M3.

**The gap.** `NeighborTableReply` decodes and `build_neighbor_table_reply`
answers, but no requester side exists and received replies are ignored.

**C source.** `bcmp/neighbors.c` (489 LoC), `bcmp_request_neighbor_table` and
`bcmp_process_neighbor_table_reply`. State: `NEIGHBOR_REQUEST_CB` is single-shot
and cleared after use; `TARGET_NODE_ID` gates acceptance; `NEIGHBOR_TIMER` is a
one-shot 1 s timer deleted and recreated per request. The reply is capped at
`bcmp_table_max_len` = 1024 bytes.

**Quirks.** Divergences #14 (the parse) and #15
(`bcmp_remove_neighbor_from_table` frees despite its doc and returns the free's
result). Divergence #18 (`bcmp_find_neighbor` never matches node id 0) is
directly relevant to what the requester accepts.

**Done when.** A requester `Node` walking a two-node table produces the same
accepted/rejected decisions as the C for every seed, including the node-id-0
case.

## M5 — resource discovery, `0x0A` and `0x0B`

**Blocked by:** nothing. Both types are unsequenced, so the requester half is
`Node::request` plus a match arm.

**C source.** `bcmp/resource_discovery.c` (444 LoC). Two `BcmpResourceList`s —
`PUB_LIST` and `SUB_LIST` — each a singly-linked list with its own semaphore
(default take timeout 100 ms), plus `RESOURCE_REQUEST_LIST` with no expiry.

**Wire format.** `BcmpResourceTableReply` is `node_id`, `num_pubs`, `num_subs`,
then `num_pubs + num_subs` variable-length `BcmpResource { uint16 len; char[] }`
records, **publishers first**. The most variable-length message in BCMP after
the neighbour table; divergence #14's domain-limiting discipline applies.

**Two probable new divergences.**

- `resource_discovery.c:105` rejects the request unless
  `req->target_node_id != node_id()` fails — so it does **not** treat
  `target_node_id == 0` as a broadcast, unlike ping, info and neighbours.
  Verify and record.
- On a populate failure the handler `break`s before `bm_free`, leaking
  `reply_buf`. `c-only`, but it belongs in the list.

**Rust to create.** `bm-wire/src/bcmp/resource.rs` for the codecs and a
fixed-capacity resource table; the two lists become one sans-io structure.

**Done when.** Codecs round-trip, the broadcast asymmetry is recorded, and the
table's add/find behaviour matches the C under a scripted comparator.

---

# The config chain

## C1 — a `no_std`, alloc-free CBOR codec

**Blocked by:** nothing.

Config values are CBOR-encoded and `bm-wire` may not take a dependency, so C3
is impossible without a CBOR reader and writer that are `no_std`, alloc-free
and dependency-free.

**Scope it to bm_core's actual use.** bm_core builds tinycbor with
`CBOR_PARSER_MAX_RECURSIONS=10` and a custom allocator shim. Read
`bcmp/configuration.c` and `middleware/cbor_service_helper.c` to find which
major types and encodings are reachable, and implement that subset rather than
all of RFC 8949. Say at the type what is not supported.

**Comparator and fuzz target.** Diff against tinycbor through the oracle;
tinycbor is already in tier T2. `cbor_service_helper_test.cpp` has a couple of
small fragments worth asserting literally. Target `cbor`, seeds in
`replay::TARGETS` (tinycbor is pure — no shim state needed).

**Done when.** Encode and decode agree with tinycbor over the fuzzed input
domain, the unsupported subset is documented, and `cargo tree -p bm-wire` still
shows no dependencies.

## C2 — the local config store

**Blocked by:** C1.

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

**Blocked by:** C1, C2. The re-flood these messages need is
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
  doubly-linked cursor. A natural follow-on once M4 lands.
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
