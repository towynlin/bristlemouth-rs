# Divergences between bm_core's C and the Rust port

`bm-wire` reproduces bm_core's observable behaviour bit-for-bit, because
interoperating with deployed C nodes is the requirement. Where the C does
something surprising, wrong, or undefined, the port matches it anyway and the
surprise is recorded here for upstream repair in
[`bristlemouth/bm_core`](https://github.com/bristlemouth/bm_core).

Nothing in this file is a licence to change `vendor/bm_core/` from this repo.
Fixes go upstream; when they land, the submodule bump is what changes `bm-wire`.

Status values:

- **replicated** — `bm-wire` deliberately matches the C. Fixing the C is a
  wire-visible change and needs coordination.
- **domain-limited** — the C is undefined for these inputs, so there is nothing
  to match. The differential harness constrains the input instead, and the
  constraint is documented at the comparator.
- **benign** — the C is technically undefined but every real toolchain produces
  the intended value, and the port produces that value by construction.
- **c-only** — a defect in state or an API that bm_core keeps and `bm-wire` has
  no counterpart for. There is nothing to replicate; it is recorded because it
  is worth fixing upstream.

| # | Where | Status | Found by |
|---|---|---|---|
| 1 | `bm_l2_policy_prepare_forwarded_copy` doc contradicts code | replicated | reading |
| 2 | `check_endianness` swaps 32 bits over a 16-bit field | replicated | reading |
| 3 | `ip_to_nodeid` returns 0 on big-endian | replicated | reading |
| 4 | `__builtin_ffs` can yield a port number above 15 | replicated | reading |
| 5 | `utc_from_date_time` reads `MONTH_DAYS` out of bounds | domain-limited | reading |
| 6 | `uint8_to_uint32` shifts into the sign bit | benign | **UBSan, via `cargo fuzz run addr`** |
| 7 | `bm_l2_policy_rx_apply` doc claims an egress-nibble clear that is not in the code | replicated | reading |
| 8 | `bcmp_tx`'s size guard uses `sizeof(BcmpHeartbeat)` where it means `sizeof(BcmpHeader)` | replicated | reading |
| 9 | `process_received_message` rewrites the source address before verifying the checksum, undocumented | replicated | reading |
| 10 | A rejected frame is left with its checksum field zeroed | replicated | reading |
| 11 | `clear_ports_legacy` performs a misaligned 32-bit access on every received frame | benign | **UBSan** |
| 12 | The egress-port checksum patch drops the one's-complement end-around carry | replicated | reading, then measured |
| 13 | L2 silently drops any frame whose destination is not multicast | replicated | reading |
| 14 | Device-info and neighbour-table replies are parsed with unchecked, attacker-supplied lengths | domain-limited | reading |
| 15 | `bcmp_remove_neighbor_from_table` frees, and reports the free's result as the removal's | c-only | **a double free while writing the comparator** |
| 16 | An advertised liveliness lease is doubled and scaled in 32-bit arithmetic, so it wraps | replicated | reading, confirmed differentially |
| 17 | A new neighbour is announced to the application twice | replicated | reading, confirmed differentially |
| 18 | A node id of zero is never recognised, so its node is rediscovered forever | replicated | reading, confirmed differentially |
| 19 | `INFO_REQUEST_LIST` grows without de-duplication or expiry | c-only | reading |
| 20 | `ll_remove` leaves `LL::tail` pointing at a freed node | domain-limited | reading, then modelled to stay out of it |
| 21 | A sequenced reply is matched on its sequence number alone, never its type | replicated | reading, confirmed differentially |
| 22 | A sequenced request's timeout is the 150 ms sweep, not the 24 ms constant | replicated | reading, then measured |

---

## 1. `bm_l2_policy_prepare_forwarded_copy` clears the egress nibble its doc promises to keep

`network/l2_policy.h` documents the function as:

> - clears ingress nibble (on-wire ingress bits must be zero)
> - **leaves egress nibble intact**

`network/l2_policy.c` does the opposite:

```c
clear_ingress_nibble(pb);
clear_egress_nibble(pb);
```

Both nibbles are zeroed, so the whole ports byte at IPv6 source offset +2 goes
to `0x00` on every forwarded copy.

**Ruling:** the code is authoritative — that is what deployed nodes put on the
wire, and changing it would be an interop break. `bm-wire` zeroes both nibbles.
**The doc comment is the bug to fix upstream.**

## 2. `check_endianness` swaps 32 bits over a 16-bit field

`bcmp/packet.c`, in the `BcmpEchoRequestMessage` arm:

```c
BcmpEchoRequest *request = (BcmpEchoRequest *)buf;
swap_64bit(&request->target_node_id);
swap_16bit(&request->id);
swap_32bit(&request->seq_num);   /* seq_num is uint16_t */
swap_16bit(&request->payload_len);
```

`BcmpEchoRequest.seq_num` is declared `uint16_t` in `bcmp/messages.h`. The
`swap_32bit` therefore reads and writes four bytes across a two-byte field,
corrupting the adjacent `payload_len` — which is then itself swapped. The
`BcmpEchoReplyMessage` arm gets this right with `swap_16bit`.

Only reachable on a big-endian host, since the whole function is guarded by
`if (!is_little_endian())`. Every shipped Bristlemouth target is little-endian,
so this is latent rather than active.

**Not reachable from this harness.** The x86 oracle never enters the swap path,
so no fuzz target can find or confirm this; it was found by reading. `bm-wire`
sidesteps the issue entirely by using explicit little-endian codecs, which are
endian-agnostic by construction.

## 3. `ip_to_nodeid` returns 0 on big-endian

`common/util.h`:

```c
static inline uint64_t ip_to_nodeid(const BmIpAddr *ip) {
  uint32_t high_word = 0, low_word = 0;
  if (ip && is_little_endian()) { /* ... */ }
  return (uint64_t)high_word << 32 | (uint64_t)low_word;
}
```

There is no `else`, so on a big-endian host every address maps to node id 0.
There is already a `//TODO: make this endian agnostic and platform agnostic`
directly above it.

The function also reads the 16-byte, 1-byte-aligned `BmIpAddr` through a
`uint32_t *`, which is both a strict-aliasing violation and a potentially
misaligned load.

`bm-wire`'s `BmIpAddr::to_node_id` always performs the little-endian-host
behaviour — a big-endian read of the low 8 bytes — which is what the C does on
every real target.

## 4. `bm_l2_policy_rx_apply` can report a port number above its documented range

`network/l2_policy.h` documents `ingress_port_num` as "1-15, or 0 if invalid".
`network/l2_policy.c` computes it with `__builtin_ffs`, which returns up to 32:

```c
const uint8_t ingress_port_num = (uint8_t)__builtin_ffs((unsigned)ingress_port_mask);
```

An `ingress_port_mask` of `0x8000` yields 16. `set_ingress_nibble` then masks
with `0x0F`, writing a **zero** ingress nibble into the frame, while the
returned struct still reports 16. So the frame and the result disagree.

Not reachable in practice today: the shim exposes 2 ports and the L2 layer never
builds a mask with a bit above 15 set. `bm-wire` replicates the C exactly.

**Open question for a maintainer:** should a mask above bit 15 be rejected
outright, or should the nibble and the reported number be made consistent?

## 5. `utc_from_date_time` reads `MONTH_DAYS` out of bounds for `month > 12`

`common/util.c`:

```c
static const uint8_t MONTH_DAYS[] = { 31, 28, /* ... */ 31 };  /* 12 entries */

for (i = 1; i < month; i++) {
  /* ... */
  seconds += secs_per_day * MONTH_DAYS[i - 1];
}
```

`month` is a `uint8_t` and is not validated. For `month` in 13..=255 the loop
indexes past the end of a 12-element array — an out-of-bounds read, and
undefined behaviour.

The RTC shim (`bm_rtc_get` → `bm_shim_generic_reset`'s zeroed `RtcTimeAndDate`)
can produce `month == 0`, which is harmless here, but a corrupted or
attacker-influenced RTC value above 12 reaches this loop.

**Status: domain-limited.** There is no defined C behaviour to match, so
`bm-wire-diff`'s `DateTimeInput` constrains `month` to 1..=12 and says so at the
type. `bm-wire::util::utc_from_date_time` stops at the end of the table rather
than reading out of bounds. Fixing this upstream — validating `month`, or
returning an error — would not be wire-visible.

## 6. `uint8_to_uint32` shifts into the sign bit

`common/util.h`:

```c
static inline uint32_t uint8_to_uint32(uint8_t *buf) {
  return (uint32_t)(buf[3] | buf[2] << 8 | buf[1] << 16 | buf[0] << 24);
}
```

`buf[0]` is a `uint8_t`, which integer-promotes to `int`. When `buf[0] >= 0x80`,
`buf[0] << 24` shifts a set bit into the sign bit of a 32-bit `int`, which is
undefined behaviour.

Reported by UndefinedBehaviorSanitizer during `cargo fuzz run addr`:

```
util.h:124:66: runtime error: left shift of 255 by 24 places
cannot be represented in type 'int'
```

This is reachable on every host and is hit by any big-endian read of a buffer
whose first byte has the high bit set — including `ethernet_get_type` on a frame
and the BCMP header parse path.

**Status: benign in practice.** Every mainstream compiler produces the intended
value, and the differential comparison shows no mismatch: `bm-wire`'s
`u32::from_be_bytes` agrees with the C on all 2^32 inputs the fuzzer reached.
The fix upstream is a one-line cast:

```c
return ((uint32_t)buf[3]) | ((uint32_t)buf[2] << 8)
     | ((uint32_t)buf[1] << 16) | ((uint32_t)buf[0] << 24);
```

`uint8_to_uint16` is not affected — `0xFF << 8` fits in an `int`.

## 7. `bm_l2_policy_rx_apply` documents an egress-nibble clear it never performs

`network/l2_policy.h` describes the function as:

> - if routing_cb is used, **clears the egress nibble in the src addr after
>   callback**

`network/l2_policy.c` does no such thing. After the callback returns it only
records the mask:

```c
policy_result.should_submit = routing_cb(ingress_port_num, &egress, src_ip, dst_ip);
policy_result.egress_mask = egress;
```

Whatever the callback wrote into the source address — including the egress
nibble — stays in the frame and is submitted up the stack that way.

This is the same class of defect as divergence #1, in the same header, and the
two interact: a caller reading the header would expect the ports byte to be
clean after `rx_apply` and fully cleared only on the forwarded copy, whereas the
code leaves the RX buffer as the callback left it and zeroes *both* nibbles on
the copy.

`bm-wire` matches the code, and the differential harness covers it: the
`cb_src_write` field of `L2PolicyInput` makes the fake callback write the ports
byte, and both implementations must end up with the same frame.

**Both doc comments in `l2_policy.h` need correcting upstream.**

## 8. `bcmp_tx`'s size guard uses the wrong `sizeof`

`bcmp/bcmp.c`:

```c
if (dst && (uint32_t)size + sizeof(BcmpHeartbeat) <= max_payload_len) {
  buf = bm_ip_tx_new(dst, size + sizeof(BcmpHeader));
```

The guard is meant to check that the message fits inside `max_payload_len`
(1460 = a 1500-byte MTU less the 40-byte IPv6 header). What actually gets sent
is `size + sizeof(BcmpHeader)`, which is what the very next line allocates —
but the guard adds `sizeof(BcmpHeartbeat)` instead.

`BcmpHeartbeat` is 12 bytes; `BcmpHeader` is 13. So a `size` of 1448 passes the
guard and then builds a 1461-byte IPv6 payload: one byte over the budget. The
`BcmpHeartbeat` in the expression is a leftover — heartbeat is simply the
message this ceiling was first written for.

**Status: replicated**, in the sense that `bm-wire` imposes no ceiling of its
own — `bcmp::tx::serialize` takes a caller-sized frame and fails cleanly if it
is too small, so the off-by-one has nowhere to land. The fix upstream is a
one-word change to `sizeof(BcmpHeader)`, and it is not wire-visible: no
conforming sender emits a message in the one-byte window.

## 9. `process_received_message` rewrites the source address before verifying the checksum

`bcmp/packet.c`, with the two macros it uses from the top of the same file:

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

Three bytes of the source address — bytes 2, 4 and 5, or frame offsets 24, 26
and 27 — are rewritten **before** the checksum is computed, and the checksum
covers the source address. So the value a receiver computes is the checksum of
an address that never appeared on the wire.

This is load-bearing and it is documented nowhere. It is what lets a receiving node
stamp the ingress port into the source address on arrival (per spec 5.4.4.1/2)
without invalidating the sender's checksum: the clear undoes the stamp. An
implementation that verifies the checksum over the address as received rejects
every frame a real Bristlemouth node sends.

`clear_ports_legacy` is explicitly marked as backwards compatibility for
bm_core < v0.13.0 and is expected to disappear once resource-based routing
lands — which will be a wire-visible change and needs coordinating.

**`bm-wire` reproduces the rewrite exactly**, in `bcmp::rx::accept`, and the
differential harness covers it: the
`ingress_stamp` and `legacy_ports` fields of `BcmpInput` stamp both after the
frame is built, and both implementations must agree on the verdict and on the
resulting buffer. **The upstream fix is a comment, not a code change.**

## 10. A rejected frame is left with its checksum field zeroed

Continuing the same function:

```c
checksum_read = data.header->checksum;
data.header->checksum = 0;
checksum_calc = PACKET.cb.checksum(payload, size + sizeof(BcmpHeader));
if (checksum_calc != checksum_read) {
  bm_debug(...);
  err = BmEBADMSG;
  return err;              /* <-- checksum field still zero */
}
data.header->checksum = checksum_read;
```

The field is zeroed to compute the checksum over it and restored afterwards —
but the early return on mismatch skips the restore. A caller that inspects,
logs, or forwards a rejected frame sees a header whose checksum reads `0x0000`
rather than the value that arrived, so the one piece of evidence needed to
diagnose the rejection has been destroyed by the code doing the rejecting.

Nothing in bm_core reads the buffer after a rejection today, which is why this
has gone unnoticed. It is still observable state, so `bm-wire` matches it and
says so at `bcmp::rx::RxError::BadChecksum`. The fix upstream is to restore
the field before returning, and it is not wire-visible.

## 11. `clear_ports_legacy` performs a misaligned 32-bit access

The same macro as in #9:

```c
#define clear_ports_legacy(x) (x[1] &= (~(0xFFFFU)))
clear_ports_legacy(((uint32_t *)data.src));
```

`data.src` points at the IPv6 source address, frame offset 22. `x[1]` is
therefore a `uint32_t` read-modify-write at frame offset **26**, which is
`2 mod 4` no matter how the frame itself is aligned — bm_linux's buffers come
from `malloc`, so the frame is at least 8-aligned and offset 26 is never
4-aligned. It is also a strict-aliasing violation, the same one already noted
against `ip_to_nodeid` in divergence #3.

UndefinedBehaviorSanitizer reports both halves:

```
runtime error: load of misaligned address 0x... for type 'uint32_t',
which requires 4 byte alignment
runtime error: store to misaligned address 0x... for type 'uint32_t',
which requires 4 byte alignment
```

This is on the path of **every BCMP frame bm_core receives**, so the alignment
check fires on the first receive of any fuzz run. `bm-wire-sys/build.rs`
therefore passes `-fno-sanitize=alignment` under `CARGO_CFG_FUZZING` and
nothing else: `shift-base`, which found divergence #6, and every other UBSan
check stay on, as does AddressSanitizer.

**Status: benign in practice.** Both shipped targets — x86-64 and Cortex-M33 —
permit unaligned word access, and `bm-wire` sidesteps it entirely by clearing
the two bytes directly. The fix upstream is to write the macro in terms of
`uint8_t`, which is what it means:

```c
#define clear_ports_legacy(x) (((uint8_t *)(x))[4] = 0, ((uint8_t *)(x))[5] = 0)
```

## 12. The egress-port checksum patch drops the end-around carry

`network/l2.c` stamps the egress port into the IPv6 source address on the way
out, which changes a byte the upper-layer checksum covers. Rather than
recompute the checksum it patches it, in `network_add_egress_port`:

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

The idea is sound. Frame offset 24 is the high byte of a 16-bit word in the
one's-complement sum, so stamping it raises the sum by `port_num << 8`, and
the checksum's high byte can be adjusted by `port_num` to match. The patch is
also *necessary*, not merely an optimisation: `process_received_message`
clears only the ingress nibble (`src[2] &= 0xF`), so the egress nibble the
sender stamped is still there when the receiver checksums the frame.

**What is missed is the carry.** A one's-complement sum wraps its carry around
into the low end. Neither branch does that correctly, and they fail
differently, because the two lvalues are different types:

- **UDP.** `payload[udp_checksum_offset]` is a `uint8_t`. `^= 0xFFFF`
  truncates to `^= 0xFF` — the constant says the author believed this was a
  16-bit lvalue — and `+= port_num` is 8-bit, so a carry out of the high byte
  is simply lost. Exhaustively over all 65536 sums and all 15 port numbers,
  the result differs from a correct one's-complement patch in **30720 of
  983040 cases (3.12%)**.
- **BCMP.** `header->checksum` is a `uint16_t`, so the carry propagates one
  place — and because the stored value is byte-swapped relative to the wire,
  that propagation lands exactly where the end-around carry belongs. It is
  right except when the carry itself carries, which needs the sum's low byte
  to be `0xFF` as well: **120 of 983040 cases (0.0122%)**.

The severity is the other way round from the rates. `bm_l2_process_tx_evt`
only stamps **link-local multicast**, and bm_core's UDP traffic — pub/sub via
`bm_pubsub_init` — goes to `multicast_global_addr`, which takes the unstamped
branch. So the 3.12% path is latent, reachable only by an integrator who
registers a middleware application on a link-local destination. BCMP, on the
other hand, sends heartbeats, pings and info to `multicast_ll_addr` and is
stamped on every transmission, so the 0.0122% path is **live on deployed
hardware**: roughly one BCMP frame in 40 000 leaves a two-port node with a
checksum the node at the other end will reject and silently drop.

`bm-wire` reproduces both branches exactly, in `l2::add_egress_port`, and
`bm-wire-diff/tests/l2_egress.rs` pins them down from both directions: the
comparator asserts the port emits the same bytes as bm_core through the TX
capture ring, and two further tests assert those bytes are *wrong* — that a
carrying UDP frame's checksum does not match the frame, and that a
double-carrying BCMP frame is rejected by `bcmp::rx::accept`. Both carry a
note to say that if they start passing, the C has been fixed and the port must
follow.

The fix upstream is to fold the carry back in, in both branches. For BCMP:

```c
uint32_t sum = (uint32_t)(uint16_t)(header->checksum ^ 0xFFFF) + port_num;
header->checksum = (uint16_t)(((sum & 0xFFFF) + (sum >> 16)) ^ 0xFFFF);
```

and for UDP the same, on a properly-read 16-bit field rather than a byte.
`network_revert_checksum` needs the mirrored change. **This is wire-visible in
the sense that it fixes frames that are currently discarded**; a node running
the fix is strictly more interoperable, not less, so it does not need to be
rolled out in lockstep.

## 13. L2 silently drops any frame whose destination is not multicast

`bm_l2_process_tx_evt` dispatches on the destination address:

```c
if (is_global_multicast(dst_ip)) {
  send_global_multicast_packet(payload, tx_evt->length, tx_evt->port_mask);
} else if (is_link_local_multicast(dst_ip)) {
  /* ... stamp and send per port ... */
}

bm_l2_free(tx_evt->buf);
```

There is no `else`. A frame addressed to a unicast address — including the
`FD00::/8` addresses `bm_ip_init` derives for every node — is accepted by
`bm_l2_link_output`, queued, dequeued, and then freed without ever reaching
the network device. No error is returned and nothing is logged: the caller
sees `BmOK` from `bm_l2_link_output` and the frame simply never arrives.

This is consistent with the protocol as it stands, where everything is
multicast and the `//TODO: Add functionality for resource based routing`
in `pubsub.c` marks unicast as future work. It is still a trap for an
integrator, and it is the reason `bm-wire`'s `l2::tx_kind` names the case
`TxKind::Dropped` explicitly rather than folding it into a default. The fix
upstream is a `bm_debug` line and a returned error, and it is not wire-visible.

## 14. Device-info and neighbour-table replies are parsed with unchecked lengths

Both variable-length BCMP replies declare their own sizes, and bm_core copies
according to those declarations without ever comparing them to how many bytes
arrived. `BcmpProcessData` carries a `size` field; neither parser reads it.

**`bcmp/info.c`**, in `populate_neighbor_info`:

```c
neighbor->version_str = (char *)bm_malloc(dev_info->ver_str_len + 1);
memcpy(neighbor->version_str, &dev_info->strings[0], dev_info->ver_str_len);
/* ... */
neighbor->device_name = (char *)bm_malloc(dev_info->dev_name_len + 1);
memcpy(neighbor->device_name, &dev_info->strings[dev_info->ver_str_len],
       dev_info->dev_name_len);
```

`ver_str_len` and `dev_name_len` are `uint8_t` fields taken straight off the
wire, so a reply carrying only its 38-byte fixed part but declaring 255 and 255
copies **510 bytes past the end of the received frame** into two heap buffers.
Those buffers are then held in the neighbour table and printed by
`bcmp_print_neighbor_info`.

**`integrations/topology.c`**, in `neighbor_request_cb`, is worse:

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

`port_len` is a `uint8_t` and `neighbor_len` a `uint16_t`, both off the wire.
Saturated, they ask for `11 + 255*2 + 65535*10` = 655 871 bytes, copied out of
a frame that may have carried eleven. Note also that the sum is accumulated in
a `uint16_t`, so it wraps: the `bm_malloc` and the `memcpy` agree with each
other but not with reality, and large declarations produce a small allocation
and a large copy in some combinations and the reverse in others.

**Reachability.** Neither is gated on anything an attacker cannot arrange.

- The info path requires an entry in `INFO_REQUEST_LIST` for the sender's node
  id. `bcmp_process_heartbeat` calls `bcmp_request_info` whenever a neighbour's
  `time_since_boot_us` goes backwards, which the neighbour itself chooses. So a
  node on the link sends a heartbeat, sends a second with a lower uptime, and
  is then asked for its info — at which point its reply is parsed this way.
- The topology path requires `SENT_REQUEST` and a matching `TARGET_NODE_ID`,
  which is the node being asked. Node ids are in every heartbeat.

Both need only link access, which is the threat model Bristlemouth already
assumes for a physical bus, but neither should be a memory-safety boundary.

**Status: domain-limited.** There is no defined C behaviour to reproduce, so
`bm-wire` does not reproduce it: `DeviceInfoReply::decode` and
`NeighborTableReply::decode` validate every declared length against the buffer
and return `BmWireError::Truncated` otherwise, and the decoded message borrows
the frame rather than copying out of it, so the bounds are checked once and the
iterators cannot walk past them.

The differential harness constrains its input to match: `BcmpMessagesInput`
only ever hands bm_core **well-formed requests**, and its `decode_probe` bytes
— which is where a fuzzer's malformed replies go — are fed to the Rust decoders
and never to the C. Injecting a malformed reply into the oracle would be
exercising undefined behaviour, not comparing against it.

The fix upstream is to check `data.size` before trusting any declared length,
in both parsers, and to accumulate the neighbour-table length in a `uint32_t`.
It is not wire-visible: no conforming sender emits a reply whose declared
lengths exceed the message.

## 15. `bcmp_remove_neighbor_from_table` frees, and reports the wrong result

`bcmp/messages/neighbors.h` presents the two halves of a removal as separate:

```c
bool bcmp_remove_neighbor_from_table(BcmpNeighbor *neighbor);
bool bcmp_free_neighbor(BcmpNeighbor *neighbor);
```

and `bcmp_free_neighbor`'s own doc comment says, in capitals, *"NOTE: this does
NOT remove neighbor from table"* — which reads as an instruction to call the
other one too. Doing so is a **double free**: `bcmp_remove_neighbor_from_table`
ends with

```c
    if (!rval) {
      bm_debug("Something went wrong...\n");
    }

    // Free the neighbor
    rval = bcmp_free_neighbor(neighbor);
  }
  return rval;
```

Two problems in those four lines:

* It frees unconditionally, **including when it did not find the node in the
  list**. A caller passing a stale or foreign pointer gets it freed anyway,
  with whatever still points at it left dangling.
* `rval` is overwritten by `bcmp_free_neighbor`'s result, so the function
  returns "did the free succeed" while its doc comment promises "true if
  successful" at removing. It returns `true` for a node that was never in the
  list.

**Found the hard way.** `bm-wire-diff/src/neighbor.rs` clears the oracle's table
between runs, and the first version called both functions, as the header reads.
It aborted on the second test; valgrind put the first free inside
`bcmp_remove_neighbor_from_table`. The comparator now calls only that one.

**Status: c-only.** The port has no such API — `NeighborTable` owns its storage
and removal is an array shift. The fix upstream is to rename the function to
say that it frees, or to split it honestly and fix the return value.

## 16. An advertised liveliness lease wraps in 32-bit arithmetic

`neighbor_check` in `bcmp/neighbors.c`:

```c
if (neighbor->online &&
    !time_remaining(neighbor->last_heartbeat_ticks, bm_get_tick_count(),
                    bm_ms_to_ticks(2 * neighbor->heartbeat_period_s * 1000))) {
```

`heartbeat_period_s` is a `uint32_t` taken straight from the heartbeat's
`liveliness_lease_dur_s`, so `2 * period * 1000` is evaluated in 32-bit
unsigned arithmetic and wraps at 2^32. A neighbour advertising 2 147 483 648
seconds — nominally 68 years, "effectively forever" — produces a lease of
**zero milliseconds**, and is marked offline by the very next check.

The wrap starts at 2 147 484 seconds, which is far below the field's range, so
a well-meaning integrator asking for a long lease gets a short one. bm_core
itself always sends `bcmp_heartbeat_s` = 10, so this is only reachable from a
peer's advertised value.

`bm-wire` reproduces it with `wrapping_mul` in `Neighbor::lease_ms`, and the
differential harness sweeps the boundary: `advertised_leases_across_the_range`
in `bm-wire-diff/tests/neighbor.rs` covers 0, 1, 10, 2 147 483, 2 147 484,
2 147 483 648 and `u32::MAX`, and replacing the wrapping multiply with a
saturating one makes the tables diverge. The fix upstream is to widen the
expression to 64-bit and clamp.

## 17. A new neighbour is announced to the application twice

`bcmp_update_neighbor` invokes the discovery callback as soon as the entry
exists:

```c
neighbor = bcmp_add_neighbor(node_id, port);
if (neighbor) {
  bcmp_neighbor_invoke_discovery_cb(true, neighbor);
  bcmp_request_info(node_id, &multicast_ll_addr, NULL);
}
```

and `bcmp_process_heartbeat`, having just called it, then invokes it again:

```c
if (!neighbor->online || neighbor_reset) {
  bcmp_neighbor_invoke_discovery_cb(true, neighbor);
}
```

`bcmp_add_neighbor` zeroes the entry, so `online` is still false at that
second test and the condition holds. **Every newly discovered neighbour is
therefore reported to the application twice**, on the same heartbeat, with the
same pointer. An application that counts discoveries, or that does work per
discovery, does it twice.

Confirmed differentially rather than assumed: the comparator registers a real
`NeighborDiscoveryCallback` and compares call counts, and reducing the port to
a single announcement makes it fail with `left: appeared: 2, right: appeared: 1`.

`bm-wire` matches it: `HeartbeatOutcome::discovery_callbacks` is 2 for a new
neighbour. The fix upstream is to drop the call in `bcmp_update_neighbor`,
whose caller already covers the case.

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

The `node_id &&` guard means a lookup for zero always fails, even when a
zero-id neighbour is sitting in the table. Node ids come from
`ip_to_nodeid(data.src)`, the low 8 bytes of the source address, so a node
whose link-local address is exactly `fe80::` has one.

Every heartbeat from such a node is therefore treated as a first sighting:
`bcmp_update_neighbor` adds it again, which evicts the previous copy of itself
from the port, fires the discovery callback (twice, per #17) and sends another
`bcmp_request_info`. The table does not grow — eviction by port sees to that —
but the node is permanently "new", its uptime history is reset on every
heartbeat so a genuine restart can never be detected, and each heartbeat costs
an info request on the wire and an `INFO_REQUEST_LIST` entry that is never
reclaimed (#19).

`bm-wire`'s `NeighborTable::find` reproduces the guard, and the harness covers
it: `a_zero_node_id_is_rediscovered_on_every_heartbeat`. Letting zero match
makes the discovery counts diverge immediately.

The fix upstream is to drop the `node_id &&` guard and reject a zero id where
it is *received* instead, which is where the real question lies.

## 19. `INFO_REQUEST_LIST` grows without de-duplication or expiry

`bcmp_request_info` records every request:

```c
item = ll_create_item(item, &info_cb, sizeof(info_cb), target_node_id);
if (item) {
  err = ll_item_add(&INFO_REQUEST_LIST, item);
```

`ll_item_add` in `common/ll.c` appends unconditionally — it does not look at
the id — so requesting information about the same node twice leaves two
entries. The only removal is in `bcmp_process_info_reply`, which removes one
entry when a reply arrives. A node that is asked and never answers leaves its
entry behind for the life of the process, and `ll_get_item` walks the list
linearly, so the cost of every subsequent reply grows with it.

Ordinary operation is bounded by how often neighbours appear. Combined with
#18 it is not: a node sending heartbeats from `fe80::` is "new" every time, so
each heartbeat appends an entry that will never be removed.

Contrast `bcmp/packet.c`, which has exactly this problem solved — its
`sequence_list` has a 150 ms sweep that expires entries and fires their
callbacks with `NULL`. `INFO_REQUEST_LIST` has no equivalent.

**Status: c-only.** `bm-wire` is sans-io and keeps no such list;
`HeartbeatOutcome::request_info` tells the runtime a request is owed and the
runtime owns any bookkeeping. The fix upstream is to de-duplicate on the id in
`bcmp_request_info`, and to expire entries the way `packet.c` does.

This is also why the `neighbor` fuzz target needs `-fork=1`: the growth is
bounded per iteration but not across a run.

## 20. `ll_remove` leaves `LL::tail` pointing at a freed node

`common/ll.c` keeps `LL` doubly linked, but only `next` is maintained on
removal. `ll_remove`:

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

Two omissions, one of which bites:

* removing the head does not clear the new head's `previous`, which is
  harmless — the only read of `previous` is in the tail branch, and a node that
  is the head takes the head branch first;
* removing a node from the **middle** does not fix up the `previous` of the
  node that follows it. That node's `previous` now points at freed memory, and
  if it is later removed as the tail, `ll->tail = current->previous` stores the
  freed pointer into the list itself.

The next `ll_item_add` dereferences it:

```c
    if (ll->head) {
      node->previous = ll->tail;
      ll->tail->next = node;      // write through a freed pointer
```

Four operations reach it. In `bcmp/packet.c`'s `sequence_list`, which removes
by sequence number in whatever order replies arrive, they are all ordinary:

1. three sequenced requests are outstanding — `[A, B, C]`;
2. the reply to `B` arrives, so `B` is unlinked from the middle and freed;
   `C->previous` now dangles;
3. the reply to `C` arrives; `C` is the tail and not the head, so
   `ll->tail = C->previous`, the freed `B`;
4. a fourth request is sent, and `ll_item_add` writes `node` into the freed
   block.

The write is what a sanitizer reports. What a deployed node sees is quieter and
worse: `ll->head` still points at `A`, whose `next` was set to `NULL` in step 3,
so **the request added in step 4 is not reachable from the head at all**.
`ll_get_item` never finds it, so its reply is treated as unsolicited;
`ll_traverse` never visits it, so the expiry sweep never fires its callback —
not with a payload, and not with `NULL` either. The caller waits forever for a
callback that cannot come, and every subsequent append extends an unreachable
chain hanging off `ll->tail`.

`ll_remove` is shared by every list in bm_core — `packet_list`,
`INFO_REQUEST_LIST`, the resource-discovery lists — so anything that removes
out of insertion order is exposed. `bcmp/config.c` is the module that will hit
it first: it is the only one that issues sequenced requests, and it issues
several concurrently.

**Status: domain-limited.** There is nothing to replicate — the port's
`Registry` keeps a fixed array and closes the gap on removal — and comparing
against a freed-pointer write would be comparing against nothing. So
`bm-wire-diff/src/registry.rs` carries a `LinkModel` that tracks exactly which
of the C's `previous` pointers are stale, and declines to perform the fourth
step above. Every step it does perform is compared in full; the seed
`bm-wire/fuzz/seeds/registry/dangling-tail` drives the shape right up to the
edge. The fix upstream is two lines: clear the new head's `previous` and set
`current->next->previous = current->previous` before freeing.

## 21. A sequenced reply is matched on its sequence number alone

`new_sequence_list_item` records what was asked:

```c
  BcmpRequestElement element;
  element.type = type;
```

and nothing ever reads that field again. `process_received_message` looks the
entry up by number:

```c
      if (cfg->sequenced_reply && !cfg->sequenced_request) {
        request_message = sequence_list_find_message(data.header->seq_num);
```

`sequence_list_find_message` is `ll_get_item(&PACKET.sequence_list, seq_num,
...)`, an id lookup. So **any** reply type whose sequence number happens to
match an outstanding request consumes that request and invokes its callback
with a payload of an entirely different shape. A `BcmpConfigValue` reply
answers a `BcmpNeighborProtoRequest`; the neighbour-proto callback is then
handed config bytes and parses them as its own.

The numbers are not hard to line up. They come from a single global counter
that starts at zero on boot and increments by one per request, and the request
carries the number in its header on the wire — so anything that can see a
request can answer it with a message of any registered reply type, and be
believed. Since `config.c` is the only module issuing sequenced requests today,
the everyday case is milder: one config exchange's reply credited to another's,
because both draw from the same counter.

Confirmed differentially rather than assumed:
`a_reply_of_the_wrong_type_answers_the_request_anyway` in
`bm-wire-diff/tests/registry.rs` sends a `NEIGHBOR_PROTO_REQUEST` and answers
it with a `CONFIG_VALUE`, and adding a type comparison to
`Registry::on_received` makes the C and the port diverge immediately:

```
left:  [Reply { slot: 0, payload: [...] }]
right: [Process { message_type: ConfigValue, seq_num: 0, payload: [...] }]
```

`bm-wire` matches the C — `Registry::on_received` compares `seq_num` and
nothing else — and `PendingRequest::message_type` is carried for the caller's
benefit, as the C carries it, without taking part in the match. The fix
upstream is to compare `element->type` against the type the reply is a reply
to, which needs the registry to say which request type each reply type answers.

## 22. A sequenced request's timeout is the sweep period, not the timeout

`bcmp/packet.c` opens with two constants:

```c
#define default_message_timeout_ms 24
#define message_timer_expiry_period_ms 150
```

Every sequenced request is stamped with the first:
`new_sequence_list_item(header->type, default_message_timeout_ms, ...)`. But
nothing consults it except `timer_traverse_cb`, which only runs from
`sequence_list_timer_callback`, which only runs when the 150 ms auto-reload
timer fires. **The 24 ms is a threshold applied on a 150 ms grid**, so what a
request actually gets is neither number.

Measured rather than inferred. `the_effective_timeout_across_the_whole_phase`
in `bm-wire-diff/tests/registry.rs` walks the clock a millisecond at a time for
every offset in the sweep's phase, against the real timer in the shim rather
than against the constant, and
`the_effective_timeout_ranges_from_25_to_174_milliseconds` in `bm-wire`'s own
unit tests asserts the resulting shape:

| Sent at | Expired at | Lived for |
|---|---|---|
| 125 ms (25 before a sweep) | 150 ms | **25 ms** |
| 0 ms (on a sweep) | 150 ms | 150 ms |
| 126 ms (24 before a sweep) | 300 ms | **174 ms** |

One millisecond earlier or later in the phase is the difference between 25 ms
and 174 ms — a factor of seven, for identical traffic, decided by nothing the
caller can see or control. The nominal 24 ms is the one value a request can
never get, because the comparison is strict: a request exactly 24 ms old when
the sweep reaches it survives to the next one.

Whether that window is long enough for a real reply is a question about a real
link, and this harness cannot answer it — the shim's clock only moves when the
test says so. What it can say is that the answer differs by 7× depending on
scheduling jitter of a single millisecond, which is not a property a timeout
should have. Card C3 is where it will matter: `bcmp/config.c` is the only
module that issues sequenced requests, and a config get whose reply arrives in
the wrong part of the phase is reported to the application as a failure
(`cb(NULL)`) and then delivered again as an unsolicited `BcmpConfigValue` a
moment later.

`bm-wire` reproduces the grid, not just the threshold: `Registry::on_tick`
carries the sweep's phase and only sweeps when one comes due, so the port times
out the same requests at the same instants a C node does. Deleting the phase
check — sweeping on every tick, which is what a reasonable reading of the
constants would suggest — makes six of the comparator's tests fail. The fix
upstream is to make the sweep period the timeout, or to drive the expiry from
the entry's own deadline rather than from a fixed tick.
