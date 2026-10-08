# Prompt: add per-port frame handling to `embassy-net-adin1110`

**Status: done.** [embassy-rs/embassy#7024](https://github.com/embassy-rs/embassy/pull/7024)
is merged to embassy `main`. `bm-phy-adin2111` pins that branch until
`embassy-net-adin1110` 0.5 is released. Kept as the design record.

Hand the section below to a Claude Code instance working in a clone of
[`embassy-rs/embassy`](https://github.com/embassy-rs/embassy). It is
self-contained and does not assume access to this repository.

Everything under **What is already true** was read out of the driver at the
commit this file was written against. Line numbers drift; file and symbol names
are the durable part. The receive-footer port encoding is deliberately not
asserted — the agent is told to confirm it from the datasheet.

---

## The task

You are working in a clone of `embassy-rs/embassy`. The goal is a pull request
against `embassy-net-adin1110` that lets a consumer **choose the egress port
per frame** and **learn the ingress port of each received frame**, on the
ADIN2111 in OPEN Alliance TC6 SPI mode.

The person asking is a maintainer of the [Bristlemouth](https://bristlemouth.org)
GitHub organisation (`bm_core`, `bm_protocol`, `bm_sbc`) and the author of the
TC6 support already in this driver — the "Added OPEN Alliance TC6 SPI protocol
support" line in `embassy-net-adin1110/CHANGELOG.md` under Unreleased. Treat
them as a domain expert on both the protocol and this driver; ask rather than
guess when the design is uncertain.

## Why it is needed

Bristlemouth is a two-wire multi-drop bus protocol. Its spec (section 5.4.4)
encodes the **ingress port number into the IPv6 source address** of every frame
a node receives, and a forwarding node **picks an egress port per copy** and
stamps that into the same byte on the way out. So:

- A driver that does not report which port a frame arrived on cannot carry the
  protocol — the node has nothing to write into the address.
- A driver that cannot select an egress port per frame cannot forward. Every
  copy would go out every port.

This is the reason the ADIN**2111** (two ports) is used instead of the
ADIN**1110** (one). A Bristlemouth node does not use `embassy-net`'s IP stack:
it does its own IPv6 and its own BCMP framing, and what it needs from this
driver is port-aware raw frame I/O.

## What is already true in the driver

Read these before designing anything; several are further along than they look.

**Transmit — the mechanism exists, only the choice is fixed.**

- `embassy-net-adin1110/src/protocol/tc6.rs` defines
  `pub enum TxPort { Port1, Port2, Flood }`.
- It is stored as `Tc6::tx_port` and set once, in
  `Tc6::new(spi, append_fcs_on_tx, tx_port)` — reached from
  `ADIN1110::new_tc6(...)` and `embassy_net_adin1110::new_tc6(...)`.
- `Tc6::send_frame` matches on that field and calls
  `send_frame_on_port(frame, pad_len, fcs, port: u8)`, which already takes a
  port and puts it in the TC6 data-chunk header:
  `val |= u32::from(port) << DATA_HDR_VS_SHIFT`, with `DATA_HDR_VS_SHIFT = 22`.

So per-frame egress selection is mostly plumbing a parameter through.

**Receive — the port is not read.**

- `struct Footer(u32)` has accessors for `exst` (31), `hdrb` (30), `sync` (29),
  `rca` (28:24), `dv` (21), `sv` (20), `swo` (19:16), `fd` (15), `ev` (14),
  `ebo` (13:8) and `txc` (5:1).
- **Bits 23:22 have no accessor.** In the OPEN Alliance TC6 receive footer that
  field is `VS` (vendor-specific) — the mirror of the transmit header's `VS` at
  the same shift, which this driver already uses for the egress port.

*Confirm this before relying on it.* Check the ADIN2111 datasheet and the OPEN
Alliance 10BASE-T1x MAC-PHY Serial Interface specification for what the
ADIN2111 puts in the receive footer's VS field, and whether it needs enabling
(look for a port-forwarding or VS-enable bit in `CONFIG0`/`CONFIG2` or the
ADIN2111-specific registers). If the ingress port is *not* available there,
stop and report that before writing code — the rest of the design depends on it.

**Per-port link state — tracked, then discarded.**

- `pub struct Runner<'d, P: Adin1110Protocol, INT, RST>` has a private
  `port_link: [bool; 2]`.
- `Runner::handle_status` calls `service_phy_int(..., MDIO_PHY_ADDR, 0)` and
  `service_phy_int(..., MDIO_PHY_ADDR_PORT2, 1)`, which maintain that array.
- It is then collapsed for `embassy-net`:
  `let any_link = port_link.iter().any(|&l| l); state_chan.set_link_state(...)`.
  Nothing exposes the per-port value. `MDIO_PHY_ADDR` is `pub`;
  `MDIO_PHY_ADDR_PORT2` is private.

**The interface is the constraint.**

- `pub type Device<'d> = embassy_net_driver_channel::Device<'d, MTU>;`
- `embassy-net-driver-channel` carries frame bytes and nothing else. There is
  no room for per-frame metadata such as an ingress port, and widening it would
  change a crate every embassy-net driver depends on.

## Suggested shape — confirm before building

Put this to the user and to the embassy maintainers before writing much code.

**Do not push port metadata through `embassy-net-driver-channel`.** Expose a
port-aware raw path *alongside* the existing `Device`, for consumers not using
`embassy-net` at all:

1. Per-frame egress on transmit — e.g. `Tc6::send_frame_to(&mut self, frame,
   TxPort)`, keeping `send_frame` as-is. `TxPort` already expresses
   `Port1`/`Port2`/`Flood`, which maps onto what Bristlemouth needs (`Flood` is
   the global-multicast case).
2. Ingress port on receive — return it alongside the frame from the raw receive
   entry point, sourced from the footer VS bits.
3. Per-port link state — a public accessor for `port_link`, or per-port
   `LinkState`. Making `MDIO_PHY_ADDR_PORT2` public may suffice for some
   consumers, but an accessor is the better interface.

Keep `new`, `new_tc6`, `Runner::run` and `Device` behaviour exactly as they
are. This should be purely additive.

**Before writing the implementation**, search embassy's issues and pull
requests for prior art on per-port ADIN2111 support or on carrying metadata
through the driver channel, and raise the design in an issue or draft PR.

## The consumer this has to satisfy

The downstream node abstracts the PHY behind this trait:

```rust
/// Where a frame should go.
pub enum Egress {
    /// Every port at once.
    AllPorts,
    /// One port, 1-based.
    Port(u8),
}

pub trait Phy {
    type Error: core::fmt::Debug;

    /// How many ports the device has. Ports are numbered 1..=`port_count`.
    fn port_count(&self) -> u8;

    /// Transmit one frame.
    async fn send(&mut self, frame: &[u8], egress: Egress) -> Result<(), Self::Error>;

    /// Wait for a frame, returning the ingress port and the length written
    /// into `buf`.
    async fn receive(&mut self, buf: &mut [u8]) -> Result<(u8, usize), Self::Error>;
}
```

Note the port-numbering mismatch: Bristlemouth numbers ports from 1, and
`send_frame_on_port` takes a 0-based index that goes straight into the VS
field. Decide which convention the public API uses, document it, and be
consistent — an off-by-one here is invisible until two nodes disagree about
which wire a frame came from.

## Conventions and verification

- `edition = "2024"`. Read the root `rustfmt.toml`, `CONTRIBUTING.md` and
  `ci.sh` before your first commit.
- The crate is `no_std` with `defmt`/`log` optional and two protocol features,
  `generic-spi` (default) and `tc6`. **Build and test with each feature
  combination** — TC6 code is behind `--features tc6`.
- There are unit tests at the bottom of `protocol/tc6.rs` driven by
  `embedded-hal-mock` with explicit SPI transaction expectations. Any change to
  header or footer handling should come with tests in that style: construct the
  footer word with the VS bits set, assert the reported ingress port.
- Add a line to `embassy-net-adin1110/CHANGELOG.md` under `## Unreleased`.
- Run `cargo fmt`, `cargo clippy` and the crate's tests before pushing. Fix
  every warning; this crate lints hard (`clippy::cast_possible_truncation` and
  friends are denied in places).

Hardware verification is the user's — they have the dev kit. Say plainly what
you could and could not verify yourself.

## What not to do

- Do not change `embassy-net-driver-channel`.
- Do not alter the behaviour of the existing `Device`, `Runner::run`, `new` or
  `new_tc6` paths.
- Do not invent register or bit semantics. Every hardware fact should be
  traceable to the ADIN2111 datasheet or the OPEN Alliance TC6 specification,
  and cited in a comment where it is not obvious.
- Do not open the pull request without the user reviewing the diff first. Push
  the branch and show them what it contains.
