# bristlemouth-rs

A Rust implementation of [Bristlemouth](https://www.bristlemouth.org/): the
wire format and protocol logic of
[bm_core](https://github.com/bristlemouth/bm_core), an
[embassy](https://embassy.dev/) node runtime, and firmware for the dev kit's
mote. It runs on a bus alongside nodes running the C firmware.

Compatibility with the C is tested, not assumed: the real bm_core is compiled
as an oracle, and differential tests and fuzzers feed both implementations the
same input and require the same output. Where bm_core has a quirk, the port
reproduces it and records it in [docs/c-divergences.md](docs/c-divergences.md).

## Crates

| Crate | What it is | Target |
| --- | --- | --- |
| [`bm-wire`](bm-wire) | Codecs and sans-io state machines. `no_std`, no `alloc`, no `unsafe`. | any |
| [`bm-stack`](bm-stack) | The node: timers, PHY and storage traits, pub/sub, services, DFU. `no_std`, on `embassy-time`. | any |
| [`bm-phy-adin2111`](bm-phy-adin2111) | `bm_stack::Phy` for the ADIN2111. | any |
| [`bm-devkit`](bm-devkit) | Board support and example firmware for the dev kit mote (STM32U575, ADIN2111). | thumbv8m |
| [`bm-mcuboot`](bm-mcuboot) | MCUboot image header, TLVs and slot trailer. `no_std`, no dependencies. | any |
| [`bm-image`](bm-image) | Builds and inspects `.dfu.bin` and `.unified.bin` images. | host |
| [`bm-wire-sys`](bm-wire-sys), [`bm-mcuboot-sys`](bm-mcuboot-sys) | The C oracles: bm_core and MCUboot's `bootutil`. | host |
| [`bm-wire-diff`](bm-wire-diff), [`bm-mcuboot-diff`](bm-mcuboot-diff) | Differential tests of the ports against the oracles. | host |

`bm-phy-adin2111`, `bm-devkit` and `bm-wire/fuzz` are separate cargo
workspaces: the first two pin embassy to git, and the root workspace stays on
crates.io releases.

## Getting started

Rust 1.97 or later, `libclang` for bindgen, and the submodules:

```
git submodule update --init --recursive bm-wire-sys/vendor/bm_core
git submodule update --init bm-mcuboot-sys/vendor/mcuboot
```

Run the tests and the example node, which runs on a scripted PHY and needs no
hardware:

```
cargo test
cargo run -p bm-stack --example hello_node
```

[bm-stack/examples/hello_node.rs](bm-stack/examples/hello_node.rs) is the
shortest complete use of the API.

## On a dev kit

With a debug probe and [probe-rs](https://probe.rs/), on a mote that already
has bm_protocol's MCUboot bootloader:

```
cd bm-devkit && cargo run --release --bin hello_world
```

[bm-devkit/README.md](bm-devkit/README.md) covers the board, image signing and
DFU over the bus.

## Contributing

[CLAUDE.md](CLAUDE.md) is the contributor guide for people as well as agents:
the per-file layout, the porting procedure, and every command CI runs.
