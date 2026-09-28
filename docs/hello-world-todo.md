# Hello world todo

What stands between the current tree and a Rust hello-world app on a
Bristlemouth dev kit, as dependency-ordered task cards sized for one agent
each. Same card format and shared contract as `docs/bcmp-port-todo.md`; read
that file's "The shared contract" first.

## The target

The dev kit's C hello world publishes a line to `spotter/printf` with
`spotter_log` (`integrations/spotter.c`), and the line appears on the Spotter
console. The Rust equivalent is done when a dev kit running Rust firmware, on a
bus with a Spotter and a C dev kit:

| Check | Exercises |
|---|---|
| appears in the C node's neighbour table and topology | heartbeat, neighbour table, info |
| answers a ping from the C node | echo |
| its `hello world` line appears on the Spotter console | UDP, pub/sub, `spotter_log` |
| receives a publication from the C node on a subscribed topic | UDP receive, wildcard match |

## Where the tree stands

| Need | State |
|---|---|
| BCMP: heartbeat, ping, info, neighbours, resources, time, config, DFU | Done (`bm-wire`, `bm_stack::Node`) |
| PHY for the ADIN2111 | Done (`bm-phy-adin2111`), untested on hardware |
| Application code running beside the node | Done (card A1): `bm_stack::App`, run by `Node::run_app`. |
| UDP over IPv6 | **Missing.** `bm-wire/src/frame.rs` has the UDP offsets and `l2::add_egress_port` its checksum patch; nothing builds, accepts or dispatches UDP. `Node::on_frame` relays a UDP frame per `l2_policy` and then drops it in `rx::accept`. |
| Pub/sub (`middleware/pubsub.c`, `middleware.c`) | **Missing.** Listed out of scope in `bcmp-port-todo.md`. |
| `spotter_log`, `spotter_tx_data` | **Missing.** |
| Dev kit board support (MCU HAL, pins, node id, time driver, flash) | **Missing.** No crate targets a board. |

A BCMP-only node — one that heartbeats, is discovered, and answers ping and
info — can be written today on the mock PHY, but not yet on a board. It cannot
say hello.

## The oracle is not the deployed stack for UDP

Deployed C nodes send UDP through lwIP (`network/bm_lwip.c`). The oracle
compiles `network/bm_linux.c`, a hand-written replacement. For BCMP the two
paths share `bcmp/packet.c`; for UDP they do not, and they differ in at least:

| Field | `bm_linux.c` | `bm_lwip.c` + lwIP |
|---|---|---|
| IPv6 source address | `fe80::<node id>` | chosen by lwIP source-address selection from `fe80::<id>` and `fd00::<id>`; for `ff03::1` likely `fd00::` (unverified) |
| Hop limit | 64 | lwIP's multicast TTL (unverified) |
| UDP checksum on receive | not checked | lwIP's `CHECKSUM_CHECK_UDP`, set in the application's `lwipopts.h`, not in bm_core |
| Destination MAC | `multicast_mac_from_ipv6` | lwIP's `ethip6_output` (likely identical; unverified) |

The receive-side checksum matters: `bm_l2_policy_rx_apply` writes the ingress
nibble into the source address before the frame reaches lwIP, so a node that
verified UDP checksums would reject every publication. Either deployed builds
disable the check or something compensates; card H0 settles it.

So for UDP, "the C is authoritative" means the frames a real C dev kit emits.
`bm-wire-diff` comparators against `bm_linux.c` still catch pub/sub-layer
divergences, but the IPv6 header fields above must be pinned by gold vectors
from card H0, and where `bm_linux.c` and the capture disagree, the capture wins
and the disagreement is a `c-divergences.md` entry.

## Suspected C defects to confirm

Found reading the source for this plan; not yet run. Each card confirms or
discards its own, and confirmed ones get a number in `c-divergences.md`.

| Where | Suspicion | Card |
|---|---|---|
| `bm_wildcard_match` (`common/util.c`) | Returns `j == pattern_len` without requiring `i == str_len`, so a pattern matches any topic it prefixes: a subscription to `spotter` receives `spotter/printf`. Confirmed by compiling the function alone; not yet through the oracle. | P1 |
| `bm_handle_msg` (`pubsub.c`) | `data_len = size - sizeof(BmPubSubData) - header->topic_len` is unchecked; a UDP payload shorter than its `topic_len` underflows and the callback reads out of bounds. Remote-triggerable. | P1 |
| `bm_pub_wl` (`pubsub.c`) | `message_size` is `uint16_t`; `5 + topic_len + len` wraps, and the `memcpy`s write `len` bytes into the short buffer before `bm_middleware_net_tx`'s size check runs. | P1 |
| `bm_middleware_rx` | Dispatches on the **source** port lwIP/`bm_linux.c` reports, not the bound destination port. Harmless while pub/sub sends from and to 4321. | U2 |
| `network_add_egress_port` UDP branch | Already divergence #12; latent because global multicast is never egress-stamped. Stays latent here. | — |

---

## Card A1 — Application seam on `Node::run`

**Blocks:** E0, P2, E1. **Blocked by:** nothing.

An application must be able to ask the running node to send (ping, publish,
request info) and to see what arrives, without owning the loop.

Options, pick one and say why in the PR:

| Option | Cost |
|---|---|
| `run_with` takes an extra `async` source of commands (a trait with `async fn next(&mut self) -> Command`) as a fifth `select` arm | No new dependency. `Command` is an enum that grows with every app-facing call. |
| A `Node` handle over an `embassy_sync::channel::Channel` | Idiomatic embassy; adds `embassy-sync`, which moves all three lockfiles. |
| A closure `FnMut(&mut Node, Event)` in place of `FnMut(Event)` | Smallest diff; app runs inside the event callback only, so it cannot act on a timer of its own. |

Done: a host test on `mock` where an app task sends a ping on its own timer
while the node runs, and sees the `EchoReply` event. No wire change; no
comparator.

**Landed** as a variant of the first option without the `Command` enum:
`bm_stack::App<N>` has a cancel-safe `async fn ready`, polled as the fifth
`select` arm; when it resolves the loop calls `act(&mut Node, now_ms)`, which
calls node methods directly and returns at most one `Outbound`. Events go to
`App::on_event`. `run_with` is `run_app` with an app that never acts. No new
dependency, and nothing to extend per app-facing call; a firmware wanting a
separate task makes `ready` receive from its own channel. Test:
`an_app_pings_on_its_own_timer_and_sees_the_reply` in `bm-stack/tests/node.rs`.

The second option is layered on top: `bm_stack::channel` (`embassy-sync`
0.8, `heapless` 0.9) gives a task a `NodeHandle` sending `Command`s and
receiving `Notification`s, and `ChannelApp` is the `App` that serves it.
Each app-facing call needs a `Command` variant and each event an owned
`Notification`; only ping has them so far. Notifications are dropped, and
counted by `ChannelApp::dropped`, when the queue is full, because the node
never waits on the application. Test:
`an_app_task_pings_through_a_channel_and_sees_the_reply`.

## Card E0 — Host example: a BCMP node on the mock PHY

**Blocks:** nothing. **Blocked by:** A1.

`bm-stack/examples/hello_node.rs`, a cargo example (auto-discovered, and built by
`cargo test`), using the `mock` feature as the tests do: brings up
a `Node`, scripts a heartbeat from a neighbour, prints the neighbour table and
an echo reply. Proves the public API is usable from outside the crate. Add
`cargo run -p bm-stack --example hello_node` to `CLAUDE.md`'s verify list and
CI.

## Card H0 — Reference captures from a C dev kit

**Blocks:** U1 (gold vectors), U2. **Blocked by:** hardware. **Needs a human.**

Record, from a dev kit running current `bm_protocol` firmware:

1. A `spotter/printf` publication from the hello-world app.
2. A publication on a topic with a two-port egress and a one-port egress.
3. One publication received from a neighbour, to see what reaches lwIP.

Via `bm_l2_register_pcap_callback` (`network/l2.c`) or a 10BASE-T1L tap. Also
record, from `bm_protocol`, the dev kit's `lwipopts.h` values for
`CHECKSUM_CHECK_UDP`, `CHECKSUM_GEN_UDP` and the multicast TTL.

Done: pcaps committed under `bm-wire-diff/testdata/`, and the table in "The
oracle is not the deployed stack for UDP" filled in with observed values.

## Card U1 — UDP over IPv6 in `bm-wire`

**Blocks:** U2, P1. **Blocked by:** H0 for gold vectors only; the codec can
start before.

C: `bm_udp_tx_perform` and the UDP branch of `bm_ip_rx` in
`network/bm_linux.c`; lwIP's `udp_sendto_if` for what deployed nodes do.

Rust: `bm-wire/src/udp.rs` — build a frame (Ethernet, IPv6, UDP, payload) into
a caller buffer from source address, destination, ports and payload; accept a
received frame and return ports, source node id and payload. Checksum via
`checksum::ipv6_pseudo_checksum`. Source address and hop limit are parameters
until H0 fixes them.

Comparator: `bm-wire-diff/src/udp.rs`, frames from `bm_udp_tx_perform` vs
ours for identical inputs; fuzz target `udp`. Gold vectors from H0 as
`bm-wire` unit tests.

Done: comparator, fuzz target with seeds, gold-vector tests, and the
`bm_linux.c`-vs-capture differences written up.

## Card U2 — UDP through `bm_stack::Node`

**Blocks:** P2. **Blocked by:** U1, H0.

C: `bm_l2_process_tx_evt` (global multicast goes out once to
`device_all_ports`, no egress stamp, no checksum patch), `bm_l2_process_rx_evt`
(ingress nibble set before submit), `bm_middleware_rx`, `middleware.c`.

Rust: `Node::on_frame` hands an accepted UDP frame for a bound port to the
application as `Event::Udp { port, source, payload }` (or straight to P2's
pub/sub). A send path producing an `Outbound` whose mask is all ports.
Receive-side checksum behaviour per H0.

Comparator: whole frames, in the style of `bm-wire-diff/tests/node_frames.rs`,
in a stack-target binary of its own (`stack::` brings up `bm_linux.c`, whose
UDP list is process-global).

Done: a UDP frame from the oracle reaches a Rust node's event, one from the
Rust node reaches `bm_middleware_rx` in the oracle, and relaying is unchanged.

## Card P1 — Pub/sub codec and topic matching in `bm-wire`

**Blocks:** P2, S1. **Blocked by:** U1.

C: `BmPubSubData` and `BmPubSubHeader` (`middleware/pubsub.h`),
`bm_pub_wl`'s header fill, `bm_handle_msg`, `bm_wildcard_match`
(`common/util.c`).

Rust: `bm-wire/src/pubsub.rs` — encode and decode the 5-byte header
(`type` 0, `flags` 0, `topic_len` u8, then `ext_header.type`,
`ext_header.version`), the topic, the data; `wildcard_match` with the C's
prefix behaviour. Topics are `< BM_TOPIC_MAX_LEN` (255).

Comparator: `wildcard_match` against `bm_wildcard_match` directly (pure C
function, no shim state), fuzz target `wildcard`; the codec through P2's
frame comparator.

Quirks: the three `pubsub.c`/`util.c` rows in "Suspected C defects". The
out-of-bounds read constrains the comparator's input domain; say so at the
type.

Done: comparator, fuzz targets, confirmed divergences numbered.

## Card P2 — Pub/sub on the node

**Blocks:** S1, E1. **Blocked by:** A1, U2, P1.

C: `bm_pubsub_init`, `bm_sub_wl`, `bm_unsub_wl`, `bm_pub_wl`,
`publish_data_locally`, `bm_handle_msg`.

Rust: `Node::subscribe`, `Node::unsubscribe`, `Node::publish`, over a
fixed-capacity subscription table (const generic, as `RESOURCES` is). Publish
to `ff03::1` port 4321; add `PUB`/`SUB` to `Node::resources` as the C does
(`Node::add_resource`, already ported): `bm_pub_wl` adds `PUB` only after a
successful send, `bm_sub_wl` adds `SUB` on every successful call. A local subscription
matching a publication delivers `Event::Publication` locally too. Reachable
through A1's seam.

Comparator: frames from `bm_pub` vs `Node::publish` for the same topic, type,
version, data and identity; an oracle publication delivered to a Rust
subscriber and vice versa. Stack-target binary.

Done: comparators, fuzz target `pubsub`, and the resource-table side effects
compared.

## Card S1 — `spotter_log` and `spotter_tx_data`

**Blocks:** E1. **Blocked by:** P2.

C: `integrations/spotter.c`, `bm_print_publication_t` and
`BmSerialNetworkDataHeader` (`bm_common_messages/bm_common_pub_sub.h`).

Rust: `bm-wire/src/spotter.rs` encodes both bodies (`target_node_id` u64,
`fname_len` u16, `data_len` u16, `print_time` u8, file name, text, trailing
NUL — the C sends the NUL); `bm-stack` wraps them as `Node::spotter_log` and
`Node::spotter_tx_data`, taking bytes rather than a format string (callers
format with `core::fmt::Write` into a buffer). Topics `spotter/printf`,
`spotter/fprintf`, `spotter/transmit-data`; type 1, version
`BM_COMMON_PUB_SUB_VERSION` (2). Size limits: `max_str_len`, 311 bytes
Iridium, 1000 cellular.

Comparator: frames from `spotter_log`/`spotter_tx_data` vs ours. `spotter_log`
is variadic; call it with `"%s"` and the text.

Done: comparator, fuzz target `spotter`.

## Card B1 — Dev kit board support

**Blocks:** E1. **Blocked by:** nothing (can run beside U1–S1).

The dev kit's mote is an STM32U5 (Cortex-M33, `thumbv8m.main-none-eabihf`)
with an ADIN2111 on SPI. The pin map, the ADIN2111 power and reset sequence,
and where the provisioned node id lives come from `bm_protocol`'s BSP, which
is not vendored here; the card starts by recording them, with file and line
references, in the crate's docs.

Rust: `bm-devkit/`, its own workspace beside `bm-phy-adin2111` (same
git-embassy constraint). `embassy-stm32` with the exact chip feature and its
time driver; `Identity` from the provisioned node id; `ConfigStorage` over
internal flash at the partitions `bm_protocol` uses, or `RamConfigStorage`
first with flash a follow-up; `NoDfu` first. `memory.x`, `.cargo/config.toml`
with a `probe-rs run` runner, `defmt` logging.

Done: `cargo build --target thumbv8m.main-none-eabihf` in CI. On-hardware
bring-up (link up on both ports, heartbeats seen by a C node) is a manual
check the PR reports as done or not done.

## Card E1 — The hello-world example

**Blocks:** nothing. **Blocked by:** A1, P2, S1, B1.

`bm-devkit/src/bin/hello_world.rs` — embassy's convention for board examples
(`examples/<board>/src/bin/*.rs`, own workspace, own `memory.x`) applies, so
if B1 is named `examples/devkit/` instead, put it there. It spawns the
ADIN2111 runner, runs the node, subscribes to one topic, and every 10 s
publishes `hello world` via `spotter_log` and logs anything received.

Done: builds in CI; the four checks under "The target" are run on a bench and
reported individually.

---

## Order

```
A1 ──► E0
A1 ─────────────────────┐
H0 ─► U1 ─► U2 ─► P1 ─► P2 ─► S1 ─► E1
B1 ──────────────────────────────────┘
```

(P1's `wildcard_match` half does not need U2 and can start after U1.)

A1, H0 and B1 have no prerequisites and can run in parallel. H0 needs a
person with hardware; U1 can start without it and add gold vectors later.

# Explicitly out of scope

- **`bm_service*.c` and the built-in services** (sys_info, power_info,
  metrics, config CBOR map, echo). Request/reply over pub/sub; not needed to
  say hello. A natural next plan once P2 lands.
- **`integrations/topology.c`**, as in `bcmp-port-todo.md`.
- **DFU slot and no-init RAM on the dev kit.** B1 starts with `NoDfu`.
