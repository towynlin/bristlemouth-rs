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

| # | Where | Status | Found by |
|---|---|---|---|
| 1 | `bm_l2_policy_prepare_forwarded_copy` doc contradicts code | replicated | reading |
| 2 | `check_endianness` swaps 32 bits over a 16-bit field | replicated | reading |
| 3 | `ip_to_nodeid` returns 0 on big-endian | replicated | reading |
| 4 | `__builtin_ffs` can yield a port number above 15 | replicated | reading |
| 5 | `utc_from_date_time` reads `MONTH_DAYS` out of bounds | domain-limited | reading |
| 6 | `uint8_to_uint32` shifts into the sign bit | benign | **UBSan, via `cargo fuzz run addr`** |
| 7 | `bm_l2_policy_rx_apply` doc claims an egress-nibble clear that is not in the code | replicated | reading |

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
