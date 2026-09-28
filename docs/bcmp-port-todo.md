# BCMP porting todo

What of BCMP is still unported, in dependency order, as task cards sized for
one agent each.

`bm-wire` carries six of BCMP's exchanges — heartbeat (`0x01`), echo
(`0x02`/`0x03`, both halves), device info (`0x04`/`0x05`, both halves),
neighbour table (`0x08`/`0x09`, both halves), resource discovery
(`0x0A`/`0x0B`, both halves), system time (`0x10`–`0x12`, both halves) and
config (`0xA0`–`0xA9`, all ten) — plus the DFU body codecs (`0xD0`–`0xD9`,
`bm-wire/src/bcmp/dfu.rs`), the DFU core state machine over them
(`bm-wire/src/bcmp/dfu_core.rs`), its client
(`bm-wire/src/bcmp/dfu_client.rs`) and its host
(`bm-wire/src/bcmp/dfu_host.rs`), both of which a `bm-stack` node runs, and
the wire engine under them
(`bcmp::tx::serialize`, `bcmp::rx::accept`, L2 egress stamping, the link-local
RX policy, the two forwarding paths in `bcmp::forward`) and three state
machines, `bm-wire/src/neighbor.rs`, `bm-wire/src/bcmp/registry.rs` and
`bm-wire/src/bcmp/resource.rs`, plus the local config store,
`bm-wire/src/configuration.rs`, and the config message codecs,
`bm-wire/src/bcmp/config.rs`.
`MessageType` names all 45 of bm_core's constants; twenty-four body structs have
a codec, and the ten DFU bodies share one, `DfuMessage`. CBOR, which the
config chain needs, is the `cbor2` crate rather than a port;
`bm-wire/src/cbor.rs` holds only the one deviation from its defaults that
bm_core requires.

`bm_stack::Node` drives that registry: `Node::register` is `packet_add`,
`Node::request` is `bcmp_tx`, and `bm_stack::Event` is where a reply, a timeout
or an unsolicited message arrives.

## Status at a glance

Every card has landed. What bm_core has that the port does not is listed
under "Explicitly out of scope". A new card goes here, in the format below.

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
state enums. They prove nothing about the wire encoding. D2 lifted the steps
the core decides into `bm-wire/src/bcmp/dfu_core/tests.rs`, with stand-in roles
making the calls the C roles make; D3 lifted the client's into
`bm-wire/src/bcmp/dfu_client/tests.rs`, where `client_golden`'s `crc16` of
0x2fdf over 2048 bytes of 0xa5 is the one image checksum bm_core asserts; D4
lifted the host's four goldens into `bm-wire/src/bcmp/dfu_host/tests.rs`.

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
C3 lives in a stack-target binary of its own (`bm-wire-diff/tests/config.rs`),
where the oracle's own `bcmp_config_init` registers them and the comparator
never issues a sequenced request through `bcmp.rs`.

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

### What the landed cards (M1, M2, M3, M4, M5, C1, C2, C3, D1, D2, D3, D4) left for the rest

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
  and has to be. C2's config store resets through `config_init` instead; see
  below.
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
- **Indefinite-length string reassembly is done, by hand.**
  `bm_wire::configuration::copy_string` is tinycbor's `iterate_string_chunks`,
  including the chunks it copies before failing (divergence #49). It does not
  use cbor2, whose string readers need `alloc`.
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
- **The config store is a byte image, and C3 should keep it one.**
  `bm_wire::configuration::ConfigPartition` holds the packed `ConfigPartition`
  struct and edits it where the C does, because stale bytes past `numKeys`,
  past a NUL and past a short value all reach flash and the CRC. Its methods
  are the C's functions (`set_cbor` is `set_config_cbor`, ...); `ConfigStore`
  is `CONFIGS`.
- **Pick the layout.** The image's layout is the compiler's (divergence #44):
  `Layout::LP64` for the oracle, `Layout::ARM_EABI_GCC` for a dev kit whose C
  firmware was built with `arm-none-eabi-gcc`'s defaults. A node never sends
  the image, so C3 is unaffected on the wire; a firmware inheriting a C node's
  flash is not.
- **`bcmp/config.c` passes keys with no NUL.** `set_config_cbor` gets
  `msg->keyAndData`, so the stored `key_buf` carries the value's first bytes
  (divergence #45). `Key::with_len(key_and_data, key_length)` reproduces it;
  `Key::new(key)` does not.
- **A `ConfigValue` reply carries the whole 50-byte slot.** `get_config_cbor`
  returns it stale tail and all, and `ConfigPartition::get_cbor` does too
  (divergence #49).
- **At 50 keys a `ConfigSet` still overwrites** — `set_config_cbor` looks the
  key up before checking the count; the typed setters do not (divergence #46).
- **`CONFIGS` resets through its own front door.** `config_init` does not touch
  `needs_commit`, so `bm-wire-diff/src/configuration.rs` saves any uncommitted
  partition, writes zeros to the shim's flash, then calls `config_init`, at the
  start of every script. The RAM image is readable through
  `get_stored_keys`, whose pointer is 9 bytes into the partition. C3's
  stack binary can do the same.
- **`bm_config_*` is the third integrator seam with no oracle.**
  `bm_stack::port::ConfigStorage` is the trait and `RamConfigStorage` the
  RAM implementation; both sides of the comparator are handed the same bytes.
  Never pass `restart = true` to `save_config` in a comparator: the shim's
  `bm_config_reset` clears every partition, `RamConfigStorage::reset` does
  nothing, and on hardware it reboots. `config.c` commits with `restart =
  true`, so C3 has to decide what that means in a stack test.
- **#42's uninitialised read needs a crafted image.** A CRC-valid image can
  put unparseable bytes in a listed key's slot (divergence #48 domain); no
  other route to a `get_config_cbor` failure for a listed key was found.
- **The config exchange is ported (C3).** `bm-wire/src/bcmp/config.rs` has the
  ten codecs plus `decode_value` and `encode_status_response`;
  `bm_stack::Node::with_config` answers `0xA0`–`0xA9` from a
  `bm_stack::Configuration`, and its requester functions
  (`Node::config_get` and siblings) issue them. `bm-wire-diff/src/config.rs`
  drives the whole thing through the oracle stack, seeding both stores the same
  way; `bm-wire-diff/tests/config.rs` is its binary and its seeds are in
  `replay::STACK_TARGETS`. DFU forwards exactly as config does.
- **Config is the only sequenced exchange, and `Node::new` registers it.** Its
  five requests are `PacketCfg::REQUEST`, its four answers `PacketCfg::REPLY`,
  `0xA3` neither. So a `bm-stack` node already carries them; a card does not
  re-register them, and the sequenced retry/timeout of divergence #22 is now
  reachable in ordinary operation, not just from a borrowed type.
- **A stack comparator that seeds `CONFIGS` NUL-terminates keys for the C.**
  The C setters take a `const char *` and `snprintf("%s")` it; a bare Rust slice
  over-reads past the key into stack memory. `Seed::apply_c` in
  `bm-wire-diff/src/config.rs` appends a NUL. This is only for seeding through
  the C API directly; a key that arrives over the wire carries its own bytes and
  the over-read past it is divergence #45.
- **`restart = true` is honoured in the stack test, and its flash is not
  compared.** `bcmp/config.c`'s commit calls `save_config(partition, true)`;
  `bm_stack::Config::commit` does the same. The shim's `bm_config_reset` then
  zeros every partition's flash while `RamConfigStorage::reset` does nothing, so
  `bm-wire-diff/src/config.rs` compares the RAM images and `needs_commit` after
  a commit but skips the flash — an integrator-seam difference, not a wire one.
- **The config partition byte and body lengths are unchecked in the C**
  (divergences #50, #51). `Partition::from_u8` bounds the first and the codecs
  bound the second; the comparator keeps the partition in range for every type
  but the clear request, which is the one that checks it.

- **The DFU codecs are ported (D1).** `bm_wire::bcmp::dfu::DfuMessage` is all
  ten bodies. `decode` dispatches on the body's `frame_type` byte, as
  `bm_dfu_process_message` does, not on the BCMP header type (divergence #54);
  `dfu_core::EventType::for_frame_type` does the same. `DfuAddress::of_body` is the read
  `dfu_copy_and_process_message` makes before it looks at the type, for the
  process-or-forward decision. `DfuAddress` is source first, and has no
  broadcast: only an exact `dst_node_id` match is acted on.
- **DFU reads bodies without checking their length** (divergence #55). The
  decoders refuse short bodies; a stack comparator must send only
  well-formed ones, and a chunk whose `payload_length` fits the frame.
- **`chunk_size` zero divides by zero in the client** (divergence #57). The
  client gives Cortex-M's `UDIV`-by-zero result; the comparator sends the C 1
  instead. The host divides by nothing: it sends empty chunks and never
  advances, and the port does the same.
- **`bm_dfu_init` registers `0xD9` twice** (divergence #56). The second entry is
  shadowed; a node mirroring the registration needs eleven registry slots.
- **Every DFU sender is compared as frames** in
  `bm-wire-diff/tests/dfu_core.rs`: the four in `dfu_core.c`, the client's
  own (`bm_dfu_client_abort`, the reboot request, the boot-complete) and the
  host's (`bm_dfu_host_req_update`, `_send_chunk`, `_send_reboot`).
  `bcmp_init` runs `bm_dfu_init`, which sets `dfu_ctx.self_node_id`.
- **The DFU core is ported (D2).** `bm_wire::bcmp::dfu_core::Dfu` is
  `dfu_core.c`: the queue, `lib_sm_run`'s run-then-transition order, `Init`,
  `Idle`, `Error`, and the four senders (now compared as frames). The seven
  client and host states are a `Roles` implementation. `Client` implements it
  for a client-only node; `bm_wire::bcmp::dfu_host::ClientHost` for a node
  running both, dispatching on the state. `Roles` gets the `Core` for
  everything the C files call back into `dfu_core.c` (`current_event`,
  `set_pending_state_change`, `set_error`, the timers, `delay`,
  `reboot_info_mut`, the senders).
- **Timers are the core's, and post events.** `Core::start_timer`,
  `stop_timer` and `delay` (`bm_delay`) run against a clock `Dfu::poll` and
  `Dfu::step` advance; a due timer posts its event behind the queue, deadline
  order first, ties in `Timer` order (creation order): chunk, ACK, update.
  `Core::change_period` is `bm_timer_change_period`, which restarts the
  timer. A delay fires every role's timers, not only the caller's, which is
  why they live in the core.
- **The host's heartbeat timer is not ported.** `s_host_update_run` starts
  it before reading a chunk and stops it after sending, so it fires only
  while the read blocks, and `Effects::host_get_chunk` and the stream buffer
  do not block. A non-internal host's application must have queued the
  chunk before the client asks for it.
- **`s_host_req_update_entry` arms the ACK timer, and only an ACK or an
  error stops it.** Forced out of `HostReqUpdate` any other way, it fires
  `AckTimeout` ten seconds later wherever the machine is — harmless in Idle.
  `the_host_ack_timer_outlives_its_state` pins it.
- **Events are checked at queue time and run later.** `Dfu::on_message` is
  `bm_dfu_process_message`, which validates the source against the state the
  machine is in *when the message arrives*. A pending change is taken after
  whatever is already queued, and a NOP that does not fit is dropped. Drive
  the machine with `Dfu::step`, not `run_event`, outside the goldens.
- **`bm-wire-diff/src/dfu_core.rs` is the stack comparator.** It drives
  `dfu_core.c` through `bm_dfu_test_set_dfu_event_and_run_sm`, one event at a
  time, and empties the C queue around every pump so the DFU task never runs
  one itself. `in_domain` discards two events on both sides: a non-internal
  `BeginHost` while the last one's stream buffer is held (#61), and a
  non-internal chunk request that would find the stream part full (#68).
  `reset` returns both machines to Idle through the public API at the start
  of every script, by running a whole empty update to client zero, which
  stops the host's timers and frees any stream buffer. `bcmp_init` already
  runs `bm_dfu_init` when `stack::oracle` brings the stack up; calling it
  again leaks the first queue and starts a second DFU task, which is what the
  fuzzer's first run found.
- **`Step::Offer` and `Step::Serve` carry whole transfers.** A `TestImage`'s
  bytes derive from a seed, `Serve` sends the chunk the client last asked for,
  and `Step::Advance` stops at each port deadline so two timers due in one
  step fire in deadline order rather than the shim's creation order.
  `in_domain_body` pads every body to what the C reads (#55). The host's
  mirror image is `Step::Host` (store an image, start an update),
  `Step::Feed` (`bm_dfu_host_queue_data`) and `Step::FromClient`.
- **Two cross-wired transfers are the end-to-end check.**
  `a_rust_host_updates_the_c_client` and `the_c_host_updates_a_rust_client`
  in `bm-wire-diff/tests/dfu_core.rs` run a whole update, reboot included,
  between the oracle and a `bm-wire` peer, with the oracle's side compared
  against the port at every step. `bm-stack/tests/dfu.rs` does the same
  between two `Node`s.
- **The slot, the boot hooks and `bm_config_reset` are seams with no
  oracle.** `csrc/bm_generic_shim.c` counts every call and refuses what
  `bm_shim_dfu_set_faults` names; the comparator's `Recorded` is the same in
  Rust, and compares counts and the 256 KiB slot byte for byte after every
  step. `bm_dfu_host_get_chunk` reads the same buffer from
  `DFU_IMG_START_OFFSET_BYTES` (18); `bm_shim_dfu_load` puts an image there
  without counting a call.
- **The C test accessors are bound.** `build.rs` now passes `-DENABLE_TESTING`
  to bindgen, so `bm_dfu_test_get_sm_ctx`, `bm_dfu_test_set_dfu_event_and_run_sm`
  and `bm_dfu_test_set_client_fa` are callable.
- **The error state's callback is the host's** (divergence #58): whichever role
  fails, `s_error_entry` calls the last host update's finish callback. The
  client and host do not "fix" this.
- **The host stores a peer's `err_code` as its own error** (divergence #59),
  and 14 or more wedges DFU until reboot. `the_c_host_adopts_a_clients_fatal_nack`
  was the C-only test; `a_clients_fatal_nack_stops_the_host` now compares
  it.
- **A non-internal host update leaks its stream buffer** unless it reaches
  `HostUpdate` (divergence #61). `bm_wire::bcmp::dfu_host::StreamBuffer`
  holds it inline, with the shim's semantics; the domain limit is only the
  leak itself.
- **`client_update_reboot_info` is `Core::reboot_info`.** `Dfu::new` takes what
  no-init RAM held; `s_idle_entry` zeroes it. `bm_stack::port::NoInitRam` is
  the seam that keeps it across a reset; `bm_stack::dfu::NodeDfu` stores it
  whenever it changes and before any reset the client asks for.
  `RebootInfo::encode` is the packed C layout, which a C image and a Rust
  image must share to hand an update across.
- **`bm-stack` runs DFU as client and host.** `Node::with_dfu` takes a
  `DfuSlot + NoInitRam`; `Node::new` and `with_config` use `NoDfu`, whose slot
  will not open or read. All eleven registrations are made (33 of 40 slots).
  `Node::on_frame` queues a DFU body for this node and re-floods one for
  another node that arrived link-local; `Node::next_dfu_transmission` runs the
  machine, as the DFU task does, and `Node::run` drains it after every wake
  and waits on its timers. `Node::dfu_initiate_update` starts a host update,
  `DfuSlot::read` is `bm_dfu_host_get_chunk`, and the finish callback is
  `Event::DfuUpdateFinished` (or `NodeDfu::take_update_finished`). The outbox
  holds four bodies of up to 1043 bytes, a whole chunk each.
- **The client's quirks are #62–#66, the host's #67–#69.** #67 is the one
  that matters on a real link: the host ignores the chunk number asked for,
  so one lost chunk fails the update.
---

## Card format

Each card gives: the C source, the Rust to create, the comparator and fuzz
target, any new port seam, the C quirks to reproduce, what blocks it, and what
"done" means.

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
  BCMP. Pub/sub, UDP and `spotter.c` are planned in `docs/hello-world-todo.md`.
- **`bm-phy-adin2111`** — embassy#7024 is merged; `docs/embassy-port-tracking-prompt.md`
  keeps the design record.

---

# Keeping this current

When a card lands, delete it — git history is the record — and move anything
the next cards need into "What the landed cards left for the rest". When a card
turns up a C defect, the finding goes in `docs/c-divergences.md` and the card
references the number. If a card turns out to be two cards, split it here
before starting.
