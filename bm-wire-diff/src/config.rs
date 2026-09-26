//! Differential comparator for the config exchange `0xA0`–`0xA9`
//! ([`bm_wire::bcmp::config`] and the config half of [`bm_stack::Node`]),
//! driven through bm_core's live stack.
//!
//! `bcmp_process_config_message` and everything it calls are `static` inside
//! `bcmp/config.c`, so as with [`crate::time`] the only way at them is the
//! wire: seed the store, inject a `0xA0`–`0xA9`, pump the tasks, and read what
//! the oracle put on the network device. [`check`] runs the identical frame
//! through [`bm_stack::Node::on_frame`] against a store seeded the same way,
//! then compares the `(port, bytes)` sequences and the resulting store images.
//!
//! # What one comparison covers
//!
//! * **the reply's bytes**, for a get, set, status request, delete or clear
//!   addressed to this node — header, checksum, and the body each builds;
//! * **the store mutation**, for a set, delete, clear or commit — the RAM
//!   images of all three partitions after the message, byte for byte;
//! * **the decision to forward**, when the target is another node. Zero is not
//!   a broadcast here (unlike system time): `bcmp_process_config_message`
//!   forwards everything that is not an exact match, so a message to node zero
//!   is re-flooded and answered by nobody.
//!
//! # The store is seeded, not shared
//!
//! The oracle brings `CONFIGS` up empty. Each seed applies the same typed sets
//! to the oracle (through the FFI) and to a fresh [`bm_stack::Config`], saves
//! both, and hands the Rust store to the node. `CONFIGS` is process-global with
//! no deinit, so `reset` clears it through its own front door — save any
//! uncommitted partition, zero the shim's flash, `config_init` — at the start
//! of every seed.
//!
//! # `restart` and flash
//!
//! `bcmp_config_process_commit_msg` calls `save_config(partition, true)`. The
//! shim's `bm_config_reset` then clears every partition's flash;
//! `RamConfigStorage`'s `reset` does nothing, and on hardware it
//! reboots. That is an integrator-seam difference, not a wire divergence, so
//! [`check`] skips the flash comparison after a commit and compares only the
//! RAM images and `needs_commit`, which the two do agree on.
//!
//! # Divergence #12 stops being rare here
//!
//! A `ConfigValue` carries whatever the slot holds, so its bytes vary freely,
//! and the egress-port checksum patch (divergence #12) corrupts about one
//! frame in 40 000. [`crate::time`] documents it; [`read_config_reply`]
//! tolerates exactly that failure and reads the body anyway, after [`check`]
//! has already compared the frames byte for byte.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`]. Its seeds are in
//! [`crate::replay::STACK_TARGETS`].

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_stack::RamConfigStorage;
use bm_stack::config::{Config, config_init, save_config};
use bm_wire::bcmp::config::{
    ConfigClearResponse, ConfigDeleteResponse, ConfigHeader, ConfigStatusResponse, ConfigValue,
};
use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, MessageType, rx, tx};
use bm_wire::configuration::{ConfigStore, Key, Partition};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::configuration::{IMAGE_LEN, LAYOUT, bounded_bytes, c_config_init, c_image, c_partition};
use crate::stack::{self, NUM_PORTS, capture, drain, inject, oracle};

/// Node id the injected message appears to come from.
pub const PEER_NODE_ID: u64 = 0x0000_0000_55AA_0011;

/// A third node, so a message can name somebody who is neither end.
pub const THIRD_NODE_ID: u64 = 0x0000_0000_0BAD_F00D;

/// Keys the seed and the injected message draw from, chosen to collide with
/// each other and to reach the 31/32-byte key boundary.
const KEYS: &[&[u8]] = &[
    b"foo",
    b"bar",
    b"baz",
    b"a",
    b"quux",
    b"abcdefghijklmnopqrstuvwxyz01234",
    b"abcdefghijklmnopqrstuvwxyz012345",
];

/// One value a seed stores, chosen so status responses have keys and gets have
/// slots worth reading back.
#[derive(Debug, Clone)]
pub enum SeedValue {
    Uint(u32),
    Int(i32),
    Str(Vec<u8>),
}

/// One key/value a seed writes to both stores before the message arrives.
#[derive(Debug, Clone)]
pub struct Seed {
    partition: Partition,
    key: Vec<u8>,
    value: SeedValue,
}

impl Seed {
    /// A seed storing `value` under `key` in `partition`.
    #[must_use]
    pub fn uint(partition: Partition, key: &[u8], value: u32) -> Self {
        Self {
            partition,
            key: key.to_vec(),
            value: SeedValue::Uint(value),
        }
    }

    /// A seed storing the signed `value`.
    #[must_use]
    pub fn int(partition: Partition, key: &[u8], value: i32) -> Self {
        Self {
            partition,
            key: key.to_vec(),
            value: SeedValue::Int(value),
        }
    }

    /// A seed storing the string `value`.
    #[must_use]
    pub fn str(partition: Partition, key: &[u8], value: &[u8]) -> Self {
        Self {
            partition,
            key: key.to_vec(),
            value: SeedValue::Str(value.to_vec()),
        }
    }
}

impl<'a> Arbitrary<'a> for Seed {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let value = match u.int_in_range(0u8..=2)? {
            0 => SeedValue::Uint(u.arbitrary()?),
            1 => SeedValue::Int(u.arbitrary()?),
            _ => SeedValue::Str(bounded_bytes(u, 20)?),
        };
        Ok(Self {
            partition: partition(u)?,
            key: KEYS[u.choose_index(KEYS.len())?].to_vec(),
            value,
        })
    }
}

impl Seed {
    fn apply_rust(&self, store: &mut ConfigStore) {
        let part = store.partition_mut(self.partition);
        let key = Key::new(&self.key);
        match &self.value {
            SeedValue::Uint(v) => part.set_uint(key, *v),
            SeedValue::Int(v) => part.set_int(key, *v),
            SeedValue::Str(v) => part.set_string(key, v),
        };
    }

    fn apply_c(&self) {
        let p = c_partition(self.partition);
        // The C setters take a NUL-terminated key; a bare slice over-reads.
        let mut key_z = self.key.clone();
        key_z.push(0);
        let key = key_z.as_ptr().cast();
        let len = self.key.len();
        // SAFETY: `key`/`len` describe a live buffer; the setters copy from it.
        unsafe {
            match &self.value {
                SeedValue::Uint(v) => bm_wire_sys::set_config_uint(p, key, len, *v),
                SeedValue::Int(v) => bm_wire_sys::set_config_int(p, key, len, *v),
                SeedValue::Str(v) => {
                    bm_wire_sys::set_config_string(p, key, len, v.as_ptr().cast(), v.len())
                }
            }
        };
    }
}

/// Which of the ten config messages to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMessage {
    /// `0xA0`, answered with a `0xA1` carrying the slot.
    Get,
    /// `0xA1`, which a C node only logs.
    Value,
    /// `0xA2`, which stores and is answered with a `0xA1`.
    Set,
    /// `0xA3`, which saves and restarts and is not answered.
    Commit,
    /// `0xA4`, answered with a `0xA5`.
    StatusRequest,
    /// `0xA5`, which a C node only logs.
    StatusResponse,
    /// `0xA6`, answered with a `0xA7`.
    DeleteRequest,
    /// `0xA7`, which a C node only logs.
    DeleteResponse,
    /// `0xA8`, answered with a `0xA9`.
    ClearRequest,
    /// `0xA9`, which a C node only logs.
    ClearResponse,
}

impl ConfigMessage {
    /// The BCMP type byte.
    #[must_use]
    pub fn message_type(self) -> MessageType {
        match self {
            Self::Get => MessageType::CONFIG_GET,
            Self::Value => MessageType::CONFIG_VALUE,
            Self::Set => MessageType::CONFIG_SET,
            Self::Commit => MessageType::CONFIG_COMMIT,
            Self::StatusRequest => MessageType::CONFIG_STATUS_REQUEST,
            Self::StatusResponse => MessageType::CONFIG_STATUS_RESPONSE,
            Self::DeleteRequest => MessageType::CONFIG_DELETE_REQUEST,
            Self::DeleteResponse => MessageType::CONFIG_DELETE_RESPONSE,
            Self::ClearRequest => MessageType::CONFIG_CLEAR_REQUEST,
            Self::ClearResponse => MessageType::CONFIG_CLEAR_RESPONSE,
        }
    }

    /// Whether receiving this message can change the store, so a commit's flash
    /// reset must not be compared.
    fn is_commit(self) -> bool {
        self == Self::Commit
    }

    /// Whether the C's handler indexes `CONFIGS` with the partition byte
    /// unchecked (divergence #50). Every type but the clear request and the
    /// four log-only responses does, so an out-of-range partition is an
    /// out-of-bounds access with no defined behaviour to compare against.
    fn indexes_configs_unchecked(self) -> bool {
        matches!(
            self,
            Self::Get | Self::Set | Self::Commit | Self::StatusRequest | Self::DeleteRequest
        )
    }
}

/// Who the injected message names in its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Zero. Not a broadcast: forwarded, and answered by nobody.
    Zero,
    /// The oracle's own node id.
    ThisNode,
    /// A third node's, which makes the oracle forward it.
    OtherNode,
}

impl Target {
    fn node_id(self) -> u64 {
        match self {
            Self::Zero => 0,
            Self::ThisNode => stack::NODE_ID,
            Self::OtherNode => THIRD_NODE_ID,
        }
    }
}

/// One config message put to both stacks, plus the store both start from.
#[derive(Debug, Clone)]
pub struct ConfigInput {
    /// What to store on both sides before the message arrives.
    pub seeds: Vec<Seed>,
    /// Which of the ten to inject.
    pub message: ConfigMessage,
    /// Who the body addresses.
    pub target: Target,
    /// The partition byte, which may be out of range (divergence #50).
    pub partition: u8,
    /// The key for a get, set or delete request.
    pub key: Vec<u8>,
    /// The value for a set, or the CBOR of a value message.
    pub value: Vec<u8>,
    /// The sequence number in the BCMP header, which a reply echoes.
    pub seq_num: u32,
    /// `committed`/`success` byte for a response type.
    pub flag: bool,
    /// Port it arrives on, 1..=[`NUM_PORTS`].
    pub ingress_port: u8,
    /// Address the frame to `FF03::1` rather than `FF02::1`, so a forwarded
    /// message is seen relayed and re-flooded (divergence #28).
    pub global_multicast: bool,
}

fn partition(u: &mut Unstructured<'_>) -> Result<Partition> {
    Ok(match u.int_in_range(0u8..=3)? {
        0 => Partition::User,
        3 => Partition::Hardware,
        _ => Partition::System,
    })
}

impl<'a> Arbitrary<'a> for ConfigInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let seed_count = u.int_in_range(0usize..=6)?;
        let mut seeds = Vec::with_capacity(seed_count);
        for _ in 0..seed_count {
            seeds.push(u.arbitrary()?);
        }
        let message = match u.int_in_range(0u8..=9)? {
            0 => ConfigMessage::Get,
            1 => ConfigMessage::Value,
            2 => ConfigMessage::Set,
            3 => ConfigMessage::Commit,
            4 => ConfigMessage::StatusRequest,
            5 => ConfigMessage::StatusResponse,
            6 => ConfigMessage::DeleteRequest,
            7 => ConfigMessage::DeleteResponse,
            8 => ConfigMessage::ClearRequest,
            _ => ConfigMessage::ClearResponse,
        };
        let target = match u.int_in_range(0u8..=2)? {
            0 => Target::Zero,
            1 => Target::ThisNode,
            _ => Target::OtherNode,
        };
        // A real partition, except the clear request, whose handler is the one
        // that range-checks the byte, so it may carry any (divergence #50).
        let partition = if message == ConfigMessage::ClearRequest && u.ratio(1u8, 4)? {
            u.arbitrary()?
        } else {
            partition(u)? as u8
        };
        let key = KEYS[u.choose_index(KEYS.len())?].to_vec();
        let value = crate::configuration::cbor_value(u)?;
        let mut input = Self {
            seeds,
            message,
            target,
            partition,
            key,
            value,
            seq_num: u.arbitrary()?,
            flag: u.arbitrary()?,
            ingress_port: u.arbitrary()?,
            global_multicast: u.arbitrary()?,
        };
        input.clamp_to_domain();
        Ok(input)
    }
}

impl Domain for ConfigInput {
    fn clamp_to_domain(&mut self) {
        self.ingress_port = self.ingress_port.wrapping_sub(1) % NUM_PORTS + 1;
        self.key.truncate(40);
        self.value.truncate(64);
        self.seeds.truncate(6);
        // A partition byte the C would index `CONFIGS` with must be in range
        // (divergence #50); only the clear request, which checks it, may carry
        // any byte.
        if self.message.indexes_configs_unchecked() && Partition::from_u8(self.partition).is_none()
        {
            self.partition = Partition::System as u8;
        }
    }
}

impl ConfigInput {
    fn destination(&self) -> BmIpAddr {
        if self.global_multicast {
            BmIpAddr::GLOBAL_MULTICAST
        } else {
            BmIpAddr::LINK_LOCAL_MULTICAST
        }
    }

    fn header(&self) -> ConfigHeader {
        ConfigHeader {
            target_node_id: self.target.node_id(),
            source_node_id: PEER_NODE_ID,
        }
    }

    /// The message body for the type this input names.
    fn body(&self) -> Vec<u8> {
        let header = self.header();
        let mut buf = vec![0u8; 2048];
        let len = match self.message {
            ConfigMessage::Get | ConfigMessage::DeleteRequest => {
                use bm_wire::bcmp::config::ConfigKeyRequest;
                ConfigKeyRequest {
                    header,
                    partition: self.partition,
                    key: &self.key,
                }
                .encode(&mut buf)
            }
            ConfigMessage::Value => ConfigValue {
                header,
                partition: self.partition,
                data: &self.value,
            }
            .encode(&mut buf),
            ConfigMessage::Set => {
                use bm_wire::bcmp::config::ConfigSet;
                ConfigSet {
                    header,
                    partition: self.partition,
                    key: &self.key,
                    data: &self.value,
                }
                .encode(&mut buf)
            }
            ConfigMessage::Commit | ConfigMessage::StatusRequest | ConfigMessage::ClearRequest => {
                use bm_wire::bcmp::config::ConfigPartitionRequest;
                ConfigPartitionRequest {
                    header,
                    partition: self.partition,
                }
                .encode(&mut buf)
            }
            ConfigMessage::StatusResponse => {
                // A hand-built body: header, partition, committed, num_keys 0.
                header.encode(&mut buf).map(|()| {
                    buf[16] = self.partition;
                    buf[17] = u8::from(self.flag);
                    buf[18] = 0;
                    ConfigStatusResponse::HEAD_LEN
                })
            }
            ConfigMessage::DeleteResponse => ConfigDeleteResponse {
                header,
                success: self.flag,
                partition: self.partition,
                key: &self.key,
            }
            .encode(&mut buf),
            ConfigMessage::ClearResponse => ConfigClearResponse {
                header,
                success: self.flag,
                partition: self.partition,
            }
            .encode(&mut buf),
        }
        .expect("2048 bytes holds any config body");
        buf.truncate(len);
        // `set_config_cbor` stores the key with `snprintf("%s", keyAndData)`,
        // which reads to a NUL past the key and the value both (divergence #45).
        // With neither carrying one it runs off the received buffer — a heap
        // over-read with no defined behaviour to compare against — so a Set body
        // is terminated. The receivers ignore the extra byte: it is past the
        // declared `data_length`, and `Key::with_len` stops at it exactly as the
        // C's `snprintf` does.
        if self.message == ConfigMessage::Set {
            buf.push(0);
        }
        buf
    }

    /// The frame both stacks are given, checksummed and ready to inject.
    fn build(&self) -> Vec<u8> {
        let body = self.body();
        let payload_len = BCMP_HEADER_LEN + body.len();
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
            .copy_from_slice(&bm_wire::addr::nodeid_to_ip(0xFE80_0000, PEER_NODE_ID).0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&self.destination().0);
        tx::serialize(&mut frame, self.message.message_type(), self.seq_num, &body)
            .expect("frame is sized for the body");
        frame
    }
}

/// The frame an input injects, for tests that want to look at it first.
#[must_use]
pub fn build_frame(input: &ConfigInput) -> Vec<u8> {
    let mut input = input.clone();
    input.clamp_to_domain();
    input.build()
}

/// Bring `CONFIGS` and the shim's flash back to empty, and hand back a Rust
/// [`Config`] in the same state, seeded with `seeds`.
///
/// The oracle's store is process-global with no deinit: any uncommitted
/// partition is saved (the only route to `needs_commit == false`), the shim's
/// flash is zeroed, and `config_init` reloads it. Then `seeds` are applied to
/// both, and both are saved so their flash matches.
fn reset(seeds: &[Seed]) -> Config<RamConfigStorage> {
    for p in Partition::ALL {
        // SAFETY: a plain read of the flag, then a save if set.
        if unsafe { bm_wire_sys::needs_commit(c_partition(p)) } {
            unsafe { bm_wire_sys::save_config(c_partition(p), false) };
        }
        let mut zero = [0u8; IMAGE_LEN];
        // SAFETY: `zero` is live and IMAGE_LEN long.
        unsafe { bm_wire_sys::bm_config_write(c_partition(p), 0, zero.as_mut_ptr(), IMAGE_LEN, 0) };
    }
    c_config_init();

    let mut store = ConfigStore::new(LAYOUT);
    let mut flash = RamConfigStorage::new();
    config_init(&mut store, &mut flash);

    for seed in seeds {
        seed.apply_rust(&mut store);
        seed.apply_c();
    }
    // Save every partition on both sides, so RAM and flash start identical and
    // committed. `save_config(_, false)` never triggers the shim's reset.
    for p in Partition::ALL {
        if store.partition(p).needs_commit() {
            save_config(&mut store, p, &mut flash, false);
            // SAFETY: writes the partition to the shim's flash.
            unsafe { bm_wire_sys::save_config(c_partition(p), false) };
        }
    }

    Config {
        store,
        storage: flash,
    }
}

/// Seed the oracle, inject `input`'s frame, and return the frames it
/// transmitted — the C side of [`check`], self-contained.
///
/// Takes the oracle lock itself, so a caller must not already hold it.
///
/// # Panics
///
/// If the ring was not drained, or the injection does not reach L2.
#[must_use]
pub fn oracle_frames(input: &ConfigInput) -> Vec<(u8, Vec<u8>)> {
    let mut input = input.clone();
    input.clamp_to_domain();
    let frame = input.build();
    let _guard = oracle();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );
    let _config = reset(&input.seeds);
    inject(input.ingress_port, &frame);
    drain()
}

/// Parse a captured reply, tolerating the one checksum failure bm_core itself
/// produces (divergence #12). Returns the header, the body, and whether the
/// checksum verified.
///
/// # Panics
///
/// If the frame is not BCMP, or fails its checksum without having been stamped.
#[must_use]
pub fn read_config_reply(port: u8, captured: &[u8]) -> (BcmpHeader, Vec<u8>, bool) {
    let stamped = captured[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F != 0;
    let payload_len = usize::from(u16::from_be_bytes([
        captured[IPV6_PAYLOAD_LENGTH_OFFSET],
        captured[IPV6_PAYLOAD_LENGTH_OFFSET + 1],
    ]));

    let mut copy = captured.to_vec();
    match rx::accept(&mut copy) {
        Ok(received) => (received.header, received.payload.to_vec(), true),
        Err(rx::RxError::BadChecksum) if stamped => {
            let bcmp = &captured[BCMP_HEADER_OFFSET..BCMP_HEADER_OFFSET + payload_len];
            (
                BcmpHeader::decode(bcmp).expect("13 bytes is a header"),
                bcmp[BCMP_HEADER_LEN..].to_vec(),
                false,
            )
        }
        Err(e) => panic!("could not accept a frame bm_core transmitted on port {port}: {e}"),
    }
}

fn assert_same_frames(what: &str, input: &ConfigInput, c: &[(u8, Vec<u8>)], rs: &[(u8, Vec<u8>)]) {
    assert_eq!(
        c.len(),
        rs.len(),
        "{what}: frame count differs -- C sent {} on ports {:?}, bm-stack sent {} on ports {:?} ({input:?})",
        c.len(),
        c.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        rs.len(),
        rs.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
    );
    for (index, ((c_port, c_frame), (rs_port, rs_frame))) in c.iter().zip(rs).enumerate() {
        assert_eq!(
            c_port, rs_port,
            "{what}: frame {index} went to different ports ({input:?})"
        );
        if c_frame != rs_frame {
            let at = c_frame
                .iter()
                .zip(rs_frame)
                .position(|(a, b)| a != b)
                .unwrap_or(c_frame.len().min(rs_frame.len()));
            panic!(
                "{what}: frame {index} (port {c_port}) diverged at byte {at}\n  \
                 input: {input:?}\n  C:        {c_frame:02x?}\n  bm-stack: {rs_frame:02x?}",
            );
        }
    }
}

/// Assert a `bm-stack` node handles one config message exactly as bm_core's
/// stack does: the same frames on the same ports, and the same store left
/// behind.
///
/// # Panics
///
/// If the frame sequences differ, or the store images do.
pub fn check(input: &ConfigInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let frame = input.build();

    let _guard = oracle();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );

    let config = reset(&input.seeds);
    let mut node = stack::node_with_config(config);

    inject(input.ingress_port, &frame);
    let c = drain();

    let mut ours = frame.clone();
    let owed = node.on_frame(0, input.ingress_port, &mut ours);
    let forward = owed.forward;
    let mut rs = Vec::new();
    if let Some(relay) = owed.relay {
        rs.extend(capture(relay));
    }
    if let Some(reply) = owed.reply {
        rs.extend(capture(reply));
    }
    if let Some(reflood) = forward {
        rs.extend(capture_reflood_config(&mut node, reflood, &ours));
    }

    assert_same_frames("config", &input, &c, &rs);

    // The store both sides now hold, byte for byte.
    for p in Partition::ALL {
        let rust = node.config().store.partition(p);
        let c_ram = c_image(p);
        if let Some(at) = rust.image().iter().zip(&c_ram).position(|(a, b)| a != b) {
            panic!(
                "{p:?} RAM image diverged at byte {at} after {:?}\n  Rust {:02x?}\n  C    {:02x?}",
                input.message,
                &rust.image()[at..(at + 16).min(IMAGE_LEN)],
                &c_ram[at..(at + 16).min(IMAGE_LEN)],
            );
        }
    }

    // A response the port built must re-decode to what bm_core sent.
    for (port, captured) in &c {
        let (header, payload, _verified) = read_config_reply(*port, captured);
        if header.message_type == MessageType::CONFIG_VALUE {
            ConfigValue::decode(&payload)
                .unwrap_or_else(|e| panic!("could not decode a 0xA1 bm_core sent: {e}"));
        }
    }

    if !input.message.is_commit() {
        // Flash agrees unless a commit reset the shim's copy.
        for p in Partition::ALL {
            let rust = node.config().storage.bytes(p);
            let mut c_buf = [0u8; IMAGE_LEN];
            // SAFETY: `c_buf` is live and IMAGE_LEN long.
            let ok = unsafe {
                bm_wire_sys::bm_config_read(c_partition(p), 0, c_buf.as_mut_ptr(), IMAGE_LEN, 0)
            };
            assert!(ok);
            assert!(
                rust[..IMAGE_LEN] == c_buf,
                "{p:?} flash diverged after {:?}",
                input.message
            );
        }
    }
}

/// [`capture_reflood`] for a [`stack::ConfigNode`].
fn capture_reflood_config(
    node: &mut stack::ConfigNode,
    reflood: bm_stack::Reflood,
    frame: &[u8],
) -> Vec<(u8, Vec<u8>)> {
    let mut phy = stack::CapturePhy::default();
    embassy_futures::block_on(node.reflood(&mut phy, reflood, frame))
        .expect("CapturePhy cannot fail");
    phy.sent
}
