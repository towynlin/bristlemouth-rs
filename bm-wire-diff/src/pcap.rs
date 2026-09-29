//! Reader for the classic pcap files in `bm-wire-diff/testdata/`.
//!
//! Only what the committed captures use: microsecond timestamps,
//! little-endian headers, link type Ethernet. Anything else panics, since the
//! input is a checked-in fixture.

/// One captured frame.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    /// Capture time, in microseconds from the first record.
    pub t_us: u64,
    /// The frame from its Ethernet header on.
    pub frame: &'a [u8],
}

const MAGIC_LE_US: u32 = 0xA1B2_C3D4;
const LINKTYPE_ETHERNET: u32 = 1;
const FILE_HEADER_LEN: usize = 24;
const RECORD_HEADER_LEN: usize = 16;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Every record of a capture, in file order.
///
/// # Panics
///
/// On a file that is not little-endian microsecond Ethernet pcap, a truncated
/// record, or a record whose captured length is short of its original length.
#[must_use]
pub fn records(file: &[u8]) -> Vec<Record<'_>> {
    assert_eq!(u32_at(file, 0), MAGIC_LE_US, "not a little-endian pcap");
    assert_eq!(
        u32_at(file, 20),
        LINKTYPE_ETHERNET,
        "not an Ethernet capture"
    );

    let mut out = Vec::new();
    let mut at = FILE_HEADER_LEN;
    let mut t0 = None;
    while at < file.len() {
        let t = u64::from(u32_at(file, at)) * 1_000_000 + u64::from(u32_at(file, at + 4));
        let incl = u32_at(file, at + 8) as usize;
        let orig = u32_at(file, at + 12) as usize;
        assert_eq!(
            incl, orig,
            "record at byte {at} is truncated by the snap length"
        );
        at += RECORD_HEADER_LEN;
        let t0 = *t0.get_or_insert(t);
        out.push(Record {
            t_us: t - t0,
            frame: &file[at..at + incl],
        });
        at += incl;
    }
    out
}
