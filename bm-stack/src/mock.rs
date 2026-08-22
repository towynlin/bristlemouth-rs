//! A PHY that replays a script and records what was sent.
//!
//! Enough to drive a whole node with no hardware: a test writes the frames the
//! network would deliver, runs the node, and reads back everything it
//! transmitted, with the egress port each copy went to.
//!
//! It also drives the clock. `embassy-time`'s mock driver only advances when
//! something advances it, so a script step that says "wait" is what makes the
//! heartbeat ticker fire — which means a test can exercise the real
//! [`crate::Node::run`] loop rather than only its synchronous halves.

extern crate alloc;

use alloc::vec::Vec;

use crate::port::{Egress, Phy};

/// One thing the network does.
#[derive(Debug, Clone)]
pub enum Script {
    /// A frame arrives on this port.
    Receive {
        /// Ingress port, 1-based.
        port: u8,
        /// The frame.
        frame: Vec<u8>,
    },
    /// Nothing arrives for this long. Advances the mock clock, so timers fire.
    Idle {
        /// Milliseconds.
        ms: u64,
    },
}

/// Why the mock stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockError {
    /// The script ran out. Not a failure — it is how a test ends a run.
    ScriptFinished,
}

/// A frame the node transmitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// Where it went.
    pub egress: Egress,
    /// The bytes, exactly as handed to the PHY.
    pub frame: Vec<u8>,
}

/// A PHY backed by a script.
#[derive(Debug)]
pub struct MockPhy {
    port_count: u8,
    script: Vec<Script>,
    next: usize,
    /// Everything the node transmitted, in order.
    pub sent: Vec<Sent>,
}

impl MockPhy {
    /// A PHY with `port_count` ports that will replay `script` and then stop.
    #[must_use]
    pub fn new(port_count: u8, script: Vec<Script>) -> Self {
        Self {
            port_count,
            script,
            next: 0,
            sent: Vec::new(),
        }
    }

    /// The frames sent to a particular egress, in order.
    #[must_use]
    pub fn sent_to(&self, egress: Egress) -> Vec<&[u8]> {
        self.sent
            .iter()
            .filter(|s| s.egress == egress)
            .map(|s| s.frame.as_slice())
            .collect()
    }
}

impl Phy for MockPhy {
    type Error = MockError;

    fn port_count(&self) -> u8 {
        self.port_count
    }

    async fn send(&mut self, frame: &[u8], egress: Egress) -> Result<(), Self::Error> {
        self.sent.push(Sent {
            egress,
            frame: frame.to_vec(),
        });
        Ok(())
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<(u8, usize), Self::Error> {
        loop {
            let step = self.script.get(self.next).cloned();
            self.next += 1;
            match step {
                Some(Script::Receive { port, frame }) => {
                    let len = frame.len().min(buf.len());
                    buf[..len].copy_from_slice(&frame[..len]);
                    return Ok((port, len));
                }
                Some(Script::Idle { ms }) => {
                    // Let the ticker win the select: advance the clock, then
                    // yield so the timer future is polled before this one is
                    // resumed.
                    embassy_time::MockDriver::get()
                        .advance(embassy_time::Duration::from_millis(ms));
                    embassy_futures::yield_now().await;
                }
                None => return Err(MockError::ScriptFinished),
            }
        }
    }
}
