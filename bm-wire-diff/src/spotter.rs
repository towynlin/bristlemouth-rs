//! Differential comparator for `integrations/spotter.c`: `spotter_log` and
//! `spotter_tx_data` against [`Node::spotter_log_with`] and
//! [`Node::spotter_tx_data_with`].
//!
//! A stack target, for the reason [`crate::stack`] gives. Driven from
//! `tests/spotter.rs`.
//!
//! | C returns | Rust returns | Local delivery | Frames |
//! |---|---|---|---|
//! | `BmOK` | `Ok` | the publication, to both sides' `*` | the oracle's are [`as_bm_linux_sends_it`], the node's [`udp::build`] from [`udp::source_address`], both of [`pubsub::encode`] of [`spotter::encode_log`] or [`spotter::encode_tx_data`] |
//! | `BmENODATA` | [`SpotterError::NoData`] | none | none |
//! | `BmEMSGSIZE` | [`SpotterError::MessageSize`] | none | none |
//! | `BmENETDOWN` | [`SpotterError::NotSent`] | the publication | none |
//!
//! The oracle's `PUB_LIST` is seeded once per process with the three topics,
//! longest first, so that `bm_pub_wl`'s lookup of each reads no shorter entry
//! (divergence #38) and the list never grows.

use std::sync::Once;

use arbitrary::Arbitrary;

use bm_stack::{Node, SoftRtc, SpotterError};
use bm_wire::spotter::{self, NetworkType};
use bm_wire::util::BmIpAddr;
use bm_wire::{pubsub, udp};

use crate::l2_egress::port_transmit;
use crate::node_udp::{
    Published, as_published, expected_publication, subscribe_all, take_published,
};
use crate::stack::{self, Captured, capture, drain, oracle, pump_until_quiet};
use crate::udp::as_bm_linux_sends_it;

/// Longest file name a [`Log`] passes: past [`spotter::MAX_FILE_NAME_LEN`],
/// so the refusal is reachable.
pub const FILE_NAME_BYTES: usize = spotter::MAX_FILE_NAME_LEN + 16;

/// Longest text a [`Log`] passes: past [`spotter::max_text_len`]`(0)`.
pub const TEXT_BYTES: usize = spotter::max_text_len(0) + 16;

/// Most data a [`TxData`] passes: past [`spotter::MAX_CELLULAR_LEN`].
pub const DATA_BYTES: usize = spotter::MAX_CELLULAR_LEN + 16;

/// A `spotter_log` call, made as `spotter_log(target_node_id, file_name,
/// print_time, "%s", text)`.
///
/// `"%s"` stops at the first NUL, so the text both sides receive is `text`
/// then `padding` bytes of `-`, cut to [`TEXT_BYTES`] and then at its first
/// NUL. [`Node::spotter_log`] takes all of its text: a NUL a C format writes
/// (`%c` of 0) is counted in `data_len` by both.
///
/// The file name is cut to [`FILE_NAME_BYTES`] and passes to the C with a NUL
/// appended; both sides read it to its first NUL.
#[derive(Debug, Clone, Arbitrary)]
pub struct Log {
    /// Spotter to print, 0 for all.
    pub target_node_id: u64,
    /// The file, or `None` (NULL) for the console.
    pub file_name: Option<Vec<u8>>,
    /// Timestamp the line or not.
    pub print_time: u8,
    /// The start of the text.
    pub text: Vec<u8>,
    /// How many `-` follow it, so the fuzzer reaches the length limits.
    pub padding: u16,
}

impl Log {
    fn file_name(&self) -> Option<&[u8]> {
        self.file_name
            .as_deref()
            .map(|f| &f[..f.len().min(FILE_NAME_BYTES)])
    }

    fn text(&self) -> Vec<u8> {
        let mut text = self.text.clone();
        text.extend(std::iter::repeat_n(b'-', usize::from(self.padding)));
        text.truncate(TEXT_BYTES);
        if let Some(nul) = text.iter().position(|b| *b == 0) {
            text.truncate(nul);
        }
        text
    }
}

/// A `spotter_tx_data` call.
#[derive(Debug, Clone, Arbitrary)]
pub struct TxData {
    /// `BmSerialNetworkType`, any byte.
    pub network: u8,
    /// The start of the data.
    pub data: Vec<u8>,
    /// How many `0xA5` bytes follow it, cut to [`DATA_BYTES`] in all.
    pub padding: u16,
}

impl TxData {
    fn data(&self) -> Vec<u8> {
        let mut data = self.data.clone();
        data.extend(std::iter::repeat_n(0xA5, usize::from(self.padding)));
        data.truncate(DATA_BYTES);
        data
    }
}

/// One call.
#[derive(Debug, Clone, Arbitrary)]
pub enum Step {
    /// `spotter_log`.
    Log(Log),
    /// `spotter_tx_data`.
    TxData(TxData),
}

/// Calls made in order against the one oracle.
#[derive(Debug, Clone, Arbitrary)]
pub struct SpotterInput {
    /// The calls.
    pub steps: Vec<Step>,
}

/// Run every step.
///
/// # Panics
///
/// On any divergence; see [`check_log`] and [`check_tx_data`].
pub fn check(input: &SpotterInput) {
    for step in &input.steps {
        match step {
            Step::Log(log) => check_log(log),
            Step::TxData(tx) => check_tx_data(tx),
        }
    }
}

static SEEDED: Once = Once::new();

/// Add the three topics to the empty `PUB_LIST`, longest first.
fn seed_pub_list() {
    for topic in [
        spotter::TRANSMIT_DATA_TOPIC,
        spotter::FPRINTF_TOPIC,
        spotter::PRINTF_TOPIC,
    ] {
        unsafe {
            if topic == spotter::TRANSMIT_DATA_TOPIC {
                let mut count = 0u16;
                assert_eq!(
                    bm_wire_sys::bcmp_resource_discovery_get_num_resources(
                        &mut count,
                        bm_wire_sys::ResourceType_PUB,
                        0,
                    ),
                    bm_wire_sys::BmErr_BmOK
                );
                assert_eq!(count, 0, "PUB_LIST is not empty");
            }
            assert_eq!(
                bm_wire_sys::bcmp_resource_discovery_add_resource(
                    topic.as_ptr().cast(),
                    topic.len() as u16,
                    bm_wire_sys::ResourceType_PUB,
                    0,
                ),
                bm_wire_sys::BmErr_BmOK
            );
        }
    }
}

/// Run `call` against the oracle, returning its `BmErr`, the frames it sent
/// and what its `*` subscription received.
fn run_oracle(
    call: impl FnOnce() -> bm_wire_sys::BmErr,
) -> (bm_wire_sys::BmErr, Vec<Captured>, Vec<Published>) {
    let guard = oracle();
    subscribe_all(&guard);
    SEEDED.call_once(seed_pub_list);
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );
    let _ = take_published();
    let err = call();
    pump_until_quiet();
    (err, drain(), take_published())
}

/// The Rust error the C's `BmErr` must equal, or `None` for `BmOK`.
fn expected_error(err: bm_wire_sys::BmErr) -> Option<SpotterError> {
    match err {
        bm_wire_sys::BmErr_BmOK => None,
        bm_wire_sys::BmErr_BmENODATA => Some(SpotterError::NoData),
        bm_wire_sys::BmErr_BmEMSGSIZE => Some(SpotterError::MessageSize),
        bm_wire_sys::BmErr_BmENETDOWN => Some(SpotterError::NotSent),
        _ => panic!("spotter.c returned {err}"),
    }
}

/// Assert the two sides agree, as the module docs say. `body` is the Rust
/// encoding, `None` where it refused.
fn compare(
    what: &dyn std::fmt::Debug,
    topic: &[u8],
    body: Option<&[u8]>,
    c: (bm_wire_sys::BmErr, Vec<Captured>, Vec<Published>),
    rs: (Result<Vec<Captured>, SpotterError>, Vec<Published>),
) {
    let (err, c_frames, c_published) = c;
    let (rs_frames, rs_published) = rs;
    assert_eq!(rs_published, c_published, "local delivery ({what:?})");
    assert_eq!(
        rs_frames.as_ref().err().copied(),
        expected_error(err),
        "result ({what:?})"
    );
    let Some(body) = body else {
        assert!(c_published.is_empty(), "local delivery ({what:?})");
        assert!(c_frames.is_empty(), "frames ({what:?})");
        return;
    };
    let mut message = vec![0u8; pubsub::HEADER_LEN + topic.len() + body.len()];
    pubsub::encode(
        &mut message,
        topic,
        spotter::KIND,
        pubsub::COMMON_VERSION,
        body,
    )
    .expect("a valid topic, sized buffer");
    assert_eq!(
        c_published,
        vec![expected_publication(stack::NODE_ID, &message)],
        "local delivery ({what:?})"
    );
    let Ok(rs_frames) = rs_frames else {
        assert!(c_frames.is_empty(), "frames ({what:?})");
        return;
    };
    let dst = BmIpAddr::GLOBAL_MULTICAST;
    assert_eq!(
        c_frames,
        as_bm_linux_sends_it(pubsub::PORT, &dst, pubsub::PORT, &message),
        "oracle frames ({what:?})"
    );
    let mut built = vec![0u8; udp::PAYLOAD_OFFSET + message.len()];
    udp::build(
        &mut built,
        &udp::source_address(stack::NODE_ID, &dst),
        &dst,
        pubsub::PORT,
        pubsub::PORT,
        &message,
    )
    .expect("sized for the message");
    assert_eq!(rs_frames, port_transmit(&built), "node frames ({what:?})");
}

/// A node subscribed to `*`, as the oracle is.
fn node() -> Node<stack::OracleIdentity, SoftRtc, 4> {
    let mut node = stack::node();
    node.subscribe(b"*").expect("an empty table");
    node
}

/// Assert `spotter_log` and [`Node::spotter_log_with`] agree.
///
/// # Panics
///
/// On any divergence.
pub fn check_log(log: &Log) {
    let file_name = log.file_name();
    let text = log.text();

    let c_name: Option<Vec<u8>> = file_name.map(|f| [f, b"\0"].concat());
    let c_text = [&text[..], b"\0"].concat();
    let c = run_oracle(|| unsafe {
        bm_wire_sys::spotter_log(
            log.target_node_id,
            c_name
                .as_ref()
                .map_or(std::ptr::null(), |n| n.as_ptr().cast()),
            log.print_time,
            c"%s".as_ptr(),
            c_text.as_ptr().cast::<core::ffi::c_char>(),
        )
    });

    let mut node = node();
    let mut rs_published = Vec::new();
    let rs = node
        .spotter_log_with(log.target_node_id, file_name, log.print_time, &text, |e| {
            rs_published.extend(as_published(&e));
        })
        .map(capture);

    let mut body = vec![0u8; spotter::MAX_LOG_LEN];
    let len = spotter::encode_log(
        &mut body,
        log.target_node_id,
        file_name,
        log.print_time,
        &text,
    );
    compare(
        log,
        spotter::log_topic(file_name),
        len.ok().map(|len| &body[..len]),
        c,
        (rs, rs_published),
    );
}

/// Assert `spotter_tx_data` and [`Node::spotter_tx_data_with`] agree.
///
/// # Panics
///
/// On any divergence.
pub fn check_tx_data(tx: &TxData) {
    let data = tx.data();
    let network = NetworkType(tx.network);
    let c = run_oracle(|| unsafe {
        bm_wire_sys::spotter_tx_data(data.as_ptr().cast(), data.len() as u16, tx.network)
    });

    let mut node = node();
    let mut rs_published = Vec::new();
    let rs = node
        .spotter_tx_data_with(&data, network, |e| {
            rs_published.extend(as_published(&e));
        })
        .map(capture);

    let mut body = vec![0u8; spotter::MAX_TX_LEN];
    let len = spotter::encode_tx_data(&mut body, network, &data);
    compare(
        tx,
        spotter::TRANSMIT_DATA_TOPIC,
        len.ok().map(|len| &body[..len]),
        c,
        (rs, rs_published),
    );
}
