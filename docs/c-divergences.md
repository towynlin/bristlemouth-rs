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
- **open** — `bm-wire` does not yet match; the entry names the card that
  changes it.
- **fixed upstream** — bm_core has repaired it, the submodule includes the
  fix, and the port and the C agree. The entry is kept as the record.

## Recommended upstream priority

Three to fix first, in this order. #29, previously first, is fixed in
bm_core `c77daa8` ([bristlemouth/bm_core#165](https://github.com/bristlemouth/bm_core/pull/165)).

| Rank | # | Why now |
|---|---|---|
| 1 | [#12](#12-the-egress-port-checksum-patch-drops-the-end-around-carry) | Live frame loss on deployed hardware: ~1 BCMP frame in 40 000 leaves a two-port node with a checksum the far end rejects. Ports M1 and M2 raised the rate from theoretical to routine, and config/DFU bodies will raise it further. The fix is additive — a fixed node is strictly more interoperable. |
| 2 | [#20](#20-ll_remove-leaves-lltail-pointing-at-a-freed-node) | Was a runner-up on reading; card M3 made it a measured remote write. `cargo fuzz run info` reaches the use-after-free in `ll_item_add` from three unauthenticated frames, because `INFO_REQUEST_LIST` is removed from out of insertion order on keys the sender chooses. Two lines to fix, but it needs a maintainer who can confirm the list invariants. |
| 3 | [#14](#14-device-info-and-neighbour-table-replies-are-parsed-with-unchecked-lengths) | Same class as #29, fixed the same way, in two more parsers, reachable by any node on the link, and `topology.c`'s length also wraps in `uint16_t`. Not wire-visible to fix. |

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
| 22 | A sequenced request's timeout is the 150 ms sweep, not the 24 ms constant | replicated | reading, then measured; retries re-measured at `c77daa8` |
| 23 | `bcmp_ll_forward` replaces the originator's source address with the forwarder's | replicated | reading, confirmed differentially |
| 24 | A forwarded frame's multicast MAC carries the egress port | replicated | reading, confirmed differentially |
| 25 | `bcmp_ll_forward` writes its new checksum into the frame it forwards | c-only | reading |
| 26 | `bcmp_ll_forward` reports a forward with nowhere to go as `BmEINVAL` | replicated | reading |
| 27 | A system-time `target_node_id` of zero is a broadcast for `0x12`, a dead letter for `0x10`/`0x11` | replicated | reading, confirmed differentially |
| 28 | A forwarded global-multicast message goes out twice, the second time link-local | replicated | reading, confirmed differentially |
| 29 | `bcmp/ping.c` echoes and compares an unchecked `payload_len` | fixed upstream (`c77daa8`) | reading; fix confirmed differentially |
| 30 | A ping reply is matched on 16 bits of node id and the payload, nothing else | replicated | reading |
| 31 | `bcmp_send_ping_reply` echoes a `seq_num` that `serialize` discards | replicated | reading, confirmed differentially |
| 32 | `bcmp/ping.c` reports the result of a ping to nobody, and never forgets one | c-only | reading |
| 33 | BCMP's node-id-keyed lists hold only the low 32 bits of the id | replicated | reading, confirmed differentially |
| 34 | A restarted neighbour is asked about `info.node_id`, which is zero until it has answered once | replicated | reading, confirmed differentially |
| 35 | A broadcast neighbour-table request is answered by every node and accepted from none | replicated | reading, confirmed differentially |
| 36 | A neighbour-table request's callback and timer outlive the request | replicated | reading, confirmed differentially |
| 37 | A resource-table request naming node zero is answered by nobody | replicated | reading, confirmed differentially |
| 38 | `find_resource` compares the needle's length against every entry | domain-limited | reading |
| 39 | `bcmp/resource_discovery.c` mishandles four allocations | c-only | reading |
| 40 | A failed `cbor_parser_init` still reports a type, and still reports valid | c-only | reading, confirmed differentially |
| 41 | `cbor_value_get_int64` overflows on the one negative integer it cannot hold | c-only | reading, confirmed differentially |
| 42 | `services_cbor_as_map` reads an uninitialised `CborValue` when a key's value cannot be read | replicated; domain-limited (first key) | reading, confirmed differentially (card C1) |
| 43 | bm_core reads only the 5-byte float encoding, so a preferred-serialization float is unreadable to it | replicated | reading, confirmed differentially |
| 44 | The saved config partition's layout is the compiler's | replicated | reading, measured |
| 45 | Storing a config key ignores `key_len`; looking one up stops at a NUL | replicated | reading, confirmed differentially |
| 46 | A refused config set still writes; the typed setters and `set_config_cbor` disagree about a full partition | replicated | reading, confirmed differentially |
| 47 | A config partition that fails to load keeps the bytes it failed with | replicated | reading, confirmed differentially |
| 48 | A config image whose CRC checks may claim up to 255 keys | domain-limited | reading |
| 49 | `set_config_cbor` checks only the first item's head, and `get_config_cbor` returns the whole slot | replicated | reading, confirmed differentially |
| 50 | `bcmp_process_config_message` indexes `CONFIGS` with an unchecked partition byte | domain-limited | reading |
| 51 | `bcmp_process_config_message` reads message bodies without checking `data.size` | domain-limited | reading |
| 52 | `bcmp_config_decode_value` writes its NUL one byte past a full buffer | domain-limited | reading |
| 53 | Config replies echo only the low 16 bits of the request's sequence number | replicated | reading, then hit by `cargo fuzz run config` |
| 54 | DFU dispatches on the body's `frame_type`, not the header type, and leaks a body whose byte it does not know | replicated | reading |
| 55 | DFU bodies are read without checking `data.size` | domain-limited | reading |
| 56 | `bm_dfu_init` registers `0xD9` twice | replicated | reading |
| 57 | A DFU start with `chunk_size` zero divides by zero on the client | domain-limited | reading |
| 58 | The DFU error state reports every failure to the last host update's callback | replicated | reading, confirmed differentially |
| 59 | A host adopts its client's error code, and a code of 14 or more stops DFU until reboot | replicated | reading, confirmed differentially and against the C host |
| 60 | A second `bm_dfu_initiate_update` before the first runs is accepted and lost | replicated | reading, confirmed differentially |
| 61 | A non-internal host update leaks its stream buffer unless it reaches `HostUpdate` | c-only | LeakSanitizer, via `cargo fuzz run dfu_core` |
| 62 | A DFU start during a transfer restarts it against the first start's image | replicated | reading, confirmed differentially |
| 63 | A failed chunk write still requests the next chunk, and on the last chunk is reported as a length mismatch | replicated | reading, confirmed differentially |
| 64 | A client refusing an image as too large leaves the update slot open | replicated | reading, confirmed differentially |
| 65 | A rebooted client confirms its image on any `0xD3` from the host, whatever its `success` byte | replicated | reading, confirmed differentially |
| 66 | A client's chunk count is 16 bits | replicated | reading |
| 67 | A DFU host ignores the chunk number it is asked for | replicated | reading, confirmed differentially |
| 68 | A non-internal DFU host sends a whole chunk after a short read | domain-limited | reading |
| 69 | A host update's `timeoutMs` of zero is a zero timer period | domain-limited | reading |
| 70 | `bm_linux.c` writes a source MAC, hop limit and UDP source address that deployed nodes do not | replicated | capture, card H0 |
| 71 | `bm_linux.c` writes the UDP checksum byte-swapped, and a zero checksum as zero | replicated | differentially (byte order); reading lwIP (zero) |
| 72 | `bm_linux.c` delivers a received datagram by its UDP length field; lwIP ignores the field | replicated | reading lwIP, confirmed differentially |
| 73 | `bm_middleware_rx` dispatches on the datagram's source port | replicated | reading, confirmed differentially (card U2) |
| 74 | `bm_wildcard_match` matches any topic a `*`-free pattern prefixes | replicated | reading, confirmed differentially |
| 75 | `bm_handle_msg` wraps the data length of a topic longer than the payload | domain-limited | reading, confirmed differentially |
| 76 | `bm_pub_wl` sizes its buffer in 16 bits and copies past it | domain-limited | reading |
| 77 | `bm_pub_wl` with NULL data sends uninitialised bytes, and dereferences NULL for a local subscriber | c-only | reading |
| 78 | `bm_get_subs` writes past its 256-byte buffer | c-only | reading |
| 79 | `bm_sub_wl` checks only a topic's first callback for a duplicate | replicated | reading, confirmed differentially (cards P2, S1) |
| 80 | `bm_unsub_wl` returns `BmEINVAL` for a topic not subscribed | replicated | reading, confirmed differentially (card P2) |
| 81 | `spotter_log` budgets its text against `max_payload_len`, not the pub/sub message limit | replicated | reading, confirmed differentially (card S1) |
| 82 | Service body decoders read uints with an unchecked `cbor_value_get_uint64` | replicated (release build); domain-limited (tags) | reading, confirmed differentially (card M1) |
| 83 | `sys_info_reply_decode` sizes `app_name` from the sender's `app_name_strlen` | replicated; c-only (terminator, leak) | reading, confirmed differentially (card M1) |
| 84 | `config_cbor_map_reply_decode` reads `cbor_data` only when `success` and a length are set | replicated; domain-limited (not a byte string) | reading, confirmed differentially (card M1) |
| 85 | `BM_FIELD_STRING` is unimplemented in both field-table functions | replicated | reading, confirmed differentially (card M2) |
| 86 | `metrics_reply_decode` checks no top-level key, and matches field keys up to a NUL | replicated | reading, confirmed differentially (card M2) |
| 87 | A tagged field value makes `bm_decode_fields_from_table` advance past its map | domain-limited | `cargo fuzz run metrics_codec` (card M2) |
| 88 | `services_cbor_as_map` reads each value by its key's stored type | replicated (release build); domain-limited (failed `cbor_assert`) | reading, confirmed differentially (card C1) |
| 89 | `bm_service.c` matches services by `strncmp` prefix, and reads a request's header unchecked | replicated; domain-limited (reads past the datagram) | reading, confirmed differentially (card S1) |
| 90 | `echo_service_handler` copies a request of any length into its 1008-byte reply buffer | domain-limited | reading (card S1) |
| 91 | A failed `bm_service_request` leaves its request listed, to time out; long timeouts wrap | replicated | reading, confirmed on the oracle (card S2) |
| 92 | `_service_request_cb` reads a reply's header and `data_size` unchecked, and matches on id, not topic | replicated; domain-limited (reads past the datagram) | reading, confirmed differentially (card S2) |
| 93 | `bm_service_request` calls `memcpy` with a NULL source for an empty request | benign | UBSan, via `cargo fuzz run services` (card E1) |
| 94 | `config_map_service_handler` sends no reply for a map over its buffer, and the same failure reply for an unknown partition and a map that fails | replicated | reading, confirmed differentially (card E2) |
| 95 | `config_map_service_handler`'s failure reply encodes `cbor_data` from a NULL pointer | benign | UBSan, via `cargo fuzz run services` (card E2) |

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

`bcmp/resource_discovery.c`'s `RESOURCE_REQUEST_LIST` is the same list with the
same three properties: `bcmp_resource_discovery_send_request` appends
unconditionally and `bcmp_process_resource_discovery_reply` is the only
removal. It is worse in one respect — #37 means a request naming zero can never
be answered, so every such call leaks an entry — and better in another, since
nothing appends to it without an application asking.

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
concurrently. Since bm_core `61e75ed` (retries) an unanswered request stays on
the list for three sweeps longer, which widens the window.

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
        if (sequence_list_claim_message(data.header->seq_num, &cb) == BmOK) {
```

`sequence_list_claim_message` is an id lookup, so any reply type whose sequence
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

`bcmp/packet.c` and `bcmp/packet.h`:

```c
#define default_message_timeout_ms 24
#define message_timer_expiry_period_ms 150
#define packet_retry_count 3
```

Every sequenced request is stamped with the first. Nothing consults it except
`timer_traverse_cb`, which runs only from `sequence_list_timer_callback`, which
runs only when the 150 ms auto-reload timer fires. The 24 ms is a threshold
applied on a 150 ms grid. A request is kept while `ms - timestamp_ms <
timeout_ms`; the first sweep that finds it expired re-sends it and restamps it,
and so do the next two, since each is 150 ms later. The fourth times it out.

| Sent at | First expiry | After | Timed out at |
|---|---|---|---|
| 126 ms (24 before a sweep) | 150 ms | **24 ms** | 600 ms |
| 0 ms (on a sweep) | 150 ms | 150 ms | 600 ms |
| 127 ms (23 before a sweep) | 300 ms | **173 ms** | 750 ms |

One millisecond of phase is the difference between a first retry after 24 ms
and one after 173 ms, for identical traffic. Before bm_core `61e75ed` there
were no retries, the comparison was strict (`>`), and the range was 25 ms to
174 ms to the timeout itself.

Measured rather than inferred: `the_effective_timeout_across_the_whole_phase`
and `the_timeout_boundary_from_both_sides` in `bm-wire-diff/tests/registry.rs`
walk the clock against the real timer in the shim, and
`the_first_expiry_ranges_from_24_to_173_milliseconds` in `bm-wire`'s unit
tests asserts the shape.

Card C3 is where it matters: `bcmp/config.c` is the only module issuing
sequenced requests, and a config get whose reply arrives after the fourth
sweep is reported to the application as a failure and then delivered again as
an unsolicited `BcmpConfigValue`.

`Registry::on_tick` carries the sweep's phase and only sweeps when one is due,
so the port retries and times out the same requests at the same instants,
reporting `Expiry::Retry` and `Expiry::TimedOut`. Sweeping on every tick
instead fails six of the comparator's tests. `bm_stack::Node::on_expiry` is
`sequence_list_timer_callback`, driven from a ticker of `EXPIRY_PERIOD_MS`
separate from the heartbeat ticker, and `Node::next_retransmission` hands back
the re-sends; `our_sequenced_request_carries_the_number_the_c_would_have_given_it`
in `bm-wire-diff/tests/node_frames.rs` compares them byte for byte against
the frames the C re-sends. See also
`an_unanswered_request_is_retried_on_the_sweep_rather_than_at_its_timeout` and
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

**fixed upstream** in bm_core `c77daa8`
([bristlemouth/bm_core#165](https://github.com/bristlemouth/bm_core/pull/165)).
Both processors now return `BmEBADMSG` unless `data.size` holds the fixed
fields and the declared payload, testing the first before reading
`payload_len`. A request declaring less than it carries is still answered,
with the declared payload.

`bm_wire::bcmp::ping`'s decoders always did the same, returning
`BmWireError::Truncated`. `bm-wire-diff/src/ping.rs` now injects its
`decode_probe` bytes into the C as an echo request body, so malformed requests
are compared rather than excluded. Against `f06b3b4`, the commit before the
fix, five tests in `bm-wire-diff/tests/ping.rs` fail with the C answering a
request the port declined:

- `a_saturated_payload_length_is_refused_rather_than_trusted`
- `a_request_declaring_one_byte_more_than_it_carries_is_answered_by_nobody`
- `a_body_too_short_for_the_fixed_fields_is_answered_by_nobody`
- `the_decoders_survive_arbitrary_bytes`
- `every_committed_seed_still_agrees_with_the_c`

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
`RESOURCE_REQUEST_LIST` is created with `target_node_id` and read with
`src_node_id`, both `uint64_t`. `ResourceRequests::key` replicates it, and
`the_request_list_is_keyed_on_half_an_id` (`bm-wire-diff/tests/resource.rs`)
compares it. That half is *not* reachable from the wire the way `bcmp/info.c`'s
is: `bcmp_process_resource_discovery_reply` requires `repl->node_id` to equal
the source address before it consults the list, so the key is the sender's real
id rather than one it chose.
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

## 35. A broadcast neighbour-table request is answered by every node and accepted from none

`bcmp/neighbors.c` tests the target twice, at opposite ends of the exchange,
and the two tests disagree about what zero means.

The responder treats it as "for everyone":

```c
if (request->target_node_id == 0 || node_id() == request->target_node_id) {
  err = bcmp_send_neighbor_table(data.dst);
}
```

The requester wants an exact match against the id the *replier* puts in its own
body, and `bcmp_send_neighbor_table` fills that in as
`neighbor_table_reply->node_id = node_id()`:

```c
if (TARGET_NODE_ID == reply->node_id) {
```

| `target_node_id` asked for | Nodes that answer | Replies accepted |
|---|---|---|
| a specific node id | that one | that one's |
| `0` | **every node on the link** | **none** |

So `bcmp_request_neighbor_table(0, ...)` puts one frame on the wire, every node
answers it, every answer is discarded, and a second later the caller's
`timeout` fires instead. bm_core's own doc comment — "target node id to send
request to (0 for all nodes)" — is true of the request and false of the reply.
`integrations/topology.c` always names a specific node, so nothing upstream
notices.

The one id that *would* match a broadcast is zero itself, which belongs to a
node whose link-local address is exactly `fe80::` — the node of #18. This is
#18's mirror image: `bcmp_find_neighbor` refuses to match zero and nothing
else, while this matches zero and nothing else. And since `TARGET_NODE_ID` is a
zero-initialised static, a reply claiming node id zero is accepted **before any
request has been made**: with no callback armed all that happens is
`bm_timer_stop(NULL)`, but ask about node zero and that reply is reported as
its answer.

Same shape as #27 in a third module.

**replicated.** `TableRequests::accept` is the exact match and
`Node::request_neighbor_table` sends the broadcast. Pinned in
`a_broadcast_request_is_answered_by_everyone_and_accepted_from_nobody` and
`a_reply_claiming_node_zero_is_accepted_before_any_request`
(`bm-wire-diff/tests/neighbor_table.rs`), with the seed
`bm-wire/fuzz/seeds/neighbor_table/broadcast-unanswerable`.

Fix by dropping the responder's zero test, or by having the requester accept
any reply when it asked for everyone — the second is what the doc promises.
Neither is wire-visible: the frames are the same either way, and nothing can be
relying on a broadcast going unanswered, since nothing has ever consumed one.

## 36. A neighbour-table request's callback and timer outlive the request

`bcmp_request_neighbor_table` writes three statics and transmits last:

```c
TARGET_NODE_ID = target_node_id;
if (NEIGHBOR_TIMER) {
  bm_timer_delete(NEIGHBOR_TIMER, 10);
}
NEIGHBOR_TIMER = bm_timer_create("neighbor_request_timer",
                                 bcmp_neighbor_timer_timeout_s * 1000, false,
                                 NULL, timeout);
if (NEIGHBOR_TIMER) {
  err = bm_timer_start(NEIGHBOR_TIMER, 10);
  bm_err_check(err, bcmp_tx(...));
  NEIGHBOR_REQUEST_CB = request;
}
```

and `bcmp_process_neighbor_table_reply` is the only thing that ever clears one:

```c
if (TARGET_NODE_ID == reply->node_id) {
  err = bm_timer_stop(NEIGHBOR_TIMER, 10);
  if (NEIGHBOR_REQUEST_CB) {
    bm_err_check(err, NEIGHBOR_REQUEST_CB(reply));
    NEIGHBOR_REQUEST_CB = NULL;
  }
}
```

Three consequences:

- **The timeout gives up on nothing.** The timer's callback is the integrator's
  `timeout`, which bm_core hands to `bm_timer_create` untouched; it never sees
  `NEIGHBOR_REQUEST_CB`. A reply arriving a minute after the timeout is matched
  and reported as an answer. `integrations/topology.c` is exposed: its
  `BcmpTopoEvtTimeout` handler increments `RETRY_COUNT` and re-requests but
  never clears `SENT_REQUEST`, which is the only thing gating
  `neighbor_request_cb`, so a late reply is inserted into the walk as the
  *current* cursor's neighbour table.
- **`TARGET_NODE_ID` is never cleared.** After a successful exchange the last
  target is still the accepted one, so a duplicate reply reaches
  `bm_timer_stop` again. Inert on its own — the callback is null by then — and
  it is what keeps the point above reachable.
- **A request that failed to transmit is still armed.** The timer is created
  and started before `bcmp_tx` and nothing undoes it, so a caller gets an error
  return *and* a timeout a second later. topology.c calls `topology_end()` on
  the error and then takes a `BcmpTopoEvtTimeout` into a finished walk.

Compare `bcmp_request_info`, which does `ll_remove` its `INFO_REQUEST_LIST`
entry when `bcmp_tx` fails, and `packet.c`, whose sweep does clear the request
it gives up on (#22).

**replicated.** `TableRequests::on_timer` clears the timer and nothing else and
says so at the type; `Node::request_neighbor_table` records before it sends and
does not undo the record. Measured in
`a_reply_after_the_timeout_is_still_reported` and
`the_timeout_fires_once_per_request` (`bm-wire-diff/tests/neighbor_table.rs`),
with the seed `bm-wire/fuzz/seeds/neighbor_table/timeout-then-late-reply`. The
third bullet is from reading only: the comparator's requests always transmit,
so it is pinned one-sidedly in `an_unsendable_request_still_arms_the_timer`
(`bm-stack/tests/node.rs`).

Fix by giving `bm_timer_create` a bm_core trampoline that clears
`NEIGHBOR_REQUEST_CB` and `TARGET_NODE_ID` before invoking the caller's
`timeout`, and by stopping the timer when `bcmp_tx` fails. Neither is
wire-visible.

## 37. A resource-table request naming node zero is answered by nobody

Four BCMP request types carry a `target_node_id` whose `bcmp/messages.h`
comment reads "Zeroed = all nodes". Three implement it:

| Module | Test |
|---|---|
| `bcmp/info.c:89` | `if ((request->target_node_id == 0) \|\| (request->target_node_id == node_id()))` |
| `bcmp/ping.c:104` | `if ((echo_req->target_node_id == 0) \|\| (echo_req->target_node_id == node_id()))` |
| `bcmp/neighbors.c:90` | `if (request->target_node_id == 0 \|\| node_id() == request->target_node_id)` |

`bcmp/resource_discovery.c:105` does not:

```c
static BmErr bcmp_process_resource_discovery_request(BcmpProcessData data) {
  BmErr err = BmEBADMSG;
  BcmpResourceTableRequest *req = (BcmpResourceTableRequest *)data.payload;
  do {
    if (req->target_node_id != node_id()) {
      break;
    }
```

So a `0x0A` naming zero reaches every node on the link and is answered by none
of them, and there is no way to enumerate a network's resources in one
exchange — the caller has to know each node id first, which is what
`bcmp_resource_discovery_send_request`'s single `target_node_id` argument
assumes.

Two consequences beyond the missing broadcast:

- **A node whose link-local address is exactly `fe80::` answers only requests
  naming zero**, since `node_id()` is then zero. That is the same address #18
  makes invisible to the neighbour table.
- **The asymmetry is invisible from the requester side.** The request is sent
  to `multicast_ll_addr` either way and nothing times out (#19), so a caller
  that asks for zero waits forever with no error.

**replicated.** `ResourceTableRequest::is_for` is the exact match and says why;
`Node::submit` calls it rather than `addressed_to_us`, which is what the other
three use. Compared in `a_request_naming_zero_is_answered_by_nobody`
(`bm-wire-diff/tests/resource.rs` and `bm-stack/tests/node.rs`), with the seed
`bm-wire/fuzz/seeds/resource/zero-is-a-dead-letter`. The `bm-stack` test puts
the same request to `0x04` and `0x08` in the same node, which do answer it.

Fix by matching the other three modules. Wire-visible: a fixed node starts
answering a request a deployed node ignores, which is additive — nothing today
sends a `0x0A` naming zero expecting silence.

## 38. `bcmp_resource_discovery_find_resource` compares the needle's length against every entry

`bcmp/resource_discovery.c:33`:

```c
static bool bcmp_resource_discovery_find_resource_priv(
    const char *resource, const uint16_t resource_len, ResourceType type) {
  BcmpResourceNode *cur = res_list->start;
  while (cur) {
    if (memcmp(resource, cur->resource->resource, resource_len) == 0) {
      return true;
    }
    cur = cur->next;
  }
```

`cur->resource_len` is never read. Each entry is a `bm_malloc(sizeof(BcmpResource)
+ resource_len)` sized for *its own* name, so the comparison is against
`resource_len` bytes of an allocation that may be shorter. Two behaviours fall
out:

- **A shorter needle is a prefix match.** Searching for `sensor` finds a stored
  `sensor/temp`, and `bcmp_resource_discovery_add_resource` therefore refuses
  to add `sensor` — reporting `BmEAGAIN` for a name that is not in the list.
  A node that publishes `sensor/temp` can never also publish `sensor`, and
  `bcmp_resource_discovery_find_resource` tells the application a resource is
  present when it is not.
- **A longer needle reads out of bounds.** Searching for `sensor/temperature`
  in a list whose head is `sensor/temp` reads eight bytes past that entry's
  allocation. The needle comes from the application rather than off the wire,
  so this is not remotely reachable — but every resource name in a Bristlemouth
  network is a pub/sub topic, and `middleware/pubsub.c:400` calls this on every
  `bm_pub`, and `pubsub.c:177` on every `bm_sub`.

**domain-limited**, in the second half only. `ResourceTable::find` reproduces
the prefix match and its unit tests pin it; a needle longer than an entry the
walk reaches simply does not match, which is a choice rather than a port,
because there is no defined C behaviour to match. `ResourceTable::find_over_reads`
reports where the C would have gone out of bounds, and
`bm-wire-diff/src/resource.rs` uses it to keep every step it performs inside
the defined half: every needle is at most twelve bytes and nothing shorter than
that is ever stored. The lists have no remove, so that rule has to look ahead —
one short entry would make every longer needle undefined for the rest of the
process.

bm_core's own start-up reaches the second half. `bristlemouth_init`
registers metrics first, so `SUB_LIST` starts with `<id>/metrics/req`, 28
bytes; a dev kit's `app_main.cpp` then registers sys_info, whose
`<id>/sys_info/req` is 29, and every `bm_sub` of a longer topic follows. Each
compares one or more bytes past the metrics entry. The bytes differ at offset
17 (`m`, `s`), so the outcome is defined in practice, but ASan's default
`strict_memcmp=1` reports it. The `services` fuzz target sets
`strict_memcmp=0` (`__asan_default_options` in
`bm-wire/fuzz/fuzz_targets/services.rs`), which checks only the bytes up to
the first difference: a needle an entry prefixes is still reported.

Fix by comparing the lengths first:

```c
if (cur->resource->resource_len == resource_len &&
    memcmp(resource, cur->resource->resource, resource_len) == 0) {
```

Not wire-visible. It does change which `bcmp_resource_discovery_add_resource`
calls succeed, so a node that was silently sharing one entry between two topics
starts advertising both.

## 39. `bcmp/resource_discovery.c` mishandles four allocations

Four separate faults in one module, none of them reachable from the wire, all
in the same two functions.

**1. An unchecked `bm_malloc` is dereferenced.** `bcmp_resource_discovery_add_resource`
checks the node allocation and not the buffer one:

```c
  uint8_t *resource_buffer = (uint8_t *)bm_malloc(resource_size);
  BcmpResource *resource = (BcmpResource *)resource_buffer;
  resource->resource_len = resource_len;          // resource may be NULL
  memcpy(resource->resource, res, resource_len);

  BcmpResourceNode *resource_node =
      (BcmpResourceNode *)bm_malloc(sizeof(BcmpResourceNode));
  if (resource_node) {                            // this one is checked
```

**2. The same function leaks on the failure it does check.** When
`resource_node` is NULL it reports `BmENOMEM` and drops `resource_buffer`.

**3. The request handler leaks its reply.** `bcmp_process_resource_discovery_request`
`break`s out of its `do {} while (0)` on a populate failure, past the
`bm_free(reply_buf)` at the bottom:

```c
    if (!bcmp_resource_populate_msg_data(PUB, reply, &data_offset)) {
      bm_debug("Failed to get publishers list\n.");
      break;                                      // reply_buf leaks
    }
```

`bcmp_resource_discovery_get_local_resources` has the same shape and gets it
right, freeing on a `success` flag.

**4. The reply buffer is sized and filled under different locks.**
`bcmp_resource_compute_list_size` takes each list's semaphore, sums the sizes
and gives it back; `bcmp_resource_populate_msg_data` then takes it again and
`memcpy`s. A `bcmp_resource_discovery_add_resource` between the two — from the
application task, which is where every call to it comes from — grows a list
after its size has been decided, and the `memcpy` runs off the end of the
`bm_malloc`. This is a heap overflow in ordinary multi-threaded operation, not
an out-of-memory path.

**c-only.** `bm-wire` has no allocator, and `ResourceTable::encode_reply`
cannot race because the table is borrowed for the call; `Node::build_resource_table_reply`
says so. Fix 1–3 by checking and freeing; fix 4 by holding each list's
semaphore across both passes, or by sizing and filling in one.

## 40. A failed `cbor_parser_init` still reports a type, and still reports valid

`third_party/tinycbor/src/cborparser.c:168`, `preparse_value`, assigns the
type and the argument before anything can fail:

```c
    it->type = CborInvalidType;
    it->flags &= FlagsToKeep;
    if (!read_bytes(it, &descriptor, 0, 1))
        return CborErrorUnexpectedEOF;

    uint8_t type = descriptor & MajorTypeMask;
    it->type = type;                      // before every error return below
    it->extra = (descriptor &= SmallValueMask);
```

Only the empty-buffer case leaves `CborInvalidType` behind. Every other
failure — additional information 28, 29 or 30; an indefinite length on a type
that cannot have one; a buffer that ends inside the argument — returns an
error with `it->type` set to the masked first byte, so `cbor_value_is_valid`
is true and `cbor_value_get_type` answers. Three consequences:

- **A truncated integer reads back as its own additional-information byte.**
  `0x1b 0x00` is a `uint64` with one of eight argument bytes present.
  `cbor_parser_init` returns `CborErrorUnexpectedEOF`, and because the
  argument-reading block is what clears `extra`, `cbor_value_is_unsigned_integer`
  is true and `cbor_value_get_uint64` yields **27**, the additional
  information itself.
- **Major type 1 is left holding `0x20`**, which is not a `CborType`
  constant at all: `CborIntegerType` is `0x00` and the rewrite to it is the
  last thing `preparse_value` does. So `cbor_value_get_type` can return a
  value no `switch` over `CborType` has a case for, and every `cbor_value_is_*`
  predicate is false.
- **A lone break byte (`0xff`) reports `CborSimpleType` and valid**, with
  `CborErrorUnexpectedBreak`.

None of this reaches `bcmp/configuration.c`, which tests the `cbor_parser_init`
error first in all four places it parses. It is one dropped error check away
from doing so, and #42 is what that looks like.

**c-only.** `bm-wire` uses the `cbor2` crate, whose `Decoder::pull` returns a
`Result<Header, _>` — an error carries no value, so there is nothing to
misread and no state to inspect afterwards. The defect has no counterpart to
replicate.

The reverse asymmetry, and the only one this harness accepts: a **break byte
at the top level**. tinycbor reports `CborErrorUnexpectedBreak`; cbor2 returns
`Ok(Header::Break)` and leaves the judgement to the caller. Neither reads a
value out of it, and bm_core's callers reject it either way.
`bm-wire-diff/src/cbor.rs`'s `check_decode` names that one case and panics on
any other disagreement about whether a byte string is an item;
`every_first_byte_reads_the_same_way` walks all 256 first bytes, and the
`cbor` fuzz target walks the rest.

Fix upstream in tinycbor by assigning `it->type` after the error returns
rather than before. Not wire-visible; it changes what a caller that ignores
the error sees.

## 41. `cbor_value_get_int64` overflows on the one negative integer it cannot hold

`third_party/tinycbor/src/cbor.h:428`:

```c
CBOR_INLINE_API CborError cbor_value_get_int64(const CborValue *value, int64_t *result)
{
    assert(cbor_value_is_integer(value));
    *result = (int64_t) _cbor_value_extract_int64_helper(value);
    if (value->flags & CborIteratorFlag_NegativeInteger)
        *result = -*result - 1;
    return CborNoError;
}
```

CBOR encodes a negative integer as `-1 - argument`, so major type 1 with an
8-byte argument spans `-1` down to `-(2^64)`. `int64_t` reaches only
`-(2^63)`. For an argument of `1 << 63` — the nine bytes
`3b 80 00 00 00 00 00 00 00`, the value `-(2^63) - 1` — the cast gives
`INT64_MIN` and the negation overflows, which C leaves undefined. gcc and
clang wrap, so the call returns `INT64_MAX`: a request for the most negative
value representable answers with the most positive one, and reports
`CborNoError` doing it.

Arguments above `1 << 63` wrap without overflowing and are merely wrong:
`3b ff ff ff ff ff ff ff ff` is `-(2^64)` and reads back as `0`.
`cbor_value_get_int64_checked` exists and rejects both, and nothing in bm_core
calls it.

Reachable from the wire once card C3 lands: a `ConfigSet` (`0xA2`) body is
stored verbatim by `set_config_cbor`, which accepts it as `INT32`, and
`get_config_int` then reads it with this function.
`bm_wire::configuration::ConfigPartition::get_int` wraps, giving `-1` after
the narrowing to `int32_t`; `bm-wire-diff/src/configuration.rs` does not call
the C on that one value.

**c-only.** `cbor2` reports a negative integer as `Header::Negative(u64)` —
the encoded argument, not the represented value — so the narrowing is the
caller's to do, at the caller's width, and nothing overflows inside the
library. `bm-wire-diff/src/cbor.rs` compares that argument against the bytes
on the wire rather than against `cbor_value_get_int64`, precisely so the
comparison does not have to enter undefined behaviour to make its point;
`integer_head_boundaries` pins both `3b 80 00…` and `3b ff ff…`.

Fix upstream in tinycbor by computing the result as
`*result = -(int64_t)(v + 1)` on the unsigned value, or by returning
`CborErrorDataTooLarge` the way `cbor_value_get_int64_checked` does. Not
wire-visible.

## 42. `services_cbor_as_map` reads an uninitialised `CborValue` when a key's value cannot be read

`middleware/cbor_service_helper.c:55`:

```c
      if (get_config_cbor(type, key.key_buf, key.key_len, tmpB, &tmpBSize) &&
          cbor_parser_init(tmpB, tmpBSize, 0, &parser, &it) != CborNoError) {
        break;
      }
      if (!cbor_value_is_valid(&it)) {
        break;
      }
```

`it` is a function-scope `CborValue` with no initialiser. The `&&`
short-circuits, so when `get_config_cbor` fails `cbor_parser_init` never runs
and `it` is whatever it was:

| Which key | What `it` is |
|---|---|
| The first one in the loop | uninitialised stack |
| Any later one | the last parsed key's iterator, over `tmpB` |

`tmpB` is `memset` to zero before `get_config_cbor`, which fails before
copying, so in the second case the iterator keeps its cached head (`type`,
`flags`, `extra`) and reads zeros for everything else. The `switch` then
reads it by the current key's `value_type`:

| Previous head | Read as | Value written under the current key |
|---|---|---|
| uint or int with an argument up to `0xffff` | `UINT32` or `INT32` | the previous key's value, from `extra` |
| any head with a 4- or 8-byte argument | `UINT32`, `INT32` or `FLOAT` | 0, from the zeroed bytes |
| a string | `STR` or `BYTES` | none: the chunk head reads `0x00`, `CborErrorIllegalType` |

A key with no value leaves the map a value short, so the last row returns
`NULL`. The encoded map is what `services_cbor_encoded_as_crc32` hashes — so
two nodes with the same configuration can publish different CRC32s depending
on which key failed.

The intent is plainly `||`: every other error check in the file breaks out of
the loop, and the `if` reads as "and the parse failed" only because the call
was folded into the condition.

`get_config_cbor` failing for a key that `get_stored_keys` just listed needs
the stored `valueBuffer` not to preparse, which `set_config_cbor` and the five
typed setters all prevent — so no wire-reachable path was established here. A
partition loaded from NVM is trusted on its CRC32 alone, and that is the one
path: a CRC-valid image can hold any bytes in a listed key's slot (#48).
Card C2 found no other — a key `get_stored_keys` lists is always found again
by `get_config_cbor` with its own `key_buf` and `key_len`, the 31-byte
truncation of #45 included, since `strncmp` then compares `key_buf` with
itself.

**A partition holding any `ARRAY` value cannot be published at all**, from the
same function. The `ARRAY` case writes into the map behind the encoder's back:

```c
      case ARRAY: {
        if (internalSuccess && map.data.ptr + tmpBSize < map.end) {
          memcpy(map.data.ptr, tmpB, tmpBSize);
          map.data.ptr += tmpBSize;
        }
        break;
      }
```

No `cbor_encode*` call, so `map.remaining` is decremented for the key and not
for the value. `cbor_encoder_close_container` then finds `remaining != 1` and
returns `CborErrorTooFewItems`, which is not `CborErrorOutOfMemory`, so
`services_cbor_as_map` frees the buffer and returns NULL and
`services_cbor_encoded_as_crc32` returns 0. The copy is wrong besides:
`tmpBSize` is the whole 50-byte `valueBuffer` that `get_config_cbor` always
reports, not the array's encoded length, so every trailing byte of the slot
goes into the map too; and the bounds test is `<` where it means `<=`.

One smaller fault, in `bcmp/configuration.c:366`: `get_config_cbor` tests
`value_len == 0` — the pointer, not `*value_len` — in the same expression that
has already dereferenced it.

`sys_info_service_handler` publishes `services_cbor_encoded_as_crc32` of the
system partition as `sys_config_crc`, so a node holding an `ARRAY` there
reports 0.

**replicated; domain-limited** for the first key.
`bm_wire::configuration::ConfigPartition::cbor_map` keeps the last parsed
`parser::Value` and reads a failed key through `Value::rebind` over 50 zero
bytes; with none it returns `MapError::Unreachable`, and
`bm-wire-diff/src/configuration.rs` does not call the C. An `ARRAY` value
gives `MapError::NoMap` and a CRC of 0. `an_unparseable_slot_reads_the_previous_iterator`
and `an_array_value_means_no_map` confirm both differentially.

Fix by turning the `&&` into `if (!get_config_cbor(...) || cbor_parser_init(...) != CborNoError)`,
and by encoding an `ARRAY` value with `cbor_encode_*` or skipping its key.
Wire-visible: it changes the CRC32 a node with an array or an unreadable slot
publishes, and makes a node with an array answer `config_map`.

## 43. bm_core reads only the 5-byte float encoding, so a preferred-serialization float is unreadable to it

RFC 8949 §4.1 lets a float be encoded at any width that holds it exactly, and
*preferred serialization* picks the shortest: `1.0` is three bytes, `f9 3c00`.
bm_core neither writes nor reads that. `cbor_encode_float` always emits the
5-byte `fa` form, and the read side is narrower still —
`third_party/tinycbor/src/cbor.h:608`:

```c
CBOR_INLINE_API bool cbor_value_is_float(const CborValue *value)
{ return value->type == CborFloatType; }
```

`CborFloatType` is `0xfa`. A half-precision float is `CborHalfFloatType`
(`0xf9`) and a double is `CborDoubleType` (`0xfb`), so neither is a float to
this predicate. Two consequences, both worse than losing precision:

| Call | With `fa` | With `f9` |
|---|---|---|
| `get_config_float` (`bcmp/configuration.c:258`) | reads the value | returns false, no value |
| `cbor_type_to_config` (`bcmp/configuration.c:633`) | `FLOAT` | falls through every case and returns **false** |

The second is the sharp one: `set_config_cbor` calls `cbor_type_to_config` and
gives up when it returns false, so a `ConfigSet` (`0xA2`) carrying a
preferred-serialization float is **rejected outright** — not stored as the
wrong type, not truncated, refused. Every "round" float — `0.0`, `1.0`, `0.5`,
`2.0`, infinities — is exactly the case that narrows, so this is the common
path, not an edge.

`bcmp/configuration.c` is also the only reader: `services_cbor_as_map` uses
`cbor_value_get_float`, which asserts the same type. A C node therefore cannot
read a config partition a preferred-serialization encoder wrote, and cannot
accept one over the wire.

A second, smaller difference on the read side: `cbor2` decodes every float
into `f64`, and that widening **quiets a signalling NaN** — `fa ff85ff01`
comes back with the quiet bit set. tinycbor copies the four bytes out
untouched. Every non-NaN `f32` round-trips through `f64` exactly, so the NaN
payload is the whole of it. Found by `cargo fuzz run cbor` in under eight
minutes; seed `bm-wire/fuzz/seeds/cbor/signalling-nan-float`.

**replicated.** `bm_wire::cbor::push_f32_wide` writes the `fa` form, bypassing
`cbor2::core::Header::Float`, and exists for no other reason; its module docs
say so. `bm-wire-diff/src/cbor.rs` encodes every fuzzed float through it and
asserts byte equality with `cbor_encode_float`, and
`cbor2_narrows_floats_and_bm_core_cannot_read_them` pins the other half:
for five values it asserts the narrow form is what cbor2 would have written,
that `cbor_value_is_float` rejects it, and that `cbor_type_to_config` refuses
to classify it at all. On the read side, `same_item` accepts a NaN for a NaN
and nothing else.

Fix upstream by testing `CborHalfFloatType` and `CborDoubleType` alongside
`CborFloatType` and converting, which `cbor_value_get_half_float_as_float`
already does — bm_core does not compile `cborparser_float.c`, so that would
have to be added to the build too. Wire-visible and additive: a fixed node
reads floats a deployed node rejects, and nothing that works today stops
working.

## 44. The saved config partition's layout is the compiler's

`bcmp/configuration.h`:

```c
typedef struct {
  char key_buf[MAX_KEY_LEN_BYTES];
  size_t key_len;
  ConfigDataTypes value_type;
} __attribute__((packed, aligned(1))) ConfigKey;
```

`save_config` writes the packed `ConfigPartition` — a 9-byte header, 50 of
these and 50 value slots of 50 bytes — to flash as it is in memory, and
`config_init` reads it back the same way. `size_t` and the enum's width are
the ABI's:

| Toolchain | `size_t` | enum | `ConfigKey` | Image |
|---|---|---|---|---|
| gcc/clang, x86-64 or AArch64 Linux (`bm_sbc`, this repo's oracle) | 8 | 4 | 44 | 4709 |
| `arm-none-eabi-gcc` (short enums by default) | 4 | 1 | 37 | 4359 |
| clang `--target=arm-none-eabi`, or gcc `-fno-short-enums` | 4 | 4 | 40 | 4509 |

The LP64 and clang rows were measured with `_Static_assert`; the gcc row is
gcc's AAPCS default, which rustc's thumb targets also follow (a `repr(C)` enum
is one byte there — checked). The CRC covers the bytes, not their meaning, so
an image carried across toolchains verifies and is then misread.

**replicated.** `bm_wire::configuration::Layout` carries both widths;
`Layout::LP64` and `Layout::ARM_EABI_GCC` are named, `Layout::new(4, 4)` is
the third row. A Rust node must use the layout of the firmware whose flash it
inherits. `the_image_the_oracle_saves` pins the LP64 bytes and CRC.

Fix upstream with fixed-width fields (`uint32_t key_len`, `uint8_t
value_type`) and a `CONFIG_VERSION` bump so old images are recognised and
migrated. Changes the flash format, not the wire.

## 45. Storing a config key ignores `key_len`; looking one up stops at a NUL

`bcmp/configuration.c` stores a key with

```c
snprintf(config_partition->keys[*key_idx].key_buf,
         sizeof(config_partition->keys[*key_idx].key_buf), "%s", key)
```

and finds one with `keys[i].key_len == len && strncmp(key, keys[i].key_buf,
len) == 0`. The first reads `key` to a NUL, whatever `key_len` says; the
second stops at a NUL in either string. Three consequences:

1. **A 32-byte key can be stored and never found.** `MAX_KEY_LEN_BYTES` is 32,
   so it passes the length check, but `snprintf` keeps 31 bytes and a NUL
   while `key_len` records 32. The lookup then compares the key's 32nd byte
   with that NUL and fails, so every set of the same key appends a new entry
   until the partition holds 50, and every get fails.
2. **`bcmp/config.c` passes keys with no NUL.** `set_config_cbor(...,
   (const char *)msg->keyAndData, msg->key_length, ...)` points at the key
   with the CBOR value directly after it, so `key_buf` receives the key and
   then the value's bytes up to the first NUL or 31 bytes. The lookup still
   works, since it compares only `key_len` bytes; the extra bytes are saved to
   flash. If the value has no NUL and the message ends first, `snprintf`
   reads past the message.
3. **Keys that differ after an embedded NUL are the same key.** `is_key_valid`
   accepts `\0`, so `"ab\0c"` and `"ab\0d"`, both with `key_len` 4, find
   each other.

**replicated.** `bm_wire::configuration::Key` carries the bytes at the
pointer and `key_len` separately, reads past its end as NUL, and stores and
compares as the C does. `bm-wire-diff/src/configuration.rs` confirms each
case: `a_32_byte_key_appends_on_every_set`, `a_key_followed_by_its_value`,
`keys_that_differ_after_a_nul_are_one_key`.

Fix upstream by copying exactly `key_len` bytes with `memcpy` and rejecting
`key_len >= MAX_KEY_LEN_BYTES` (or dropping the terminator), and by rejecting
`\0` in `is_key_valid`. Changes which keys a node accepts from the wire.

## 46. A refused config set still writes; the typed setters and `set_config_cbor` disagree about a full partition

Three things `bcmp/configuration.c` does in the order shown:

| Step | Where | Consequence |
|---|---|---|
| Write the key into slot `numKeys` before the value is encoded or classified | `prepare_cbor_encoder:90`, `set_config_cbor:603` | A refused set of a new key leaves its name in the unused slot, and the next save writes it to flash |
| Encode a string's head, then fail on its body | tinycbor `encode_string`, via `set_config_string` and `set_config_buffer` | For an existing key the old value's first bytes become the new head: the set returns false, `needs_commit` stays false, and the stored value is now a string claiming more bytes than its slot has |
| Check `numKeys >= MAX_NUM_KV` before looking the key up | `prepare_cbor_encoder:80` | At 50 keys the typed setters refuse to overwrite an existing key; `set_config_cbor` looks up first and overwrites it |

The second is data loss behind a false return: after `set_config_string(k,
"hello")` then a 60-byte `set_config_string(k, ...)`, `get_config_string(k)`
fails and `get_value_size(k)` reports 60. Only `set_config_string` and
`set_config_buffer` reach it; `ConfigSet` goes through `set_config_cbor`,
which refuses an oversized value before writing.

**replicated.** `ConfigPartition::set_typed` and `set_cbor` keep the C's order;
`push_string` writes the head before testing the body.
`an_oversized_string_overwrites_the_head_of_the_old_value` and
`a_full_partition` confirm it differentially.

Fix upstream by encoding into a scratch buffer and committing key and value
only on success, and by moving the full-partition test after the lookup in
`prepare_cbor_encoder`. Not wire-visible, except that a full partition's keys
become settable by `set_config_uint` and friends.

## 47. A config partition that fails to load keeps the bytes it failed with

`config_init` reads the whole image into RAM, then checks its CRC. On a
mismatch it sets `numKeys = 0` and `version = CONFIG_VERSION` and leaves
everything else — the stale CRC, every key and every value of the image it
just rejected. The partition behaves as empty, and the next `save_config`
writes the rejected keys and values back out under a fresh CRC, where they sit
in slots past `numKeys`. `bm_config_read` failing is the same, with whatever
the integrator's read left in the buffer.

Two neighbours: `config_init` does not touch `needs_commit`, so a partition
changed before a reload still reports it; and `save_config` writes the new CRC
into the RAM header before `bm_config_write` runs, so a failed write leaves it
there.

**replicated.** `ConfigPartition::load_with` reads in place and resets only
those two header fields. `a_corrupt_image_survives_in_ram` confirms it, and
the fuzzer's `Corrupt` and `Reload` steps compare the RAM images byte for
byte after every load.

Fix upstream by zeroing the partition (as `clear_partition` does) when the
load fails. Not wire-visible.

## 48. A config image whose CRC checks may claim up to 255 keys

`load_and_verify_nvm_config` checks the CRC and nothing else, so `numKeys` can
be anything up to 255. `find_key_idx` then walks `keys[0..numKeys]`: slots
from 50 on overlay the value array and, from about 232 on (LP64), run past the
10 KiB `ram_buffer` into the next partition's — or, for the hardware
partition, past `CONFIGS`. A key found at such an index is written by
`set_config_cbor` at `values[idx]`, further out still. The typed setters are
safe only because they refuse at `numKeys >= MAX_NUM_KV`.

A CRC-valid image needs someone to write it: flash corruption that happens to
match, or firmware with a different `MAX_NUM_KV`.

**domain-limited.** `ConfigPartition::load_with` refuses such an image and
takes the failed-load path of #47. The comparator never presents one:
`Op::FixCrc` clamps `numKeys` to 50 before computing the CRC.
`an_image_claiming_more_than_50_keys_is_refused` pins the port's side.

Fix upstream by rejecting `numKeys > MAX_NUM_KV` in
`load_and_verify_nvm_config`. Not wire-visible.

## 49. `set_config_cbor` checks only the first item's head, and `get_config_cbor` returns the whole slot

`set_config_cbor` accepts a value if `cbor_parser_init` accepts it and
`cbor_type_to_config` classifies it. `cbor_parser_init` reads one head, so:

- a string whose head claims more bytes than the value holds (`78 40 61`) is
  stored as `STR`; `get_config_string` then fails and `get_value_size`
  reports 64;
- bytes after the first item are stored with it;
- the `memcpy` copies `value_len` bytes and leaves the rest of the 50-byte
  slot as the previous value had it.

`get_config_cbor` then hands back all 50 bytes regardless, and fails unless
the caller's buffer holds 50 — `bcmp/config.c` sends that whole slot in a
`ConfigValue` (`0xA1`). It also tests `value_len == 0`, the pointer, after
dereferencing it (noted under #42).

Chunked strings are copied chunk by chunk as tinycbor's
`iterate_string_chunks` reads them, so a `get_config_string` that fails on a
malformed later chunk has already written the earlier ones to the caller's
buffer, and one that runs out of room sets `*value_len` to the full length.

**replicated.** `bm_wire::configuration::Head::parse` is `preparse_value` and
`copy_string` is `iterate_string_chunks`, partial copies included; the
comparator compares the caller's buffer and `*value_len` on failure as well as
success. `chunked_strings` and `cbor_get_set` pin the cases above.

Fix upstream with `cbor_value_validate` (or a full `cbor_value_advance`
checking the item ends at `value_len`) in `set_config_cbor`, and by storing
the value's length so `get_config_cbor` can return only that. The second is
wire-visible: `ConfigValue` bodies would shrink to the value.

## 50. `bcmp_process_config_message` indexes `CONFIGS` with an unchecked partition byte

Every config message carries a `partition` byte from the wire. The handlers
pass it straight to `configuration.c`, which indexes
`CONFIGS[partition]` — an array of `BM_CFG_PARTITION_COUNT` (3) — with no range
check in `get_config_cbor`, `set_config_cbor`, `get_stored_keys`, `remove_key`
or `save_config`. A `ConfigGet`, `ConfigSet`, `ConfigCommit`,
`ConfigStatusRequest` or `ConfigDeleteRequest` naming partition 3 or more reads,
and for a set or delete writes, past the end of `CONFIGS`. Only
`clear_partition` checks `partition < BM_CFG_PARTITION_COUNT`, so the clear
request is the one type that answers an out-of-range partition rather than
running off the array.

**domain-limited.** `bm_wire::configuration::Partition::from_u8` returns `None`
past 2, and every handler in `bm_stack::Node` that would index the store stops
there; only the clear request acts on the byte, reporting `success == false`.
The comparator keeps the partition byte in range for the five types that index
`CONFIGS` unchecked (`ConfigInput::clamp_to_domain`) and lets the clear request
carry any byte; `a_clear_checks_the_partition_byte` in
`bm-wire-diff/tests/config.rs` pins the clear case.

Fix upstream by checking `partition < BM_CFG_PARTITION_COUNT` in each entry
point of `configuration.c`, as `clear_partition` already does.

## 51. `bcmp_process_config_message` reads message bodies without checking `data.size`

`bcmp_process_config_message` casts `data.payload` to the message struct and
reads its fixed fields, and then the variable ones by their declared lengths,
without consulting `data.size` — the same shape as divergence #14 in a fresh
set of parsers. A `ConfigGet` shorter than its header, or one whose
`key_length` runs past the frame, is read out of bounds; a `ConfigSet` reads
`key_length + data_length` bytes past its header wherever the frame ends; the
status-response receive loop walks `num_keys` entries with no bound on the body
(its per-entry advance is correct — see
`bm_wire::bcmp::config::ConfigStatusResponse::keys` and divergence #45 for the
`snprintf` over-read of the key itself).

**domain-limited.** The decoders in `bm_wire::bcmp::config` refuse a body
shorter than its declared fields with `BmWireError::Truncated`. The comparator
sends only well-formed, exactly-sized bodies, so the C never reads out of
bounds; the decoders' bound checks are pinned by their unit tests instead.

Fix upstream by validating `data.size` against each message's declared lengths
before reading, as the fix for #14 does for the reply parsers.

## 52. `bcmp_config_decode_value` writes its NUL one byte past a full buffer

`bcmp_config_decode_value`'s `STR` case copies the text with
`cbor_value_copy_text_string`, which sets `*buf_length` to the length copied,
and then writes `p[*buf_length] = '\0'`. The overflow guard is
`if (*buf_length > init_length) break;`, which admits the equal case: a string
exactly as long as the caller's buffer fills it and then writes the terminator
at `p[buf_len]`, one byte past the end.

**domain-limited.** `bm_wire::bcmp::config::decode_value` appends the NUL only
when the buffer has room after a complete copy, so a string that exactly fills
the buffer is returned without a terminator and nothing is written past the
end. `decode_value_follows_the_c_for_each_type` in `bm-wire/src/bcmp/config.rs`
pins the boundary; the function is not driven differentially, because it shares
the cbor-getter surface whose own divergences (#40, #41) are pinned in
`bm-wire-diff/src/cbor.rs` and `configuration.rs`.

Fix upstream by rejecting a string whose length equals the buffer's, or by
copying into a buffer one byte larger.


## 53. Config replies echo only the low 16 bits of the request's sequence number

The BCMP header's `seq_num` is a `uint32_t` on the wire, and `packet.c`
serialises a `sequenced_reply` by copying the request's number into it whole.
But every handler and response builder in `bcmp/config.c` takes the number as a
`uint16_t`: `bcmp_config_process_config_get_msg`,
`bcmp_config_process_config_set_msg`,
`bcmp_config_process_status_request_msg`, `bcmp_process_del_request_message`,
`bcmp_process_clear_request_message` and the `bcmp_config_*_response`,
`bcmp_config_send_value` and `bcmp_config_status_response` they call. A request
whose `seq_num` exceeds `0xFFFF` is answered with only its low 16 bits.

The counter that feeds it, `serialize`'s `message_count`, is a 32-bit global
incrementing once per sequenced request, so a node that has issued more than
65535 config requests, or a peer that sends a crafted request, reaches it.

**replicated.** `bm_stack::Node::process_config` truncates the echoed number to
16 bits before building any reply.
`a_reply_echoes_only_the_low_sixteen_bits_of_the_sequence_number` in
`bm-wire-diff/tests/config.rs` reads it off the wire, and `cargo fuzz run
config` reaches it from a single request.

Fix upstream by widening the `seq_num` parameters in `bcmp/config.c` to
`uint32_t`. Wire-visible: `packet.c` matches a reply on the number it stored,
so today a reply to a request numbered above `0xFFFF` carries a number that
matches no outstanding request, and a fixed node's reply would.


## 54. DFU dispatches on the body's `frame_type`, not the header type, and leaks a body whose byte it does not know

Every DFU body starts with `BmDfuFrameHeader.frame_type`, a copy of the low
byte of the BCMP header's type. `bm_dfu_init` registers all ten types with the
same handler, so `packet.c`'s dispatch on the header type decides nothing, and
`bm_dfu_process_message` then switches on `frame->header.frame_type`. A frame
whose header says `0xD0` and whose body byte says `0xD4` is handled as an ack.

A body byte outside `0xD0`–`0xD9` reaches the `default:` branch, which logs
and returns without `bm_free(buf)`. `dfu_copy_and_process_message` allocated
`buf` (`data.size` bytes) for it, so each such frame addressed to the node
leaks its body. Every other drop path in the function frees.

**replicated** for the dispatch: `bm_wire::bcmp::dfu::DfuMessage::decode`
chooses the variant from the body byte and refuses an unknown one with
`BmWireError::Invalid`. The leak has no counterpart. D2's integration must
dispatch on the decoded body, not on the header type.
`bm-wire-diff/src/dfu_codec.rs` checks every body byte against the C's switch.

Fix upstream by freeing `buf` in the `default:` case, and either dispatching on
`data.header->type` or rejecting a body whose byte disagrees with it.

## 55. DFU bodies are read without checking `data.size`

`dfu_copy_and_process_message` reads `BmDfuEventAddress` at offset 1 of the
body, and `bm_dfu_process_message` reads it again, before anything consults
`data.size`; a body shorter than 17 bytes is read out of bounds. The state
handlers then cast the copied body to the full struct for its type.
`s_client_receiving_run` bounds a chunk's `payload_length` by
`bm_dfu_max_chunk_size` only, not by the body, then runs `crc16_ccitt` over
and copies that many bytes — up to 1024 past a body that carried none. Same
shape as #14 and #51.

**domain-limited.** `DfuMessage::decode` and `DfuAddress::of_body` refuse a
body shorter than its fields, or a chunk declaring more than arrived, with
`BmWireError::Truncated`. The `dfu_codec` comparator reads the C struct only
when the body holds it, and otherwise asserts the port refuses.

Fix upstream by checking `data.size` against `sizeof` the type's struct in
`dfu_copy_and_process_message`, and a chunk's `payload_length` against the
bytes after it.

## 56. `bm_dfu_init` registers `0xD9` twice

`dfu_core.c:621` registers `BcmpDFUBootCompleteMessage` and `dfu_core.c:623`
registers `BcmpDFULastMessageMessage`, its alias in `messages.h`, both with
`process_dfu_message`. `packet_add` appends without checking, so the packet
list holds two `0xD9` entries. `ll_get_item` returns the first, so dispatch is
unchanged and the second is unreachable. It costs one list item, and a single
`packet_remove(0xD9)` would uncover the duplicate rather than unregister the
type. Nothing in bm_core removes a DFU type.

**replicated.** `bm_wire::bcmp::registry::Registry::add` accepts duplicates and
`cfg` returns the first, pinned by
`a_duplicate_registration_is_shadowed_by_the_first`. A `bm-stack` node that
registers DFU as `bm_dfu_init` does needs eleven registry slots for ten types.

Fix upstream by deleting the second `packet_add`.

## 57. A DFU start with `chunk_size` zero divides by zero on the client

`bm_dfu_client_process_update_request` rejects a `chunk_size` above
`bm_dfu_max_chunk_size` and then computes `image_size % chunk_size` and
`image_size / chunk_size`. Zero is not rejected. One `0xD0` addressed to an
idle client, carrying a `gitSHA` different from the client's or
`BM_DFU_IMG_INFO_FORCE_UPDATE` as `filter_key`, reaches the division. The
host side does not check for zero either: `bm_dfu_initiate_update` bounds
`chunk_size` from above only.

On a host build this is undefined behaviour. On a Cortex-M33 with
`CCR.DIV_0_TRP` clear, `UDIV` returns 0, which would make `num_chunks` 1 for
a non-zero `image_size` and 0 otherwise; with the trap set it is a
UsageFault. What the dev kit firmware does was not measured.

**domain-limited.** The codec carries zero unchanged
(`bm_wire::bcmp::dfu::ImgInfo::chunk_size`). `bm_wire::bcmp::dfu_client::Client`
gives what `UDIV` gives with the trap clear — quotient 0, remainder the
dividend — so one chunk for a non-empty image, none for an empty one; pinned by
`chunk_size_zero_is_one_chunk`. `bm_wire_diff::dfu_core::in_domain_body` sends
the C a `chunk_size` of 1 in place of 0.

The host divides by nothing: with `chunk_size` zero, `bm_dfu_host_send_chunk`
sends an empty `0xD2` for every request and `bytes_remaining` never falls.
`bm_wire::bcmp::dfu_host::Host` does the same, pinned by
`a_zero_chunk_size_sends_empty_chunks`; the comparator's `Step::Initiate`
reaches it against the C.

Fix upstream by rejecting `chunk_size == 0` in both
`bm_dfu_client_process_update_request` and `bm_dfu_initiate_update`.

## 58. The DFU error state reports every failure to the last host update's callback

`s_idle_run` stores a `BeginHost`'s `finish_cb` in `dfu_ctx.update_finish_callback`
and its destination in `dfu_ctx.client_node_id`. Only the next `BeginHost` replaces them.
`s_error_entry` calls the callback, if set, with `(false, dfu_ctx.error,
dfu_ctx.client_node_id)` on every entry into `BmDfuStateError` — including
entries from the client states, and entries long after that host update
finished.

So an application that started a host update with a callback is later told
that the same update failed, with the same client id, whenever this node fails
an update *as a client* (chunk timeout, bad CRC, ...) before it next hosts
one.

**replicated.** `bm_wire::bcmp::dfu_core::Core::notify` and `client_node_id`
persist the same way. Pinned against the C by
`every_later_error_is_reported_to_the_last_hosts_callback` in
`bm-wire-diff/tests/dfu_core.rs`, and in `bm-wire` by
`a_client_error_is_reported_to_the_last_hosts_callback`.

Fix upstream by clearing `update_finish_callback` when a host update ends, or
by calling it only from the host states.

## 59. A host adopts its client's error code, and a code of 14 or more stops DFU until reboot

`s_error_entry` returns to Idle only if `dfu_ctx.error < BmDfuErrFlashAccess`
(14); anything else is treated as a fatal local flash fault and the machine
stays in `BmDfuStateError`, where `s_error_run` does nothing.
`bm_dfu_initiate_update` then refuses every request with `BmDfuErrInProgress`
until the node reboots.

The host sets that error from the wire. `s_host_req_update_run` passes a
failed ACK's `err_code` to `bm_dfu_host_transition_to_error`, and
`s_host_req_update_run` and `s_host_update_run` do the same with an abort's.
The byte is cast to `BmDfuErr` unchecked, so any of 14–255 is "fatal".

Two routes reach it:

- A real client whose flash fails: `bm_dfu_client_process_update_request`
  NACKs with `BmDfuErrFlashAccess` when `bm_dfu_client_flash_area_open` or
  `_erase` fails. The client's fault disables DFU on the **host**.
- Any node on the link: the only check on the sender is that the body's
  `src_node_id` equals the client being updated, and that field is not
  authenticated.

**replicated.** `bm_wire::bcmp::dfu_core::DfuErr` is a byte and
`DfuErr::is_fatal` is the C's comparison. Pinned by `a_fatal_error_is_permanent`
(core) and `the_c_host_adopts_a_clients_fatal_nack` (the C's `dfu_host.c` taking
a NACK carrying 14) in `bm-wire-diff/tests/dfu_core.rs` (formerly
`the_c_host_adopts_a_clients_fatal_nack`, C only).
`bm_wire::bcmp::dfu_host::Host` reproduces the host half, compared against the
C by `a_clients_fatal_nack_stops_the_host` and pinned in `bm-wire` by
`a_nack_carrying_flash_access_leaves_the_host_in_error` and
`host_update_fail_upon_reboot`.

Fix upstream by mapping a received `err_code` to a non-fatal host error
(`BmDfuErrAborted`, or a new "client failed" value) instead of casting it.

## 60. A second `bm_dfu_initiate_update` before the first runs is accepted and lost

`bm_dfu_initiate_update` checks for `BmDfuStateIdle` when it is called, on
the caller's task, then queues a `BeginHost` for the DFU task. Two calls before
the DFU task runs both pass the check and both return `true`. The first
`BeginHost` moves the machine to `BmDfuStateHostReqUpdate`; the second is run
there, and `s_host_req_update_run` ignores it. Its finish callback is never
called.

`dfu_ctx.internal` is written by each call that queues an event, so the
update that runs uses the **second** call's `internal`: whether the host reads
the image from `bm_dfu_host_get_chunk` or from `bm_dfu_host_queue_data`.

**replicated.** `bm_wire::bcmp::dfu_core::Dfu::initiate_update` checks and
writes the same way. Pinned by `a_second_initiate_is_accepted_and_lost` in
`bm-wire-diff/tests/dfu_core.rs` and
`a_second_initiate_before_the_first_runs_is_accepted_and_lost` in `bm-wire`.

Fix upstream by moving the Idle check and the `internal` write into
`s_idle_run`'s `BeginHost` branch, and reporting `BmDfuErrInProgress` from
the other states' run functions.

## 61. A non-internal host update leaks its stream buffer unless it reaches `HostUpdate`

When `bm_dfu_internal()` is false, `s_host_req_update_entry` sets
`host_ctx.data_queue = bm_stream_buffer_create(chunk_size)`, overwriting any
previous value. The only `bm_stream_buffer_delete` is in `s_host_update_exit`.
An update that leaves `BmDfuStateHostReqUpdate` any other way — ACK timeout
after two retries, a NACK, an abort — leaks the buffer: `chunk_size` bytes plus
the stream buffer's own allocation, per attempt. A host retrying an update to
an unreachable client loses up to about 1 KiB of heap each time.

Found by LeakSanitizer on `cargo fuzz run dfu_core`'s second run, with a
`BeginHost` run with `internal` false followed by a forced change to Error.

**c-only.** `bm-wire` holds no heap: `bm_wire::bcmp::dfu_host::Host` keeps
the stream buffer inline, and a new non-internal update replaces it.
`bm-wire-diff/src/dfu_core.rs` treats a non-internal `BeginHost` as out of
domain only while the last one's buffer is still held, which is exactly when
the C would leak it; `a_leaked_stream_buffer_bars_the_next_non_internal_update`
pins that.

Fix upstream by deleting `data_queue` in `bm_dfu_host_transition_to_error`,
or by creating it in `s_host_update_entry` instead.

## 62. A DFU start during a transfer restarts it against the first start's image

`s_client_receiving_run` answers a `DfuEventReceivedUpdateRequest` (a `0xD0`
from the host) by ACKing, zeroing `current_chunk`, the page buffer count, the
flash offset and the running CRC, and asking for chunk 0 again. It does not
read the new `0xD0`: `image_size`, `num_chunks` and `crc16` stay those of the
start that began the transfer, and the slot is not erased again. The comment
says this is for a host that lost the first ACK, which resends the same start;
a host that resends a *different* image mid-transfer has it received and
validated against the old one.

The rewrite from offset 0 lands on pages already programmed. The shim's RAM
slot accepts that; flash that must be erased before it is programmed would
not, and the update would fail with `BmDfuErrBmFrame` (#63's path). What the
dev kit's slot does was not measured.

**replicated.** `bm_wire::bcmp::dfu_client::Client` restarts the same way.
Pinned by `a_resync_keeps_the_first_images_size_and_crc` in `bm-wire` and
`a_second_offer_restarts_against_the_first_image` in
`bm-wire-diff/tests/dfu_core.rs`.

Fix upstream by re-reading the image info and re-erasing, or by NACKing a
start whose image info differs from the one in progress.

## 63. A failed chunk write still requests the next chunk, and on the last chunk is reported as a length mismatch

When `bm_dfu_process_payload` fails a page write, `s_client_receiving_run`
calls `bm_dfu_client_transition_to_error(BmDfuErrBmFrame)` — which stops the
chunk timer and sets a pending change to `Error` — and then carries on:
`current_chunk++`, then either `bm_dfu_req_next_chunk` and `bm_timer_start`,
or, on the last chunk, `bm_dfu_process_end` and a pending change to
`ClientValidating`, which replaces the one to `Error`.

So:

- The host receives a chunk request from a client about to enter `Error`, and
  the chunk timer is left running into `Error` and then `Idle`, where its
  timeout is ignored — or run as a retry by the next transfer if one starts
  within two seconds.
- On the last chunk the client validates instead, finds `image_size` ahead of
  the flash offset the failed write did not advance, and reports
  `BmDfuErrMismatchLen` with a `0xD3`, overwriting `BmDfuErrBmFrame`.

`bm_dfu_process_payload` also returns before copying the part of the chunk
that belongs to the next page, so those bytes are lost either way.

**replicated.** Pinned by `a_failed_write_still_requests_the_next_chunk` and
`a_failed_write_on_the_last_chunk_goes_to_validating` in `bm-wire`, and
`slot_failures_agree_with_the_c` against the C, through the shim's write fault.

Fix upstream by returning from `s_client_receiving_run` after the transition
to error.

## 64. A client refusing an image as too large leaves the update slot open

`bm_dfu_client_process_update_request` opens the slot, then NACKs with
`BmDfuErrTooLarge` if `bm_dfu_client_flash_area_get_size` is not greater than
`image_size`, and returns without `bm_dfu_client_flash_area_close`. The next
start opens it again. With MCUboot's `flash_area_open` this is a reference
count that only goes up. An image exactly the size of the slot is refused too:
the test is `>`, not `>=`.

**replicated.** The port makes the same seam calls; `offers_the_client_refuses`
compares the open and close counts with the C's.

Fix upstream by closing the slot before the NACK.

## 65. A rebooted client confirms its image on any `0xD3` from the host, whatever its `success` byte

In `BmDfuStateClientRebootDone`, `s_client_update_done_run` takes any
`DfuEventUpdateEnd` with a buffer as the host's confirmation: it calls
`bm_dfu_client_set_confirmed`, answers with a successful `0xD3` and goes idle.
The `success` and `err_code` bytes of the host's message are not read. The
only check on the sender is `bm_dfu_client_host_node_valid`, which compares
the body's unauthenticated `src_node_id` with the host recorded before the
reboot.

**replicated.** Pinned by `a_rebooted_client_confirms_on_the_hosts_end`, which
sends `success` 1; the port reads neither byte.

Fix upstream by confirming only on `success`, and failing the update
otherwise.

## 66. A client's chunk count is 16 bits

`DfuClientCtx::num_chunks` and `current_chunk` are `uint16_t`.
`bm_dfu_client_process_update_request` computes the chunk count in 32 bits and
truncates it, so an image of 65 536 chunks or more — 64 MiB at the largest
chunk size, 64 KiB at a `chunk_size` of 1 — is asked for in the truncated
count of chunks, and then fails validation with `BmDfuErrMismatchLen`.

**replicated.** `Client` truncates the same way.

Fix upstream by refusing an image whose chunk count exceeds `UINT16_MAX`.

## 67. A DFU host ignores the chunk number it is asked for

`s_host_update_run` answers every `DfuEventChunkRequest` with
`bm_dfu_host_send_chunk`, which never reads `seq_num`. It sends the
`min(bytes_remaining, chunk_size)` bytes at
`DFU_IMG_START_OFFSET_BYTES + image_size - bytes_remaining`, then subtracts
them. So:

- A client that re-requests chunk *n* after its chunk timer fires — the only
  recovery `dfu_client.c` has for a lost `0xD2` — is sent chunk *n + 1*. It
  writes it where chunk *n* belongs, and the update fails validation with
  `BmDfuErrBadCrc`, or `BmDfuErrMismatchLen` once the host runs out.
- A client restarting from chunk 0 on a resent `0xD0` (#62) is sent whatever
  follows the last chunk sent.
- Once `bytes_remaining` is zero, every request is answered with an empty
  `0xD2`, which the client treats as a failed write (#63).

A single dropped chunk frame fails the update.

**replicated.** `bm_wire::bcmp::dfu_host::Host` serves in sequence the same
way. Pinned by `chunks_are_served_in_sequence_whatever_is_asked_for` in
`bm-wire` and `the_host_serves_chunks_in_sequence_whatever_is_asked_for`
against the C.

Fix upstream by computing the offset from `seq_num * chunk_size` and the
length from what remains after it.

## 68. A non-internal DFU host sends a whole chunk after a short read

When `bm_dfu_internal()` is false, `bm_dfu_host_send_chunk` reads the chunk
with `bm_stream_buffer_receive`, which may return fewer bytes than asked. It
has already written `payload_length` and computed the frame length from the
full chunk, so it transmits the full chunk: the bytes read, then the rest of
a `bm_malloc` buffer that was never written. It subtracts only the bytes read
from `bytes_remaining`, so the next chunk starts where the short one ended.

The integrations disagree about an empty stream at the timeout:

| `bm_stream_buffer_receive` | Empty at the timeout | Effect in the host |
|---|---|---|
| `common/bm_freertos.c` | `BmOK`, `*size = 0` | a whole chunk of uninitialised heap on the wire |
| `common/bm_posix.c`, `csrc/bm_os_shim.c` | `BmETIMEDOUT` | `BmDfuErrFlashAccess`, which is fatal (#59) |

On FreeRTOS, then, a host whose application is late feeding the stream sends
heap contents to the client; on the others it disables its own DFU until
reboot.

**domain-limited.** `bm_wire::bcmp::dfu_host::StreamBuffer` has the shim's
semantics, and a short read sends zeros where the C sends uninitialised heap;
pinned by `a_short_read_sends_a_whole_chunk`. The comparator discards, on both
sides, a chunk request that would find the stream holding some but not all of
the chunk (`bm_wire_diff::dfu_core::in_domain`).

Fix upstream by sending only the bytes read, with `payload_length` to match,
and by treating a zero-byte read as a timeout in `bm_freertos.c`.

## 69. A host update's `timeoutMs` of zero is a zero timer period

`bm_dfu_initiate_update` passes `timeoutMs` through to `bm_dfu_host_set_params`
unchecked, and `s_host_update_entry` makes it `update_timer`'s period with
`bm_timer_change_period`. FreeRTOS's timer task asserts that a period is
greater than zero (`configASSERT` in `prvProcessReceivedCommands`), so with
`configASSERT` defined a zero timeout halts the node on entry to
`BmDfuStateHostUpdate`. The shim arms
the timer due at the current tick and fires it at the next tick.

**domain-limited.** `bm_wire::bcmp::dfu_core::Core::change_period` accepts
zero and the timer is due at once, so the next poll aborts the update. The
comparator's `Step::Initiate` and `Step::Host` send the C a timeout of 1 in
place of 0.

Fix upstream by rejecting `timeoutMs == 0` in `bm_dfu_initiate_update`, or
treating it as `bm_dfu_update_default_timeout_ms`.

## 70. `bm_linux.c` writes a source MAC, hop limit and UDP source address that deployed nodes do not

The oracle's IP layer is `network/bm_linux.c`; deployed nodes use
`network/bm_lwip.c` and lwIP. Card H0's capture from a `bm_protocol` dev kit
(`bm-wire-diff/testdata/hello-pub-card-h0.pcap`, asserted by
`bm-wire-diff/tests/capture_h0.rs`) differs from `bm_linux.c` in three fields:

| Field | `bm_linux.c` | Deployed | Deployed value comes from |
|---|---|---|---|
| Source MAC | `mac_from_nodeid`: low 48 bits of the id, byte 0 `\|= 0x02` | `00:00` + low 32 bits of the id | `mac_address` (`common/device.c`), the netif `hwaddr` in `bm_ip_init` |
| Hop limit, UDP and BCMP | 64 | 255 | `UDP_TTL` 255 in `bm_protocol`'s `lwipopts.h`; lwIP's default `RAW_TTL` for BCMP's raw pcb |
| UDP source to `ff03::1` | `fe80::<id>` | `fd00::<id>` | lwIP source-address selection |

Both agree on the destination MAC, the version/class/flow word, BCMP's
`fe80::<id>` source, and a present UDP checksum on transmit, which only
lwIP writes in network order (#71). Neither
verifies a received UDP checksum: `bm_linux.c` never does, and `bm_protocol`
sets `CHECKSUM_CHECK_UDP` 0. That is what lets `bm_l2_policy_rx_apply` write
the ingress nibble without patching the UDP checksum.

No field is known to break interoperation: receivers ignore source MAC and hop
limit, and `ip_to_nodeid` reads only the address's low 64 bits.

**replicated**, against the deployed values:

| Field | `bm-wire` |
|---|---|
| Source MAC | `frame::write_headers` writes `addr::mac_address` |
| Hop limit | `frame::HOP_LIMIT` is 255, for BCMP and UDP |
| UDP source address | `udp::source_address`: lwIP's `ip6_select_source_address` over the netif's two addresses |

Pinned by `bm-wire-diff/tests/capture_h0.rs`, which rebuilds all 2300 UDP
frames in the capture byte for byte, and by
`bcmp::tx::tests::build_reproduces_a_deployed_heartbeat` for BCMP.
Comparators against the oracle read its frames through `stack::drain`, which
rewrites the source MAC and hop limit of every frame the oracle built
(`stack::normalise`); `bm-wire-diff/src/udp.rs` builds its Rust side from
`bm_linux.c`'s `fe80::<id>`.

Fix upstream by having `bm_linux.c` use `mac_address`, hop limit 255 and
`fd00::<id>` as the UDP source for `ff03::1`.

## 71. `bm_linux.c` writes the UDP checksum byte-swapped, and a zero checksum as zero

`ipv6_pseudo_checksum` returns `ntohs(~sum)`: byte-swapped on a
little-endian host, ready to be stored into a packed little-endian field, as
`packet.c` stores BCMP's. `bm_udp_tx_perform` instead writes it high byte
first:

```c
uint16_t cksum = ipv6_pseudo_checksum(&src_addr, dest_addr, ip_proto_udp,
                                      udp_total, udp);
udp[6] = (uint8_t)(cksum >> 8);
udp[7] = (uint8_t)(cksum);
```

so on every little-endian host the checksum's two bytes are reversed on the
wire. It also sends a checksum that computes to zero as zero, which UDP
reserves for "no checksum" and RFC 8200 section 8.1 forbids over IPv6. lwIP's
`udp_sendto_if_chksum` writes the checksum in network order and replaces zero
with `0xFFFF`; card H0's capture holds 2300 UDP frames with valid checksums.

Invisible between Bristlemouth nodes, because none checks a received UDP
checksum (#70). A standard IPv6 stack drops these datagrams.

**replicated**, against lwIP: `bm_wire::udp::build` writes what lwIP writes.
`bm-wire-diff/src/udp.rs` rewrites the Rust frame's checksum to what
`bm_linux.c` would have written before comparing, and so asserts that the
checksum is the only difference; `a_checksum_of_zero_is_the_one_difference`
(`bm-wire-diff/tests/udp.rs`) reaches the zero case.

Fix upstream by storing the checksum in network order and writing `0xFFFF` for
zero.

## 72. `bm_linux.c` delivers a received datagram by its UDP length field; lwIP ignores the field

| | `bm_l2_submit` (`bm_linux.c`) | lwIP (`ip6_input`, `udp_input`) |
|---|---|---|
| UDP length under 8, or past the IPv6 payload | refused | delivered |
| Payload delivered | UDP length less 8 | IPv6 payload less 8 |

lwIP trims the frame to the IPv6 payload length in `ip6_input` and reads
`udphdr->len` only under `CHECKSUM_CHECK_UDP`, which `bm_protocol` sets to 0.
The two agree whenever the UDP length equals the IPv6 payload length, which is
true of every frame either sends.

lwIP's `ip6_input` also drops a version other than 6 and a destination the
netif has not joined; `bm_linux.c` checks neither, and neither does
`bm_wire::udp::accept` or `bm_wire::bcmp::rx::accept`.

**replicated**, against lwIP: `bm_wire::udp::accept` returns the IPv6 payload
after the UDP header and does not read the length field.
`bm-wire-diff/src/udp.rs` asserts the C refuses exactly the frames whose
length field is out of range and otherwise delivers a prefix of the Rust
payload; `the_udp_length_field` (`bm-wire-diff/tests/udp.rs`) covers both.

Fix upstream in `bm_linux.c` by following lwIP, or in both by checking the
length field is equal to the IPv6 payload length.

## 73. `bm_middleware_rx` dispatches on the datagram's source port

`bm_udp_bind_port`'s callback is called with the sender's port: `bm_lwip.c`'s
`udp_recv_cb` passes lwIP's `port` argument, which is the remote port, and
`bm_linux.c`'s `bm_l2_submit` passes `src_port`. `bm_middleware_rx` queues it
as `NetQueueItem::port`, and `middleware_net_task` looks the application up
with `ll_get_item(&CTX.applications, item.port, ...)`.

| Datagram to 4321 from | Reaches `bm_handle_msg` |
|---|---|
| port 4321 | yes |
| any other port | no; dropped in `middleware_net_task` |

Every C node publishes from 4321 to 4321, so the two agree on the wire today.
A sender on an ephemeral port, as a standard UDP socket uses, is never heard.

**replicated.** `bm_stack::Node` matches a datagram to a bound port on its
destination port, as lwIP's `udp_input` does, and reports the source port
beside it in `Event::Udp`. A datagram to 4321 reaches pub/sub only from 4321,
and is otherwise dropped. `bm-wire-diff/src/node_udp.rs` and
`bm-wire-diff/src/pubsub.rs` assert both sides deliver exactly when both ports
are 4321; `the_middleware_dispatches_on_the_source_port`
(`bm-wire-diff/tests/node_udp.rs`) and `received_publications`
(`bm-wire-diff/tests/pubsub.rs`) cover it.

Fix upstream by passing the bound port rather than the source port, or by
keying the lookup on the pcb.

## 74. `bm_wildcard_match` matches any topic a `*`-free pattern prefixes

`common/util.c`, `bm_wildcard_match(str, str_len, pattern, pattern_len)`
returns `j == pattern_len` without requiring `i == str_len`. The loop stops
when the pattern runs out and no `*` precedes that point, and the result is
then true.

| Subscription (`pattern`) | Publication (`str`) | Matches |
|---|---|---|
| `spotter` | `spotter/printf` | yes |
| `spot?er` | `spotter/printf` | yes |
| empty | anything | yes |
| `spotter*x` | `spotter/printf` | no: a `*` backtracks |
| `spotter/printf` | `spotter` | no |

`bm_handle_msg` and `bm_pub_wl`'s local-subscriber check call it with the
topic as `str` and the subscription as `pattern`, so a node subscribed to
`spotter` receives `spotter/printf`, and a service subscribed to
`<id>/metrics/req` receives `<id>/metrics/request`. `bm_sub` refuses an empty
topic, so the last row is unreachable from the API.

**replicated.** `bm_wire::util::bm_wildcard_match` is the C's loop.
`bm-wire-diff/src/util.rs` compares it with the C function (`wildcard` fuzz
target); `a_subscription_receives_topics_it_prefixes`
(`bm-wire-diff/tests/node_udp.rs`) shows the oracle's `bm_handle_msg`
delivering by prefix.

Fix upstream by returning `i == str_len && j == pattern_len`. Wire-visible:
a subscriber stops receiving topics it only prefixes.

## 75. `bm_handle_msg` wraps the data length of a topic longer than the payload

`middleware/pubsub.c`:

```c
BmPubSubData *header = (BmPubSubData *)bm_udp_get_payload(buf);
uint16_t data_len = size - sizeof(BmPubSubData) - header->topic_len;
```

Neither `size >= 5` nor `size >= 5 + topic_len` is checked. With a
`topic_len` past the payload, `data_len` wraps to at least 65 276, and
`bm_handle_msg` still:

| Step | Reads past the payload |
|---|---|
| `header->topic_len` etc. when `size < 5` | up to 5 bytes |
| `bm_wildcard_match` against each subscription | up to `topic_len` bytes of topic |
| each matching callback, given `data_len` | up to 65 535 bytes of data |

Any node on the bus can send such a datagram from and to port 4321 at
`ff03::1`, and every C node's middleware task parses it.

**domain-limited.** `bm_wire::pubsub::decode` returns
`BmWireError::Truncated` for both short cases, and `bm_stack::Node` drops the
datagram. `bm-wire-diff/src/node_udp.rs`
sends the oracle such datagrams only where it reads nothing past them
(`pubsub_domain`: at least five bytes, and a first topic byte no subscription
starts with), and asserts the `*` subscriber is called with the wrapped
length; `a_topic_past_the_payload_wraps_the_data_length` covers it.

Fix upstream by dropping a datagram shorter than `sizeof(BmPubSubData) +
topic_len`. Not wire-visible for well-formed traffic.

## 76. `bm_pub_wl` sizes its buffer in 16 bits and copies past it

`middleware/pubsub.c`:

```c
uint16_t message_size = sizeof(BmPubSubData) + topic_len + len;
void *buf = bm_udp_new(message_size);
/* ... */
memcpy((void *)header->topic, topic, topic_len);
if (data && len) {
  memcpy((void *)&header->topic[header->topic_len], data, len);
}
```

`len` is a `uint16_t`, so `5 + topic_len + len` exceeds 65 535 for any `len`
above `65 530 - topic_len`, and `message_size` wraps below it. Both
`memcpy`s then write past the allocation, as does the local-subscriber copy.
`bm_middleware_net_tx`'s check against `max_payload_len_udp` (1452) runs after
the copies. Reached only by the local caller passing such a length.

**domain-limited.** `bm_wire::pubsub::encode` takes `usize` lengths and
refuses a buffer too short. `bm-wire-diff/src/node_udp.rs`'s `Publish` caps
data at `MAX_MESSAGE_LEN` bytes.

Fix upstream by computing `message_size` in `uint32_t` and refusing a message
longer than `max_payload_len_udp` before allocating.

## 77. `bm_pub_wl` with NULL data sends uninitialised bytes, and dereferences NULL for a local subscriber

`bm_pub_wl(topic, topic_len, NULL, len, ...)` with `len > 0`:

| Copy | Guard | Result |
|---|---|---|
| to the network buffer | `if (data && len)` | skipped; `len` bytes of the unzeroed `bm_udp_new` buffer are sent |
| to the local buffer, when a local subscription matches | none | `memcpy` from NULL |

**c-only.** `bm_wire::pubsub::encode` takes the data as a slice.

Fix upstream by refusing NULL data with a non-zero `len`.

## 78. `bm_get_subs` writes past its 256-byte buffer

`middleware/pubsub.c`, `bm_get_subs`, allocates `max_sub_str_len` (256) bytes
and appends every subscription's topic and a `" | "` separator with
`strcat`/`strncat`, checking no length. Topics may be 254 bytes, so two
subscriptions can overflow it.

**c-only.** No port. `bm-wire-diff/src/node_udp.rs` reads the oracle's
subscriptions through it and keeps them few;
`bm_wire_diff::pubsub::oracle_subscriptions` reads it under
`bm_shim_alloc_floor`, which makes `bm_malloc` return 4096 zeroed bytes.

Fix upstream by bounding each append by the space left.

## 79. `bm_sub_wl` checks only a topic's first callback for a duplicate

`middleware/pubsub.c`, `bm_sub_wl`, for a topic already subscribed:

```c
while ((ptr->sub.callbacks->callback_fn != callback) &&
       last_cb_node->next) {
  last_cb_node = last_cb_node->next;
}
if (ptr->sub.callbacks->callback_fn == callback) {
```

Both tests read the list head, `ptr->sub.callbacks`, not `last_cb_node`. A
callback that is not first is appended again on every call:

| Calls on topic `t` | Callbacks per publication on `t` |
|---|---|
| `bm_sub(t, A)` twice | `A` once |
| `bm_sub(t, A)`, `bm_sub(t, B)` | `A` once, `B` once |
| `bm_sub(t, A)`, `bm_sub(t, B)` twice | `A` once, `B` twice |
| the same, then `bm_unsub(t, B)` | `A` once, `B` once |

Reachable through the service layer: a node whose application subscribed
`<name>/req` before `bm_service_register(name)` lists the service callback
again on every registration, and replies once per listing (#89).

**replicated.** `bm_wire::pubsub::Subscriptions` keeps each topic's
callbacks, of two kinds (the application and the service layer), and
`subscribe_as` tests only the head. `a_second_callback_subscribed_twice_is_called_twice`
(`bm-wire-diff/tests/pubsub.rs`) measures the oracle with two C callbacks;
`a_service_registered_twice_after_the_application`
(`bm-wire-diff/tests/services.rs`) compares a node against it.

Fix upstream by testing `last_cb_node->callback_fn` in both places.

## 80. `bm_unsub_wl` returns `BmEINVAL` for a topic not subscribed

`bm_unsub_wl` initialises `err` to `BmEINVAL` and assigns it only when the
topic is found, so a topic with no subscription returns `BmEINVAL`, the code
for an empty topic. A subscribed topic without the given callback returns
`BmENOENT`.

**replicated**, in meaning: `bm_wire::pubsub::Subscriptions::unsubscribe`
returns `SubscriptionError::NotSubscribed`, documented as the C's `BmEINVAL`,
and a node has no second callback to be missing. `bm-wire-diff/src/pubsub.rs`
maps each `BmErr` to the Rust error it must equal.

Fix upstream by returning `BmENOENT` when `get_sub` finds nothing.

## 81. `spotter_log` budgets its text against `max_payload_len`, not the pub/sub message limit

`integrations/spotter.c`:

```c
#define max_str_len(fname_len) \
  (int32_t)(max_payload_len - sizeof(bm_print_publication_t) - fname_len)
```

`max_payload_len` is 1460, the IPv6 payload of a 1500-byte frame.
`bm_middleware_net_tx` refuses a publication longer than
`max_payload_len_udp`, 1452, which also holds the pub/sub header and the
topic. Text `bm_pub` sends, with a file name of `f` bytes:

| Topic | `max_str_len` allows | `bm_pub` sends |
|---|---|---|
| `spotter/printf` | 1447 − `f` | 1419 − `f` |
| `spotter/fprintf` | 1447 − `f` | 1418 − `f` |

Text between the two passes the check, is delivered to local subscribers by
`bm_pub_wl`, and is then refused; `spotter_log` returns `BmENETDOWN`, its code
for any `bm_pub` failure, not `BmEMSGSIZE`.

**replicated.** `bm_wire::spotter::encode_log` checks `max_str_len`;
`bm_stack::Node::spotter_log` then returns `SpotterError::NotSent` after the
local deliveries. `text_lengths` (`bm-wire-diff/tests/spotter.rs`) compares
both sides of each limit.

Fix upstream by budgeting against `max_payload_len_udp` less the pub/sub
header and the topic, and returning `BmEMSGSIZE`.

## 82. Service body decoders read uints with an unchecked `cbor_value_get_uint64`

`sys_info_reply_decode` (four fields), `config_cbor_map_request_decode` (one)
and `config_cbor_map_reply_decode` (four) check that each key is a text
string, then call `cbor_value_get_uint64` on the value without
`cbor_value_is_unsigned_integer`. `cbor_value_get_uint64` is a `cbor.h`
inline that `assert`s the type:

| Value | Debug build | Release build (`NDEBUG`) |
|---|---|---|
| unsigned integer | the integer | the integer |
| any other item | `assert` fails: the node aborts | the head's argument: a string's or container's length, 31 for an indefinite length, 0 for `false`, 21 for `true`, a float's bits, a tag's number |
| a tag | aborts, as above | the tag's number; the iterator then sits on the tagged item, which does not count as a map entry, so `cbor_value_leave_container` fails its `cbor_assert`: `unreachable()`, undefined |

`config_cbor_map_request_decode` runs in `config_map_service_handler`, on
every node that registers `config_map` (a dev kit's `app_main.cpp` does), for
any node's request. `{"p": "ab"}`, six bytes, aborts a debug build; bm_protocol
builds the dev kit `hello_world` and the Bridge as `Debug`
(`tools/scripts/release/configs/default.yml`). `power_info_reply_decode`
goes through `bm_messages_helper.c`'s `decode_key_value_uint32`, which checks
the type, and is not affected.

**replicated**, as a release build: `bm_wire::cbor::parser::Value::extract`
is `_cbor_value_extract_int64_helper`, and `bm-wire-sys/build.rs` compiles
the four codecs with `NDEBUG` (`T2_RELEASE`) so the oracle can be called on
these inputs. **domain-limited** for a tag: the port returns
`CborError::Unreachable` and `bm-wire-diff/src/service_codecs.rs` does not
call the C.

Fix upstream by checking `cbor_value_is_unsigned_integer` before each read,
as `decode_key_value_uint32` does.

## 83. `sys_info_reply_decode` sizes `app_name` from the sender's `app_name_strlen`

```c
size_t buflen = d->app_name_strlen + 1;
char *buf = (char *)bm_malloc(sizeof(char) * buflen);
...
err = cbor_value_copy_text_string(&value, buf, &buflen, NULL);
if (err != CborNoError) {
  break;
}
d->app_name = buf;
```

`app_name_strlen` is read from the body, not from the string. With a name of
`n` bytes:

| `app_name_strlen` | Result |
|---|---|
| ≥ `n` | success, NUL-terminated |
| `n − 1` | success, **no terminator**: the copy fills the buffer and `cbor_value_copy_text_string` writes a NUL only if there is room |
| < `n − 1` | `CborErrorOutOfMemory`; `buf` is **leaked** |
| `UINT32_MAX` | the `+ 1` wraps in 32 bits to a 0-byte allocation: `malloc(0)` on a hosted libc, NULL on FreeRTOS's heaps |
| larger than the free heap | `CborErrorOutOfMemory` |

The decoder runs on the requester (a Bridge's `topology_sampler.cpp`), which
then reads `app_name` as a C string.

**replicated** for success and failure: `bm_wire::service::sys_info::DecodedSysInfoReply::decode_into`
refuses a name longer than `app_name_strlen + 1` and returns the name as a
`CborString`, with its length. **c-only** for the terminator and the leak.
`bm-wire-diff/src/service_codecs.rs` checks the C's terminator where it has
room for one, frees the leaked buffer, and skips inputs whose allocation the
shim refuses (zero bytes or over 64 KiB, `bm_shim_heap_watch_begin`).

Fix upstream by sizing the buffer with `cbor_value_calculate_string_length`
and freeing it on failure.

## 84. `config_cbor_map_reply_decode` reads `cbor_data` only when `success` and a length are set

After the `cbor_data` key:

```c
if (d->cbor_encoded_map_len && d->success) {
  ... copy, advance, leave the map ...
}
```

| `success`, `cbor_encoded_map_len` | Value | Result |
|---|---|---|
| either zero | anything well-formed | `CborNoError`; the value is not read and the map is not left |
| both set | byte string of exactly that length | `CborNoError`, data copied |
| both set | shorter byte string | `CborErrorIllegalType`, with the copy left in `cbor_data` |
| both set | longer byte string | `CborErrorOutOfMemory`, with the chunks that fitted left in `cbor_data` |
| both set | not a byte string | `cbor_value_copy_byte_string` fails its `assert` (debug) or reaches `iterate_string_chunks`' `cbor_assert` (release, undefined) |
| both set, length larger than the free heap | anything | `CborNoError` with `cbor_data` NULL |

`success` is any non-zero value (`(bool)tmp_uint64`), and both fields are
read as #82 describes.

**replicated**, assuming the allocation succeeds:
`bm_wire::service::config_map::DecodedConfigMapReply::decode_into`.
**domain-limited** where the value is not a byte string: the port returns
`CborError::Unreachable` and the comparator does not call the C. The C's
`cbor_data` on failure is not compared, and an input whose allocation the
shim refuses is skipped.

Fix upstream by checking `cbor_value_is_byte_string` and the length before
allocating, reading the value in every case, and failing when the
allocation fails.

## 85. `BM_FIELD_STRING` is unimplemented in both field-table functions

`bm_common_messages/bm_messages_helper.c`. `bm_encode_fields_from_table`
encodes each entry's key, then its value in a `switch` whose `default:` sets
`CborErrorUnsupportedType`; the next entry's key encode overwrites `err`. The
per-entry error check sits inside the `switch` after `default:`'s `break`, so
it never runs. `bm_decode_fields_from_table` has the `STRING` case commented
out ("TODO"); its `default:` sets `CborErrorUnsupportedType`, which the
`cbor_value_advance` after the `switch` overwrites.

| A `STRING` entry | Effect |
|---|---|
| encoded, last in its table | `metrics_reply_encode` returns `CborErrorUnsupportedType` |
| encoded, elsewhere | its key is written with no value; closing the component's map returns `CborErrorTooFewItems` |
| decoded | its key matches, nothing is written, and the key is not counted as unknown |

**replicated.** `bm_wire::service::metrics::Field::String`;
`string_fields` (`bm-wire-diff/src/metrics_codec.rs`) compares each row.

Fix upstream by implementing the type, or by refusing it before encoding the
key.

## 86. `metrics_reply_decode` checks no top-level key, and matches field keys up to a NUL

`bm_common_messages/metrics_reply_msg.c` and `bm_messages_helper.c`:

| Step | The C |
|---|---|
| `version`, `node_id`, `uptime_ms`, `data` | takes any four text keys in that position; `decode_key_value_uint*` never reads `key_expected` except to log |
| component lookup | `cbor_value_map_find_value`: exact match, first wins |
| field lookup | `cbor_value_copy_text_string` into `key[64]`, then `strcmp`: a wire key `"a\0x"` fills the entry `"a"` |
| a field key over 63 bytes | skipped, and not counted as unknown |
| a field of the wrong type | not written; the component returns `CborErrorImproperValue`, which ends the decode, so later components are not read |
| a field key not in the table | skipped; `CborErrorUnsupportedType`, which the caller ignores |

Every write happens as the pair is read, so a decode that fails leaves what
it decoded before the failure.

**replicated.** `bm_wire::service::metrics::decode` and `decode_fields`;
`key_quirks` (`bm-wire-diff/src/metrics_codec.rs`) and the `metrics_codec`
fuzz target compare the error and every destination after each decode.

Fix upstream by comparing each top-level key with `key_expected`, and by
comparing field keys by length (`cbor_value_text_string_equals`).

## 87. A tagged field value makes `bm_decode_fields_from_table` advance past its map

`bm_decode_fields_from_table` steps over each value with one
`cbor_value_advance`. On a tag, that steps over the tag alone; tags do not
count as items, so the iterator is then at the tagged item, which the loop
reads as the next key. If it is a text string, the advance over "its value"
starts at the end of the map. `cbor_value_advance` asserts
`it->type != CborInvalidType` first:

| Build | Effect |
|---|---|
| asserts on (the oracle) | abort |
| `NDEBUG` | `cbor_assert` is `unreachable()`: undefined behaviour |

Reached by `{"k": 1("s")}` in any component a requester decodes; the body
comes from the node being asked.

**domain-limited.** `bm_wire::service::metrics::decode_fields` returns
`CborError::Unreachable`, as `bm_wire::cbor::parser` does at every tinycbor
assertion. The comparator (`bm-wire-diff/src/metrics_codec.rs`) does not call
the C for a body the port decodes to that, and asserts the body holds a tag.
`seeds/metrics_codec/tag-on-field-value` is the fuzzer's input.

Fix upstream by `cbor_value_skip_tag` before reading a key or value, or by
refusing a tag.

## 88. `services_cbor_as_map` reads each value by its key's stored type

`middleware/cbor_service_helper.c` switches on `key.value_type` and calls the
matching `cbor_value_get_*` without checking the slot's head. The two can
disagree without a crafted image: divergence #46 leaves the head of a refused
`set_config_string` or `set_config_buffer` over an existing key of any type.
After `set_config_uint(k, 7)` then a 60-byte `set_config_string(k, ...)`,
`k` is `UINT32` over `78 3c`:

| `value_type` | Head | Debug build | Release build (`NDEBUG`) |
|---|---|---|---|
| `UINT32`, `INT32` | any | `cbor.h` `assert` fails: abort | the head's argument, as #82: `k: 60` |
| `FLOAT` | 4- or 8-byte argument | abort | the argument's low 32 bits as a float |
| `FLOAT` | shorter argument | abort | `_cbor_value_decode_int64_internal`'s `cbor_assert`: undefined |
| `STR` | byte string | abort | copied as text |
| `BYTES` | text string | abort | copied as bytes |
| `STR`, `BYTES` | anything else | abort | `iterate_string_chunks`'s `cbor_assert`: undefined |

`sys_info_service_handler` calls this for the system partition on every
`sys_info` request, and `config_map_service_handler` for any partition a
requester names, so on a debug build (#82: the dev kit and Bridge firmware)
such a key aborts the node on the next request.

**replicated** as a release build: `bm-wire-sys/build.rs` compiles
`cbor_service_helper.c` with `NDEBUG` (`T2_RELEASE`), and `ConfigPartition::cbor_map`
reads through `parser::Value::extract`, `get_int64` and `get_float`.
**domain-limited** where a `cbor_assert` fails: the port returns
`MapError::Unreachable` and the comparator does not call the C.
`values_are_read_by_their_key_type` confirms it differentially.

Fix upstream by testing the head's type before each read, or by fixing #46 so
that a key's type and its slot cannot disagree.

## 89. `bm_service.c` matches services by `strncmp` prefix, and reads a request's header unchecked

`middleware/bm_service.c`. `_service_request_received_cb` walks
`BM_SERVICE_CONTEXT.service_list` for every publication reaching a service
subscription:

```c
if (strncmp(current->service, topic, current->service_strlen) == 0) {
  BmServiceRequestDataHeader *request_header = (BmServiceRequestDataHeader *)data;
  if (data_len != sizeof(BmServiceRequestDataHeader) + request_header->data_size) {
    break;
  }
  if (topic_len != current->service_strlen + strlen(BM_SERVICE_REQ_STR)) {
    break;
  }
```

| Step | Effect |
|---|---|
| `strncmp` over the service's length | the first service whose name prefixes the topic decides; a mismatch after it `break`s, so service `a` listed before `ab` leaves `ab/req` unanswered |
| `strncmp` past `topic_len` | reads the data that follows the topic in the datagram, then past the datagram if the name is longer still |
| `request_header->data_size` | read before `data_len >= 8` is checked: up to 8 bytes past the datagram |
| `break` on either length check | no later service is tried |

`_service_list_remove_service` removes the first service with
`strncmp(current->service, service, service_strlen) == 0`, the unregistered
name's length: unregistering `a` removes `ab` if it is listed first, and the
empty name removes the first service. `bm_service_unregister` unsubscribes
`<name>/req` before that, so the removed service's subscription stays and the
named service stays listed with none. The list has no de-duplication and no
reset; an entry left without its subscription can never be removed.

The callback is called once per listing on each matching subscription
(#74, #79), and each call walks the same list and publishes the same reply. A
C requester takes the first and drops the rest.

**replicated**, except where it reads past the datagram.
`bm_wire::service::ServiceTable::lookup` and `remove` are the C's walks;
`Lookup::OverRead` and `Lookup::ShortRequest` are the out-of-bounds cases.
`bm_stack::Node` calls the handler once per publication and sends one reply,
however many times the C would. `bm-wire-diff/src/services.rs` (`services`
fuzz target) asserts the C's `k` replies, handler calls and local deliveries
are the Rust node's one repeated, and skips requests the lookup reports as
out of bounds. `a_prefixing_name_shadows_a_later_service` and
`unregistering_a_prefix_removes_an_earlier_service`
(`bm-wire-diff/tests/services.rs`) cover the two prefix matches.

Fix upstream by comparing names by length and bytes (`topic_len ==
service_strlen + 4 && memcmp`), checking `data_len >= 8` before reading the
header, `continue` rather than `break` on a mismatch, and removing by exact
name. Wire-visible: a request a shadowing service swallows is answered.

## 90. `echo_service_handler` copies a request of any length into its 1008-byte reply buffer

`middleware/echo_service.c`:

```c
if (*buffer_len <= MAX_BM_SERVICE_DATA_SIZE) {
  *buffer_len = req_data_len;
  memcpy(reply_data, req_data, req_data_len);
```

`*buffer_len` is `MAX_BM_SERVICE_DATA_SIZE - 16` (1008) on entry, so the test
always passes, and the request's length is never compared. A request carries
up to 1414 bytes (a 1452-byte publication less the header, the 25-byte topic
and 8 bytes of request header), so `memcpy` writes up to 406 bytes past the
1024-byte `bm_malloc` in `_service_request_received_cb`, and `bm_pub_wl` then
reads the same span to send the reply. Any node on the bus can send one; a
dev kit's firmware registers echo (bm_protocol
`src/apps/bm_devkit/bmdk_common/app_main.cpp:413-415`).

**domain-limited.** `bm_wire::service::echo` returns `None` for a request
longer than the buffer, and the node sends no reply.
`bm-wire-diff/src/services.rs` does not send the oracle such a request;
`echo_past_its_buffer_is_skipped` (`bm-wire-diff/tests/services.rs`) shows the
domain check, and `echo_refuses_what_does_not_fit_the_reply`
(`bm-stack/tests/service.rs`) the port's behaviour.

Fix upstream by testing `req_data_len <= *buffer_len`.

## 91. A failed `bm_service_request` leaves its request listed, to time out; long timeouts wrap

`middleware/bm_service_request.c`, `bm_service_request`:

```c
BmServiceRequestNode *node = NULL;
do {
  /* ... */
  BmServiceRequestNode *node =
      _create_node(service_strlen, service, reply_cb, (timeout_s * 1000));
  /* ... add, subscribe, send; break on failure */
} while (0);
if (!rval) {
  if (node) { /* free */ }
}
```

The inner `node` shadows the outer, so the cleanup never runs. A request is
listed, with an id taken, before its reply topic is subscribed and before it
is sent:

| Failure | Returns | Then |
|---|---|---|
| `data_len` > 1024 | false | nothing listed, no id taken |
| `_create_node`'s `bm_malloc` | false | nothing listed, no id taken |
| `bm_sub_wl` of `<service>/rep` (a name of 251 bytes or more, or `bm_malloc`) | false | listed; nothing sent; `reply_cb(false, id, ...)` at the first sweep past its timeout |
| `bm_pub_wl` of `<service>/req` (`bm_malloc`, or `bm_middleware_net_tx`) | false | listed and subscribed; local subscribers have the request; `reply_cb(false, ...)` as above |

A caller that treats false as "nothing happened" gets a callback later.

`timeout_s * 1000` is computed in `uint32_t` and wraps for `timeout_s` above
4 294 967. The sweep's `time_remaining_ms` is signed, so a request whose
`timeout - elapsed` exceeds 2^31 ms reads as overdue: `timeout_s` from
2 147 486 to 4 294 967 expires at the next sweep.

The sweep's timer hands its work to `timer_callback_handler.c`'s task.
`bristlemouth_init` does not start that task; bm_protocol's `app_main.cpp`
does. An integrator that does not gets requests that never time out.

**replicated.** `bm_wire::service::Requests::add` takes the id and wraps the
timeout; `bm_stack::Node::service_request` returns
`ServiceRequestError::NotSubscribed` or `NotSent` with the request listed.
The Rust node's ceilings (names of at most 48 bytes, data of at most 1024)
refuse what makes the C's subscribe or send fail before an id is taken, so
`bm-wire-diff/tests/service_request_failures.rs` measures the C's two paths
on the oracle alone; `a_request_not_subscribed_stays_listed`
(`bm-stack/tests/service.rs`) shows the port's. `long_timeouts_wrap`
(`bm-wire-diff/tests/services.rs`) compares the timeouts.

Fix upstream by removing the inner declaration, and removing the node from
the list on failure; computing the timeout in 64 bits or capping
`timeout_s`; and starting the timer callback task in `bm_service_init`.
Wire-visible only in what the caller is told.

## 92. `_service_request_cb` reads a reply's header and `data_size` unchecked, and matches on id, not topic

`middleware/bm_service_request.c`:

```c
BmServiceReplyDataHeader *header = (BmServiceReplyDataHeader *)data;
if (header->target_node_id == node_id()) {
  /* ... */
  BmServiceRequestNode *node = _service_request_list_get_node_by_id(header->id);
  if (node) {
    node->reply_cb(true, header->id, node->service_strlen, node->service,
                   header->data_size, header->data);
```

| Step | Effect |
|---|---|
| `header->target_node_id` with `data_len` < 16 | reads up to 16 bytes past the publication |
| `header->data_size` passed as `reply_len` | the callback reads up to 4 GiB past the publication; a sys_info or config_map decoder reads what follows |
| lookup by `header->id` alone | a reply on any topic reaching a reply subscription answers whichever request has that id, reported with that request's service |

Reply subscriptions are `<service>/rep` and match by prefix and pattern
(#74), so a reply on `<a>/rep` reaches the callback for a request to a
service named `<a>*` or a prefix of it. Ids are a node-wide counter, so any
node that can guess the next ids can answer another node's requests.

The callback is called once per listing on each matching subscription
(#79); the first call removes the request, and the rest find nothing.

**replicated**, except where it reads past the datagram.
`bm_wire::service::Requests::on_reply` matches id and target only;
`ReplyOutcome::Short` is a body under 16 bytes, which the Rust node drops;
`ReplyOutcome::Answered`'s data is `data_size` bytes or what arrived if
fewer. `bm-wire-diff/src/services.rs` (`services` fuzz target) skips
publications under 16 bytes reaching a reply subscription and compares
reply data up to what arrived. `a_reply_is_matched_by_id_not_topic`,
`a_reply_claiming_more_data_than_it_carries` and `a_short_reply_is_skipped`
(`bm-wire-diff/tests/services.rs`) cover the three.

Fix upstream by checking `data_len >= sizeof(BmServiceReplyDataHeader) +
header->data_size`, and comparing the topic with the request's
`<service>/rep`. Wire-visible: a reply on another topic stops answering.

## 93. `bm_service_request` calls `memcpy` with a NULL source for an empty request

`middleware/bm_service_request.c`:

```c
memcpy(header->data, data, data_len);
```

`sys_info_service_request`, `metrics_service_request` and
`power_info_service_request` pass `data_len` 0 and `data` NULL. C17 requires
`memcpy`'s pointers to be valid even when the length is 0, so this is
undefined. Reported by UBSan during `cargo fuzz run services`:

```
bm_service_request.c:258:3: runtime error: null pointer passed as argument 2,
which is declared to never be null
```

Reached by every sys_info, metrics and power_info request a C node makes.

**benign.** Every libc copies nothing for a zero length, and the request's
frame matches the Rust node's. C2y makes the call defined (N3322). The fix is
to skip the copy when `data_len` is 0.

## 94. `config_map_service_handler` sends no reply for a map over its buffer, and the same failure reply for an unknown partition and a map that fails

`middleware/config_cbor_map_service.c`, `config_map_service_handler`:

| Request | Reply |
|---|---|
| does not decode | none |
| `partition_id` not 1, 2 or 3 | `success` 0, `cbor_encoded_map_len` 0, the id echoed |
| a partition `services_cbor_as_map` returns `NULL` for (every `ARRAY` key, #42) | the same |
| a map that, with the other fields, exceeds the handler's 1008 bytes | none: `config_cbor_map_reply_encode` returns `CborErrorOutOfMemory` and the handler returns false |
| otherwise | `success` 1 and the map |

The fields before the map take up to 78 bytes, so a map over about 930
bytes gets no reply. A partition holds up to 50 keys of up to 32-byte names
and 50-byte values, about 4 kB of map. The requester (a Bridge's
`sensorController.cpp`) sees a timeout, the same as for an absent node, and
cannot tell an unknown partition from one whose map failed.

**replicated.** `bm_wire::service::config_map::handle` returns `None` for a
reply that does not fit and the failure reply for the two `success` 0 cases.
`config_map_past_its_buffer_is_no_reply` and
`config_map_answers_each_partition` (`bm-wire-diff/tests/services.rs`)
confirm both against the oracle; `the_handler_sends_nothing_past_its_buffer`
(`bm-wire/src/service/config_map.rs`) shows the boundary.

Fix upstream by replying `success` 0 when the map does not fit, or by
splitting the map across replies. Wire-visible: a reply where there was none.

## 95. `config_map_service_handler`'s failure reply encodes `cbor_data` from a NULL pointer

`middleware/config_cbor_map_service.c` sets `reply.cbor_data` to
`services_cbor_as_map`'s result, `NULL` for an unknown partition or a map that
fails (#94), and `cbor_encoded_map_len` to 0.
`config_cbor_map_reply_encode` passes both to `cbor_encode_byte_string`, and
tinycbor's `append_to_buffer` calls `memcpy(ptr, NULL, 0)`, which is undefined
as #93's is. Reported by UBSan during `cargo fuzz run services`:

```
third_party/tinycbor/src/cborencoder.c:298:5: runtime error: null pointer
passed as argument 2, which is declared to never be null
```

**benign.** Every libc copies nothing for a zero length; the reply ends in an
empty byte string, `40`, on both sides. The fix is to pass a non-NULL
pointer, or to skip the copy for a zero length in tinycbor.

