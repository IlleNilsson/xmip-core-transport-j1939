//! The 29-bit identifier of a J1939 frame: a priority, a parameter group
//! number and the address of the node that sent it.
//!
//! SAE J1939-21 spends CAN's extended identifier on meaning. Three bits of
//! priority come first, then the parameter group number — a reserved bit, a
//! data page bit, the PDU format and the PDU specific byte — and the source
//! address last. A PDU format below 240 is destination-specific, PDU1: the
//! specific byte is the destination address and no part of the group's
//! number. From 240 up, PDU2, the byte is a group extension, part of the
//! number, and the frame is for every node on the bus.

use transport::error::{Result, protocol_error};

/// The address every node listens at: a PDU2 frame's destination, and a
/// PDU1 frame's when it is broadcast.
pub const GLOBAL: u8 = 0xff;
/// The largest parameter group number: eighteen bits.
pub const MAX_PGN: u32 = 0x3_ffff;
/// The priority a data frame goes out with unless told otherwise.
pub const DEFAULT_PRIORITY: u8 = 6;
/// Proprietary A: a destination-specific group any manufacturer may use.
pub const PROPRIETARY_A: u32 = 0xef00;
/// Proprietary B: a broadcast group any manufacturer may use.
pub const PROPRIETARY_B: u32 = 0xff00;
/// The first PDU format that is a group extension rather than a destination.
const PDU2: u32 = 240;

/// One frame's identifier, taken apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identifier {
    /// Zero is the most urgent, seven the least.
    pub priority: u8,
    /// The parameter group number, its specific byte zero when it is PDU1.
    pub pgn: u32,
    /// Where a PDU1 frame goes; [`GLOBAL`] for a PDU2 frame.
    pub destination: u8,
    /// The node that sent it.
    pub source: u8,
}

impl Identifier {
    /// An identifier, refusing a priority over seven or a group over
    /// eighteen bits. A PDU2 group's destination is always [`GLOBAL`].
    ///
    /// # Errors
    /// Outside those bounds.
    pub fn new(priority: u8, pgn: u32, destination: u8, source: u8) -> Result<Self> {
        if priority > 7 {
            return Err(protocol_error("a priority wider than three bits"));
        }
        if pgn > MAX_PGN {
            return Err(protocol_error("a parameter group wider than eighteen bits"));
        }
        let specific = is_destination_specific(pgn);
        Ok(Self {
            priority,
            pgn: if specific { pgn & !0xff } else { pgn },
            destination: if specific { destination } else { GLOBAL },
            source,
        })
    }

    /// The identifier a raw 29-bit value names.
    ///
    /// # Errors
    /// A value wider than 29 bits.
    pub fn parse(raw: u32) -> Result<Self> {
        if raw > 0x1fff_ffff {
            return Err(protocol_error("an identifier wider than 29 bits"));
        }
        let format = (raw >> 16) & 0xff;
        let specific = (raw >> 8) & 0xff;
        let pages = (raw >> 24) & 0x03;
        let pgn = (pages << 16) | (format << 8) | if format < PDU2 { 0 } else { specific };
        Ok(Self {
            priority: u8::try_from((raw >> 26) & 0x07).unwrap_or(7),
            pgn,
            destination: if format < PDU2 {
                u8::try_from(specific).unwrap_or(GLOBAL)
            } else {
                GLOBAL
            },
            source: u8::try_from(raw & 0xff).unwrap_or(0),
        })
    }

    /// The 29 bits on the wire.
    #[must_use]
    pub fn raw(&self) -> u32 {
        let specific = if is_destination_specific(self.pgn) {
            u32::from(self.destination)
        } else {
            self.pgn & 0xff
        };
        (u32::from(self.priority) << 26)
            | ((self.pgn & 0x3_ff00) << 8)
            | (specific << 8)
            | u32::from(self.source)
    }

    /// `j1939://<bus>/0x<pgn>?from=0x<source>`.
    #[must_use]
    pub fn origin(&self, bus: &str) -> String {
        format!("j1939://{bus}/{:#x}?from={:#x}", self.pgn, self.source)
    }
}

/// Whether `pgn` is PDU1, carrying a destination in its specific byte.
#[must_use]
pub const fn is_destination_specific(pgn: u32) -> bool {
    (pgn >> 8) & 0xff < PDU2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_specific_group_carries_its_destination_in_the_identifier() {
        let id = Identifier::new(6, PROPRIETARY_A, 0x21, 0x80).expect("identifier");
        assert_eq!(id.raw(), 0x18ef_2180);
        assert_eq!(Identifier::parse(0x18ef_2180).expect("parse"), id);
        assert_eq!(id.origin("loopback"), "j1939://loopback/0xef00?from=0x80");
        let dressed = Identifier::new(6, PROPRIETARY_A | 0x21, 0x33, 0x80).expect("identifier");
        assert_eq!(
            dressed.pgn, PROPRIETARY_A,
            "the specific byte is no part of a PDU1 number"
        );
        assert_eq!(dressed.destination, 0x33);
    }

    #[test]
    fn a_broadcast_group_keeps_its_extension_and_goes_to_everyone() {
        let id = Identifier::new(3, 0xfeca, 0x21, 0x00).expect("identifier");
        assert_eq!(id.destination, GLOBAL);
        assert_eq!(id.raw(), 0x0cfe_ca00);
        assert_eq!(Identifier::parse(0x0cfe_ca00).expect("parse").pgn, 0xfeca);
        assert!(!is_destination_specific(0xfeca));
        assert!(is_destination_specific(0xec00));
    }

    #[test]
    fn an_identifier_outside_its_bits_is_refused() {
        assert!(Identifier::new(8, 0, 0, 0).is_err(), "priority");
        assert!(Identifier::new(0, MAX_PGN + 1, 0, 0).is_err(), "group");
        assert!(Identifier::parse(0x2000_0000).is_err(), "thirty bits");
        let paged = Identifier::parse(0x1dff_0001).expect("parse");
        assert_eq!(paged.pgn, 0x1_ff00, "the data page is bit sixteen");
        assert_eq!(paged.priority, 7);
    }
}
