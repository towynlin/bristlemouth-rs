# Divergences between bm_core's C and the Rust port

`bm-wire` reproduces bm_core's observable behaviour bit-for-bit, because
interoperating with deployed C nodes is the requirement. Where the C does
something surprising, wrong, or undefined, the port matches it and the finding
is recorded here for repair in
[`bristlemouth/bm_core`](https://github.com/bristlemouth/bm_core).

Fixes go upstream; a submodule bump is what changes `bm-wire`. Do not edit
`vendor/bm_core/` from this repo.

Status values:

- **replicated** — `bm-wire` matches the C. Fixing the C is wire-visible and
  needs coordination.
- **domain-limited** — the C is undefined for these inputs. The differential
  harness constrains the input instead, documented at the comparator.
- **benign** — technically undefined, but every real toolchain produces the
  intended value and the port produces it by construction.
- **c-only** — a defect in state or an API `bm-wire` has no counterpart for.

## Recommended upstream priority

Four to fix first, in this order.

| Rank | # | Why now |
|---|---|---|
| 1 | [#29](#29-bcmppingc-echoes-and-compares-an-unchecked-payload_len) | Remote memory disclosure. One unauthenticated frame from any node on the link makes a node transmit up to 1460 bytes of adjacent heap to a multicast address. No prior state needed. The fix is one comparison against `data.size`, already in the struct. |
| 2 | [#12](#12-the-egress-port-checksum-patch-drops-the-end-around-carry) | Live frame loss on deployed hardware: ~1 BCMP frame in 40 000 leaves a two-port node with a checksum the far end rejects. Ports M1 and M2 raised the rate from theoretical to routine, and config/DFU bodies will raise it further. The fix is additive — a fixed node is strictly more interoperable. |
| 3 | [#20](#20-ll_remove-leaves-lltail-pointing-at-a-freed-node) | Was a runner-up on reading; card M3 made it a measured remote write. `cargo fuzz run info` reaches the use-after-free in `ll_item_add` from three unauthenticated frames, because `INFO_REQUEST_LIST` is removed from out of insertion order on keys the sender chooses. Two lines to fix, but it needs a maintainer who can confirm the list invariants. |
| 4 | [#14](#14-device-info-and-neighbour-table-replies-are-parsed-with-unchecked-lengths) | Same class as #29 in two more parsers, reachable by any node on the link, and `topology.c`'s length also wraps in `uint16_t`. Not wire-visible to fix. |

## Index

| # | Where | Status | Found by |
|---|---|---|---|
| 1 | `bm_l2_policy_prepare_forwarded_copy` doc contradicts code | replicated | reading |
| 2 | `check_endianness` swaps 32 bits over a 16-bit field | replicated | reading |
| 3 | `ip_to_nodeid` returns 0 on big-endian | replicated | reading |
| 4 | `__builtin_ffs` can yield a port number above 15 | replicated | reading |
| 5 | `utc_from_date_time` reads `MONTH_DAYS` out of bounds | domain-limited | reading |
| 6 | `uint8_to_uint32` shifts into the sign bit | benign | UBSan, via `cargo fuzz run addr` |
| 7 | `bm_l2_policy_rx_apply` doc claims an egress-nibble clear that is not in the code | replicated | reading |
| 8 | `bcmp_tx`'s size guard uses `sizeof(BcmpHeartbeat)` where it means `sizeof(BcmpHeader)` | replicated | reading |
| 9 | `process_received_message` rewrites the source address before verifying the checksum | replicated | reading |
| 10 | A rejected frame is left with its checksum field zeroed | replicated | reading |
| 11 | `clear_ports_legacy` does a misaligned 32-bit access on every received frame | benign | UBSan |
| 12 | The egress-port checksum patch drops the end-around carry | replicated | reading, measured, hit by `cargo fuzz run ping` |
| 13 | L2 silently drops any frame whose destination is not multicast | replicated | reading |
| 14 | Device-info and neighbour-table replies are parsed with unchecked lengths | domain-limited | reading |
| 15 | `bcmp_remove_neighbor_from_table` frees, and reports the free's result | c-only | a double free while writing the comparator |
| 16 | An advertised liveliness lease wraps in 32-bit arithmetic | replicated | reading, confirmed differentially |
| 17 | A new neighbour is announced to the application twice | replicated | reading, confirmed differentially |
| 18 | A node id of zero is never recognised | replicated | reading, confirmed differentially |
| 19 | `INFO_REQUEST_LIST` grows without de-duplication or expiry | c-only | reading |
| 20 | `ll_remove` leaves `LL::tail` pointing at a freed node | domain-limited | reading, then hit by `cargo fuzz run info` |
| 21 | A sequenced reply is matched on its sequence number alone | replicated | reading, confirmed differentially |
| 22 | A sequenced request's timeout is the 150 ms sweep, not the 24 ms constant | replicated | reading, then measured |
| 23 | `bcmp_ll_forward` replaces the originator's source address with the forwarder's | replicated | reading, confirmed differentially |
| 24 | A forwarded frame's multicast MAC carries the egress port | replicated | reading, confirmed differentially |
| 25 | `bcmp_ll_forward` writes its new checksum into the frame it forwards | c-only | reading |
| 26 | `bcmp_ll_forward` reports a forward with nowhere to go as `BmEINVAL` | replicated | reading |
| 27 | A system-time `target_node_id` of zero is a broadcast for `0x12`, a dead letter for `0x10`/`0x11` | replicated | reading, confirmed differentially |
| 28 | A forwarded global-multicast message goes out twice, the second time link-local | replicated | reading, confirmed differentially |
| 29 | `bcmp/ping.c` echoes and compares an unchecked `payload_len` | domain-limited | reading |
| 30 | A ping reply is matched on 16 bits of node id and the payload, nothing else | replicated | reading |
| 31 | `bcmp_send_ping_reply` echoes a `seq_num` that `serialize` discards | replicated | reading, confirmed differentially |
| 32 | `bcmp/ping.c` reports the result of a ping to nobody, and never forgets one | c-only | reading |
| 33 | BCMP's node-id-keyed lists hold only the low 32 bits of the id | replicated | reading, confirmed differentially |
| 34 | A restarted neighbour is asked about `info.node_id`, which is zero until it has answered once | replicated | reading, confirmed differentially |

---

## 1. `bm_l2_policy_prepare_forwarded_copy` clears the egress nibble its doc promises to keep

`network/l2_policy.h` documents the function as clearing the ingress nibble and
leaving the egress nibble intact. `network/l2_policy.c` calls both
`clear_ingress_nibble(pb)` and `clear_egress_nibble(pb)`, so the whole ports
byte at IPv6 source offset +2 goes to `0x00` on every forwarded copy.

**Ruling:** the code is authoritative — changing it is an interop break.
`bm-wire` zeroes both nibbles. The doc comment is the bug to fix.

## 2. `check_endianness` swaps 32 bits over a 16-bit field

`bcmp/packet.c`, `BcmpEchoRequestMessage` arm:

```c
swap_16bit(&request->id);
swap_32bit(&request->seq_num);   /* seq_num is uint16_t */
swap_16bit(&request->payload_len);
```

The `swap_32bit` reads and writes four bytes across a two-byte field,
corrupting `payload_len`, which is then swapped again. The
`BcmpEchoReplyMessage` arm uses `swap_16bit` correctly.

Only reachable on a big-endian host — the function is guarded by
`if (!is_little_endian())` — so it is latent on every shipped target, and not
reachable from this harness. `bm-wire` uses explicit little-endian codecs and
is endian-agnostic by construction.

## 3. `ip_to_nodeid` returns 0 on big-endian

`common/util.h`:

```c
static inline uint64_t ip_to_nodeid(const BmIpAddr *ip) {
  uint32_t high_word = 0, low_word = 0;
  if (ip && is_little_endian()) { /* ... */ }
  return (uint64_t)high_word << 32 | (uint64_t)low_word;
}
```

No `else`, so on a big-endian host every address maps to node id 0. A
`//TODO: make this endian agnostic and platform agnostic` sits above it. The
function also reads the 16-byte, 1-byte-aligned `BmIpAddr` through a
`uint32_t *` — a strict-aliasing violation and a possibly misaligned load.

`BmIpAddr::to_node_id` always performs the little-endian-host behaviour, which
is what the C does on every real target.

## 4. `bm_l2_policy_rx_apply` can report a port number above its documented range

`network/l2_policy.h` documents `ingress_port_num` as "1-15, or 0 if invalid".
`network/l2_policy.c` computes it with `__builtin_ffs`, which returns up to 32:

```c
const uint8_t ingress_port_num = (uint8_t)__builtin_ffs((unsigned)ingress_port_mask);
```

A mask of `0x8000` yields 16. `set_ingress_nibble` then masks with `0x0F`,
writing a zero ingress nibble into the frame while the returned struct still
reports 16, so the frame and the result disagree.

Not reachable today: the shim exposes 2 ports and L2 never builds a mask with a
bit above 15 set. `bm-wire` replicates it.

**Open question:** reject a mask above bit 15, or make the nibble and the
reported number consistent?

## 5. `utc_from_date_time` reads `MONTH_DAYS` out of bounds for `month > 12`

`common/util.c`:

```c
static const uint8_t MONTH_DAYS[] = { 31, 28, /* ... */ 31 };  /* 12 entries */

for (i = 1; i < month; i++) {
  seconds += secs_per_day * MONTH_DAYS[i - 1];
}
```

`month` is an unvalidated `uint8_t`. For 13..=255 the loop indexes past the
array. An attacker-influenced or corrupted RTC value reaches this.

**domain-limited.** `bm-wire-diff`'s `DateTimeInput` constrains `month` to
1..=12 and says so at the type; `bm_wire::util::utc_from_date_time` stops at
the end of the table. Validating `month` upstream is not wire-visible.

## 6. `uint8_to_uint32` shifts into the sign bit

`common/util.h`:

```c
static inline uint32_t uint8_to_uint32(uint8_t *buf) {
  return (uint32_t)(buf[3] | buf[2] << 8 | buf[1] << 16 | buf[0] << 24);
}
```

`buf[0]` promotes to `int`, so `buf[0] << 24` shifts a set bit into the sign
bit when `buf[0] >= 0x80`. Reported by UBSan during `cargo fuzz run addr`:

```
util.h:124:66: runtime error: left shift of 255 by 24 places
cannot be represented in type 'int'
```

Reachable on every host, including `ethernet_get_type` and the BCMP header
parse path.

**benign.** Every mainstream compiler produces the intended value and the
differential comparison shows no mismatch. The fix is a cast on each operand.
`uint8_to_uint16` is unaffected — `0xFF << 8` fits in an `int`.

## 7. `bm_l2_policy_rx_apply` documents an egress-nibble clear it never performs

`network/l2_policy.h` says the function "clears the egress nibble in the src
addr after callback". `network/l2_policy.c` only records the mask:

```c
policy_result.should_submit = routing_cb(ingress_port_num, &egress, src_ip, dst_ip);
policy_result.egress_mask = egress;
```

Whatever the callback wrote into the source address stays in the frame and is
submitted up the stack that way. Same class as #1, in the same header, and the
two interact: the RX buffer keeps what the callback left, and the forwarded
copy has *both* nibbles zeroed.

`bm-wire` matches the code. `L2PolicyInput::cb_src_write` makes the fake
callback write the ports byte, and both implementations must end up with the
same frame. Both doc comments need correcting.

## 8. `bcmp_tx`'s size guard uses the wrong `sizeof`

`bcmp/bcmp.c`:

```c
if (dst && (uint32_t)size + sizeof(BcmpHeartbeat) <= max_payload_len) {
  buf = bm_ip_tx_new(dst, size + sizeof(BcmpHeader));
```

The guard checks against `sizeof(BcmpHeartbeat)` (12) while the allocation uses
`sizeof(BcmpHeader)` (13), so a `size` of 1448 passes the guard and builds a
1461-byte IPv6 payload, one byte over `max_payload_len` (1460).

**replicated**, in that `bm-wire` imposes no ceiling of its own:
`bcmp::tx::serialize` takes a caller-sized frame and fails cleanly if it is too
small. The fix is a one-word change, and is not wire-visible.

`bm_stack::Node::send` does have a ceiling, because a node has one transmit
buffer: `MTU` is 1514, so a body of 1447 is the largest that fits and 1448 is
refused. Nothing reaches that size today.

## 9. `process_received_message` rewrites the source address before verifying the checksum

`bcmp/packet.c`:

```c
#define clear_ports_legacy(x) (x[1] &= (~(0xFFFFU)))
#define clear_ingress_port(x) (((uint8_t *)x)[2] &= 0xF)
/* ... */
data.ingress_port = (((uint8_t *)data.src)[2] >> 4) & 0xF;
clear_ports_legacy(((uint32_t *)data.src));
clear_ingress_port(data.src);

checksum_read = data.header->checksum;
data.header->checksum = 0;
checksum_calc = PACKET.cb.checksum(payload, size + sizeof(BcmpHeader));
```

Source-address bytes 2, 4 and 5 — frame offsets 24, 26 and 27 — are rewritten
*before* the checksum is computed, and the checksum covers the source address.
The receiver therefore checksums an address that never appeared on the wire.

This is load-bearing and undocumented: it is what lets a receiver stamp the
ingress port into the source address on arrival (spec 5.4.4.1/2) without
invalidating the sender's checksum. An implementation that verifies over the
address as received rejects every frame a real node sends.

`clear_ports_legacy` is marked as backwards compatibility for bm_core < v0.13.0
and is expected to disappear once resource-based routing lands, which will be
wire-visible.

`bm-wire` reproduces the rewrite in `bcmp::rx::accept`; `BcmpInput`'s
`ingress_stamp` and `legacy_ports` fields stamp both after the frame is built.
**The upstream fix is a comment, not a code change.**

One detail to state precisely: "a frame carrying the legacy port bytes fails
its checksum" is false for two values. Frame bytes 26–27 are one aligned 16-bit
word of the one's-complement sum, and both `0x0000` and `0xFFFF` are zero in
one's-complement arithmetic, so a frame whose legacy bytes are `FF FF` survives
the clear with a valid checksum. `cargo fuzz run forward` found this against a
comparator predicate that said `legacy == [0, 0]`. See
`legacy_port_bytes_of_all_ones_leave_the_checksum_valid` in
`bm-wire-diff/tests/forward.rs` and the seed
`bm-wire/fuzz/seeds/forward/system-time-for-another-legacy-ones`.

## 10. A rejected frame is left with its checksum field zeroed

```c
checksum_read = data.header->checksum;
data.header->checksum = 0;
checksum_calc = PACKET.cb.checksum(payload, size + sizeof(BcmpHeader));
if (checksum_calc != checksum_read) {
  err = BmEBADMSG;
  return err;              /* checksum field still zero */
}
data.header->checksum = checksum_read;
```

The early return skips the restore, so a caller that inspects, logs or forwards
a rejected frame sees `0x0000` rather than what arrived. Nothing in bm_core
reads the buffer after a rejection today. It is still observable state, so
`bm-wire` matches it and says so at `bcmp::rx::RxError::BadChecksum`. Restoring
the field before returning is not wire-visible.

## 11. `clear_ports_legacy` performs a misaligned 32-bit access

```c
#define clear_ports_legacy(x) (x[1] &= (~(0xFFFFU)))
clear_ports_legacy(((uint32_t *)data.src));
```

`data.src` is frame offset 22, so `x[1]` is a `uint32_t` read-modify-write at
offset 26 — `2 mod 4` however the frame is aligned. Also a strict-aliasing
violation, as in #3. UBSan reports both the load and the store.

This is on the path of **every BCMP frame bm_core receives**, so it fires on
the first receive of any fuzz run. `bm-wire-sys/build.rs` therefore passes
`-fno-sanitize=alignment` under `CARGO_CFG_FUZZING` and nothing else;
`shift-base` (which found #6), the rest of UBSan and ASan stay on.

**benign.** Both shipped targets permit unaligned word access, and `bm-wire`
clears the two bytes directly. The fix is to write the macro in `uint8_t`
terms:

```c
#define clear_ports_legacy(x) (((uint8_t *)(x))[4] = 0, ((uint8_t *)(x))[5] = 0)
```

## 12. The egress-port checksum patch drops the end-around carry

`network/l2.c` stamps the egress port into the IPv6 source address on the way
out, changing a byte the upper-layer checksum covers. `network_add_egress_port`
patches the checksum rather than recomputing it:

```c
add_egress_port(payload, port_num);   /* payload[24] |= port_num */

if (ipv6_get_next_header(payload) == ip_proto_udp) {
  payload[udp_checksum_offset] ^= 0xFFFF;
  payload[udp_checksum_offset] += port_num;
  payload[udp_checksum_offset] ^= 0xFFFF;
} else if (ipv6_get_next_header(payload) == ip_proto_bcmp) {
  BcmpHeader *header = (BcmpHeader *)&payload[bcmp_packet_offset];
  header->checksum ^= 0xFFFF;
  header->checksum += port_num;
  header->checksum ^= 0xFFFF;
}
```

The approach is sound and necessary — `process_received_message` clears only
the ingress nibble, so the sender's egress nibble is still present when the
receiver checksums. What is missed is the one's-complement end-around carry,
and the two branches fail differently because the lvalues have different types:

- **UDP.** `payload[udp_checksum_offset]` is a `uint8_t`. `^= 0xFFFF`
  truncates to `^= 0xFF` and `+= port_num` is 8-bit, so a carry out of the high
  byte is lost. Exhaustively: wrong in **30720 of 983040 cases (3.12%)**.
- **BCMP.** `header->checksum` is a `uint16_t`, so the carry propagates one
  place — and because the stored value is byte-swapped relative to the wire,
  that lands where the end-around carry belongs. Wrong only when the carry
  itself carries: **120 of 983040 cases (0.0122%)**.

Severity runs the other way from the rates. `bm_l2_process_tx_evt` stamps only
**link-local multicast**, and bm_core's UDP traffic goes to
`multicast_global_addr`, so the 3.12% path is latent. BCMP sends heartbeats,
pings and info to `multicast_ll_addr` and is stamped on every transmission, so
the 0.0122% path is **live on deployed hardware**: roughly one BCMP frame in
40 000 leaves a two-port node with a checksum the far end rejects and drops.

Since ping and system time were ported, that rate is real rather than
theoretical. Before them, every BCMP body came from a fixed `DeviceCfg` or a
slow-moving uptime counter, so a node either hit the case constantly or never.
Both new exchanges put caller-chosen bytes on the wire — a free-running 64-bit
timestamp, an echoed payload — and the first run of each fuzz target produced
an unverifiable frame within minutes. Config and DFU will be worse.

Pinned from both directions in `bm-wire-diff/tests/l2_egress.rs`: the
comparator asserts `l2::add_egress_port` emits the same bytes as bm_core, and
two further tests assert those bytes are *wrong*. Seeds
`bm-wire/fuzz/seeds/time/stamped-checksum-carries-twice` and
`bm-wire/fuzz/seeds/ping/reply-checksum-double-carry`, with
`a_response_whose_stamped_checksum_carries_twice_is_unverifiable`
(`tests/time.rs`) and
`a_reply_whose_stamped_checksum_double_carries_is_wrong_on_both_sides`
(`tests/ping.rs`). If those start passing, the C has been fixed and the port
must follow.

A comparator that reads a frame bm_core stamped must not require it to
validate: some of them correctly do not. `bm-wire-diff/src/ping.rs` first
failed for exactly that reason, classifying captured frames with `rx::accept`.

Fix, for BCMP:

```c
uint32_t sum = (uint32_t)(uint16_t)(header->checksum ^ 0xFFFF) + port_num;
header->checksum = (uint16_t)(((sum & 0xFFFF) + (sum >> 16)) ^ 0xFFFF);
```

and the same for UDP on a properly-read 16-bit field.
`network_revert_checksum` needs the mirrored change. Wire-visible only in that
it fixes frames currently discarded, so it needs no lockstep rollout.

## 13. L2 silently drops any frame whose destination is not multicast

`bm_l2_process_tx_evt`:

```c
if (is_global_multicast(dst_ip)) {
  send_global_multicast_packet(payload, tx_evt->length, tx_evt->port_mask);
} else if (is_link_local_multicast(dst_ip)) {
  /* ... stamp and send per port ... */
}

bm_l2_free(tx_evt->buf);
```

No `else`. A unicast-addressed frame — including the `FD00::/8` addresses
`bm_ip_init` derives for every node — is accepted by `bm_l2_link_output`,
queued, dequeued and freed without reaching the network device. `BmOK` is
returned and nothing is logged.

Consistent with the protocol as it stands, where everything is multicast and
`pubsub.c`'s `//TODO: Add functionality for resource based routing` marks
unicast as future work. `bm-wire`'s `l2::tx_kind` names the case
`TxKind::Dropped` rather than folding it into a default. A `bm_debug` line and
a returned error would fix it, and are not wire-visible.

## 14. Device-info and neighbour-table replies are parsed with unchecked lengths

Both variable-length BCMP replies declare their own sizes, and bm_core copies
per those declarations without comparing them to how many bytes arrived.
`BcmpProcessData` carries a `size` field; neither parser reads it.

`bcmp/info.c`, `populate_neighbor_info`:

```c
neighbor->version_str = (char *)bm_malloc(dev_info->ver_str_len + 1);
memcpy(neighbor->version_str, &dev_info->strings[0], dev_info->ver_str_len);
/* ... */
neighbor->device_name = (char *)bm_malloc(dev_info->dev_name_len + 1);
memcpy(neighbor->device_name, &dev_info->strings[dev_info->ver_str_len],
       dev_info->dev_name_len);
```

Both lengths are `uint8_t` off the wire, so a reply carrying only its 38-byte
fixed part but declaring 255 and 255 copies 510 bytes past the end of the
frame into two heap buffers, which are then held in the neighbour table and
printed by `bcmp_print_neighbor_info`.

`integrations/topology.c`, `neighbor_request_cb`, is worse:

```c
uint16_t neighbor_table_len =
    sizeof(BcmpNeighborTableReply) +
    sizeof(BcmpPortInfo) * reply->port_len +
    sizeof(BcmpNeighborInfo) * reply->neighbor_len;
neighbor_entry->neighbor_table_reply =
    (BcmpNeighborTableReply *)bm_malloc(neighbor_table_len);
/* ... */
memcpy(neighbor_entry->neighbor_table_reply, reply, neighbor_table_len);
```

Saturated, the declarations ask for 655 871 bytes out of a frame that may have
carried eleven. The sum is also accumulated in a `uint16_t`, so it wraps: the
`bm_malloc` and the `memcpy` agree with each other but not with reality.

**Reachability.** Neither is gated on anything an attacker cannot arrange.

- The info path needs an `INFO_REQUEST_LIST` entry for the sender.
  `bcmp_process_heartbeat` calls `bcmp_request_info` whenever a neighbour's
  `time_since_boot_us` goes backwards, which the neighbour chooses. Send a
  heartbeat, send a second with a lower uptime, and the node asks for info.
- The topology path needs `SENT_REQUEST` and a matching `TARGET_NODE_ID`, which
  is the node being asked. Node ids are in every heartbeat.

Both need only link access. That is the threat model Bristlemouth assumes for a
physical bus, but neither should be a memory-safety boundary.

**domain-limited.** `DeviceInfoReply::decode` and `NeighborTableReply::decode`
validate every declared length against the buffer and return
`BmWireError::Truncated` otherwise, and the decoded message borrows the frame
rather than copying out of it. `BcmpMessagesInput` only ever hands bm_core
well-formed requests; its `decode_probe` bytes go to the Rust decoders and
never to the C.

The fix is to check `data.size` before trusting any declared length in both
parsers, and to accumulate the neighbour-table length in a `uint32_t`. Not
wire-visible.

## 15. `bcmp_remove_neighbor_from_table` frees, and reports the wrong result

`bcmp/messages/neighbors.h` presents the two halves of a removal as separate:

```c
bool bcmp_remove_neighbor_from_table(BcmpNeighbor *neighbor);
bool bcmp_free_neighbor(BcmpNeighbor *neighbor);
```

and `bcmp_free_neighbor`'s doc comment says "NOTE: this does NOT remove
neighbor from table", which reads as an instruction to call both. Doing so is a
double free:

```c
    if (!rval) {
      bm_debug("Something went wrong...\n");
    }

    // Free the neighbor
    rval = bcmp_free_neighbor(neighbor);
  }
  return rval;
```

Two problems:

- It frees unconditionally, including when the node was not found in the list,
  so a stale or foreign pointer is freed anyway.
- `rval` is overwritten by the free's result, so the function returns "did the
  free succeed" while its doc promises "true if successful" at removing. It
  returns `true` for a node that was never in the list.

Found by `bm-wire-diff/src/neighbor.rs` aborting on its second test, with
valgrind putting the first free inside `bcmp_remove_neighbor_from_table`; the
comparator now calls only that one.

**c-only.** `NeighborTable` owns its storage and removal is an array shift. Fix
upstream by renaming the function to say that it frees, or splitting it
honestly and fixing the return value.

## 16. An advertised liveliness lease wraps in 32-bit arithmetic

`neighbor_check` in `bcmp/neighbors.c`:

```c
if (neighbor->online &&
    !time_remaining(neighbor->last_heartbeat_ticks, bm_get_tick_count(),
                    bm_ms_to_ticks(2 * neighbor->heartbeat_period_s * 1000))) {
```

`heartbeat_period_s` is a `uint32_t` from the heartbeat's
`liveliness_lease_dur_s`, so `2 * period * 1000` wraps at 2^32. A neighbour
advertising 2 147 483 648 seconds gets a lease of **zero milliseconds** and is
marked offline by the next check. The wrap starts at 2 147 484 seconds, well
below the field's range. bm_core itself always sends 10, so this is reachable
only from a peer's advertised value.

`Neighbor::lease_ms` reproduces it with `wrapping_mul`, and
`advertised_leases_across_the_range` in `bm-wire-diff/tests/neighbor.rs` sweeps
the boundary. Fix by widening to 64-bit and clamping.

## 17. A new neighbour is announced to the application twice

`bcmp_update_neighbor` invokes the discovery callback on insert:

```c
neighbor = bcmp_add_neighbor(node_id, port);
if (neighbor) {
  bcmp_neighbor_invoke_discovery_cb(true, neighbor);
  bcmp_request_info(node_id, &multicast_ll_addr, NULL);
}
```

and `bcmp_process_heartbeat`, having just called it, invokes it again:

```c
if (!neighbor->online || neighbor_reset) {
  bcmp_neighbor_invoke_discovery_cb(true, neighbor);
}
```

`bcmp_add_neighbor` zeroes the entry, so `online` is still false at the second
test. Every newly discovered neighbour is reported twice, on the same
heartbeat, with the same pointer.

Confirmed differentially: the comparator registers a real
`NeighborDiscoveryCallback` and compares call counts.
`HeartbeatOutcome::discovery_callbacks` is 2 for a new neighbour. Fix by
dropping the call in `bcmp_update_neighbor`, whose caller already covers it.

## 18. A node id of zero is never recognised

`bcmp_find_neighbor`:

```c
while (neighbor != NULL) {
  if (node_id && node_id == neighbor->node_id) {
    break;
  }
  neighbor = neighbor->next;
}
```

The `node_id &&` guard means a lookup for zero always fails, even with a
zero-id neighbour in the table. Node ids come from `ip_to_nodeid(data.src)`, so
a node whose link-local address is exactly `fe80::` has one.

Every heartbeat from such a node is a first sighting: it is added again (which
evicts the previous copy of itself from the port), fires the discovery callback
twice (#17) and sends another `bcmp_request_info`. The table does not grow, but
the node is permanently "new", a genuine restart can never be detected, and
each heartbeat leaks an `INFO_REQUEST_LIST` entry (#19).

`NeighborTable::find` reproduces the guard;
`a_zero_node_id_is_rediscovered_on_every_heartbeat` covers it. Fix by dropping
the guard and rejecting a zero id where it is *received* instead.

## 19. `INFO_REQUEST_LIST` grows without de-duplication or expiry

`bcmp_request_info` records every request:

```c
item = ll_create_item(item, &info_cb, sizeof(info_cb), target_node_id);
if (item) {
  err = ll_item_add(&INFO_REQUEST_LIST, item);
```

`ll_item_add` appends unconditionally, so requesting the same node twice leaves
two entries. The only removal is in `bcmp_process_info_reply`, one entry per
reply. A node that is asked and never answers leaves its entry for the life of
the process, and `ll_get_item` walks the list linearly.

Ordinary operation is bounded by how often neighbours appear. Combined with #18
it is not: a node heartbeating from `fe80::` appends an entry per heartbeat.
`bcmp/packet.c` solves the same problem with a 150 ms sweep that expires
entries and fires their callbacks with `NULL`; `INFO_REQUEST_LIST` has no
equivalent.

**c-only.** `bm-wire` is sans-io; `HeartbeatOutcome::request_info` tells the
runtime a request is owed. This is why the `neighbor` fuzz target needs
`-fork=1`. Fix by de-duplicating on the id and expiring entries as `packet.c`
does.

## 20. `ll_remove` leaves `LL::tail` pointing at a freed node

`common/ll.c` keeps `LL` doubly linked but maintains only `next` on removal:

```c
    if (current) {
      if (current == ll->head) {
        ll->head = current->next;
        if (current == ll->tail) {
          ll->tail = NULL;
        }
      } else {
        ret = BmOK;
        if (current == ll->tail) {
          ll->tail = current->previous;
        }
        previous->next = current->next;
      }
      ret = ll_delete_item(current);
    }
```

Two omissions:

- removing the head does not clear the new head's `previous` — harmless, since
  the only read of `previous` is in the tail branch and a head takes the head
  branch first;
- removing a node from the **middle** does not fix up the following node's
  `previous`, which now points at freed memory. If that node is later removed
  as the tail, `ll->tail = current->previous` stores the freed pointer into the
  list.

The next `ll_item_add` dereferences it:

```c
    if (ll->head) {
      node->previous = ll->tail;
      ll->tail->next = node;      // write through a freed pointer
```

Four ordinary operations on `packet.c`'s `sequence_list` reach it:

1. three sequenced requests outstanding — `[A, B, C]`;
2. `B`'s reply arrives, `B` is unlinked from the middle and freed;
   `C->previous` dangles;
3. `C`'s reply arrives; `C` is the tail and not the head, so
   `ll->tail = C->previous`, the freed `B`;
4. a fourth request is sent, and `ll_item_add` writes into the freed block.

The write is what a sanitizer reports. What a deployed node sees is quieter:
`ll->head` still points at `A`, whose `next` was set to `NULL` in step 3, so the
request added in step 4 is **not reachable from the head**. `ll_get_item` never
finds it, so its reply is treated as unsolicited; `ll_traverse` never visits it,
so the expiry sweep never fires its callback — not with a payload, and not with
`NULL`. The caller waits forever.

`ll_remove` is shared by every list in bm_core, so anything removing out of
insertion order is exposed.

**`INFO_REQUEST_LIST` reaches it from the network, and card M3 measured that.**
`cargo fuzz run info` reported the heap-use-after-free at `ll.c:159` within
four minutes of its first run, from a sequence any node on the link can send:

1. three `bcmp_request_info` calls leave three entries — two of them are what
   `bcmp_update_neighbor` issues for any two new neighbours;
2. a device-info reply whose body claims the *middle* entry's node id unlinks
   it, leaving the third entry's `previous` dangling;
3. a second reply claiming the third entry's node id stores that freed pointer
   into `LL::tail`;
4. the next new neighbour — one heartbeat — makes `bcmp_request_info` write
   through it.

The keys are the sender's to choose, since `bcmp_process_info_reply` looks up
`info->info.node_id` out of the reply body and compares only its low 32 bits
(#33). Nothing about this needs the attacker to be a neighbour, to guess a
sequence number, or to win a race, and #19 means the entries never expire out
from under it.

`bcmp/config.c` is exposed on `packet.c`'s `sequence_list` for the same reason:
it is the only module issuing sequenced requests, and it issues several
concurrently.

**domain-limited.** `bm-wire-diff/src/ll.rs`'s `LinkModel` tracks which of the
C's `previous` pointers are stale and declines the append that would be
undefined; `bm-wire-diff/src/registry.rs` and `bm-wire-diff/src/info.rs` both
use it, and the seeds `bm-wire/fuzz/seeds/registry/dangling-tail` and
`bm-wire/fuzz/seeds/info/ll-tail-uaf-domain-limit` drive each shape to the
edge. The fix is two lines: clear the new head's `previous`, and set
`current->next->previous = current->previous` before freeing.

## 21. A sequenced reply is matched on its sequence number alone

`new_sequence_list_item` records `element.type = type`, and nothing ever reads
it again. `process_received_message` looks the entry up by number:

```c
      if (cfg->sequenced_reply && !cfg->sequenced_request) {
        request_message = sequence_list_find_message(data.header->seq_num);
```

`sequence_list_find_message` is an id lookup, so any reply type whose sequence
number matches an outstanding request consumes it and invokes its callback with
a payload of a different shape — a `BcmpConfigValue` can answer a
`BcmpNeighborProtoRequest`.

The numbers are not hard to line up: a single global counter starting at zero
on boot, incrementing by one per request, carried in the request's header on
the wire. Anything that can see a request can answer it with any registered
reply type and be believed. Since `config.c` is the only module issuing
sequenced requests today, the everyday case is one config exchange's reply
credited to another's.

`a_reply_of_the_wrong_type_answers_the_request_anyway` in
`bm-wire-diff/tests/registry.rs` sends a `NEIGHBOR_PROTO_REQUEST` and answers it
with a `CONFIG_VALUE`; adding a type comparison to `Registry::on_received` makes
the C and the port diverge. `PendingRequest::message_type` is carried for the
caller as the C carries it, without taking part in the match.

`bm_stack::Node` inherits it —
`a_reply_of_the_wrong_type_answers_the_request_anyway` in
`bm-stack/tests/node.rs` — so `Event::Reply` carries both the request's and the
reply's type and says at the type that they need not agree.

Fix by comparing `element->type` against the type the reply answers, which
needs the registry to record which request type each reply type answers.

## 22. A sequenced request's timeout is the sweep period, not the timeout

`bcmp/packet.c`:

```c
#define default_message_timeout_ms 24
#define message_timer_expiry_period_ms 150
```

Every sequenced request is stamped with the first. Nothing consults it except
`timer_traverse_cb`, which runs only from `sequence_list_timer_callback`, which
runs only when the 150 ms auto-reload timer fires. The 24 ms is a threshold
applied on a 150 ms grid, so a request gets neither number:

| Sent at | Expired at | Lived for |
|---|---|---|
| 125 ms (25 before a sweep) | 150 ms | **25 ms** |
| 0 ms (on a sweep) | 150 ms | 150 ms |
| 126 ms (24 before a sweep) | 300 ms | **174 ms** |

One millisecond of phase is the difference between 25 ms and 174 ms, for
identical traffic. The nominal 24 ms is the one value a request can never get,
because the comparison is strict.

Measured rather than inferred: `the_effective_timeout_across_the_whole_phase`
in `bm-wire-diff/tests/registry.rs` walks the clock a millisecond at a time
against the real timer in the shim, and
`the_effective_timeout_ranges_from_25_to_174_milliseconds` in `bm-wire`'s unit
tests asserts the shape.

Card C3 is where it matters: `bcmp/config.c` is the only module issuing
sequenced requests, and a config get whose reply arrives in the wrong part of
the phase is reported to the application as a failure (`cb(NULL)`) and then
delivered again as an unsolicited `BcmpConfigValue`.

`Registry::on_tick` carries the sweep's phase and only sweeps when one is due,
so the port times out the same requests at the same instants. Sweeping on every
tick instead fails six of the comparator's tests. `bm_stack::Node::on_expiry`
is `sequence_list_timer_callback`, driven from a ticker of `EXPIRY_PERIOD_MS`
separate from the heartbeat ticker; see
`an_unanswered_request_dies_on_the_sweep_rather_than_on_its_timeout` and
`a_reply_that_arrives_after_the_timeout_is_reported_twice` in
`bm-stack/tests/node.rs`.

Fix by making the sweep period the timeout, or by driving expiry from each
entry's own deadline.

## 23. `bcmp_ll_forward` replaces the originator's source address with the forwarder's

`bcmp/bcmp.c`'s doc comment says only "Forward the payload to all ports other
than the ingress port." What the code does is build a new datagram:

```c
void *forward = bm_ip_tx_new(&multicast_ll_addr, size + sizeof(BcmpHeader));
```

`bm_ip_tx_new` fills in the source address itself, and has only one to give:

```c
uint8_t *ip = (uint8_t *)bm_l2_get_payload(buf) + ETH_HDR_LEN;
memcpy(ip + 8, &CTX.ll_addr, 16);   /* this node's link-local address */
```

So the frame leaving the far port claims the forwarder as its IPv6 source. The
originator survives only in whatever the message body carries, and the checksum
is recomputed. The hop limit is reset to 64, so hop count is not recoverable
either.

Harmless for the three exchanges that forward today — system time, config and
DFU all carry a `source_node_id` in their own body headers. But
`process_received_message` hands every processor `data.src`, and
`ip_to_nodeid(data.src)` is how several of them identify a peer, so anything
that grows a reliance on it will silently see the last hop, and only on
multi-hop networks.

`bcmp::forward::serialize_forwarded` checksums against whatever source address
the caller wrote, and `bm_stack::Node::forward_link_local` writes this node's
own. `the_c_puts_its_own_address_on_a_forwarded_message` in
`bm-wire-diff/tests/forward.rs` asserts the C does the same. Fix by documenting
the rewrite, or by carrying the originator's address over — the second is
wire-visible.

## 24. A forwarded frame's multicast MAC carries the egress port

bm_core has no per-port transmit call, so `bcmp_ll_forward` encodes the port
into the IPv6 destination and lets L2 read it back out:

```c
uint8_t port_specific_dst[sizeof(multicast_ll_addr)];
memcpy(port_specific_dst, &multicast_ll_addr, sizeof(multicast_ll_addr));
((uint32_t *)port_specific_dst)[3] = 0x1000000 | (egress_port << 8);

BmErr tx_err = bm_ip_tx_perform(forward, (BmIpAddr *)port_specific_dst);
```

`bm_l2_link_output` reads destination byte 13, sets the port mask from it, and
clears the byte, so the address on the wire is a clean `FF02::1`. The C
checksums before the byte goes on.

The Ethernet header is not so lucky. `bm_ip_tx_perform` derives the destination
MAC from the address it was handed, **before** L2 clears anything:

```c
if (is_multicast(effective_dst)) {
  multicast_mac_from_ipv6(frame, effective_dst);   /* 33:33 + bytes 12..16 */
}
```

Those bytes are `00 <port> 00 01` at that moment, so a forwarded frame leaves
port 2 addressed to `33:33:00:02:00:01` while its IPv6 destination says
`FF02::1`, whose correct mapped MAC is `33:33:00:00:00:01`.

On a point-to-point Bristlemouth link there is nothing filtering on a multicast
MAC group address. Anything that does filter — a switch, a non-promiscuous host
NIC, a capture matched on the mapped MAC — sees a frame addressed to a group
nobody joined.

`bcmp::forward::apply_port_specific_destination` writes the address and
re-derives the MAC from it in `bm_ip_tx_perform`'s order, and
`the_egress_port_survives_in_the_multicast_mac` in
`bm-wire-diff/tests/forward.rs` pins the byte. Fix by building the MAC from
`multicast_ll_addr` and passing the port to L2 some other way; wire-visible,
but only to a receiver that was dropping these frames already.

## 25. `bcmp_ll_forward` writes its new checksum into the frame it was asked to forward

`header` points into the received frame — `process_received_message` sets
`data.header = (BcmpHeader *)buf` — and `bcmp_ll_forward` uses it as scratch:

```c
header->checksum = 0;
bm_ip_tx_copy(forward, header, sizeof(BcmpHeader), 0);
bm_ip_tx_copy(forward, payload, size, sizeof(BcmpHeader));
header->checksum = packet_checksum(forward, size + sizeof(BcmpHeader));
bm_ip_tx_copy(forward, header, sizeof(BcmpHeader), 0);
```

Two bytes of the caller's received frame are overwritten with a checksum
computed for a different frame. Invisible today: the only readers of the RX
buffer after a processor returns are `bm_ip_rx_cleanup` and the `free` under
it, and the loop re-zeroes the field before each port. A processor that
forwarded a message and then re-examined its own header would find the wrong
checksum, and nothing in the signature warns it.

`bcmp::forward::serialize_forwarded` takes the received message as `&[u8]` and
cannot write to it. Fix with a local `BcmpHeader` copy — thirteen bytes of
stack, nothing on the wire.

## 26. `bcmp_ll_forward` reports a forward with nowhere to go as `BmEINVAL`

```c
BmErr err = BmEINVAL;
for (uint8_t egress_port = 1; egress_port <= num_ports; egress_port++) {
  if (egress_port == ingress_port) {
    continue;
  }
  /* ... */
  err = BmOK;   /* only ever set here */
}
return err;
```

On a one-port device — or wherever `ingress_port` is the only port — the loop
body never runs and the caller is told its arguments were invalid for a forward
that was correctly a no-op. `bcmp_time_process_time_message`,
`bcmp_process_config_message` and `dfu_copy_and_process_message` return that
value as their own, so a single-port node reports every forwarded message as a
failure.

An `ingress_port` of zero has the opposite effect: zero means the sender encoded
no port, no port matches the `continue`, and the message is forwarded back out
the interface it arrived on. On a two-port node that is a duplicate on one port
and a loop on the other.

`bcmp::forward::egress_ports` skips nothing for an ingress port of zero, and
`bcmp::forward::ll_forward_is_a_no_op` is the predicate for the `BmEINVAL`. Fix
by returning `BmOK` when there was nothing to do, and refusing an ingress port
of zero.

## 27. A system-time `target_node_id` of zero is a broadcast for one message type and a dead letter for the other two

`bcmp_time_process_time_message` tests `target_node_id` twice, and the two tests
disagree about what zero means:

```c
if (msg_header->target_node_id != node_id() &&
    msg_header->target_node_id != 0) {
  should_forward = true;                      /* zero is "for everyone" */
  break;
}
switch (time_msg_type) {
case BcmpSystemTimeRequestMessage: {
  if (msg_header->target_node_id != node_id()) {
    break;                                    /* zero is "for nobody" */
  }
  /* ... */
}
case BcmpSystemTimeResponseMessage: {
  if (msg_header->target_node_id != node_id()) {
    break;                                    /* zero is "for nobody" */
  }
  /* ... */
}
case BcmpSystemTimeSetMessage: {
  bcmp_time_process_time_set_msg(...);        /* no second test at all */
}
```

| Message | `target == node_id()` | `target == 0` | `target == somebody else` |
|---|---|---|---|
| `0x10` system time request | answered | **silently dropped** | forwarded |
| `0x11` system time response | logged | **silently dropped** | forwarded |
| `0x12` system time set | applied, answered | applied, answered | forwarded |

So `bcmp_time_get_time(0)` builds a message, transmits it, and every node
discards it — the obvious way to ask "does anybody here know what time it is"
cannot work. `bcmp_time_set_time(0)` is the mirror image and does work: one
frame sets every node's clock and every one answers. Undocumented;
`bcmp_time_get_time`'s doc comment says only "Gets the system time from a
target node."

bm_core's `time_test.cpp` pins all three rows:

```cpp
// Test request process with target id of 0
req_msg.header.target_node_id = 0;
ASSERT_EQ(packet_process_invoke(BcmpSystemTimeRequestMessage, data), BmEINVAL);
ASSERT_EQ(bcmp_tx_fake.call_count, 0);
```

`SystemTimeRequest::is_for` and `SystemTimeResponse::is_for` are the exact-match
test, `SystemTimeSet::is_for` the broadcast one, and `SystemTimeHeader::is_local`
the forwarding test all three share. The table is asserted in
`zero_is_a_broadcast_for_a_set_and_a_dead_letter_for_the_other_two`
(`bm-wire/src/bcmp/time.rs`) and pinned against the C in
`a_broadcast_is_honoured_only_by_the_set_message` (`bm-wire-diff/tests/time.rs`).

Fix by dropping the two inner tests, which makes `0x10` and `0x11` agree with
`0x12` and with every other addressed BCMP message. Wire-visible in the useful
direction: nothing can currently be relying on these being ignored, since
nothing has ever answered them.

## 28. A global-multicast message that is forwarded is put on the wire twice, the second time link-local

Two layers can move a received frame onward, and neither knows about the other.

L2 goes first: `bm_l2_process_rx_evt` applies the routing policy, and for an
`FF03::1` destination the policy asks for egress on every port but the ingress
one *and* for local submission. The frame is copied, both port nibbles are
cleared, and the copy is queued.

The local copy then reaches BCMP, and if it is a system-time message for a third
node, `bcmp_time_process_time_message` hands it to `bcmp_ll_forward` — which
knows nothing of destinations and always builds for `multicast_ll_addr`.

So one `FF03::1` system-time message arriving on port 1 of a two-port node
leaves port 2 twice:

1. the relayed copy, still `FF03::1`, still from the originator;
2. a re-flood, `FF02::1`, from the forwarder (#23).

The second copy is the damaging one. `FF03::1` is Bristlemouth's *global*
multicast, and the re-flood demotes it to link-local. A node one hop further on
sees the link-local copy and forwards it link-local again, so the message keeps
travelling, but its destination no longer says what it is, and anything routing
on the destination rather than on `target_node_id` sees a global message that
stopped being global at the first node that did not own it. Every hop carries
two copies.

`bcmp/config.c` and `bcmp/dfu_core.c` forward the same way.

`bm_stack::Owed` carries the L2 relay and the re-flood as separate fields,
`bm_stack::Node::reflood` performs the second, and
`a_global_multicast_for_a_third_node_is_forwarded_twice` in
`bm-wire-diff/tests/time.rs` asserts the C emits the same two frames on the same
port in the same order.

Fix by having `bcmp_ll_forward` take the received destination, or by having the
processors skip the forward when L2 has already relayed. Either is wire-visible,
but the copy that stops arriving is a duplicate.

## 29. `bcmp/ping.c` echoes and compares an unchecked `payload_len`

Same shape as #14, in a second module. `BcmpProcessData` carries `size` — how
many body bytes arrived — and both of ping's processors ignore it in favour of
a length read out of the frame.

`bcmp_process_ping_request` answers with:

```c
return bcmp_tx(addr, BcmpEchoReplyMessage, (uint8_t *)echo_reply,
               sizeof(*echo_reply) + echo_reply->payload_len, seq_num, NULL);
```

A fourteen-byte request declaring `payload_len = 0xFFFF` makes the node
transmit 65 549 bytes starting at the received frame — up to 64 KiB of whatever
the allocator had next — to a multicast address. `bcmp_tx`'s `max_payload_len`
guard rejects the largest of these, but everything up to 1460 bytes goes out.
Remote memory disclosure reachable by any node on the link, needing one frame
and no prior state.

`bcmp_process_ping_reply` has the read half:

```c
if (EXPECTED_PAYLOAD_LEN == echo_reply->payload_len && ...) {
  if (EXPECTED_PAYLOAD != NULL) {
    if (memcmp(EXPECTED_PAYLOAD, echo_reply->payload, echo_reply->payload_len) != 0) {
```

Here the declared length must equal what this node last pinged with, so the read
is bounded by the node's own choice — but it is still a read of `payload_len`
bytes from a frame that may carry none of them.

**domain-limited.** `bm_wire::bcmp::ping`'s decoders validate the declared
length against the buffer and return `BmWireError::Truncated`;
`bm-wire-diff/src/ping.rs` never injects a request whose `payload_len` is not
what it carries, and its `decode_probe` bytes never reach the C.

The fix is one comparison against `data.size`, which is already in the struct.

## 30. A ping reply is matched on sixteen bits of node id and the payload, and nothing else

`bcmp_process_ping_reply` accepts a reply when the declared `payload_len` equals
`EXPECTED_PAYLOAD_LEN`, `(uint16_t)node_id()` equals the reply's `id`, and — only
if `EXPECTED_PAYLOAD` is non-null — the payload bytes compare equal.

What it never looks at:

- **`echo_reply->seq_num`.** `bcmp_send_ping_request` increments `BCMP_SEQ` per
  request and the reply echoes it faithfully; nothing compares it. A reply to a
  ping sent an hour ago answers today's, as long as the payload is the same.
- **`echo_reply->node_id`,** and the source address it arrived from. A ping
  aimed at one node is answered by whichever node replies first, or by one that
  was never pinged.
- **Whether a ping is outstanding.** The statics are never cleared, so a node
  that has pinged once accepts that reply's shape for the rest of its uptime.

The `id` is the only correlation, and `ping.c:42` makes it `(uint16_t)node_id()`
with a `TODO` saying it should be random. Two nodes sharing the low sixteen bits
of their ids cannot tell each other's ping traffic apart.

**replicated.** `bm_wire::bcmp::ping::EchoReply::answers` is that rule and
`bm_stack::Node` keeps the single slot it reads from. Fix with a random `id` per
request and a `seq_num` comparison; both are wire-compatible, since the fields
already exist and are already echoed.

## 31. `bcmp_send_ping_reply` echoes a `seq_num` that `serialize` then discards

```c
static BmErr bcmp_send_ping_reply(BcmpEchoReply *echo_reply, void *addr,
                                  uint16_t seq_num) {
  return bcmp_tx(addr, BcmpEchoReplyMessage, (uint8_t *)echo_reply,
                 sizeof(*echo_reply) + echo_reply->payload_len, seq_num, NULL);
}
```

called as `bcmp_send_ping_reply((BcmpEchoReply *)echo_req, data.dst, echo_req->seq_num)`
— the request's *body* sequence number, passed down to be written into the
*header*. It never gets there: `ping_init` registers both echo types as
`{false, false}`, and `serialize` writes the caller's number only for a
`sequenced_reply`, zero otherwise.

So every echo reply on the wire carries a header sequence number of zero, and
the parameter and its three casts exist to be thrown away. The correlation the
author reached for is in the body, where the in-place reuse of the request
buffer had already preserved it.

**replicated**, and measured: `bm-wire-diff/tests/ping.rs` compares the reply
frame the oracle builds against the one `bm_stack::Node` builds, header
included, and both carry zero. `Node::build_echo_reply` passes the body's
`seq_num` to the registry anyway, so the discarding happens in the same place.

Fix by deleting the parameter, or by registering the reply as `sequenced_reply`
and using it — the second is wire-visible.

See also #2, the other thing that happens to `BcmpEchoRequest::seq_num`.

## 32. `bcmp/ping.c` reports the result of a ping to nobody, and never forgets one

`bcmp_send_ping_request` takes no callback, and `BcmpEchoReplyMessage` is
registered unsequenced, so `packet.c`'s sequenced-reply machinery — the one path
that reaches an application with `cb(data.payload)` — is never involved. When
`bcmp_process_ping_reply` decides a reply matches, all that happens is a
`bm_debug` line and a `BmOK` that `process_received_message` discards. Nothing
above BCMP can learn that a ping succeeded, and nothing at all can learn that
one failed.

Three smaller things travel with it:

- **`PING_REQUEST_TIMEOUT` times nothing out.** It is stamped after every
  `bcmp_tx` and read once, to print `time=%llu ms`.
- **`EXPECTED_PAYLOAD` is kept forever.** Nothing frees it but the next
  `bcmp_send_ping_request`, which is what makes #30's "for the rest of its
  uptime" true.
- **Its `bm_malloc` is not checked.** `EXPECTED_PAYLOAD = bm_malloc(payload_len)`
  is followed immediately by a `memcpy`, so an allocation failure is a
  null-pointer write rather than a refused ping.

**c-only** for the reporting: `bm_stack::Event::EchoReply` is the verdict
bm_core keeps to itself, reported alongside the `Event::Message` that
`process_received_message` dispatched. The acceptance rule has no oracle —
`bcmp_process_ping_reply` is `static`, transmits nothing and reports nothing —
so `bm-wire-diff/src/ping.rs` compares the two frames on the wire and
`bm_wire::bcmp::ping`'s unit tests assert the rule from the reading.

The allocation is the one place `bm-stack` diverges deliberately: it keeps a
fixed slot, `Node`'s `PING_PAYLOAD`, and `Node::ping` refuses a payload that
will not fit rather than sending a ping whose reply it could not check.

Fix with a callback argument on `bcmp_send_ping_request`, a null check, and a
real timeout.

## 33. BCMP's node-id-keyed lists hold only the low 32 bits of the id

`common/ll.h` gives an `LLItem` a 32-bit identifier:

```c
typedef struct LLItem {
  struct LLItem *next;
  struct LLItem *previous;
  void *data;
  uint32_t id;
  uint8_t dynamic;
} LLItem;
```

`bcmp/info.c` keys that list on node ids, which are 64-bit, at both ends of
the exchange:

```c
item = ll_create_item(item, &info_cb, sizeof(info_cb), target_node_id);
/* ... */
err = ll_get_item(&INFO_REQUEST_LIST, info->info.node_id, (void **)&cb);
/* ... */
ll_remove(&INFO_REQUEST_LIST, info->info.node_id);
```

Both `uint64_t` arguments are truncated by the implicit conversion, so two
nodes whose ids agree in their low 32 bits are one entry. A device-info reply
from either one satisfies and clears a request made about the other, and takes
that request's callback with it.

Reachability is the same as #14's: the sender chooses the `node_id` in the
reply body, so no id collision is even needed — a node that answers with
someone else's low half is matched against their outstanding request. What
happens next is gated on `bcmp_find_neighbor(info->info.node_id)`, which does
compare all 64 bits, so the *cache* is written against the claim rather than
against the request.

`bcmp/resource_discovery.c` has the same defect in the same shape:
`RESOURCE_REQUEST_LIST` is created with `target_node_id` at line 355 and read
with `src_node_id` at 164, both `uint64_t`. Card M5 inherits it.
`bcmp/packet.c` is unaffected — its two lists are keyed on a `uint32_t`
sequence number and a `uint16_t` message type.

**replicated.** `bm_wire::bcmp::info::InfoRequests` keys on
`node_id as u32` and says so at `InfoRequests::key`. Fix by widening `LLItem::id`
to `uint64_t`, which is not wire-visible, or by comparing the full id in
`bcmp_process_info_reply` after the list hit.

## 34. A restarted neighbour is asked about `info.node_id`, which is zero until it has answered once

`bcmp/info.c` is asked for a neighbour's information from two places, and they
name the neighbour differently. `bcmp/neighbors.c:304`, on insert:

```c
      bcmp_request_info(node_id, &multicast_ll_addr, NULL);
```

`bcmp/heartbeat.c:56`, when a neighbour's `time_since_boot_us` goes backwards:

```c
      bcmp_request_info(neighbor->info.node_id, &multicast_ll_addr, NULL);
```

`neighbor->info` is the `BcmpDeviceInfo` that `populate_neighbor_info` writes
when a reply is consumed, and `bcmp_add_neighbor` `memset`s the whole entry to
zero. So `info.node_id` is **0** for any neighbour that has not answered a
device-info request yet, and the restart request goes out as a broadcast —
`target_node_id == 0`, which every node on the link answers — rather than as a
question for the node that restarted.

Two consequences, both on the wire:

| State when the restart arrives | `target_node_id` sent | Who answers |
|---|---|---|
| No reply cached | `0` | every node on the link |
| A reply cached | the neighbour's id | the neighbour |

and `INFO_REQUEST_LIST` gains an entry keyed `0` in the first case, which the
first reply to arrive from *any* node claiming id 0 would clear — and nothing
else ever will, per #19.

A neighbour that restarts before answering is the common case, not a corner
one: `bcmp_update_neighbor` asks for information the moment the entry exists,
and a node that reboots twice inside one round trip is in exactly this state.

**replicated.** `bm_stack::Node::on_frame` reads the target out of its info
cache on the reset path and sends zero when there is nothing there;
`bm-wire-diff/src/info.rs` compares the request frame the two nodes build for
every step, so the byte that differs is the one under test. Fix by passing
`neighbor->node_id`, which is always populated. Wire-visible: a fixed node
stops broadcasting where an unfixed one does.
