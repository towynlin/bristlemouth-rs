# BCMP porting todo

What of BCMP is still unported, in dependency order, as task cards sized for one
agent each.

`bm-wire` currently carries four of BCMP's exchanges — heartbeat (`0x01`),
echo (`0x02`/`0x03`, both halves), device info (`0x04`/`0x05`, responder only)
and neighbour table (`0x08`/`0x09`, responder only) — plus the wire engine
under them (`bcmp::tx::serialize`, `bcmp::rx::accept`, L2 egress stamping, the
link-local RX policy, the two forwarding paths in `bcmp::forward`) and two
state machines,
`bm-wire/src/neighbor.rs` and `bm-wire/src/bcmp/registry.rs`. `MessageType` in
`bm-wire/src/bcmp/header.rs` already names all 45 of bm_core's constants, but
only seven body structs have a codec.

`bm_stack::Node` drives that registry: `Node::register` is `packet_add`,
`Node::request` is `bcmp_tx`, and `bm_stack::Event` is where a reply, a timeout
or an unsolicited message arrives. A card that adds an exchange has somewhere to
put its requester half.

Everything below is absent from Rust: no files, no stubs, no `TODO` markers.

## Status at a glance

| Area | C source | LoC | Card |
|---|---|---|---|
| Echo / ping `0x02`,`0x03` | `bcmp/ping.c` | 176 | M1 — **landed** |
| System time `0x10`–`0x12` | `bcmp/time.c` | 195 | M2 |
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

Dependency order: M2, M3, M4, M5, C1 and D1 are all unblocked and may run in
parallel. C2 needs C1. C3 needs C1 and C2. D2 needs D1; D3 and D4 each need D2.
M1 has landed.

---

## The shared contract

Read this before starting any card. It is the part that is easy to get wrong and
expensive to get wrong.

### Bring the oracle up first

The `bm_core` submodule is not checked out by a plain clone, and it has nested
submodules that tier T2 and above need:

```
git submodule update --init --recursive
```

Without this, `bm-wire-sys` does not build and every differential test fails for
the wrong reason.

### `bm-wire` rules

`no_std`, no `alloc`, `forbid(unsafe_code)`, **zero dependencies**, and it may
never depend on `bm-wire-sys` in any configuration. Use explicit little-endian
codecs, never `repr(packed)` mirrors — bm_core's own `check_endianness` is a
no-op on a little-endian host, so the byte order has to be written down
somewhere. The `std` feature exists only for tests and fuzzing.

### The recipe

`CLAUDE.md`'s "Porting a function to bm-wire" is the procedure, unchanged:

1. Read the C. Note anything that wraps, truncates, reads out of bounds, or
   contradicts its own doc comment.
2. Write the Rust, matching observable behaviour including the quirks.
3. Add a comparator in `bm-wire-diff` and a fuzz target that calls it.
4. Where bm_core's gtest suite asserts a value for the same input, assert that
   literal value in a `bm-wire` unit test too. **See the note below — for almost
   every card here, step 4 is not available.**
5. Where the C is undefined, constrain the comparator's input domain and say why
   at the type. Never relax the assertion.
6. Add the finding to `docs/c-divergences.md`.

### Step 4 is mostly unavailable, and that includes DFU

bm_core ships **no on-wire gold vectors for any BCMP message type**. Its
message-level tests (`packet_test.cpp`, `ping_test.cpp`, `info_test.cpp`,
`neighbors_test.cpp`, `time_test.cpp`, `config_test.cpp`,
`resource_discovery_test.cpp`) build randomized C structs and compare against C
structs again, so there is nothing to lift.

Literal byte arrays exist in only five test files, and most are inputs rather
than expected encodings:

| Test file | What the hex actually is | Useful as a gold vector? |
|---|---|---|
| `bm_linux_test.cpp` | Real captured frames with the checksum a live node agreed on | **Yes** — already harvested into `bm-wire-diff/src/gold_vectors.rs` |
| `l2_policy_test.cpp` | 16-byte IPv6 address constants | Partly; the l2_policy port already uses them |
| `configuration_test.cpp` | A 10-byte test payload being stored | No |
| `cbor_service_helper_test.cpp` | Small CBOR fragments | Marginal; useful for C1 |
| `pcap_test.cpp` | A pcap file header | Not BCMP |

`dfu_test.cpp`'s `client_golden` and `host_golden` are **state-machine
transition traces**, not byte vectors: they build `BcmpDfuStart` structs in
memory and assert state enums such as `BmDfuStateIdle`. They are genuinely
valuable — for D2 they are the reference event/state sequence — but they prove
nothing about the wire encoding. Do not go looking for DFU frame bytes; there
are none.

So for every card below, ground truth for the encoding comes from running the
compiled oracle, not from lifting fixtures. That is what the comparator is for.
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
- After injecting, use `stack::pump_until_quiet`, not a bare `bm_shim_pump`: one
  pump runs each task once in creation order, so a received frame reaches L2,
  then BCMP, then L2 again over successive pumps.
- Everything in `bm-wire-sys/csrc/` stays deterministic — no threads, sockets,
  wall clock or randomness. `check_symbols.sh --check` deliberately omits `rand`
  and `time` from its libc allowlist so a non-deterministic reach trips CI.

### The sequence-list hazard

`bm-wire-diff/src/bcmp.rs` registers its 25 message types as `sequenced_reply`
**specifically** so that the C's `sequence_list` never grows, which is what lets
the `bcmp` fuzz target run in-process instead of needing fork mode.

`bcmp/config.c` is the **only** module in bm_core that sets
`BcmpPacketCfg::sequenced_request`. Its ten registrations at `config.c:789-835`
use positional initializers `{sequenced_reply, sequenced_request, process}`:
get, set, status-request, delete-request and clear-request are
`sequenced_request`; value, status-response, delete-response and clear-response
are `sequenced_reply`; commit is neither.

**Registering any of them in `bcmp.rs` would break the in-process property.**
Card C3 must live in a stack-target binary of its own.

One sequenced request already exists outside it:
`our_sequenced_request_carries_the_number_the_c_would_have_given_it` in
`bm-wire-diff/tests/node_frames.rs` sends three `BcmpConfigGetMessage`s through
`bcmp_tx` to pin the sequence number a request carries. `message_count` is a
function-level `static` with nothing that resets it, so that test assumes it is
the only sequenced sender in its binary. A card adding another to
`node_frames.rs` has to say where the counter had got to — or put its
comparison in a binary of its own, which is what C3 is doing anyway.

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

When a fuzzer finds a crash: `cargo fuzz tmin <target> <artifact>`, drop the
minimized file into `bm-wire/fuzz/seeds/<target>/`, and it becomes a permanent
regression test.

---

## Card format

Each card gives: the C source, the Rust to create, the comparator and fuzz
target, any new port seam, the C quirks to reproduce, what blocks it, and what
"done" means.

---

# Messages bm_core implements

## M1 — echo / ping, `0x02` and `0x03` — **landed**

**What landed.** `bm-wire/src/bcmp/ping.rs` (the two codecs, plus
`EchoRequest::into_reply` for the C's in-place cast and `EchoReply::answers`
for its acceptance rule), `Node::ping` and the single-slot state on
`bm_stack::Node` behind a new `PING_PAYLOAD` const generic,
`Node::Event::EchoReply`, the comparator in `bm-wire-diff/src/ping.rs` driven
from `bm-wire-diff/tests/ping.rs`, twelve seeds, and divergences #27 to #30.

It also found divergence #12's live case. Ping is the first exchange whose
frames vary freely enough to land on the 0.0122% of stamped BCMP checksums the
C's egress patch gets wrong, and `cargo fuzz run ping` found one in two
minutes. It arrived as the comparator failing rather than the port —
classifying captured frames with `rx::accept` was the wrong tool, because some
of the C's frames correctly do not validate. `seeds/ping/reply-checksum-double-carry`
keeps the input.

Two corrections to the card as it was written, both found by doing it:

- **It is a stack target, not a `bcmp.rs` one.** The card says registering
  `0x02`/`0x03` in `bm-wire-diff/src/bcmp.rs` keeps `ping` in
  `replay::TARGETS`, and registering them there would indeed be safe — but it
  would not help. The only public encoder in `bcmp/ping.c` is
  `bcmp_send_ping_request`, which goes through `bcmp_tx` → `bm_ip_tx_new`, and
  the `packet.c`-only oracle has no IP layer under it. Anything reachable from
  that oracle is `serialize`, which the `bcmp` target already covers. So `ping`
  brings the whole stack up, lives in `replay::STACK_TARGETS`, and has its own
  test binary. It needs no `-fork=1`: nothing accumulates.
- **The reply does *not* carry the request's `seq_num` in the header.**
  `bcmp_send_ping_reply` passes it to `bcmp_tx`, and `serialize` discards it,
  because `ping_init` registers the type as neither `sequenced_reply` nor
  `sequenced_request` — so the header's number is zero. The echo survives in
  the *body*, where the in-place buffer reuse preserved it. Divergence #29.
- **The reply does not arrive as `Event::Reply`** either, for the reason the
  card's own next paragraph gives: `0x03` is unsequenced, so it comes through
  `Node::submit`'s dispatch as `Event::Message`. `Event::EchoReply` follows it
  when `ping.c`'s rule accepts it, and is new.

Step 4 of the recipe was unavailable, as the shared contract predicted:
`bm_core` has no gold vectors for `0x02`/`0x03`, and no `ping_test.cpp` at all.
Ground truth for the encoding is the compiled oracle. One further thing has no
oracle either — `bcmp_process_ping_reply` is `static`, transmits nothing and
reports to nobody, so the acceptance rule is ported by reading and asserted in
`bm_wire::bcmp::ping`'s and `bm-stack`'s unit tests. That is divergence #30.

**Blocked by:** nothing — I3 landed. `Node::request` sends the echo request and
`Node::Event::Reply` is where the echo reply arrives; ping's own single-slot
state (`EXPECTED_PAYLOAD`, `BCMP_SEQ`) is what this card adds on top, because
`0x02`/`0x03` are registered unsequenced and so are matched by `ping.c` rather
than by `packet.c`.

**C source.** `bcmp/ping.c` (176 LoC). Four file-scope statics at
`ping.c:10-13` — `PING_REQUEST_TIMEOUT`, `BCMP_SEQ`, `EXPECTED_PAYLOAD` (a heap
copy, freed and reallocated per request) and `EXPECTED_PAYLOAD_LEN`. Only one
outstanding ping is tracked at a time.

**Quirks to reproduce.**

- The request id is `(uint16_t)node_id()` — the node id truncated to 16 bits.
- `BCMP_SEQ` at `ping.c:11` is a `uint32_t` counter stored into a `uint16_t`
  field at `ping.c:43`, and is a **second sequence space** entirely independent
  of `packet.c`'s `message_count`.
- A reply is accepted only if `payload_len` matches, the id matches, and the
  payload bytes compare equal.
- The reply handler reuses the request buffer in place, casting it to
  `BcmpEchoReply` after overwriting `target_node_id`.
- The reply carries the request's `seq_num` in the BCMP header even though the
  cfg is not sequenced.
- Divergence #2 already records that `packet.c:70` applies `swap_32bit` to
  `BcmpEchoRequest::seq_num`, which is `uint16_t`. Note in the writeup that on a
  big-endian host this also clobbers the adjacent `payload_len`; the reply path
  uses `swap_16bit` correctly.

**Rust to create.** `bm-wire/src/bcmp/ping.rs` for the two codecs; the
single-outstanding-request state in `bm-stack` on top of I3.

**Comparator and fuzz target.** `bm-wire-diff/src/ping.rs`, target `ping`, seeds
`bm-wire/fuzz/seeds/ping/`. Registering `0x02`/`0x03` is safe in `bcmp.rs` (both
are unsequenced), so this can stay in `replay::TARGETS`. Add `ping` to the fuzz
matrix.

**Why it is first.** Ping is the first thing anyone does to a node that is not
answering. It is also the smallest complete request/reply in BCMP, which makes
it the right card to prove I3 on.

## M2 — system time, `0x10`, `0x11`, `0x12`

**Blocked by:** nothing — I2 landed. `bm_wire::bcmp::forward::egress_ports` and
`bm_stack::Node::forward_link_local` are the re-flood this card needs; call the
second once per port of the first, transmitting each frame before building the
next, because there is one transmit buffer.

**C source.** `bcmp/time.c` (195 LoC), one handler
(`bcmp_time_process_time_message`) for all three types. No module state; it
leans on `bm_rtc_get`/`bm_rtc_set` and `date_time_from_utc` in `common/util.c`,
both of which `bm-wire/src/util.rs` already ports.

**New port seam.** An `Rtc` trait in `bm-stack/src/port.rs`. That file's header
comment already anticipates it: "Configuration storage, the RTC and the DFU
flash slot are seams too, and they will arrive with the code that uses them."
This is that code. Follow the existing `Phy`/`Identity` shape.

**Quirks to reproduce.** A message whose `target_node_id` is neither ours nor 0
is re-flooded via `bcmp_ll_forward`, which is already ported — note divergences
#23, #24 and #26, which the forward carries with it. A *request* with
`target_node_id == 0` reaches the switch and is then dropped by an inner
exact-match check, so broadcast time requests are silently ignored: probably a
divergence. All transmits go to `multicast_ll_addr` with `seq_num` 0.

**Done when.** The three codecs round-trip against the oracle, and a `Node` with
a mock RTC answers a time request byte-identically to the C — add the case to
`bm-wire-diff/tests/node_frames.rs`. The forward path is already compared per
port by `bm-wire-diff/tests/forward.rs`, which calls `bcmp_ll_forward` directly;
what M2 adds is the *decision* to forward, so extend `check_relay` there with a
system-time message whose target is another node, and it will compare the C's
whole receive path against `Node::on_frame`.

## M3 — device-info reply consumption, `0x05` receive side

**Blocked by:** nothing — I3 landed. `0x05` is registered unsequenced, so the
reply arrives as `Node::Event::Message` and reaches `Node::submit`'s match on
message type; the cache this card adds hangs off that arm.

**The gap.** `bm-wire/src/bcmp/info.rs` already decodes `DeviceInfoReply`, and
`Node` already emits a request when it discovers a neighbour. Nothing consumes
the reply — there is no info cache and no topology table.

**C source.** `bcmp/info.c` (264 LoC), `bcmp_process_info_reply` plus the
`INFO_REQUEST_LIST` (node id → callback) and `INFO_EXPECT_NODE_ID`.

**Quirks.** Divergence #19 already records that `INFO_REQUEST_LIST` grows with
no de-duplication and no expiry — entries are removed only when a matching reply
arrives, so an unanswered request leaks its list item. This is why the `neighbor`
fuzz target needs `-fork=1`, and the same will apply here. Divergence #14 covers
the variable-length parse copying per attacker-declared lengths without
consulting `BcmpProcessData.size`; keep the comparator's malformed inputs away
from the C, as `bcmp_messages.rs` already does with `decode_probe`.

**Done when.** A `Node` that requested info from a neighbour caches the reply,
and the cache contents match what the C's callback reports for the same frames.

## M4 — neighbour-table reply consumption, `0x09` receive side

**Blocked by:** nothing — I3 landed, on the same terms as M3: `0x09` is
unsequenced, so the reply comes through `Node::submit`'s match rather than
through the registry.

**The gap.** Same shape as M3: `NeighborTableReply` decodes,
`build_neighbor_table_reply` answers, but no requester side exists and received
replies are ignored.

**C source.** `bcmp/neighbors.c` (489 LoC), `bcmp_request_neighbor_table` and
`bcmp_process_neighbor_table_reply`. State: `NEIGHBOR_REQUEST_CB` is single-shot
and cleared after use; `TARGET_NODE_ID` gates acceptance — only the neighbour
whose `node_id` matches is accepted; `NEIGHBOR_TIMER` is a one-shot 1 s timer
deleted and recreated on each request. The reply is capped at
`bcmp_table_max_len` = 1024 bytes.

**Quirks.** Divergences #14 (the parse) and #15 (`bcmp_remove_neighbor_from_table`
frees despite its doc and returns the free's result as the removal's — found as
a double free) are recorded and must keep being matched. Divergence #18
(`bcmp_find_neighbor` never matches node id 0) is directly relevant to what the
requester accepts.

**Done when.** A requester `Node` walking a two-node table produces the same
accepted/rejected decisions as the C for every seed, including the node-id-0
case.

## M5 — resource discovery, `0x0A` and `0x0B`

**Blocked by:** nothing — I3 landed. As with M3 and M4, both types are
unsequenced, so the requester half is `Node::request` plus a match arm.

**C source.** `bcmp/resource_discovery.c` (444 LoC). Two `BcmpResourceList`s —
`PUB_LIST` and `SUB_LIST` — each a singly-linked list with its own semaphore
(default take timeout 100 ms), plus `RESOURCE_REQUEST_LIST` with no expiry.

**Wire format.** `BcmpResourceTableReply` is `node_id`, `num_pubs`, `num_subs`,
then `num_pubs + num_subs` variable-length `BcmpResource { uint16 len; char[] }`
records, **publishers first**. This is the most variable-length message in BCMP
after the neighbour table; the same domain-limiting discipline as divergence #14
applies.

**Two probable new divergences.**

- `resource_discovery.c:105` rejects the request unless
  `req->target_node_id != node_id()` fails — i.e. it does **not** treat
  `target_node_id == 0` as a broadcast, unlike ping, info and neighbours. Verify
  and record.
- On a populate failure the handler `break`s before `bm_free`, leaking
  `reply_buf`. This is `c-only` (no Rust counterpart) but belongs in the list.

**Rust to create.** `bm-wire/src/bcmp/resource.rs` for the codecs and a
fixed-capacity resource table; the two lists become one sans-io structure.

**Done when.** Codecs round-trip, the broadcast asymmetry is recorded, and the
table's add/find behaviour matches the C under a scripted comparator.

---

# The config chain

## C1 — a `no_std`, alloc-free CBOR codec

**Blocked by:** nothing.

**Why this card exists at all.** Config values are CBOR-encoded, and `bm-wire`
may not take a dependency. There is no way to port C3 without first having a
CBOR reader and writer that are `no_std`, alloc-free and dependency-free. This
is the hidden cost inside "port config" and the reason C3 is not a single card.

**Scope it to bm_core's actual use.** bm_core builds tinycbor with
`CBOR_PARSER_MAX_RECURSIONS=10` and a custom allocator shim. Read
`bcmp/configuration.c` and `middleware/cbor_service_helper.c` to find which
major types and encodings are genuinely reachable, and implement that subset
rather than all of RFC 8949. Say at the type what is not supported.

**Comparator and fuzz target.** Diff against tinycbor through the oracle;
tinycbor is already compiled into tier T2. `cbor_service_helper_test.cpp` has a
couple of small CBOR fragments worth asserting literally. Target `cbor`, seeds
in `replay::TARGETS` (tinycbor is pure — no shim state needed).

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

**Gold vectors.** `configuration_test.cpp` is one of the few files with literal
hex, but note what it is: a 10-byte test payload being stored, not an expected
encoding. Treat the CRC32 and partition-header layout as the things to pin, and
derive their expected bytes from the oracle.

**Done when.** A partition written by the Rust store is byte-identical to one
written by the C for the same key sequence, CRC included, and both reject the
same corrupt images.

## C3 — config over BCMP, `0xA0`–`0xA9`

**Blocked by:** C1, C2. I2 landed, so the re-flood these messages need is
`bm_stack::Node::forward_link_local`.

**C source.** `bcmp/config.c` (857 LoC), one handler
(`bcmp_process_config_message`) for all ten types.

**Read the sequence-list hazard in the shared contract before starting.** This
is the only module that uses `packet.c`'s sequence machinery, and registering
its types in `bm-wire-diff/src/bcmp.rs` would break the `bcmp` fuzz target's
in-process property. C3 gets its own binary under `bm-wire-diff/tests/` and its
seeds go in `replay::STACK_TARGETS`.

The flags, from the positional initializers at `config.c:789-835`
(`{sequenced_reply, sequenced_request, process}`):

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
  struct size rather than being a byte offset. Any status response carrying more
  than one key walks off into hyperspace. Confirm the exact consequence against
  the oracle and record it.
- Messages not addressed to this node are forwarded — hence the I2 dependency.
- Divergence #22 applies here first and hardest: a sequenced request's real
  timeout is the 150 ms sweep it lands before, anywhere from 25 ms to 174 ms,
  and the ten messages below are the only ones in bm_core that use it.

**Test coverage to expect.** `config_test.cpp` has exactly two cases — `decode`
and `ClearPartitionRequest`. Eight of the ten messages have no C test at all, so
the comparator is the only thing standing between this port and a silent
divergence. Budget accordingly.

---

# DFU

1832 LoC across three files, the heaviest state in bm_core. Split four ways so
no single card is unreviewable.

## D1 — DFU message codecs, `0xD0`–`0xD9`

**Blocked by:** nothing.

**C source.** `bcmp/dfu_message_structs.h` (73 LoC) — `BmDfuImgInfo`,
`BmDfuFrameHeader`/`BmDfuFrame`, `BmDfuEventAddress`, `BmDfuEventChunkRequest`,
`BmDfuEventImageChunk`, `BmDfuEventResult`, `BmDfuEventImgInfo` — plus the ten
`Bcmp Dfu*` wrappers in `bcmp/messages.h`. Pure wire format, no state.

**Quirk to record.** `dfu_core.c:621` and `dfu_core.c:623` both call
`packet_add` for type `0xD9`, once as `BcmpDFUBootCompleteMessage` and once as
its alias `BcmpDFULastMessageMessage`, leaving a duplicate entry in the registry
linked list. Worth an entry, and worth checking what the duplicate does to
dispatch.

**Done when.** All ten codecs round-trip against the oracle under a `dfu_codec`
fuzz target. `bm_dfu_max_chunk_size` is 1024, so the comparator's body bound
must accommodate it.

## D2 — the DFU core state machine

**Blocked by:** D1.

**C source.** `dfu_core.c` (689 LoC): `LibSmContext` over the 10 states of
`BmDfuHfsmStates`, 15 `BmDfuEvtType` event types, a 5-deep event queue and its
own task, `BmDfuErr`'s 15 values, and the pending-state-change mechanism.

**Port it sans-io.** Entry points take the current time and an event, and return
what the caller owes the network. No task, no queue thread.

**This is where the DFU goldens pay off.** `dfu_test.cpp`'s `client_golden`,
`client_golden_image_has_updated` and `host_golden` are reference **event and
state sequences** — they drive `bm_dfu_test_set_dfu_event_and_run_sm` and assert
`get_current_state_enum`. Replicate those sequences as `bm-wire` unit tests
asserting the same state enums. This is the one card where step 4 of the recipe
applies, in that modified form.

**Note.** `dfu_copy_and_process_message` copies the payload to the heap and
hands ownership to the event thread; if the message is not addressed to us and
the destination is link-local multicast, it forwards instead. The forwarding
half needs I2; the sans-io core does not, so D2 can land before I2 if the
forward arm is left to the `bm-stack` integration.

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
already has `crc16_ccitt` and is currently used by nothing — this is its first
consumer.

**Note.** The no-init-RAM reboot handshake is a genuine hardware seam, not
something the mock can fully model. Express it as a trait with an explicit
"survives reboot" contract and say in the docs what the mock does instead.

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
  to diff against and no deployed node that answers them. Implementing them
  would put `bm-wire` ahead of the oracle with nothing to prove compatibility
  against — which is the opposite of what this repo is for. Revisit if bm_core
  implements them upstream.
- **`integrations/topology.c`** (674 LoC) — the network topology walk. A
  *consumer* of M4 rather than part of BCMP, with its own thread, timer and
  doubly-linked cursor structure. A natural follow-on once M4 lands.
- **`middleware/pubsub.c`, `bm_service*.c` and the built-in services** — above
  the wire.
- **`bm-phy-adin2111` and embassy#7024** — tracked in
  `docs/embassy-port-tracking-prompt.md`.

---

# Keeping this current

When a card lands, delete it rather than marking it done — git history is the
record. When a card turns up a C defect, the finding goes in
`docs/c-divergences.md` and the card references the number. If a card turns out
to be two cards, split it here before starting, so the next agent inherits the
smaller piece.
