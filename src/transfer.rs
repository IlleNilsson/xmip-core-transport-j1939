//! The transport protocol of SAE J1939-21: how a parameter group longer
//! than one frame crosses the bus.
//!
//! Two parameter groups carry it. TP.CM, connection management, is the
//! control frame: a request to send and the clear to send that answers it
//! between two nodes, a broadcast announce message to everyone, the
//! end-of-message acknowledgement, an abort. TP.DT, data transfer, is the
//! data: a sequence number and seven bytes, padded to eight with `0xff`.
//! At most 255 packets, so a payload is at most 1785 bytes — the ceiling,
//! and a fact of the protocol.

use transport::ceiling;
use transport::error::{Result, protocol_error};

/// Connection management, PDU1: its specific byte is the destination.
pub const TP_CM: u32 = 0xec00;
/// Data transfer, PDU1: its specific byte is the destination.
pub const TP_DT: u32 = 0xeb00;
/// A data packet carries this many bytes beside its sequence number.
pub const PACKET_DATA: usize = 7;
/// The sequence number is one byte and starts at one.
pub const MAX_PACKETS: usize = 255;
/// The largest payload the transport protocol carries whole.
pub const CEILING: usize = PACKET_DATA * MAX_PACKETS;
/// What fills a frame after the data ends.
pub const PADDING: u8 = 0xff;
/// A payload this long or shorter rides in one frame of its own group.
pub const SINGLE_FRAME: usize = 8;

const REQUEST_TO_SEND: u8 = 16;
const CLEAR_TO_SEND: u8 = 17;
const END_OF_MESSAGE: u8 = 19;
const BROADCAST: u8 = 32;
const ABORT: u8 = 255;

/// One TP.CM frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// A node asks to send `size` bytes in `packets` packets of `pgn`.
    RequestToSend { size: u16, packets: u8, pgn: u32 },
    /// The receiver clears `packets` packets starting at `next`; zero
    /// packets is "hold".
    ClearToSend { packets: u8, next: u8, pgn: u32 },
    /// The receiver has the whole message.
    EndOfMessage { size: u16, packets: u8, pgn: u32 },
    /// A node announces `size` bytes in `packets` packets to everyone; no
    /// answer is expected.
    Broadcast { size: u16, packets: u8, pgn: u32 },
    /// Either side gives up, with a reason.
    Abort { reason: u8, pgn: u32 },
}

impl Control {
    /// The group the transfer carries.
    #[must_use]
    pub const fn pgn(self) -> u32 {
        match self {
            Self::RequestToSend { pgn, .. }
            | Self::ClearToSend { pgn, .. }
            | Self::EndOfMessage { pgn, .. }
            | Self::Broadcast { pgn, .. }
            | Self::Abort { pgn, .. } => pgn,
        }
    }

    /// The eight bytes on the wire: the control byte, its parameters, and
    /// the group's number little-endian in the last three.
    #[must_use]
    pub fn encode(self) -> [u8; 8] {
        let [lo, mid, hi, _] = self.pgn().to_le_bytes();
        let head: [u8; 5] = match self {
            Self::RequestToSend { size, packets, .. } => {
                let [a, b] = size.to_le_bytes();
                [REQUEST_TO_SEND, a, b, packets, PADDING]
            }
            Self::ClearToSend { packets, next, .. } => {
                [CLEAR_TO_SEND, packets, next, PADDING, PADDING]
            }
            Self::EndOfMessage { size, packets, .. } => {
                let [a, b] = size.to_le_bytes();
                [END_OF_MESSAGE, a, b, packets, PADDING]
            }
            Self::Broadcast { size, packets, .. } => {
                let [a, b] = size.to_le_bytes();
                [BROADCAST, a, b, packets, PADDING]
            }
            Self::Abort { reason, .. } => [ABORT, reason, PADDING, PADDING, PADDING],
        };
        [head[0], head[1], head[2], head[3], head[4], lo, mid, hi]
    }

    /// The control frame eight bytes name.
    ///
    /// # Errors
    /// A frame that is not eight bytes, or a control byte outside the five.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let [control, a, b, c, _, lo, mid, hi] = bytes else {
            return Err(protocol_error("a TP.CM frame is eight bytes"));
        };
        let pgn = u32::from_le_bytes([*lo, *mid, *hi, 0]);
        let size = u16::from_le_bytes([*a, *b]);
        Ok(match *control {
            REQUEST_TO_SEND => Self::RequestToSend {
                size,
                packets: *c,
                pgn,
            },
            CLEAR_TO_SEND => Self::ClearToSend {
                packets: *a,
                next: *b,
                pgn,
            },
            END_OF_MESSAGE => Self::EndOfMessage {
                size,
                packets: *c,
                pgn,
            },
            BROADCAST => Self::Broadcast {
                size,
                packets: *c,
                pgn,
            },
            ABORT => Self::Abort { reason: *a, pgn },
            other => return Err(protocol_error(format!("a TP.CM control byte of {other}"))),
        })
    }
}

/// How many packets carry `size` bytes: at least one.
///
/// # Errors
/// A size over [`CEILING`].
pub fn packets_for(size: usize) -> Result<u8> {
    ceiling::within(size, CEILING, "one J1939 transfer carries")?;
    Ok(u8::try_from(size.div_ceil(PACKET_DATA).max(1)).unwrap_or(u8::MAX))
}

/// The `index`th data packet, carrying `chunk` padded to seven bytes.
///
/// # Errors
/// An index of zero, or a chunk over [`PACKET_DATA`].
pub fn packet(index: u8, chunk: &[u8]) -> Result<[u8; 8]> {
    if index == 0 {
        return Err(protocol_error("a sequence number starts at one"));
    }
    if chunk.len() > PACKET_DATA {
        return Err(protocol_error("a data packet carries at most seven bytes"));
    }
    let mut frame = [PADDING; 8];
    frame[0] = index;
    frame[1..=chunk.len()].copy_from_slice(chunk);
    Ok(frame)
}

/// The sequence number and the seven bytes of a data packet.
///
/// # Errors
/// A frame that is not eight bytes, or a sequence number of zero.
pub fn unpacket(bytes: &[u8]) -> Result<(u8, &[u8])> {
    let Some((index, data)) = bytes.split_first() else {
        return Err(protocol_error("an empty TP.DT frame"));
    };
    if data.len() != PACKET_DATA {
        return Err(protocol_error("a TP.DT frame is eight bytes"));
    }
    if *index == 0 {
        return Err(protocol_error("a sequence number of zero"));
    }
    Ok((*index, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_frame_encodes_and_parses_back() {
        let controls = [
            Control::RequestToSend {
                size: 1785,
                packets: 255,
                pgn: 0xef00,
            },
            Control::ClearToSend {
                packets: 4,
                next: 9,
                pgn: 0xef00,
            },
            Control::EndOfMessage {
                size: 100,
                packets: 15,
                pgn: 0xfeca,
            },
            Control::Broadcast {
                size: 9,
                packets: 2,
                pgn: 0x1_ff00,
            },
            Control::Abort {
                reason: 1,
                pgn: 0xef00,
            },
        ];
        for control in controls {
            let bytes = control.encode();
            assert_eq!(
                Control::parse(&bytes).expect("parse"),
                control,
                "{control:?}"
            );
        }
        assert_eq!(
            Control::RequestToSend {
                size: 0x1234,
                packets: 7,
                pgn: 0x1_ef00
            }
            .encode(),
            [16, 0x34, 0x12, 7, 0xff, 0x00, 0xef, 0x01]
        );
    }

    #[test]
    fn a_control_frame_that_is_not_one_is_refused() {
        assert!(Control::parse(&[16, 0, 0, 0, 0xff, 0, 0]).is_err(), "seven");
        assert!(
            Control::parse(&[18, 0, 0, 0, 0xff, 0, 0, 0]).is_err(),
            "no 18"
        );
        assert!(packets_for(CEILING + 1).is_err(), "over the ceiling");
        assert_eq!(packets_for(CEILING).expect("brim"), 255);
        assert_eq!(packets_for(0).expect("none"), 1);
        assert_eq!(packets_for(8).expect("two"), 2);
    }

    #[test]
    fn a_data_packet_is_padded_and_read_back_by_its_sequence() {
        let frame = packet(3, &[1, 2, 3]).expect("packet");
        assert_eq!(frame, [3, 1, 2, 3, 0xff, 0xff, 0xff, 0xff]);
        let (index, data) = unpacket(&frame).expect("unpacket");
        assert_eq!(index, 3);
        assert_eq!(data, [1, 2, 3, 0xff, 0xff, 0xff, 0xff]);
        assert!(packet(0, &[]).is_err(), "zero");
        assert!(packet(1, &[0; 8]).is_err(), "eight");
        assert!(unpacket(&[1, 2]).is_err(), "short");
        assert!(unpacket(&[0; 8]).is_err(), "zero on the wire");
    }
}
