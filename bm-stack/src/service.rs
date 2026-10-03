//! Services: `middleware/bm_service.c` on a node.
//!
//! The list and its dispatch are [`bm_wire::service::ServiceTable`];
//! [`crate::Node`] holds one and runs it from [`crate::Node::on_frame`].
//! Application handlers come through [`Services`], the `S` of `Node`.
//!
//! | C | Here |
//! |---|---|
//! | `bm_service_register` | [`crate::Node::register_service`] |
//! | `bm_service_unregister` | [`crate::Node::unregister_service`] |
//! | `echo_service_init` | [`crate::Node::register_echo_service`] |
//! | `_service_request_received_cb` | [`crate::Node::on_frame`], for each service callback a publication reaches |
//! | a `BmServiceHandler` | [`Services::handle`], or [`ServiceHandler::Echo`] |
//! | `bm_service_request` | [`crate::Node::service_request`] |
//! | a `BmServiceReplyCb` | [`crate::Event::ServiceReply`], [`crate::Event::ServiceTimeout`] |
//! | `_service_request_timer_expiry_cb` | [`crate::Node::on_service_expiry`] |

use bm_wire::pubsub::SubscriptionError;
use bm_wire::service::Requests;

use crate::node::SubscribeError;

/// How many services a node lists: `BM_SERVICE_CONTEXT.service_list`, which
/// in bm_core is unbounded. A dev kit's C firmware registers four (metrics,
/// echo, sys_info, config_map).
pub const SERVICES: usize = 16;

/// The longest service name a node lists, a ceiling bm_core does not have.
/// bm_core's own names are 16 hex digits and a suffix of at most 11 bytes.
pub const SERVICE_NAME_BYTES: usize = 48;

/// How many service requests a node waits on at once:
/// `CTX.service_request_list`, which in bm_core is unbounded.
pub const SERVICE_REQUESTS: usize = 8;

/// The requests a node waits on, each naming a service of up to
/// [`SERVICE_NAME_BYTES`].
pub type ServiceRequests = Requests<SERVICE_REQUESTS, SERVICE_NAME_BYTES>;

/// The application's service handlers.
///
/// One method serves every service [`crate::Node::register_service`] lists;
/// `service` says which.
pub trait Services {
    /// `BmServiceHandler`: answer `request` to `service` by writing up to
    /// `reply.len()` bytes into `reply`, and return how many. `None` sends no
    /// reply, as a handler returning false.
    ///
    /// `reply` is [`bm_wire::service::REPLY_DATA_LEN`] bytes, zeroed, as the
    /// C's buffer is. A length past it sends no reply.
    ///
    /// Called once per publication, however many of the node's subscriptions
    /// it reaches (see [`crate::Node::on_frame_with`]).
    fn handle(&mut self, service: &[u8], request: &[u8], reply: &mut [u8]) -> Option<usize> {
        let _ = (service, request, reply);
        None
    }
}

/// No application services: anything registered with
/// [`crate::Node::register_service`] goes unanswered.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoServices;

impl Services for NoServices {}

/// Which handler a listed service has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceHandler {
    /// `echo_service_handler`: [`bm_wire::service::echo`].
    Echo,
    /// The application's, [`Services::handle`].
    Application,
}

/// Why [`crate::Node::register_service`] returned what `bm_service_register`
/// returns false for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    /// [`SERVICES`] are listed, or the name is longer than
    /// [`SERVICE_NAME_BYTES`]. Nothing changed: the C's equivalent is a
    /// `bm_malloc` failure.
    Full,
    /// Listed, but `<name>/req` was not subscribed, or was not advertised.
    /// The service stays listed, as in the C.
    Subscribe(SubscribeError),
}

/// Why [`crate::Node::unregister_service`] returned what
/// `bm_service_unregister` returns false for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnregisterError {
    /// `<name>/req` has no service callback to remove, as
    /// [`bm_wire::pubsub::Subscriptions::unsubscribe_as`] reports. Nothing
    /// changed.
    Unsubscribe(SubscriptionError),
    /// `<name>/req` was unsubscribed, and no listed service starts with
    /// `name` (divergence #89).
    NotListed,
}

/// Why [`crate::Node::service_request`] returned what `bm_service_request`
/// returns false for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRequestError {
    /// The data is longer than [`bm_wire::service::MAX_DATA_SIZE`]. Nothing
    /// changed.
    TooLarge,
    /// [`SERVICE_REQUESTS`] requests are waiting, or the name is longer than
    /// [`SERVICE_NAME_BYTES`]. Nothing changed and no id was taken: the C's
    /// equivalent is a `bm_malloc` failure in `_create_node`.
    Full,
    /// Listed as `id`, but `<service>/rep` was not subscribed, or was not
    /// advertised. Nothing was sent. The request stays listed and times out
    /// (divergence #91).
    NotSubscribed {
        /// The id the request took.
        id: u32,
        /// Why.
        error: SubscribeError,
    },
    /// Listed as `id` and subscribed, but the publication did not fit the
    /// transmit buffer, which with the ceilings above it always does. The
    /// C's failed `bm_pub_wl`: the request stays listed and times out
    /// (divergence #91).
    NotSent {
        /// The id the request took.
        id: u32,
    },
}
