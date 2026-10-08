//! Publications to a Spotter: the bodies `spotter_log` and `spotter_tx_data`
//! (`integrations/spotter.c`) publish.
//!
//! `bm_print_publication_t` (`bm_common_messages/bm_common_pub_sub.h`),
//! written by [`encode_log`]:
//!
//! | Offset | Field | Size |
//! |---|---|---|
//! | 0 | `target_node_id` | 8, little-endian |
//! | 8 | `fname_len` | 2, little-endian |
//! | 10 | `data_len` | 2, little-endian; the text, not its NUL |
//! | 12 | `print_time` | 1 |
//! | 13 | file name | `fname_len` |
//! | 13 + `fname_len` | text, then a NUL | `data_len` + 1 |
//!
//! `BmSerialNetworkDataHeader` (`integrations/spotter.h`), written by
//! [`encode_tx_data`]: one [`NetworkType`] byte, then the data.
//!
//! | C behaviour | Here | Divergence |
//! |---|---|---|
//! | A file name is read to its first NUL, at most [`MAX_FILE_NAME_LEN`] bytes | [`encode_log`] does the same | — |
//! | A non-NULL empty file name publishes to [`FPRINTF_TOPIC`] with `fname_len` 0 | [`log_topic`] of `Some(b"")` is [`FPRINTF_TOPIC`] | — |
//! | `max_str_len` budgets the text against `max_payload_len` (1460), not the pub/sub message limit, so a long enough line passes it and `bm_pub` refuses it | [`encode_log`] checks what the C checks; `bm_stack::Node::spotter_log` then fails as `bm_pub` does | #81 |
//! | Every [`NetworkType`] other than [`NetworkType::CELLULAR_ONLY`] gets the Iridium limit, and is sent as given | [`NetworkType::max_len`] | — |

use crate::util::bm_strnlen;

/// `SPOTTER_PRINTF_TOPIC`: a line for the Spotter console.
pub const PRINTF_TOPIC: &[u8] = b"spotter/printf";

/// `SPOTTER_FPRINTF_TOPIC`: a line for a file on the Spotter's SD card.
pub const FPRINTF_TOPIC: &[u8] = b"spotter/fprintf";

/// `SPOTTER_TRANSMIT_DATA_TOPIC`: data for the Spotter to send over satellite
/// or cellular.
pub const TRANSMIT_DATA_TOPIC: &[u8] = b"spotter/transmit-data";

/// `ext_header.type` of all three: `PRINTF_TYPE`, `FPRINTF_TYPE` and
/// `SPOTTER_TRANSMIT_TOPIC_TYPE` are each 1. The version is
/// [`crate::pubsub::COMMON_VERSION`].
pub const KIND: u8 = 1;

/// `print_time` asking the Spotter to prefix the line with its time.
pub const USE_TIMESTAMP: u8 = 1;

/// `print_time` asking it not to.
pub const NO_TIMESTAMP: u8 = 0;

/// `sizeof(bm_print_publication_t)`.
pub const LOG_HEADER_LEN: usize = 13;

/// `max_file_name_len`. A file name this long or longer is refused.
pub const MAX_FILE_NAME_LEN: usize = 64;

/// `max_payload_len` (`bcmp/bcmp.h`), which `max_str_len` budgets against:
/// the IPv6 payload of a 1500-byte frame.
pub const LOG_BUDGET: usize = 1460;

/// The longest [`encode_log`] body: header, text and file name at
/// [`max_text_len`], and the NUL.
pub const MAX_LOG_LEN: usize = LOG_BUDGET + 1;

/// `max_str_len(fname_len)`: the longest text [`encode_log`] accepts with a
/// file name of `fname_len` bytes.
#[must_use]
pub const fn max_text_len(fname_len: usize) -> usize {
    LOG_BUDGET - LOG_HEADER_LEN - fname_len
}

/// `spotter_tx_max_iridium_payload_bytes`.
pub const MAX_IRIDIUM_LEN: usize = 311;

/// `spotter_tx_max_cellular_payload_bytes`.
pub const MAX_CELLULAR_LEN: usize = 1000;

/// `sizeof(BmSerialNetworkDataHeader)`.
pub const TX_HEADER_LEN: usize = 1;

/// The longest [`encode_tx_data`] body.
pub const MAX_TX_LEN: usize = TX_HEADER_LEN + MAX_CELLULAR_LEN;

/// `BmSerialNetworkType`: which network the Spotter sends data over. A `u8`,
/// as in the C, which sends any value it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetworkType(pub u8);

impl NetworkType {
    /// `BmNetworkTypeCellularIriFallback`: cellular, else Iridium.
    pub const CELLULAR_IRI_FALLBACK: Self = Self(1 << 0);
    /// `BmNetworkTypeCellularOnly`.
    pub const CELLULAR_ONLY: Self = Self(1 << 1);

    /// The most data `spotter_tx_data` accepts for this type:
    /// [`MAX_CELLULAR_LEN`] for [`Self::CELLULAR_ONLY`], else
    /// [`MAX_IRIDIUM_LEN`].
    #[must_use]
    pub const fn max_len(self) -> usize {
        if self.0 == Self::CELLULAR_ONLY.0 {
            MAX_CELLULAR_LEN
        } else {
            MAX_IRIDIUM_LEN
        }
    }
}

/// Why [`encode_log`] or [`encode_tx_data`] wrote nothing, with the `BmErr`
/// the C returns for the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EncodeError {
    /// The text is empty: `BmENODATA`.
    NoData,
    /// The file name, the text or the data is too long: `BmEMSGSIZE`.
    MessageSize,
    /// `buf` cannot hold the body. The C allocates instead.
    Truncated,
}

/// The topic `spotter_log` publishes to: [`FPRINTF_TOPIC`] if it is given a
/// file name, even an empty one, else [`PRINTF_TOPIC`].
#[must_use]
pub const fn log_topic(file_name: Option<&[u8]>) -> &'static [u8] {
    match file_name {
        Some(_) => FPRINTF_TOPIC,
        None => PRINTF_TOPIC,
    }
}

/// Write the body `spotter_log` publishes, returning its length.
///
/// `text` is what `vsnprintf` produced, all of it: the C counts a NUL a
/// format wrote (`%c` of 0) in `data_len`. `file_name` is read as
/// `bm_strnlen(file_name, MAX_FILE_NAME_LEN)` reads it, to its first NUL.
///
/// # Errors
///
/// In the C's order:
///
/// 1. [`EncodeError::NoData`] if `text` is empty;
/// 2. [`EncodeError::MessageSize`] if the file name is
///    [`MAX_FILE_NAME_LEN`] bytes or longer, or `text` is longer than
///    [`max_text_len`] of it;
/// 3. [`EncodeError::Truncated`] if `buf` cannot hold the body.
pub fn encode_log(
    buf: &mut [u8],
    target_node_id: u64,
    file_name: Option<&[u8]>,
    print_time: u8,
    text: &[u8],
) -> Result<usize, EncodeError> {
    if text.is_empty() {
        return Err(EncodeError::NoData);
    }
    let file_name = file_name.map_or(&[][..], |f| &f[..bm_strnlen(f, MAX_FILE_NAME_LEN)]);
    if file_name.len() >= MAX_FILE_NAME_LEN || text.len() > max_text_len(file_name.len()) {
        return Err(EncodeError::MessageSize);
    }
    let len = LOG_HEADER_LEN + file_name.len() + text.len() + 1;
    let out = buf.get_mut(..len).ok_or(EncodeError::Truncated)?;
    out[..8].copy_from_slice(&target_node_id.to_le_bytes());
    // Both fit: MAX_FILE_NAME_LEN and LOG_BUDGET are below u16::MAX.
    out[8..10].copy_from_slice(&(file_name.len() as u16).to_le_bytes());
    out[10..12].copy_from_slice(&(text.len() as u16).to_le_bytes());
    out[12] = print_time;
    let (name_out, rest) = out[LOG_HEADER_LEN..].split_at_mut(file_name.len());
    name_out.copy_from_slice(file_name);
    let (text_out, nul) = rest.split_at_mut(text.len());
    text_out.copy_from_slice(text);
    nul[0] = 0;
    Ok(len)
}

/// Write the body `spotter_tx_data` publishes, returning its length.
///
/// # Errors
///
/// - [`EncodeError::MessageSize`] if `data` is longer than
///   [`NetworkType::max_len`].
/// - [`EncodeError::Truncated`] if `buf` cannot hold the body.
pub fn encode_tx_data(
    buf: &mut [u8],
    network: NetworkType,
    data: &[u8],
) -> Result<usize, EncodeError> {
    if data.len() > network.max_len() {
        return Err(EncodeError::MessageSize);
    }
    let len = TX_HEADER_LEN + data.len();
    let out = buf.get_mut(..len).ok_or(EncodeError::Truncated)?;
    out[0] = network.0;
    out[TX_HEADER_LEN..].copy_from_slice(data);
    Ok(len)
}

#[cfg(test)]
mod tests;
