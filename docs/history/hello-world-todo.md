# Hello world todo

> **Complete and closed. There is no work here.** Every card has landed and
> the four checks in "The target" passed on a bench. Do not add cards, pick
> work from this file, or edit it as part of a card. It is kept as
> documentation: the sections below record what was built, the API shapes,
> and the reasons for decisions. "Working a card" and the "Order" section
> describe the process the plan followed and are historical.

What stood between the tree and a Rust hello-world app on a Bristlemouth dev
kit, as dependency-ordered task cards sized for one agent each. Same card
format and shared contract as `docs/history/bcmp-port-todo.md`, which is also
complete.

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
| PHY for the ADIN2111 | Done (`bm-phy-adin2111`), run on a dev kit |
| Application code running beside the node | Done: `bm_stack::App`, run by `Node::run_app`; `bm_stack::channel` for an app in its own task |
| UDP over IPv6 | Done: `bm_wire::udp`; `Node::bind_udp`, `Node::send_udp`, `Event::Udp` |
| Pub/sub codec and topic matching | Done: `bm_wire::pubsub`, `bm_wire::util::bm_wildcard_match` |
| Pub/sub on the node (`middleware/pubsub.c`, `middleware.c`) | Done: `Node::subscribe`, `unsubscribe`, `publish`, `Event::Publication` |
| `spotter_log`, `spotter_tx_data` | Done: `bm_wire::spotter`; `Node::spotter_log`, `Node::spotter_tx_data` |
| Dev kit board support (MCU HAL, pins, node id, time driver) | Done (`bm-devkit`), run on a dev kit |
| Config in the dev kit's NOR flash | Done (`bm-devkit`: `w25`, `storage`), untested on hardware |
| The hello-world app | Done: `bm-devkit/src/bin/hello_world.rs`; the four checks above passed on a bench |
| RTC set from the Spotter's `spotter/utc-time` | Done: `bm_stack::utc_time`, `bm_devkit::rtc::DevkitRtc`, in `hello_world`; run on a bench |

A node that heartbeats, is discovered, answers ping and info, publishes,
subscribes and calls `spotter_log` runs on the mock PHY
(`bm-stack/examples/hello_node.rs`) and on a dev kit
(`bm-devkit/src/bin/hello_world.rs`).

## The oracle is not the deployed stack for UDP

Deployed C nodes send UDP through lwIP (`network/bm_lwip.c`). The oracle
compiles `network/bm_linux.c`, a hand-written replacement. For BCMP the two
paths share `bcmp/packet.c`; for UDP they do not. Observed in
`bm-wire-diff/testdata/hello-pub.pcap` and asserted by
`bm-wire-diff/tests/capture_hello_pub.rs` (divergence #70):

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
| `network_add_egress_port` UDP branch | Already divergence #12; latent because global multicast is never egress-stamped. Stays latent here. | — |

## What the landed cards (A1, B1, B2, E0, E1, F1, H0, P1, P2, S1, T1, U1, U2) left for the rest

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
- **The channel covers ping, pub/sub and the Spotter calls.** Each new
  app-facing call needs a `Command` variant, and each new event an owned
  `Notification`. `Event::Udp` has none: bm_core applications never see raw
  UDP, only pub/sub. Owned topics are at most `channel::TOPIC_BYTES` (64) and
  data `channel::DATA_BYTES` (256), fixed rather than generic so `Command`
  stays one type; larger publications go through `App`. A full notification
  queue, or a publication too large to own, is dropped and counted by
  `ChannelApp::dropped`: the node never waits on the application. Commands
  report no result.
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
  lwIP (#70, #71, #72); `capture_hello_pub.rs` rebuilds all 2300 captured UDP frames
  with `build`. `build` takes the source address rather than a node id so a
  comparator can build the frame `bm_linux.c` would, from `fe80::<id>`.
  `accept` does not filter on destination address or port.
- **UDP on the node.**

  | Item | Shape |
  |---|---|
  | `Node::bind_udp(port) -> Result<(), UdpBindError>` | `UDP_PORTS` (4) slots; `InUse`, `Full`. `unbind_udp`, `udp_bound`. |
  | `Event::Udp { port, src_port, source, payload }` | a datagram to a bound port, reported from `on_frame_with` after the relay decision |
  | `Node::send_udp(src_port, dst, dst_port, payload) -> Option<Outbound>` | from `udp::source_address`; mask from byte 13 of `dst` (`take_requested_egress_port`), else all ports; `None` past `MTU` |

  Matching is on the destination port, as lwIP's `udp_input`. Port 4321 is
  pub/sub's: `bind_udp(4321)` is `InUse`, and a datagram to it is never
  `Event::Udp`; it reaches pub/sub only from 4321 (divergence #73). The
  application's own ports are the other `UDP_PORTS`.
  No destination-address filter: `bm_linux.c` has none, and lwIP's
  (`ip6_input`: joined groups, `ff01::1`, `ff02::1`, the netif's addresses) is
  not in the tree to port; #72 records it. A Rust node therefore also
  delivers, for example, `ff02::2` datagrams a C node drops.
  `on_frame` passes no link-local routing callback: `bm_middleware_init`
  registers `handle_middleware_routing`, but with pub/sub's `FF03::1` as the
  only application address it returns what no callback does.
- **Comparing UDP frames with the oracle.** `stack::drain` rewrites the
  source MAC and hop limit of every frame the oracle built
  (`stack::normalise`), for BCMP and UDP alike. The UDP source address and
  checksum it cannot rewrite, because the address is under the checksum. A
  Rust node sends from `source_address`, `fd00::<id>` for `ff03::1`, so frame
  comparators compare in two steps, as
  `bm_wire_diff::node_udp::check_send` does: the node's frame equals
  `udp::build` of its UDP payload from `source_address`; and `udp::build` of
  the same payload from `fe80::<id>`, passed through
  `bm_wire_diff::udp::as_bm_linux_writes_it`, equals the oracle's frame.
  `bm_wire_diff::l2_egress::port_transmit` turns one built frame into what
  reaches each port.
- **Receiving in the oracle.** `bm-wire-diff/src/udp.rs` binds five ports
  with `bm_udp_bind_port` once per process (the UDP list has no unbind);
  `udp::bind` and `udp::take_delivered` expose them. `udp.rs` calls
  `bm_l2_submit` directly; `bm-wire-diff/src/node_udp.rs` injects through L2
  and subscribes the oracle to `*` (`bm_sub_wl`), which matches every topic,
  to see what reaches `bm_handle_msg`; `node_udp::receiver` is a Rust node
  bound and subscribed alike.
- **Scripted peer frames come from `bm_stack::mock::frames`, comparator
  inputs from `bm_wire_diff::frames`.** Both wrap `tx::build` in a `Vec`.
  Both have `udp(src, dst, src_port, dst_port, payload)`, from
  `udp::source_address`, `publication(src, topic, kind, version, data)`,
  `spotter_log(src, target_node_id, file_name, print_time, text)` and
  `spotter_tx_data(src, data, network)`. A comparator needing non-standard header bytes
  calls `write_headers` and mutates the result, as `bm-wire-diff/src/forward.rs`
  does.
- **The capture: `bm-wire-diff/testdata/hello-pub.pcap`.** 166 s,
  2403 frames, from `bm_l2_register_pcap_callback` on a `bm_protocol` dev kit
  (`0b54ccce5c7978bf`, two ports) beside a Spotter bridge
  (`e4ce8ae3662e97df`, issued `bm info 0`) and a bm soft module
  (`e5d14eea4fc2db6b`, temperature). The callback sees received frames before
  `bm_l2_policy_rx_apply`, so none carries an ingress nibble, and it does not
  record the port. `tests/capture_hello_pub.rs` names the nodes and asserts the
  header table above; `bm_wire_diff::pcap::records` reads the file.
  `node_spotter_calls_rebuild_the_dev_kits_frames` rebuilds all 51 of the
  dev kit's Spotter publications with `Node`. The dev kit published every 10 s:

  | Call | Topic | Body as captured |
  |---|---|---|
  | `spotter_log(0, "hello.log", USE_TIMESTAMP, …)` | `spotter/fprintf` | `target_node_id` 0, `fname_len` 9, `data_len` excluding the NUL, `print_time` 1, `hello.log`, text, NUL |
  | `spotter_log_console(0, …)` | `spotter/printf` | as above with `fname_len` 0 and no file name |
  | `spotter_tx_data(&u32, 4, BmNetworkTypeCellularOnly)` | `spotter/transmit-data` | `02`, then the four data bytes |

  All three: pub/sub header type 0, flags 0, ext type 1, version 2. Frames
  84–86 are the first round (counter 100). The card asked for a publication
  with two-port and one-port egress: the dev kit's own is the first (one
  frame to all ports), a neighbour's relayed copy the second.
- **`bm_wire::pubsub`, the codec.**

  | Item | Shape |
  |---|---|
  | `encode(buf, topic, kind, version, data) -> Result<usize>` | header type 0, flags 0, as `bm_pub_wl`; `Invalid` for an empty topic or one of `TOPIC_MAX_LEN` (255) or more; `Truncated` past `buf` |
  | `decode(payload) -> Result<Publication>` | `header_type`, `flags`, `topic`, `kind`, `version`, `data`, borrowed; `Truncated` where `bm_handle_msg` would read past the payload (#75); accepts `topic_len` 0 and 255, as the C does |
  | `PORT`, `HEADER_LEN`, `COMMON_VERSION`, `MAX_MESSAGE_LEN` | 4321; 5; 2; 1452, `max_payload_len_udp` |

  Matching stays `bm_wire::util::bm_wildcard_match`, where `common/util.c`
  has it, with its prefix match (#74). `capture_hello_pub.rs` re-encodes all 2300
  captured publications byte for byte.
- **Pub/sub on the node.**

  | Item | Shape |
  |---|---|
  | `Node::subscribe(topic) -> Result<(), SubscribeError>` | `Refused(SubscriptionError)` with nothing changed; `NotAdvertised` when subscribed but the resource table is full, as `bm_sub_wl` keeps the subscription. Adds `SUB` on every success. |
  | `Node::unsubscribe(topic) -> Result<(), SubscriptionError>` | `NotSubscribed` is the C's `BmEINVAL` (#80); `SUB` stays |
  | `Node::publish(topic, kind, version, data) -> Result<Outbound, PublishError>` | one frame to `FF03::1`, all ports; `PUB` added once built. `publish_with` also reports local deliveries, before `MessageTooLong` as in the C. |
  | `Event::Publication { source, subscription, topic, kind, version, data }` | once per matching subscription, in list order |
  | `Node<…, SUBSCRIPTIONS = 8>` | after `RESOURCE_REQUESTS`; a topic is at most `RESOURCE_NAME` bytes, since it is also a resource |

  One subscriber per topic, the application, so bm_core's callback lists
  (and #79) have no counterpart.
- **Comparing publications.** `node_udp::Step::Publish` compares `bm_pub_wl`
  with `Node::publish_with`: result, local delivery to `*`, and frames in the
  two steps above. `bm-wire-diff/src/pubsub.rs` (stack target `pubsub`)
  mirrors the oracle's subscription and resource lists in one Rust node for
  the life of the process and compares everything after every step.
- **Divergence #38 constrains every oracle publish and subscribe.**
  `bm_pub_wl` and `bm_sub_wl` look the topic up in `PUB_LIST`/`SUB_LIST`,
  which reads past every shorter entry before a match. `node_udp` therefore:

  | List | Holds | Rule |
  |---|---|---|
  | `PUB_LIST` | `node_udp::pool_topic(254)`, seeded first, once per process | every published topic is a prefix of it, so the lookup matches the head |
  | `SUB_LIST` | `<id>/metrics/req` (from `metrics_service_init`), then `*` | a new subscription must be a prefix of `<id>/metrics/req`; `node_udp::oracle_delivers` asserts it |

  `oracle_delivers` publishes with `MAX_MESSAGE_LEN` bytes of data, which is
  delivered locally and never reaches `PUB_LIST`, so it can use any topic.
  `pubsub.rs` keeps every pattern 10 bytes and every topic 14.
  `bm_wire_diff::spotter` seeds a fresh `PUB_LIST` with its three topics
  longest first, so every lookup meets only longer entries before its own.
  `node_udp::subscribe_all` is the `*` subscription without `node_udp`'s
  `PUB_LIST` seed, for a comparator that seeds its own.
- **Malformed publications reach the oracle only in bounds.**
  `node_udp::pubsub_domain` pads a payload to the header and, where its topic
  runs past the end, sets the first topic byte to `OFF_PATTERN` (`#`), which no
  subscription starts with; `bm_wildcard_match` then reads only that byte.
  The `*` callback reads nothing when the data length is wrapped, and reports
  `Published::Wrapped`. A test subscription must not start with `#`, `*` or
  `?`; every `node_udp` comparator asserts it through `bm_get_subs` (#78: keep
  the subscriptions few).
- **`bm-devkit`, the board.** `README.md` there is the only record of
  bm_protocol's BSP in this repo (pins, clocks, ADIN2111 sequence, node id,
  config partitions, flash layout), with commit, file and line references;
  later cards read it rather than bm_protocol.

  | Item | Shape |
  |---|---|
  | `start() -> Board` | `embassy_stm32::init(config())`, ADIN2111 powered and up; `Board { node_id, phy, adin_runner, adin_power }` |
  | `Board::adin_runner` | spawn it in a task; nothing moves until it runs |
  | `Board::adin_power` | hold it: dropping an embassy `Output` disconnects PH1 and powers the ADIN2111 off |
  | `Board::flash` | the W25Q64JV on SPI2, `bm_devkit::Flash` |
  | `node(node_id, flash) -> Devkit` | `Node` with `DevkitIdentity`, `SoftRtc`, `Config<DevkitConfigStorage>` loaded from flash in `Layout::ARM_EABI_GCC`, `NoDfu`, 2 ports |
  | `node_id_from_uid`, `uid_string` | `getNodeId` and `getUIDStr`, checked on host against a Python rendering of the C, not yet against a kit |

  Decisions, and why:

  | Decision | Reason |
  |---|---|
  | Own workspace, `Cargo.lock` on bm-phy-adin2111's embassy commit | the git embassy; one commit keeps the two crates from diverging. `cargo update -p embassy-stm32 --precise <rev>` moves it. |
  | `start` takes every peripheral | `hello_world` needs only the ADIN2111 and the flash; a card needing more (LEDs) splits it |
  | `memory.x` at `0x08000000`, no bootloader, top 512 bytes of RAM left out | NoDfu needs no MCUboot; the 512 bytes are where the C keeps its no-init block, for a DFU card |
  | Logging is defmt over RTT through probe-rs | the C console is USB CDC, which is not ported |
  | SMPS, watchdog, Bristlefin expander and LEDs not configured | not needed to say hello; the embassy example runs without them |
  | No host tests (`test = false`) | the crate builds only for thumb; its pure functions are small and cross-checked by hand |

  Run on a kit by E1's bench run, below. Run `bringup` first if a later
  bench check fails at link-up or heartbeats.
  Flashing with `probe-rs run` replaces an installed MCUboot bootloader.
- **Config in NOR flash (B2).** `bm_devkit::w25::W25` is the driver,
  generic over `embedded-hal` blocking `SpiDevice` and `DelayNs`;
  `bm_devkit::storage::FlashConfigStorage` is the `ConfigStorage` over it,
  and `reset` is `SCB::sys_reset`. `bm-devkit/README.md` lists where it
  differs from the C.

  | Decision | Reason |
  |---|---|
  | The driver takes `embedded-hal` traits, not embassy types | keeps it off the HAL, so a simulated part on a host can drive it; nothing in the repo does yet, since `bm-devkit` has no host tests |
  | Blocking SPI | `ConfigStorage` is synchronous. A write blocks the node's loop for two sector erases and 32 page programs, about 100 ms typical (W25Q64JV datasheet), and the only write, a commit, resets the MCU after it |
  | `W25` owns SPI2 through `ExclusiveDevice` | only config uses the flash. The C's DFU client writes to internal flash; only the DFU host reads the W25 (`bm-devkit/README.md`, "DFU image locations") |
  | 4 KB sector buffer in `W25` | off the node task's stack, which the Spotter calls already need |

  Not yet run on a kit: a key set (`0xA2`) and committed (`0xA3`) surviving
  the reset, and a partition written by C firmware loading. `bringup` logs
  each partition's keys at start, which is how both are read.
- **`bm-stack/examples/hello_node.rs` is the host twin of `hello_world`.** It subscribes
  to `hello/*`, publishes, receives the scripted neighbour's publication, and
  sends `hello world` with `spotter_log`. It asserts its outcome, and CI's
  `test` job runs it: `cargo test` only builds examples.
- **Spotter calls.**

  | Item | Shape |
  |---|---|
  | `Node::spotter_log(target_node_id, file_name, print_time, text) -> Result<Outbound, SpotterError>` | `file_name` `None` is `spotter/printf` (`spotter_log_console` with `USE_TIMESTAMP`), `Some` is `spotter/fprintf`, even `Some(b"")` |
  | `Node::spotter_tx_data(data, NetworkType) -> Result<Outbound, SpotterError>` | `spotter/transmit-data`; 311 bytes, or 1000 for `CELLULAR_ONLY` |
  | `SpotterError` | `NoData` (`BmENODATA`), `MessageSize` (`BmEMSGSIZE`), `NotSent` (`BmENETDOWN`, after local delivery, #81) |
  | `_with` twins | report local deliveries, as `publish_with` |
  | `channel::Command::SpotterLog`, `SpotterTxData` | text and data at most `DATA_BYTES`, file name `FILE_NAME_BYTES` (63) |

  Text is bytes the caller formatted, as the card decided: format with
  `core::fmt::Write` into a buffer. It is taken whole, NULs included, as a C
  format can write one; a file name is read to its first NUL, as
  `bm_strnlen` reads it. Each call builds its body in a buffer on the stack
  (`spotter::MAX_LOG_LEN`, 1461 bytes, or `MAX_TX_LEN`, 1001) rather than in
  the node, so a node carries no second 1.5 KB buffer; the node task needs
  the stack for it, which `hello_world` has at the default `memory.x`. `NetworkType` is a `u8` newtype, not an enum, because the
  C sends any byte and gives every value but `CELLULAR_ONLY` the Iridium
  limit.

- **`hello_world` on a bench (E1).** Rust node `0b54ccce5c7978bf` (the
  capture's dev kit, reflashed) beside a Spotter bridge (`5428d5d73b4e298a`)
  and a C dev kit (`62326760da4e237a`, `hello_world@ENG-v0.13.12`):

  | Check | Evidence |
  |---|---|
  | neighbour table and topology | `bm topo` on the C dev kit: `(root)62326760da4e237a:1 \| 2:0b54ccce5c7978bf:1 \| 1:5428d5d73b4e298a`; `bm info 0` on the Spotter lists the Rust node |
  | answers a ping | `bm ping` from the C dev kit; defmt `echo request from 62326760da4e237a` |
  | `hello world` on the Spotter console | `1790825964.886 0b54ccce5c7978bf, hello world` |
  | receives a publication | `spotter/printf`, `fprintf`, `transmit-data` from the C dev kit |

  It subscribes to `spotter/*` rather than an app topic because that matches
  what stock C dev kit firmware publishes, so the receive check needs no C
  change. It also receives the Spotter's `spotter/utc-time` (T1, below).
  `DevkitIdentity` reports the crate version and the first 8 hex digits of
  `HEAD` (`bm-devkit/build.rs`), as the C reports its version and SHA.
- **`spotter/utc-time` (T1).** The handler is bm_protocol application code
  (`bmdk_common/app_main.cpp:238-272`), not bm_core; `bm-devkit/README.md`
  "Time" records it and `stm32_rtc.c`.

  | Item | Shape |
  |---|---|
  | `bm_stack::utc_time::decode(topic, kind, version, data) -> Result<u64, UtcTimeError>` | the C's checks: `strncmp` topic, type 1 and version 1, then the first 8 bytes little-endian; `Short` where the C reads past `data` |
  | `UtcTimeSetter` | `on_event` holds a time from a publication delivered to the `TOPIC` subscription only; `is_pending` for `ready`; `apply(rtc)` sets it in `act` |
  | `bm_devkit::rtc::DevkitRtc` | `stm32_rtc.c` over the STM32 RTC on LSE: `DR0` magic, two-digit year, `SHIFTR` for milliseconds; `get` fails until set; `Devkit`'s clock, from `Board::rtc` |

  | Decision | Reason |
  |---|---|
  | In `bm-stack`, not `bm-devkit` | `bm-devkit` has no host tests; `the_spotters_utc_time_sets_the_clock` feeds the bench's bytes through `MockPhy` |
  | A helper for an `App`, not a `Node` method | the C has it in the app; `on_event` has no node, so the set waits one loop pass |
  | The STM32 RTC, as the C | survives a reset; same `DR0` flag and calendar as the C, so time the C set should survive a reflash; `config` turns LSE on for it |

  On E1's bench (PR #45): LSE starts; an unset clock leaves `bm time get`
  unanswered; the Spotter publishes type 1, version 1 and the clock is set
  from it; `get` returns the Spotter's time and 10 s later a time 10 s
  later; the clock answers after a reset; `bringup` runs. Not run: a
  reflash over C firmware that had set its clock.

---

## Order

No cards remain.
