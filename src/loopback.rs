//! Both ends of one J1939 exchange on this machine (ADR-0051).
//!
//! A sending node and a receiving node on a fresh simulated bus per round:
//! the sender's request to send reaches the receiver, its clear to send comes
//! back, and the data follows. Addressed to
//! everyone, the same payload goes by BAM with nothing coming back. The two
//! ends need two threads, so the capability's `round` drives it.
//!
//! The two nodes are CAN's [`Session`], the pair ISO-TP, UDS and OBD-II
//! stand up too: the sender is its near node and the receiver its far one.
//! This crate stood up the same pair under its own type until 2026-09-25.
//! What crosses between them is J1939-21's own transport protocol, not
//! ISO 15765-2's, which is why J1939 shares CAN's session and not ISO-TP.

use std::sync::Arc;

use can_bus::Bus;
use can_bus::loopback::Session;
use sdk::broadcast::Medium;
use transport::error::Result;
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};

use crate::J1939Transport;
use crate::identifier::PROPRIETARY_A;
use crate::transfer::{CEILING, Control, TP_CM};

/// The loopback's sending node.
pub const SENDER: u8 = 0x80;
/// The loopback's receiving node.
pub const RECEIVER: u8 = 0x21;

impl J1939Transport {
    /// Both ends on this machine: a node at [`SENDER`] whose far end is a
    /// node at [`RECEIVER`], sending [`PROPRIETARY_A`] to it by RTS/CTS,
    /// the two nodes on a fresh simulated bus per round, the loopback
    /// timeout on both. Addressed to [`GLOBAL`], a long payload goes by BAM
    /// instead. The bus this instance itself holds carries nothing; every
    /// round stands up its own.
    #[must_use]
    pub fn loopback() -> Self {
        let idle: Arc<dyn Bus> = Arc::new(Medium::new("loopback").node());
        Self::new(Arc::clone(&idle), idle, SENDER)
            .addressed_to(PROPRIETARY_A, RECEIVER)
            .timing_out_after(LOOPBACK_TIMEOUT)
    }

    /// The sending node's end of the session at `address`.
    fn sender(&self, address: &str) -> Result<Self> {
        let session = self.standing.session(address)?;
        Ok(
            Self::new(Arc::clone(&session.near), session.near, self.source)
                .addressed_to(self.pgn, self.destination)
                .at_priority(self.priority)
                .clearing(self.block)
                .timing_out_after(self.timeout),
        )
    }
}

impl Loopback for J1939Transport {
    /// [`CEILING`]: 255 packets of seven bytes, the fact J1939-21 states
    /// about its one-byte sequence number.
    fn ceiling(&self) -> Option<usize> {
        Some(CEILING)
    }

    /// A node waiting to collect its one parameter group. It owns the
    /// session: the address is forgotten once the group is taken.
    fn far_end(&self) -> Result<Box<dyn FarEnd>> {
        let session = Session::fresh();
        let receiver = Self::new(Arc::clone(&session.far), Arc::clone(&session.far), RECEIVER)
            .at_priority(self.priority)
            .clearing(self.block)
            .timing_out_after(self.timeout);
        let address = self.standing.stand("j1939", session);
        Ok(self.standing.far_end(address, move || receiver.collect()))
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
    use transport::payload::{edge_payloads, patterned};

    use crate::identifier::{GLOBAL, PROPRIETARY_B};

    #[test]
    fn the_loopback_returns_the_edge_payloads_whole_and_refuses_over_the_brim() {
        let loopback = J1939Transport::loopback();
        let mut edges = edge_payloads();
        edges.push(("the brim", patterned(CEILING)));
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
        assert!(loopback.standing.is_empty(), "a taken session is forgotten");
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
