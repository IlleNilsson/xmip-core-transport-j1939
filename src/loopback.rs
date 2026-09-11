//! Both ends of one J1939 exchange on this machine (ADR-0051).
//!
//! A sending node and a receiving node on a fresh pair of directed loopback
//! buses per round: the sender's request to send crosses one, the receiver's
//! clear to send comes back on the other, and the data follows. Addressed to
//! everyone, the same payload goes by BAM with nothing coming back. The two
//! ends need two threads, so the capability's `round` drives it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use can_bus::{Bus, Loopback as LoopbackBus};
use transport::Arrived;
use transport::error::{Result, protocol_error};
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};

use crate::J1939Transport;
use crate::identifier::PROPRIETARY_A;
use crate::transfer::{CEILING, Control, TP_CM};

/// The two directed buses of one loopback exchange: the sender transmits
/// on `to_receiver` and reads `to_sender`, the receiver the other way round.
#[derive(Clone)]
pub(crate) struct Session {
    to_receiver: Arc<dyn Bus>,
    to_sender: Arc<dyn Bus>,
}

/// The sessions a loopback has stood up and not yet taken, by address. A
/// fresh pair of buses per round, so rounds driven at once from several
/// threads never read each other's frames.
pub(crate) type Standing = Arc<Mutex<HashMap<String, Session>>>;

/// Numbers the sessions, so each address names one.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// The loopback's sending node.
pub const SENDER: u8 = 0x80;
/// The loopback's receiving node.
pub const RECEIVER: u8 = 0x21;

impl J1939Transport {
    /// Both ends on this machine: a node at [`SENDER`] whose far end is a
    /// node at [`RECEIVER`], sending [`PROPRIETARY_A`] to it by RTS/CTS,
    /// the two on a fresh pair of directed loopback buses per round, the
    /// loopback timeout on both. Addressed to [`GLOBAL`], a long payload
    /// goes by BAM instead. The buses this instance itself holds carry
    /// nothing; every round stands up its own.
    #[must_use]
    pub fn loopback() -> Self {
        let idle: Arc<dyn Bus> = Arc::new(LoopbackBus::new());
        Self::new(Arc::clone(&idle), idle, SENDER)
            .addressed_to(PROPRIETARY_A, RECEIVER)
            .timing_out_after(LOOPBACK_TIMEOUT)
    }

    /// The sending node's end of the session at `address`.
    fn sender(&self, address: &str) -> Result<Self> {
        let session = self
            .standing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(address)
            .cloned()
            .ok_or_else(|| protocol_error(format!("{address} is not a session stood up here")))?;
        Ok(
            Self::new(session.to_receiver, session.to_sender, self.source)
                .addressed_to(self.pgn, self.destination)
                .at_priority(self.priority)
                .clearing(self.block)
                .timing_out_after(self.timeout),
        )
    }
}

/// A node waiting to collect its one parameter group. It owns the session:
/// the address is forgotten once the group is taken.
struct Node {
    end: J1939Transport,
    standing: Standing,
    address: String,
}

impl FarEnd for Node {
    fn address(&self) -> &str {
        &self.address
    }

    fn take_one(self: Box<Self>) -> Result<Arrived> {
        let taken = self.end.collect();
        self.standing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.address);
        taken
    }
}

impl Loopback for J1939Transport {
    /// [`CEILING`]: 255 packets of seven bytes, the fact J1939-21 states
    /// about its one-byte sequence number.
    fn ceiling(&self) -> Option<usize> {
        Some(CEILING)
    }

    fn far_end(&self) -> Result<Box<dyn FarEnd>> {
        let session = Session {
            to_receiver: Arc::new(LoopbackBus::new()),
            to_sender: Arc::new(LoopbackBus::new()),
        };
        let receiver = Self::new(
            Arc::clone(&session.to_sender),
            Arc::clone(&session.to_receiver),
            RECEIVER,
        )
        .at_priority(self.priority)
        .clearing(self.block)
        .timing_out_after(self.timeout);
        let address = format!(
            "j1939://loopback/{}",
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        self.standing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(address.clone(), session);
        Ok(Box::new(Node {
            end: receiver,
            standing: Arc::clone(&self.standing),
            address,
        }))
    }

    fn send_to(&self, address: &str, payload: &[u8]) -> Result<()> {
        let sender = self.sender(address)?;
        sender.deliver(sender.pgn, sender.destination, payload)
    }

    /// No socket to poke. An abort is what no exchange opens with, so a
    /// receiver whose sender was refused reads it and is judged now rather
    /// than at its deadline.
    fn unblock(&self, address: &str) {
        if let Ok(sender) = self.sender(address) {
            let abort = Control::Abort {
                reason: 1,
                pgn: sender.pgn,
            };
            drop(sender.transmit(TP_CM, sender.destination, &abort.encode()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    use crate::identifier::{GLOBAL, PROPRIETARY_B};

    /// `len` bytes that a truncation, a reorder or a duplicate would change.
    fn patterned(len: usize) -> Vec<u8> {
        (0..len)
            .map(|at| u8::try_from((at * 31 + at / 251) % 256).unwrap_or(0))
            .collect()
    }

    #[test]
    fn the_loopback_returns_the_edge_payloads_whole_and_refuses_over_the_brim() {
        let loopback = J1939Transport::loopback();
        let edges: [(&str, Vec<u8>); 7] = [
            ("empty", Vec::new()),
            ("one byte", vec![0x2a]),
            ("every byte", (0..=255).collect()),
            ("nul run", vec![0; 512]),
            ("high bytes", vec![0xff; 512]),
            ("crlf storm", b"\r\n".repeat(400)),
            ("the brim", patterned(CEILING)),
        ];
        for (name, bytes) in edges {
            let arrived = loopback
                .round(&bytes)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(arrived.bytes, bytes, "{name}");
            assert_eq!(
                arrived.origin_uri, "j1939://loopback/0xef00?from=0x80",
                "{name}"
            );
        }
        assert_eq!(Loopback::ceiling(&loopback), Some(1785));
        assert!(loopback.refuses(b"x").is_none());
        let started = Instant::now();
        let error = loopback
            .round(&patterned(CEILING + 1))
            .expect_err("one over the brim");
        assert!(error.message.starts_with("send failed:"), "{error}");
        assert!(
            started.elapsed() < LOOPBACK_TIMEOUT,
            "a refused send is judged, never waited on"
        );
        assert!(
            loopback.standing.lock().expect("lock").is_empty(),
            "a taken session is forgotten"
        );
    }

    #[test]
    fn a_broadcast_goes_by_bam_and_a_short_group_by_one_frame() {
        let loopback = J1939Transport::loopback().addressed_to(PROPRIETARY_B, GLOBAL);
        let long = patterned(1000);
        let arrived = loopback.round(&long).expect("bam");
        assert_eq!(arrived.bytes, long);
        assert_eq!(arrived.origin_uri, "j1939://loopback/0xff00?from=0x80");
        let arrived = loopback.round(b"8 bytes!").expect("one frame");
        assert_eq!(arrived.bytes, b"8 bytes!");
        let arrived = loopback.round(b"9 bytes!!").expect("two packets");
        assert_eq!(arrived.bytes, b"9 bytes!!");
    }

    #[test]
    fn a_receiver_that_clears_in_blocks_paces_the_sender_the_same() {
        let loopback = J1939Transport::loopback().clearing(4);
        let payload = patterned(100);
        assert_eq!(
            loopback.round(&payload).expect("fifteen packets").bytes,
            payload
        );
        let loopback = J1939Transport::loopback().clearing(1).at_priority(3);
        assert_eq!(
            loopback.round(&payload).expect("one at a time").bytes,
            payload
        );
    }
}
