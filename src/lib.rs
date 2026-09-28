#![forbid(unsafe_code)]

//! Streams that cross a vehicle's bus as SAE J1939 parameter groups.
//!
//! J1939 is the truck, the tractor and the ship: every engine, gearbox and
//! body controller on a 250 kbit/s CAN, talking in parameter groups under
//! 29-bit identifiers that carry a priority, the group's number and the
//! sending node's address. A group of eight bytes or fewer is one frame. A
//! longer one crosses by the transport protocol of J1939-21: a request to
//! send and a clear to send between two nodes, or a broadcast announce
//! message to everyone, and then the data seven bytes a packet, at most 255
//! packets — 1785 bytes, the ceiling.
//!
//! The carrier is [`can_bus`](can_bus): J1939 reuses its [`Bus`],
//! [`Frame`] rather than knowing a wire of its own. A sender and a receiver
//! are two nodes on one bus — in process, the SDK's simulated one — so they
//! round-trip with no hardware, which is what [`J1939Transport::loopback`]
//! stands up (ADR-0051). On a vehicle the bus is `can0`.
//!
//! The origin URI names the group and the node it came from:
//! `j1939://<bus>/0x<pgn>?from=0x<source>`.

pub mod identifier;
pub mod loopback;
mod settings;
pub mod transfer;

use std::sync::Arc;
use std::time::{Duration, Instant};

use can_bus::loopback::Session;
use can_bus::{Bus, Frame};
use codec::hex::prefixed_number;
use net::Target;
use transport::error::{Result, protocol_error};
use transport::standing::Standing;
use transport::{Arrived, Directions, Transport};

pub use identifier::{DEFAULT_PRIORITY, GLOBAL, Identifier, PROPRIETARY_A, PROPRIETARY_B};
pub use loopback::{RECEIVER, SENDER};
pub use transfer::{CEILING, Control};

use crate::transfer::{PACKET_DATA, SINGLE_FRAME, TP_CM, TP_DT};

/// One node on a J1939 bus.
///
/// `outbound` carries what this node transmits, `inbound` what the others
/// do; on a vehicle and on the simulated bus they are one node.
#[derive(Clone)]
pub struct J1939Transport {
    outbound: Arc<dyn Bus>,
    inbound: Arc<dyn Bus>,
    source: u8,
    destination: u8,
    pgn: u32,
    priority: u8,
    block: u8,
    timeout: Duration,
    standing: Standing<Session>,
}

impl J1939Transport {
    /// A node at address `source`, sending [`PROPRIETARY_B`] to everyone
    /// until told otherwise.
    #[must_use]
    pub fn new(outbound: Arc<dyn Bus>, inbound: Arc<dyn Bus>, source: u8) -> Self {
        Self {
            outbound,
            inbound,
            source,
            destination: GLOBAL,
            pgn: PROPRIETARY_B,
            priority: DEFAULT_PRIORITY,
            block: u8::MAX,
            timeout: Duration::from_secs(1),
            standing: Standing::default(),
        }
    }

    /// Send `pgn` to `destination`: [`GLOBAL`] broadcasts, and a long
    /// payload then goes by BAM rather than RTS/CTS.
    #[must_use]
    pub const fn addressed_to(mut self, pgn: u32, destination: u8) -> Self {
        self.pgn = pgn;
        self.destination = destination;
        self
    }

    /// Zero is the most urgent, seven the least.
    #[must_use]
    pub const fn at_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// How many packets this node clears at once when it receives by
    /// RTS/CTS; 255 clears them all.
    #[must_use]
    pub const fn clearing(mut self, block: u8) -> Self {
        self.block = if block == 0 { 1 } else { block };
        self
    }

    /// Give up on a peer that stops mid-transfer.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Put `payload` on the bus as `pgn` to `destination`: one frame when
    /// it fits, the transport protocol when it does not.
    ///
    /// # Errors
    /// A payload over [`CEILING`], a receiver that aborts or never clears,
    /// or a bus that refused a frame.
    pub fn deliver(&self, pgn: u32, destination: u8, payload: &[u8]) -> Result<()> {
        let packets = transfer::packets_for(payload.len())?;
        if payload.len() <= SINGLE_FRAME {
            return self.transmit(pgn, destination, payload);
        }
        let size = u16::try_from(payload.len()).unwrap_or(u16::MAX);
        if destination == GLOBAL {
            let announce = Control::Broadcast { size, packets, pgn };
            self.transmit(TP_CM, GLOBAL, &announce.encode())?;
            return self.send_packets(destination, payload, 1, packets);
        }
        let request = Control::RequestToSend { size, packets, pgn };
        self.transmit(TP_CM, destination, &request.encode())?;
        loop {
            match self.read_control()? {
                Control::ClearToSend { packets: 0, .. } => {}
                Control::ClearToSend { packets, next, .. } => {
                    self.send_packets(destination, payload, next, packets)?;
                }
                Control::EndOfMessage { .. } => return Ok(()),
                Control::Abort { reason, .. } => {
                    return Err(protocol_error(format!(
                        "the receiver aborted the transfer, reason {reason}"
                    )));
                }
                other => {
                    return Err(protocol_error(format!("{other:?} while sending")));
                }
            }
        }
    }

    /// Take one parameter group off the bus: a single frame as it is, a
    /// transfer collected packet by packet.
    ///
    /// # Errors
    /// A malformed frame, a packet out of sequence, or a peer that stops.
    pub fn collect(&self) -> Result<Arrived> {
        let (id, data) = self.read_frame()?;
        match id.pgn {
            TP_CM => self.collect_transfer(id.source, &data),
            TP_DT => Err(protocol_error("a data packet with no connection open")),
            _ => Ok(Arrived::new(id.origin(self.inbound.name()), data)),
        }
    }

    fn collect_transfer(&self, peer: u8, control: &[u8]) -> Result<Arrived> {
        match Control::parse(control)? {
            Control::Broadcast { size, packets, pgn } => {
                self.receive_packets(peer, pgn, size, packets, false)
            }
            Control::RequestToSend { size, packets, pgn } => {
                self.receive_packets(peer, pgn, size, packets, true)
            }
            Control::Abort { reason, .. } => Err(protocol_error(format!(
                "the sender aborted the transfer, reason {reason}"
            ))),
            other => Err(protocol_error(format!("{other:?} with no request open"))),
        }
    }

    fn send_packets(&self, destination: u8, payload: &[u8], from: u8, count: u8) -> Result<()> {
        let total = u16::from(transfer::packets_for(payload.len())?);
        let from = u16::from(from.max(1));
        let last = (from + u16::from(count) - 1).min(total);
        for index in from..=last {
            let start = usize::from(index - 1) * PACKET_DATA;
            let end = (start + PACKET_DATA).min(payload.len());
            let sequence = u8::try_from(index).unwrap_or(u8::MAX);
            let frame = transfer::packet(sequence, &payload[start..end])?;
            self.transmit(TP_DT, destination, &frame)?;
        }
        Ok(())
    }

    fn receive_packets(
        &self,
        peer: u8,
        pgn: u32,
        size: u16,
        packets: u8,
        clearing: bool,
    ) -> Result<Arrived> {
        let mut bytes = Vec::with_capacity(usize::from(size));
        let mut next: u16 = 1;
        while next <= u16::from(packets) {
            let left = u16::from(packets) - next + 1;
            let window = if clearing {
                let cleared = left.min(u16::from(self.block));
                let clear = Control::ClearToSend {
                    packets: u8::try_from(cleared).unwrap_or(u8::MAX),
                    next: u8::try_from(next).unwrap_or(u8::MAX),
                    pgn,
                };
                self.transmit(TP_CM, peer, &clear.encode())?;
                cleared
            } else {
                left
            };
            for _ in 0..window {
                let (id, data) = self.read_frame()?;
                if id.pgn == TP_CM {
                    return self.collect_transfer(peer, &data).and(Err(protocol_error(
                        "a control frame in the middle of the data",
                    )));
                }
                if id.pgn != TP_DT {
                    return Err(protocol_error("a frame of another group mid-transfer"));
                }
                let (index, chunk) = transfer::unpacket(&data)?;
                if u16::from(index) != next {
                    return Err(protocol_error("a data packet out of sequence"));
                }
                bytes.extend_from_slice(chunk);
                next += 1;
            }
        }
        bytes.truncate(usize::from(size));
        if clearing {
            let done = Control::EndOfMessage { size, packets, pgn };
            self.transmit(TP_CM, peer, &done.encode())?;
        }
        let origin = Identifier::new(self.priority, pgn, self.source, peer)?;
        Ok(Arrived::new(origin.origin(self.inbound.name()), bytes))
    }

    fn transmit(&self, pgn: u32, destination: u8, data: &[u8]) -> Result<()> {
        let id = Identifier::new(self.priority, pgn, destination, self.source)?;
        self.outbound.transmit(&Frame::new(id.raw(), true, data)?)
    }

    fn read_control(&self) -> Result<Control> {
        let (id, data) = self.read_frame()?;
        if id.pgn != TP_CM {
            return Err(protocol_error("a frame that was not connection management"));
        }
        Control::parse(&data)
    }

    fn read_frame(&self) -> Result<(Identifier, Vec<u8>)> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(frame) = self.inbound.receive(self.timeout)? {
                if !frame.extended {
                    return Err(protocol_error("an 11-bit identifier is no J1939 frame"));
                }
                return Ok((Identifier::parse(frame.id)?, frame.data));
            }
            if Instant::now() >= deadline {
                return Err(protocol_error("no J1939 frame before the deadline"));
            }
            // An in-process bus answers at once when it is empty; let the
            // peer's thread have the core rather than spin on it.
            std::thread::yield_now();
        }
    }

    /// `j1939://<bus>/0x<pgn>?to=0x<destination>`, either part optional:
    /// what `target` overrides of this node's group and destination.
    fn addressed(&self, target: &str) -> Result<(u32, u8)> {
        let Some(named) = Target::under(&["j1939"], target) else {
            return Ok((self.pgn, self.destination));
        };
        let path = named.path();
        let pgn = if path.is_empty() {
            self.pgn
        } else {
            prefixed_number(path)
                .map_err(|_| protocol_error(format!("{path} is not a group number")))?
        };
        let destination = match named.query_value("to") {
            Some(to) => prefixed_number(&to)
                .map_err(|_| protocol_error(format!("{to} is not an address")))?,
            None => self.destination,
        };
        Ok((pgn, destination))
    }
}

impl Transport for J1939Transport {
    fn name(&self) -> &'static str {
        "j1939"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    fn receive(&self) -> Result<Vec<Arrived>> {
        Ok(vec![self.collect()?])
    }

    /// `target` may name a group and a destination, overriding the node's.
    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        let (pgn, destination) = self.addressed(target)?;
        self.deliver(pgn, destination, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two nodes on one simulated bus.
    fn two_nodes() -> (Arc<dyn Bus>, Arc<dyn Bus>) {
        let session = Session::fresh();
        (session.near, session.far)
    }

    #[test]
    fn a_target_overrides_the_group_and_the_destination() {
        // A node does not hear its own frames, so another sends what it
        // collects, from the same source address.
        let (there, here) = two_nodes();
        let sender = J1939Transport::new(Arc::clone(&there), there, 0x10);
        let node = J1939Transport::new(Arc::clone(&here), here, 0x10)
            .timing_out_after(Duration::from_millis(10));
        sender.send("j1939://can0/0xfeca", b"rpm").expect("sending");
        let arrived = node.collect().expect("collect");
        assert_eq!(arrived.bytes, b"rpm");
        assert_eq!(arrived.origin_uri, "j1939://loopback/0xfeca?from=0x10");
        assert_eq!(
            node.addressed("j1939://can0/0xef00?to=0x21").expect("both"),
            (0xef00, 0x21)
        );
        assert_eq!(
            node.addressed("j1939://can0/?to=0x21").expect("to"),
            (PROPRIETARY_B, 0x21)
        );
        assert_eq!(
            node.addressed("elsewhere").expect("neither"),
            (PROPRIETARY_B, GLOBAL)
        );
        assert!(node.addressed("j1939://can0/zz").is_err());
        assert!(node.addressed("j1939://can0/0xef00?to=0x100").is_err());
        assert!(node.collect().is_err(), "the bus is quiet");
        assert_eq!(node.name(), "j1939");
        assert!(node.claims().is_none());
        assert!(node.directions().receives() && node.directions().sends());
    }

    #[test]
    fn a_frame_that_breaks_the_protocol_is_refused() {
        // The frames that break the protocol come from another node.
        let (bus, here) = two_nodes();
        let node = J1939Transport::new(Arc::clone(&here), here, 0x10)
            .timing_out_after(Duration::from_millis(10));
        bus.transmit(&Frame::new(0x181, false, b"std").expect("frame"))
            .expect("transmit");
        assert!(node.collect().is_err(), "an 11-bit frame is not J1939");
        let data = Identifier::new(6, TP_DT, 0x10, 0x20).expect("id");
        bus.transmit(&Frame::new(data.raw(), true, &[1; 8]).expect("frame"))
            .expect("transmit");
        assert!(node.collect().is_err(), "a packet with no connection");
        let control = Identifier::new(6, TP_CM, 0x10, 0x20).expect("id");
        let request = Control::RequestToSend {
            size: 20,
            packets: 3,
            pgn: PROPRIETARY_A,
        };
        bus.transmit(&Frame::new(control.raw(), true, &request.encode()).expect("frame"))
            .expect("transmit");
        bus.transmit(&Frame::new(data.raw(), true, &[2; 8]).expect("frame"))
            .expect("transmit");
        let error = node.collect().expect_err("the second packet came first");
        assert!(error.message.contains("out of sequence"), "{error}");
        assert!(!error.retryable);
    }
}
