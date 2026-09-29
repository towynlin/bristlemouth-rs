# Hello world todo

What stands between the current tree and a Rust hello-world app on a
Bristlemouth dev kit, as dependency-ordered task cards sized for one agent
each. This is the active plan. Same card format and shared contract as
`docs/bcmp-port-todo.md`, which is complete and takes no new cards; read that
file's "The shared contract" first.

## Working a card

Before starting:

1. Pick a card whose **Blocked by** is "nothing". If it turns out to be two
   cards, split it here first, in its own commit.
2. Read "What the landed cards left for the rest" below.

When the card's code is done and verified, edit this file in a separate
commit, the last one of the card's branch:

| Section | Edit |
|---|---|
| The card | Delete it. Git history is the record of the work; do not leave a "Landed" note or a list of what was done. |
| What the landed cards left for the rest | Add what a remaining card needs to know: the API shape, a limit or gap left open, and **the reason for any decision between options**, so no later card reopens it. Delete entries no remaining card needs. Keep the heading's card list current. |
| Other cards | Remove the card from every **Blocks** and **Blocked by**; write "nothing" where none remain. Fix any text that names it. |
| Order | Remove it from the graph and the paragraph under it. |
| Suspected C defects | Confirmed rows: add to `docs/c-divergences.md` with a number, cite the number in the commit message, delete the row. Discarded rows: delete, and say why in the commit message. |

If the card added files, crates or verify commands, `CLAUDE.md`'s layout and
"Verifying" sections are updated in the card's code commits, not here.

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
| Application code running beside the node | Done: `bm_stack::App`, run by `Node::run_app`; `bm_stack::channel` for an app in its own task |
| UDP over IPv6 | **Half.** `bm_wire::udp` builds and accepts a datagram's frame. `Node` neither sends nor dispatches UDP: `Node::on_frame` relays a UDP frame per `l2_policy` and then drops it in `rx::accept`. |
| Pub/sub (`middleware/pubsub.c`, `middleware.c`) | **Missing.** Listed out of scope in `bcmp-port-todo.md`. |
| `spotter_log`, `spotter_tx_data` | **Missing.** |
| Dev kit board support (MCU HAL, pins, node id, time driver, flash) | **Missing.** No crate targets a board. |

A BCMP-only node — one that heartbeats, is discovered, and answers ping and
info — runs on the mock PHY (`bm-stack/examples/hello_node.rs`), but not yet on
a board. It cannot say hello.

## The oracle is not the deployed stack for UDP

Deployed C nodes send UDP through lwIP (`network/bm_lwip.c`). The oracle
compiles `network/bm_linux.c`, a hand-written replacement. For BCMP the two
paths share `bcmp/packet.c`; for UDP they do not. Observed in
`bm-wire-diff/testdata/hello-pub-card-h0.pcap` and asserted by
`bm-wire-diff/tests/capture_h0.rs` (divergence #70):

| Field | `bm_linux.c` | `bm_lwip.c` + lwIP, observed |
|---|---|---|
| IPv6 source address, UDP to `ff03::1` | `fe80::<id>` | `fd00::<id>` |
| IPv6 source address, BCMP | `fe80::<id>` | `fe80::<id>` |
| Hop limit, UDP and BCMP | 64 | 255 (`UDP_TTL` 255; no multicast TTL option set) |
| UDP checksum on transmit | computed, byte-swapped; zero sent as zero (#71) | computed, valid (`CHECKSUM_GEN_UDP` 1); zero sent as `0xFFFF` |
| UDP checksum on receive | not checked | not checked (`CHECKSUM_CHECK_UDP` 0) |
| UDP payload on receive | UDP length field less 8; refused if out of range (#72) | IPv6 payload less 8; length field not read |
| Destination MAC | `multicast_mac_from_ipv6` | the same: `33:33:00:00:00:01` for `ff03::1` |
| Source MAC | `mac_from_nodeid`: low 48 bits of the id, byte 0 `\|= 0x02` | `mac_address` (`common/device.c`): `00:00` + low 32 bits |
| Port nibbles, `ff03::1` | none | none, sent once to all ports; a relayed copy is byte-identical |

`bm_l2_policy_rx_apply` writes the ingress nibble without patching the UDP
checksum, so a received publication reaches lwIP with a bad checksum; deployed
builds accept it because they do not check.

So for UDP, "the C is authoritative" means the frames a real C dev kit emits.
`bm-wire-diff` comparators against `bm_linux.c` still catch pub/sub-layer
divergences, but the header fields above must be pinned by gold vectors
from the capture, and where `bm_linux.c` and the capture disagree, the capture wins
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

## What the landed cards (A1, E0, F1, H0, U1) left for the rest

- **Two ways for application code to reach the node, and why.** Some
  applications need a task of their own and some fit in the node's loop, so
  both exist and the second is opt-in:

  | Seam | Shape | Test |
  |---|---|---|
  | `bm_stack::App<N>`, run by `Node::run_app` | `ready` is the loop's fifth `select` arm and must be cancel-safe; then `act(&mut Node, now_ms)` calls node methods directly and returns at most one `Outbound`; events arrive at `on_event`. `run_with` is `run_app` with an app that never acts. No dependency, and nothing to add per app-facing call. | `an_app_pings_on_its_own_timer_and_sees_the_reply` |
  | `bm_stack::channel`, behind the `channel` feature (`embassy-sync`, `heapless`) | A task holds a `NodeHandle`, sends `Command`s and receives owned `Notification`s; `ChannelApp` is the `App` serving it. Costs one loop pass of latency over `App`. | `an_app_task_pings_through_a_channel_and_sees_the_reply` |

  Rejected: a closure `FnMut(&mut Node, Event)` in place of `FnMut(Event)`,
  because the app could then act only inside an event and not on a timer of
  its own. Both tests are in `bm-stack/tests/node.rs` under "Application
  seam".
- **The channel covers ping only.** Each new app-facing call (P2's publish and
  subscribe, S1's `spotter_log`) needs a `Command` variant, and each new event
  an owned `Notification`. A full notification queue drops, counted by
  `ChannelApp::dropped`: the node never waits on the application.
- **The mock clock is process-global.** A test or example driving
  `Node::run*` on `MockPhy` holds `tests/node.rs`'s `CLOCK` lock or runs in a
  process of its own.
- **One header writer.** `bm_wire::frame::write_headers(buf, src, dst,
  next_header, hop_limit, payload_len)` writes Ethernet and IPv6 with a
  deployed node's source MAC, `addr::mac_address`. `bm_wire::bcmp::tx::build`
  wraps it for BCMP from a node's `fe80::` address, `bm_wire::udp::build` for
  UDP; both use `HOP_LIMIT`, 255. The destination MAC rule (multicast MAC,
  else broadcast) is fixed inside it. `LINK_LOCAL_PREFIX` and
  `UNIQUE_LOCAL_PREFIX` are in `bm_wire::addr`, `HOP_LIMIT` in
  `bm_wire::frame`; `bm_stack::node` re-exports `LINK_LOCAL_PREFIX` and
  `HOP_LIMIT`.
- **`bm_wire::udp`, and why it follows lwIP.**

  | Item | Shape |
  |---|---|
  | `build(buf, src, dst, src_port, dst_port, payload) -> Result<usize>` | whole frame, checksum in network order, `0xFFFF` for zero; no egress stamp |
  | `source_address(node_id, dst)` | `fe80::<id>` for link-local scope or narrower (`ff02::1`), else `fd00::<id>` (`ff03::1`) |
  | `accept(frame) -> Result<Datagram>` | `src_port`, `dst_port`, `source` node id, `payload`; checksum and UDP length field not read |
  | `PAYLOAD_OFFSET`, `MAX_PAYLOAD_LEN` | 62; 65527 |

  Deployed nodes run lwIP, so every field where `bm_linux.c` differs follows
  lwIP (#70, #71, #72); `capture_h0.rs` rebuilds all 2300 captured UDP frames
  with `build`. `build` takes the source address rather than a node id so a
  comparator can build the frame `bm_linux.c` would, from `fe80::<id>`.
  `accept` does not filter on destination address or port: U2 decides what
  the node is bound to.
- **Comparing UDP frames with the oracle.** `stack::drain` rewrites the
  source MAC and hop limit of every frame the oracle built
  (`stack::normalise`), for BCMP and UDP alike. The UDP source address and
  checksum it cannot rewrite, because the address is under the checksum. A
  Rust node sends from `source_address`, `fd00::<id>` for `ff03::1`, so U2,
  P2 and S1's frame comparators compare in two steps: the node's frame equals
  `udp::build` of its UDP payload from `source_address`; and `udp::build` of
  the same payload from `fe80::<id>`, passed through
  `bm_wire_diff::udp::as_bm_linux_writes_it`, equals the oracle's frame.
  `bm_wire_diff::l2_egress::port_transmit` turns one built frame into what
  reaches each port.
- **Receiving in the oracle without L2.** `bm-wire-diff/src/udp.rs` binds five
  ports with `bm_udp_bind_port` once per process (the UDP list has no unbind)
  and calls `bm_l2_submit` directly. Port 4321 is already bound to
  `bm_middleware_rx`, which reaches `bm_handle_msg`'s out-of-bounds read
  ("Suspected C defects"), so that comparator never delivers to it; U2 and P2
  will, and must constrain the payload per P1's finding.
- **Scripted peer frames come from `bm_stack::mock::frames`, comparator
  inputs from `bm_wire_diff::frames`.** Both wrap `tx::build` in a `Vec`.
  U2, P2 and S1 add their UDP, publication and `spotter_log` builders there
  rather than in a test file. A comparator needing non-standard header bytes
  calls `write_headers` and mutates the result, as `bm-wire-diff/src/forward.rs`
  does.
- **The capture: `bm-wire-diff/testdata/hello-pub-card-h0.pcap`.** 166 s,
  2403 frames, from `bm_l2_register_pcap_callback` on a `bm_protocol` dev kit
  (`0b54ccce5c7978bf`, two ports) beside a Spotter bridge
  (`e4ce8ae3662e97df`, issued `bm info 0`) and a bm soft module
  (`e5d14eea4fc2db6b`, temperature). The callback sees received frames before
  `bm_l2_policy_rx_apply`, so none carries an ingress nibble, and it does not
  record the port. `tests/capture_h0.rs` names the nodes and asserts the
  header table above; `bm_wire_diff::pcap::records` reads the file for S1's
  gold vectors. The dev kit published every 10 s:

  | Call | Topic | Body as captured |
  |---|---|---|
  | `spotter_log(0, "hello.log", USE_TIMESTAMP, …)` | `spotter/fprintf` | `target_node_id` 0, `fname_len` 9, `data_len` excluding the NUL, `print_time` 1, `hello.log`, text, NUL |
  | `spotter_log_console(0, …)` | `spotter/printf` | as above with `fname_len` 0 and no file name |
  | `spotter_tx_data(&u32, 4, BmNetworkTypeCellularOnly)` | `spotter/transmit-data` | `02`, then the four data bytes |

  All three: pub/sub header type 0, flags 0, ext type 1, version 2. Frames
  84–86 are the first round (counter 100). The card asked for a publication
  with two-port and one-port egress: the dev kit's own is the first (one
  frame to all ports), a neighbour's relayed copy the second.
- **`bm-stack/examples/hello_node.rs` is the host twin of E1.** P2 and S1 can
  extend it with a publish, a subscription the scripted neighbour publishes
  to, and a `spotter_log` line. It asserts its outcome, and CI's `test` job runs it:
  `cargo test` only builds examples.

---

## Card U2 — UDP through `bm_stack::Node`

**Blocks:** P2. **Blocked by:** nothing.

C: `bm_l2_process_tx_evt` (global multicast goes out once to
`device_all_ports`, no egress stamp, no checksum patch), `bm_l2_process_rx_evt`
(ingress nibble set before submit), `bm_middleware_rx`, `middleware.c`.

Rust: `Node::on_frame` hands an accepted UDP frame for a bound port to the
application as `Event::Udp { port, source, payload }` (or straight to P2's
pub/sub). A send path producing an `Outbound` whose mask is all ports.
No UDP checksum check on receive, as deployed builds.

Comparator: whole frames, in the style of `bm-wire-diff/tests/node_frames.rs`,
in a stack-target binary of its own (`stack::` brings up `bm_linux.c`, whose
UDP list is process-global).

Done: a UDP frame from the oracle reaches a Rust node's event, one from the
Rust node reaches `bm_middleware_rx` in the oracle, and relaying is unchanged.

## Card P1 — Pub/sub codec and topic matching in `bm-wire`

**Blocks:** P2, S1. **Blocked by:** nothing.

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

**Blocks:** S1, E1. **Blocked by:** U2, P1.

C: `bm_pubsub_init`, `bm_sub_wl`, `bm_unsub_wl`, `bm_pub_wl`,
`publish_data_locally`, `bm_handle_msg`.

Rust: `Node::subscribe`, `Node::unsubscribe`, `Node::publish`, over a
fixed-capacity subscription table (const generic, as `RESOURCES` is). Publish
to `ff03::1` port 4321; add `PUB`/`SUB` to `Node::resources` as the C does
(`Node::add_resource`, already ported): `bm_pub_wl` adds `PUB` only after a
successful send, `bm_sub_wl` adds `SUB` on every successful call. A local subscription
matching a publication delivers `Event::Publication` locally too. Reachable
from `App::act`, and through `bm_stack::channel` as new `Command` and
`Notification` variants.

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

**Blocks:** E1. **Blocked by:** nothing (can run beside U2–S1).

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

**Blocks:** nothing. **Blocked by:** P2, S1, B1.

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
U2 ─┬─► P2 ─► S1 ─► E1
P1 ─┘                ▲
B1 ──────────────────┘
```

U2, P1 and B1 can start now and run in parallel.

# Explicitly out of scope

- **`bm_service*.c` and the built-in services** (sys_info, power_info,
  metrics, config CBOR map, echo). Request/reply over pub/sub; not needed to
  say hello. A natural next plan once P2 lands.
- **`integrations/topology.c`**, as in `bcmp-port-todo.md`.
- **DFU slot and no-init RAM on the dev kit.** B1 starts with `NoDfu`.
