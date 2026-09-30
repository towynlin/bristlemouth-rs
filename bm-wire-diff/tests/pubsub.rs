//! Pub/sub on `bm_stack::Node`, compared against the oracle's whole stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire_diff::pubsub::{
    Arrival, Call, PATTERNS, PubSubInput, Step, TOPICS, Topic, check, duplicate_callbacks,
};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};

fn arrival(topic: &[u8], from_pubsub_port: bool) -> Step {
    Step::Receive(Arrival {
        ingress: 1,
        from_pubsub_port,
        topic: topic.to_vec(),
        kind: 1,
        version: 2,
        data: b"data".to_vec(),
    })
}

fn publish(topic: Topic, data_len: usize) -> Step {
    Step::Publish {
        topic,
        kind: 1,
        version: 2,
        data: vec![0xA5; data_len],
    }
}

/// Every pattern subscribed, every topic published and received, then every
/// pattern unsubscribed.
#[test]
fn every_pattern_and_topic() {
    let mut steps: Vec<Step> = (0..PATTERNS.len() as u8)
        .map(|i| Step::Subscribe(Topic::Pool(i)))
        .collect();
    for i in 0..TOPICS.len() as u8 {
        steps.push(publish(Topic::Pool(i), 4));
        steps.push(arrival(TOPICS[usize::from(i)], true));
    }
    steps.extend((0..PATTERNS.len() as u8).map(|i| Step::Unsubscribe(Topic::Pool(i))));
    check(&PubSubInput { steps });
}

/// Subscribing twice changes nothing; a topic unsubscribed and subscribed
/// again goes to the end of the list, which is the delivery order.
#[test]
fn list_order_is_delivery_order() {
    check(&PubSubInput {
        steps: vec![
            Step::Subscribe(Topic::Pool(0)),
            Step::Subscribe(Topic::Pool(4)),
            Step::Subscribe(Topic::Pool(0)),
            arrival(b"sensor/tmp/1", true),
            Step::Unsubscribe(Topic::Pool(0)),
            Step::Subscribe(Topic::Pool(0)),
            arrival(b"sensor/tmp/1", true),
            Step::Unsubscribe(Topic::Pool(0)),
            Step::Unsubscribe(Topic::Pool(0)),
            Step::Unsubscribe(Topic::Pool(4)),
        ],
    });
}

/// The refusals, error for error: empty topics, 255-byte topics, an unknown
/// topic, and a publication too long to send, which is delivered locally
/// first.
#[test]
fn refusals() {
    let fits = bm_wire::pubsub::MAX_MESSAGE_LEN
        - bm_wire::pubsub::HEADER_LEN
        - bm_wire_diff::pubsub::TOPIC_LEN;
    check(&PubSubInput {
        steps: vec![
            Step::Subscribe(Topic::Empty),
            Step::Subscribe(Topic::TooLong),
            Step::Unsubscribe(Topic::Empty),
            Step::Unsubscribe(Topic::TooLong),
            Step::Unsubscribe(Topic::Pool(1)),
            publish(Topic::Empty, 0),
            publish(Topic::TooLong, 0),
            Step::Subscribe(Topic::Pool(4)),
            publish(Topic::Pool(2), fits),
            publish(Topic::Pool(2), fits + 1),
            Step::Unsubscribe(Topic::Pool(4)),
        ],
    });
}

/// Divergence #73: a publication to 4321 from any other port reaches nobody.
/// Topic lengths 0 and 255, which `bm_pub_wl` refuses, are delivered.
#[test]
fn received_publications() {
    check(&PubSubInput {
        steps: vec![
            Step::Subscribe(Topic::Pool(4)),
            Step::Subscribe(Topic::Pool(5)),
            arrival(b"spotter/printf", false),
            arrival(b"spotter/printf", true),
            arrival(b"", true),
            arrival(&[b'x'; 255], true),
            Step::Unsubscribe(Topic::Pool(4)),
            Step::Unsubscribe(Topic::Pool(5)),
        ],
    });
}

/// Divergence #79: `bm_sub_wl` checks only a topic's first callback for a
/// duplicate, so a second callback subscribed twice is linked twice and called
/// twice; one `bm_unsub_wl` removes one link.
#[test]
fn a_second_callback_subscribed_twice_is_called_twice() {
    use Call::{Sub, Unsub};
    assert_eq!(duplicate_callbacks(&[Sub('a'), Sub('a')]), (1, 0));
    assert_eq!(duplicate_callbacks(&[Sub('a'), Sub('b')]), (1, 1));
    assert_eq!(duplicate_callbacks(&[Sub('a'), Sub('b'), Sub('b')]), (1, 2));
    assert_eq!(
        duplicate_callbacks(&[Sub('a'), Sub('b'), Sub('b'), Unsub('b')]),
        (1, 1)
    );
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("pubsub");
    assert!(
        replayed > 0,
        "no pubsub seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} pubsub seeds");
}
