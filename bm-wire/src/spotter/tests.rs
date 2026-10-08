use super::*;

// Bodies from `bm-wire-diff/testdata/hello-pub.pcap`, frames numbered
// from 0: what follows the pub/sub header and topic in the dev kit's first
// round of publications.

/// Frame 84: `spotter_log(0, "hello.log", USE_TIMESTAMP, ...)`.
const BODY_84: &[u8] =
    b"\0\0\0\0\0\0\0\0\x09\x00\x24\x00\x01hello.logspotter_log for card H0, counter=100\0";

/// Frame 85: `spotter_log_console(0, ...)`.
const BODY_85: &[u8] =
    b"\0\0\0\0\0\0\0\0\x00\x00\x2c\x00\x01spotter_log_console for card H0, counter=100\0";

/// Frame 86: `spotter_tx_data(&counter, 4, BmNetworkTypeCellularOnly)`.
const BODY_86: &[u8] = b"\x02\x64\x00\x00\x00";

#[test]
fn the_captured_bodies() {
    let mut buf = [0u8; MAX_LOG_LEN];
    let len = encode_log(
        &mut buf,
        0,
        Some(b"hello.log"),
        USE_TIMESTAMP,
        b"spotter_log for card H0, counter=100",
    )
    .unwrap();
    assert_eq!(&buf[..len], BODY_84);
    let len = encode_log(
        &mut buf,
        0,
        None,
        USE_TIMESTAMP,
        b"spotter_log_console for card H0, counter=100",
    )
    .unwrap();
    assert_eq!(&buf[..len], BODY_85);
    let len = encode_tx_data(&mut buf, NetworkType::CELLULAR_ONLY, &100u32.to_le_bytes()).unwrap();
    assert_eq!(&buf[..len], BODY_86);
}

#[test]
fn topics() {
    assert_eq!(log_topic(Some(b"hello.log")), FPRINTF_TOPIC);
    assert_eq!(log_topic(Some(b"")), FPRINTF_TOPIC);
    assert_eq!(log_topic(None), PRINTF_TOPIC);
}

/// `spotter_test.cpp`, `Spotter.printf`: the refusals it asserts.
#[test]
fn log_refusals() {
    let mut buf = [0u8; MAX_LOG_LEN];
    let name = b"hello_world.txt";
    let t = USE_TIMESTAMP;
    assert!(encode_log(&mut buf, 1, Some(name), t, b"testing 1:testing 2").is_ok());
    assert!(encode_log(&mut buf, 1, None, t, b"testing 1:testing 2").is_ok());
    assert_eq!(
        encode_log(&mut buf, 1, Some(name), t, b""),
        Err(EncodeError::NoData)
    );
    assert_eq!(
        encode_log(&mut buf, 1, Some(&[b'a'; 254]), t, b"x"),
        Err(EncodeError::MessageSize)
    );
    assert_eq!(
        encode_log(&mut buf, 1, Some(name), t, &[b'a'; LOG_BUDGET - 1]),
        Err(EncodeError::MessageSize)
    );
}

#[test]
fn log_limits() {
    let mut buf = [0u8; MAX_LOG_LEN];
    let text = [b'a'; LOG_BUDGET];
    assert_eq!(max_text_len(0), 1447);
    assert_eq!(
        encode_log(&mut buf, 0, None, 0, &text[..1447]),
        Ok(MAX_LOG_LEN)
    );
    assert_eq!(
        encode_log(&mut buf, 0, None, 0, &text[..1448]),
        Err(EncodeError::MessageSize)
    );
    let name = [b'n'; MAX_FILE_NAME_LEN];
    assert_eq!(
        encode_log(&mut buf, 0, Some(&name[..63]), 0, &text[..1384]),
        Ok(MAX_LOG_LEN)
    );
    assert_eq!(
        encode_log(&mut buf, 0, Some(&name[..63]), 0, &text[..1385]),
        Err(EncodeError::MessageSize)
    );
    assert_eq!(
        encode_log(&mut buf, 0, Some(&name), 0, b"x"),
        Err(EncodeError::MessageSize)
    );
    assert_eq!(encode_log(&mut buf[..15], 0, None, 0, b"x"), Ok(15));
    assert_eq!(
        encode_log(&mut buf[..14], 0, None, 0, b"x"),
        Err(EncodeError::Truncated)
    );
}

/// The empty-text check comes first, as in the C.
#[test]
fn no_data_before_message_size() {
    let mut buf = [0u8; MAX_LOG_LEN];
    assert_eq!(
        encode_log(&mut buf, 0, Some(&[b'n'; 100]), 0, b""),
        Err(EncodeError::NoData)
    );
}

/// `bm_strnlen`: a file name stops at its first NUL, so a long buffer holding
/// a short name is accepted.
#[test]
fn a_file_name_stops_at_nul() {
    let mut buf = [0u8; MAX_LOG_LEN];
    let mut name = [b'z'; 100];
    name[3] = 0;
    let len = encode_log(&mut buf, 7, Some(&name), 0, b"hi").unwrap();
    assert_eq!(
        &buf[..len],
        b"\x07\0\0\0\0\0\0\0\x03\x00\x02\x00\x00zzzhi\0"
    );
}

/// `spotter_test.cpp`, `Spotter.tx_data` and
/// `tx_data_network_type_is_one_byte`.
#[test]
fn tx_data() {
    let mut buf = [0u8; MAX_TX_LEN];
    let data = [0xA5; MAX_CELLULAR_LEN + 1];
    let iri = NetworkType::CELLULAR_IRI_FALLBACK;
    let cell = NetworkType::CELLULAR_ONLY;
    assert_eq!(encode_tx_data(&mut buf, iri, &data[..311]), Ok(312));
    assert_eq!(encode_tx_data(&mut buf, cell, &data[..1000]), Ok(1001));
    assert_eq!(encode_tx_data(&mut buf, iri, &[]), Ok(1));
    assert_eq!(encode_tx_data(&mut buf, cell, &[]), Ok(1));
    assert_eq!(
        encode_tx_data(&mut buf, iri, &data[..312]),
        Err(EncodeError::MessageSize)
    );
    assert_eq!(
        encode_tx_data(&mut buf, cell, &data[..1001]),
        Err(EncodeError::MessageSize)
    );
    let len = encode_tx_data(&mut buf, cell, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
    assert_eq!(&buf[..len], &[2, 0xDE, 0xAD, 0xBE, 0xEF]);
}

/// Any type but `CELLULAR_ONLY` gets the Iridium limit, and is sent as given.
#[test]
fn other_network_types() {
    let mut buf = [0u8; MAX_TX_LEN];
    for network in [0, 3, 0xFF].map(NetworkType) {
        assert_eq!(network.max_len(), MAX_IRIDIUM_LEN);
        assert_eq!(encode_tx_data(&mut buf, network, b"x"), Ok(2));
        assert_eq!(buf[0], network.0);
    }
}
