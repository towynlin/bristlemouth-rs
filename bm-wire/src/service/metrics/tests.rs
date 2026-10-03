use super::*;

fn port_stats(num_ports: u8, sqi: u8, mse: u16) -> [Entry<'static>; 3] {
    [
        Entry {
            key: "num_ports",
            field: Field::U8(num_ports),
        },
        Entry {
            key: "sqi_1",
            field: Field::U8(sqi),
        },
        Entry {
            key: "mse_1",
            field: Field::U16(mse),
        },
    ]
}

/// `MetricsReplyMsg.EncodesAndDecodesEnvelope`.
#[test]
fn the_gtest_envelope_round_trips() {
    let fields = port_stats(1, 5, 1234);
    let reply = Reply {
        version: VERSION,
        node_id: 0x0123_4567_89ab_cdef,
        uptime_ms: 42000,
    };
    let comp = [Component {
        key: "network_port_stats",
        fields: &fields,
    }];
    let mut buf = [0u8; 256];
    let len = encode(&reply, &comp, &mut buf).unwrap();
    assert!(len > 0);

    let mut got = Reply::default();
    let mut dec = port_stats(0, 0, 0);
    let mut out = [ComponentMut {
        key: "network_port_stats",
        fields: &mut dec,
    }];
    assert_eq!(decode(&buf[..len], &mut got, &mut out), Ok(()));
    assert_eq!(got.version, VERSION);
    assert_eq!(got.node_id, 0x0123_4567_89ab_cdef);
    assert_eq!(got.uptime_ms, 42000);
    assert_eq!(dec, port_stats(1, 5, 1234));
}

/// `MetricsReplyMsg.DecodeSkipsAbsentComponent`.
#[test]
fn an_absent_component_is_untouched() {
    let reply = Reply {
        version: VERSION,
        node_id: 1,
        uptime_ms: 0,
    };
    let mut buf = [0u8; 64];
    let len = encode(&reply, &[], &mut buf).unwrap();

    let mut dec = [Entry {
        key: "num_ports",
        field: Field::U8(0xaa),
    }];
    let mut out = [ComponentMut {
        key: "network_port_stats",
        fields: &mut dec,
    }];
    assert_eq!(decode(&buf[..len], &mut Reply::default(), &mut out), Ok(()));
    assert_eq!(dec[0].field, Field::U8(0xaa));
}

#[test]
fn the_empty_reply_is_these_bytes() {
    let reply = Reply {
        version: 1,
        node_id: 2,
        uptime_ms: 3,
    };
    let mut buf = [0u8; 64];
    let len = encode(&reply, &[], &mut buf).unwrap();
    #[rustfmt::skip]
    let want = [
        0xa4,
        0x67, b'v', b'e', b'r', b's', b'i', b'o', b'n', 0x01,
        0x67, b'n', b'o', b'd', b'e', b'_', b'i', b'd', 0x02,
        0x69, b'u', b'p', b't', b'i', b'm', b'e', b'_', b'm', b's', 0x03,
        0x64, b'd', b'a', b't', b'a', 0xa0,
    ];
    assert_eq!(&buf[..len], &want[..]);
    assert_eq!(
        encode(&reply, &[], &mut buf[..len - 1]),
        Err(CborError::OutOfMemory)
    );
}

#[test]
fn a_string_field_fails_the_encode() {
    let s = Entry {
        key: "s",
        field: Field::String,
    };
    let u = Entry {
        key: "u",
        field: Field::U8(1),
    };
    let mut buf = [0u8; 64];
    for (fields, err) in [
        (&[u, s][..], CborError::UnsupportedType),
        (&[s, u][..], CborError::TooFewItems),
        (&[s, s][..], CborError::UnsupportedType),
    ] {
        let comp = [Component { key: "c", fields }];
        assert_eq!(encode(&Reply::default(), &comp, &mut buf), Err(err));
    }
}

#[test]
fn fields_match_up_to_a_nul_and_mismatches_win() {
    // {"a\0x": 7, "b": 1.0 as fa, "zz": 0}
    let map = [
        0xa3, 0x63, b'a', 0, b'x', 0x07, 0x61, b'b', 0xfa, 0x3f, 0x80, 0, 0, 0x62, b'z', b'z', 0x00,
    ];
    let mut entries = [
        Entry {
            key: "a",
            field: Field::U32(0),
        },
        Entry {
            key: "b",
            field: Field::Double(0.0),
        },
    ];
    let mut it = Value::parse(&map).unwrap().enter_container().unwrap();
    assert_eq!(
        decode_fields(&mut it, &mut entries),
        Err(CborError::ImproperValue)
    );
    assert_eq!(entries[0].field, Field::U32(7));
    assert_eq!(entries[1].field, Field::Double(0.0));
}

/// Divergence #87: the tag is stepped over alone, its text read as a key,
/// and the advance over that key's value runs past the map.
#[test]
fn a_tagged_field_value_runs_past_the_component() {
    // {"k": 1("s")}
    let map = [0xa1, 0x61, b'k', 0xc1, 0x61, b's'];
    let mut it = Value::parse(&map).unwrap().enter_container().unwrap();
    assert_eq!(decode_fields(&mut it, &mut []), Err(CborError::Unreachable));
}

#[test]
fn the_handler_sends_nothing_past_its_buffer() {
    let mut out = [0u8; crate::service::REPLY_DATA_LEN];
    let fields = [Entry {
        key: "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijk",
        field: Field::U64(u64::MAX),
    }; 32];
    let fits = |n: usize, out: &mut [u8]| {
        let comp = [Component {
            key: "c",
            fields: &fields[..n],
        }];
        handle(0x0123_4567_89ab_cdef, u32::MAX, &comp, out)
    };
    let last = (0..=fields.len())
        .take_while(|n| fits(*n, &mut out).is_some())
        .last()
        .unwrap();
    assert!(last < fields.len(), "32 fields overflow 1008 bytes");
    let len = fits(last, &mut out).unwrap();
    assert!(len <= out.len() && len + 73 > out.len(), "{len}");
    assert_eq!(fits(last + 1, &mut out), None);

    let len = handle(2, 3, &[], &mut out).unwrap();
    assert_eq!(
        out[..len][..10],
        [
            0xa4, 0x67, b'v', b'e', b'r', b's', b'i', b'o', b'n', VERSION
        ]
    );
}
