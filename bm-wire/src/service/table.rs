//! `middleware/bm_service.c`'s service list and request dispatch, and
//! `middleware/echo_service.c`'s handler.
//!
//! | C | Here |
//! |---|---|
//! | `BmServiceRequestDataHeader`, `BmServiceReplyDataHeader` | [`RequestHeader`], [`ReplyHeader`] |
//! | `"%016" PRIx64 "<suffix>"`, then `BM_SERVICE_REQ_STR` or `BM_SERVICE_REP_STR` | [`service_name`], [`topic`] |
//! | `BM_SERVICE_CONTEXT.service_list` | [`ServiceTable`] |
//! | `_service_list_add_service` | [`ServiceTable::add`]: appended, no de-duplication |
//! | `_service_list_remove_service` | [`ServiceTable::remove`]: the first entry the name prefixes |
//! | `_service_request_received_cb`'s walk | [`ServiceTable::lookup`] |
//! | `echo_service_handler` | [`echo`] |
//!
//! | C behaviour | Here | Divergence |
//! |---|---|---|
//! | The walk stops at the first service whose name `strncmp`-prefixes the topic, and a length mismatch there ends it: a service whose name prefixes another's request topic shadows it | [`ServiceTable::lookup`] does the same | #89 |
//! | `strncmp` reads the topic past its end, into the data and past that | [`Lookup::OverRead`] where it leaves the publication | #89 |
//! | The header is read before `data_len >= 8` is checked | [`Lookup::ShortRequest`] | #89 |
//! | `_service_list_remove_service` removes the first service its argument prefixes | [`ServiceTable::remove`] does the same | #89 |
//! | `echo_service_handler` copies a request of any length into a 1008-byte buffer | [`echo`] refuses one longer than the buffer | #90 |

use crate::BmWireError;
use crate::pubsub::TOPIC_MAX_LEN;

/// `BM_SERVICE_REQ_STR`, appended to a service's name for its request topic.
pub const REQUEST_SUFFIX: &[u8] = b"/req";

/// `BM_SERVICE_REP_STR`, appended to a service's name for its reply topic.
pub const REPLY_SUFFIX: &[u8] = b"/rep";

/// `MAX_BM_SERVICE_DATA_SIZE`: the reply buffer, header included.
pub const MAX_DATA_SIZE: usize = 1024;

/// What a handler may write: [`MAX_DATA_SIZE`] less the [`ReplyHeader`].
pub const REPLY_DATA_LEN: usize = MAX_DATA_SIZE - ReplyHeader::LEN;

/// `BM_SERVICE_MAX_SERVICE_STRLEN`: the longest name whose request topic
/// `bm_sub_wl` accepts is one less.
pub const MAX_SERVICE_LEN: usize = TOPIC_MAX_LEN - REQUEST_SUFFIX.len();

/// `BmServiceRequestDataHeader`: the first eight bytes of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestHeader {
    /// `id`, echoed in the reply.
    pub id: u32,
    /// `data_size`, the bytes after the header.
    pub data_size: u32,
}

impl RequestHeader {
    /// `sizeof(BmServiceRequestDataHeader)`.
    pub const LEN: usize = 8;

    /// Read the header from the front of `body`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `body` is shorter than [`Self::LEN`].
    pub fn decode(body: &[u8]) -> Result<Self, BmWireError> {
        let b = body.get(..Self::LEN).ok_or(BmWireError::Truncated)?;
        Ok(Self {
            id: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            data_size: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        })
    }

    /// Write the header to the front of `out`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `out` is shorter than [`Self::LEN`].
    pub fn encode(&self, out: &mut [u8]) -> Result<(), BmWireError> {
        let o = out.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        o[..4].copy_from_slice(&self.id.to_le_bytes());
        o[4..].copy_from_slice(&self.data_size.to_le_bytes());
        Ok(())
    }
}

/// `BmServiceReplyDataHeader`: the first sixteen bytes of a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyHeader {
    /// `target_node_id`: the node the request came from.
    pub target_node_id: u64,
    /// `id`: the request's.
    pub id: u32,
    /// `data_size`, the bytes after the header.
    pub data_size: u32,
}

impl ReplyHeader {
    /// `sizeof(BmServiceReplyDataHeader)`.
    pub const LEN: usize = 16;

    /// Read the header from the front of `body`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `body` is shorter than [`Self::LEN`].
    pub fn decode(body: &[u8]) -> Result<Self, BmWireError> {
        let b = body.get(..Self::LEN).ok_or(BmWireError::Truncated)?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&b[..8]);
        Ok(Self {
            target_node_id: u64::from_le_bytes(id),
            id: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            data_size: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        })
    }

    /// Write the header to the front of `out`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `out` is shorter than [`Self::LEN`].
    pub fn encode(&self, out: &mut [u8]) -> Result<(), BmWireError> {
        let o = out.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        o[..8].copy_from_slice(&self.target_node_id.to_le_bytes());
        o[8..12].copy_from_slice(&self.id.to_le_bytes());
        o[12..].copy_from_slice(&self.data_size.to_le_bytes());
        Ok(())
    }
}

/// A built-in service's name, `"%016" PRIx64 "%s"` of `node_id` and
/// `suffix` (`"/echo"`, `"/sys_info"`, ...), into `out`. Returns its length.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `out` cannot hold it. `echo_service_init`'s
/// `snprintf` truncates instead, at [`MAX_SERVICE_LEN`] − 1 bytes, which no
/// suffix bm_core uses reaches.
pub fn service_name(out: &mut [u8], node_id: u64, suffix: &[u8]) -> Result<usize, BmWireError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let len = 16 + suffix.len();
    let o = out.get_mut(..len).ok_or(BmWireError::Truncated)?;
    for (i, byte) in o[..16].iter_mut().enumerate() {
        *byte = HEX[((node_id >> (60 - 4 * i)) & 0xf) as usize];
    }
    o[16..].copy_from_slice(suffix);
    Ok(len)
}

/// `service` followed by `suffix` ([`REQUEST_SUFFIX`] or [`REPLY_SUFFIX`]),
/// into `out`, as `_service_sub_unsub_to_req_topic` and the reply's
/// `pub_topic` are built. Returns its length.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `out` cannot hold it.
pub fn topic(out: &mut [u8], service: &[u8], suffix: &[u8]) -> Result<usize, BmWireError> {
    let len = service.len() + suffix.len();
    let o = out.get_mut(..len).ok_or(BmWireError::Truncated)?;
    o[..service.len()].copy_from_slice(service);
    o[service.len()..].copy_from_slice(suffix);
    Ok(len)
}

/// `echo_service_handler`: the request's data, copied into `reply`. Returns
/// the reply's length.
///
/// Returns `None` for a request longer than `reply`. The C's check is
/// `*buffer_len <= MAX_BM_SERVICE_DATA_SIZE`, which is always true, and it
/// copies the request into its 1008 bytes whatever its length (divergence
/// #90).
#[must_use]
pub fn echo(request: &[u8], reply: &mut [u8]) -> Option<usize> {
    reply.get_mut(..request.len())?.copy_from_slice(request);
    Some(request.len())
}

/// `strncmp(a, b, n) == 0`, for `a` at least `n` bytes and `b` read through
/// `b_at`, which returns `None` past what may be read.
///
/// `Err(())` where `strncmp` would read a byte of `b` that `b_at` withholds.
fn strncmp_eq(a: &[u8], n: usize, b_at: impl Fn(usize) -> Option<u8>) -> Result<bool, ()> {
    for (i, &x) in a.iter().enumerate().take(n) {
        let y = b_at(i).ok_or(())?;
        if x != y {
            return Ok(false);
        }
        if x == 0 {
            return Ok(true);
        }
    }
    Ok(true)
}

/// What `_service_request_received_cb` makes of one publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup<'a, H> {
    /// No service's name `strncmp`-matches the topic: nothing is called.
    NoService,
    /// `strncmp` against service `index` would read past the publication's
    /// data: undefined in the C (divergence #89).
    OverRead {
        /// The service being compared.
        index: usize,
    },
    /// Service `index` matched, and the body is shorter than
    /// [`RequestHeader::LEN`]. The C reads `data_size` past the body before
    /// the length check, which on a 64-bit host then always fails
    /// (divergence #89). Nothing is called.
    ShortRequest {
        /// The service that matched.
        index: usize,
    },
    /// Service `index` matched, and the body is not `8 + data_size` bytes.
    /// The walk ends; nothing is called.
    LengthMismatch {
        /// The service that matched.
        index: usize,
    },
    /// Service `index` matched, and the topic is not its name and
    /// [`REQUEST_SUFFIX`] in length. The walk ends; nothing is called.
    TopicMismatch {
        /// The service that matched.
        index: usize,
    },
    /// Call service `index`'s handler.
    Call {
        /// The service.
        index: usize,
        /// Its name, the handler's `service`.
        name: &'a [u8],
        /// Its handler.
        handler: H,
        /// The request's header.
        header: RequestHeader,
        /// The request's data, `header.data_size` bytes.
        data: &'a [u8],
    },
}

#[derive(Debug, Clone, Copy)]
struct Service<H, const NAME: usize> {
    name: [u8; NAME],
    len: usize,
    handler: H,
}

/// Why [`ServiceTable::add`] refused: `N` services are listed, or the name is
/// longer than `NAME`. Ceilings bm_core does not have; its nearest is a
/// `bm_malloc` failure, after which `bm_service_register` returns false
/// having listed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableFull;

/// `BM_SERVICE_CONTEXT.service_list`: services by name, in registration
/// order, each with a handler `H`.
///
/// The C stores the caller's `const char *` and reads it with `strncmp`, so
/// it treats a name as ending at its first NUL as well as at its length. A
/// name is copied here, and read as NUL past its end, as a NUL-terminated
/// caller string reads.
///
/// `N` services of up to `NAME` bytes each; bm_core has neither ceiling.
#[derive(Debug, Clone)]
pub struct ServiceTable<H, const N: usize, const NAME: usize> {
    services: [Option<Service<H, NAME>>; N],
    len: usize,
}

impl<H: Copy, const N: usize, const NAME: usize> Default for ServiceTable<H, N, NAME> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Copy, const N: usize, const NAME: usize> ServiceTable<H, N, NAME> {
    /// No services.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            services: [None; N],
            len: 0,
        }
    }

    /// How many services are listed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no service is listed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The services' names and handlers, in list order.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], H)> + '_ {
        self.services[..self.len]
            .iter()
            .flatten()
            .map(|s| (&s.name[..s.len], s.handler))
    }

    /// Append `name`: `_service_create_list_elem` and
    /// `_service_list_add_service`. A name already listed is listed again.
    ///
    /// # Errors
    ///
    /// [`TableFull`]; nothing changes.
    pub fn add(&mut self, name: &[u8], handler: H) -> Result<(), TableFull> {
        if self.len == N || name.len() > NAME {
            return Err(TableFull);
        }
        let mut held = [0u8; NAME];
        held[..name.len()].copy_from_slice(name);
        self.services[self.len] = Some(Service {
            name: held,
            len: name.len(),
            handler,
        });
        self.len += 1;
        Ok(())
    }

    /// `_service_list_remove_service`: remove the first service whose name
    /// `name` prefixes, `strncmp(service, name, name.len()) == 0`. Returns
    /// whether one was removed.
    ///
    /// So removing `a` removes `ab` if it comes first, and removing the empty
    /// name removes the first service (divergence #89).
    pub fn remove(&mut self, name: &[u8]) -> bool {
        let found = self.services[..self.len].iter().flatten().position(|s| {
            // A held name reads as NUL past its end, so this never fails.
            strncmp_eq(name, name.len(), |i| {
                Some(s.name.get(i).copied().unwrap_or(0))
            })
            .unwrap_or(false)
        });
        let Some(index) = found else {
            return false;
        };
        self.services[index..self.len].rotate_left(1);
        self.len -= 1;
        self.services[self.len] = None;
        true
    }

    /// `_service_request_received_cb`'s walk, for a publication on `topic`
    /// carrying `data`.
    ///
    /// Each service's name is compared with `strncmp` over its own length,
    /// against the topic and, past the topic, the data that follows it in
    /// the datagram. The first that matches decides: its checks failing end
    /// the walk.
    #[must_use]
    pub fn lookup<'a>(&'a self, topic: &'a [u8], data: &'a [u8]) -> Lookup<'a, H> {
        let hay = |i: usize| {
            topic
                .get(i)
                .or_else(|| data.get(i.wrapping_sub(topic.len())))
                .copied()
        };
        for (index, service) in self.services[..self.len].iter().flatten().enumerate() {
            let name = &service.name[..service.len];
            match strncmp_eq(name, name.len(), hay) {
                Err(()) => return Lookup::OverRead { index },
                Ok(false) => continue,
                Ok(true) => {}
            }
            let Ok(header) = RequestHeader::decode(data) else {
                return Lookup::ShortRequest { index };
            };
            if data.len() as u64 != RequestHeader::LEN as u64 + u64::from(header.data_size) {
                return Lookup::LengthMismatch { index };
            }
            if topic.len() != name.len() + REQUEST_SUFFIX.len() {
                return Lookup::TopicMismatch { index };
            }
            return Lookup::Call {
                index,
                name,
                handler: service.handler,
                header,
                data: &data[RequestHeader::LEN..],
            };
        }
        Lookup::NoService
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u32, data: &[u8]) -> ([u8; 64], usize) {
        let mut out = [0u8; 64];
        RequestHeader {
            id,
            data_size: data.len() as u32,
        }
        .encode(&mut out)
        .unwrap();
        out[8..8 + data.len()].copy_from_slice(data);
        (out, 8 + data.len())
    }

    #[test]
    fn headers_are_packed_little_endian() {
        let mut out = [0u8; 16];
        RequestHeader {
            id: 0x0403_0201,
            data_size: 0x0807_0605,
        }
        .encode(&mut out)
        .unwrap();
        assert_eq!(out[..8], [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(RequestHeader::decode(&out).unwrap().data_size, 0x0807_0605);
        let reply = ReplyHeader {
            target_node_id: 0x1122_3344_5566_7788,
            id: 9,
            data_size: 3,
        };
        reply.encode(&mut out).unwrap();
        assert_eq!(
            out,
            [
                0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 9, 0, 0, 0, 3, 0, 0, 0
            ]
        );
        assert_eq!(ReplyHeader::decode(&out), Ok(reply));
        assert_eq!(
            RequestHeader::decode(&out[..7]),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn names_and_topics_are_the_cs() {
        let mut name = [0u8; 32];
        let len = service_name(&mut name, 0xC0FF_EE00_1234_5678, b"/echo").unwrap();
        assert_eq!(&name[..len], b"c0ffee0012345678/echo");
        let mut t = [0u8; 32];
        let len = topic(&mut t, b"c0ffee0012345678/echo", REQUEST_SUFFIX).unwrap();
        assert_eq!(&t[..len], b"c0ffee0012345678/echo/req");
        assert_eq!(
            service_name(&mut [0u8; 20], 1, b"/echo"),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn a_request_reaches_its_service() {
        let mut table: ServiceTable<u8, 4, 16> = ServiceTable::new();
        table.add(b"svc", 7).unwrap();
        let (body, len) = request(5, b"hi");
        assert_eq!(
            table.lookup(b"svc/req", &body[..len]),
            Lookup::Call {
                index: 0,
                name: b"svc",
                handler: 7,
                header: RequestHeader {
                    id: 5,
                    data_size: 2
                },
                data: b"hi",
            }
        );
        assert_eq!(table.lookup(b"other/req", &body[..len]), Lookup::NoService);
    }

    /// Divergence #89: the first name that prefixes the topic decides.
    #[test]
    fn a_prefixing_name_shadows_a_later_service() {
        let mut table: ServiceTable<u8, 4, 16> = ServiceTable::new();
        table.add(b"a", 1).unwrap();
        table.add(b"ab", 2).unwrap();
        let (body, len) = request(5, b"");
        assert_eq!(
            table.lookup(b"ab/req", &body[..len]),
            Lookup::TopicMismatch { index: 0 }
        );
        assert!(matches!(
            table.lookup(b"a/req", &body[..len]),
            Lookup::Call { handler: 1, .. }
        ));
        assert_eq!(
            table.lookup(b"a/req", &body[..len - 1]),
            Lookup::ShortRequest { index: 0 }
        );
        let (mut body, len) = request(5, b"xy");
        body[4] = 1;
        assert_eq!(
            table.lookup(b"a/req", &body[..len]),
            Lookup::LengthMismatch { index: 0 },
            "the length check comes first"
        );
        assert_eq!(
            table.lookup(b"ab", &body[..len]),
            Lookup::LengthMismatch { index: 0 }
        );
    }

    /// Divergence #89: `strncmp` reads past the topic into the data, and
    /// stops at a NUL both sides share.
    #[test]
    fn the_name_is_compared_into_the_data() {
        let mut table: ServiceTable<u8, 4, 16> = ServiceTable::new();
        table.add(b"abcdefgh", 1).unwrap();
        table.add(b"ab", 2).unwrap();
        assert_eq!(
            table.lookup(b"ab", b"cdefgh"),
            Lookup::ShortRequest { index: 0 }
        );
        assert_eq!(table.lookup(b"ab", b"cd"), Lookup::OverRead { index: 0 });
        assert_eq!(table.lookup(b"ab", b"x"), Lookup::ShortRequest { index: 1 });
        let mut nul: ServiceTable<u8, 4, 16> = ServiceTable::new();
        nul.add(b"a\0zzz", 1).unwrap();
        assert_eq!(nul.lookup(b"a", b"\0"), Lookup::ShortRequest { index: 0 });
        assert_eq!(nul.lookup(b"a", b"x"), Lookup::NoService);
    }

    /// Divergence #89: removal is by prefix, first match.
    #[test]
    fn removal_takes_the_first_service_the_name_prefixes() {
        let mut table: ServiceTable<u8, 4, 16> = ServiceTable::new();
        table.add(b"ab", 1).unwrap();
        table.add(b"a", 2).unwrap();
        table.add(b"a", 3).unwrap();
        assert!(!table.remove(b"abc"), "a longer name reads the NUL");
        assert!(table.remove(b"a"));
        assert!(table.iter().eq([(&b"a"[..], 2), (&b"a"[..], 3)]));
        assert!(table.remove(b""));
        assert!(table.iter().eq([(&b"a"[..], 3)]));
        assert!(!table.remove(b"b"));
    }

    #[test]
    fn the_table_refuses_past_its_ceilings() {
        let mut table: ServiceTable<u8, 1, 2> = ServiceTable::new();
        assert_eq!(table.add(b"abc", 1), Err(TableFull));
        table.add(b"ab", 1).unwrap();
        assert_eq!(table.add(b"a", 1), Err(TableFull));
    }

    /// Divergence #90: the C copies past its buffer.
    #[test]
    fn echo_copies_and_refuses_what_does_not_fit() {
        let mut reply = [0u8; 4];
        assert_eq!(echo(b"abc", &mut reply), Some(3));
        assert_eq!(&reply[..3], b"abc");
        assert_eq!(echo(b"abcde", &mut reply), None);
        assert_eq!(echo(b"", &mut reply), Some(0));
    }
}
