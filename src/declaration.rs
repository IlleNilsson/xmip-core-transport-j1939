//! This technology's declaration: the transport guide's "The declaration",
//! `module/core/capability/transport/doc/adding-a-transport.md`. The bounds
//! of J1939-21 and the defaults a Location's settings name are declared here,
//! and the code takes them from here.

use std::time::Duration;

use xcore::settings::{Applies, Fixed, Kind, Presence, Setting, Settings};

/// How long a peer that stops mid-transfer is waited on unless a Location
/// says.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

/// The address every node listens at: a PDU2 frame's destination, and a
/// PDU1 frame's when it is broadcast.
pub const GLOBAL: u8 = 0xff;
/// The largest parameter group number: eighteen bits.
pub const MAX_PGN: u32 = 0x3_ffff;
/// The priority a data frame goes out with unless told otherwise.
pub const DEFAULT_PRIORITY: u8 = 6;
/// Proprietary B: a broadcast group any manufacturer may use.
pub const PROPRIETARY_B: u32 = 0xff00;

/// Every setting a Location takes beyond its address, the CAN interface,
/// `can0`, the node sits on: the node's own address and what it sends.
pub const SETTINGS: &Settings = &Settings {
    technology: env!("CARGO_PKG_NAME"),
    settings: &[
        Setting {
            name: "source",
            kind: Kind::Integer {
                minimum: 0,
                maximum: 253,
            },
            presence: Presence::Required,
            meaning: "The address this node sends from and is sent to on the bus.",
            applies: Applies::Both,
        },
        Setting {
            name: "pgn",
            kind: Kind::Integer {
                minimum: 0,
                maximum: MAX_PGN as i64,
            },
            presence: Presence::Default(Fixed::Integer(PROPRIETARY_B as i64)),
            meaning: "The parameter group sent when the target names none.",
            applies: Applies::Send,
        },
        Setting {
            name: "destination",
            kind: Kind::Integer {
                minimum: 0,
                maximum: 255,
            },
            presence: Presence::Default(Fixed::Integer(GLOBAL as i64)),
            meaning: "The node sent to when the target names none; 255 broadcasts.",
            applies: Applies::Send,
        },
        Setting {
            name: "priority",
            kind: Kind::Integer {
                minimum: 0,
                maximum: 7,
            },
            presence: Presence::Default(Fixed::Integer(DEFAULT_PRIORITY as i64)),
            meaning: "The priority of every frame this node sends, zero the most urgent.",
            applies: Applies::Both,
        },
        Setting {
            name: "block",
            kind: Kind::Integer {
                minimum: 1,
                maximum: 255,
            },
            presence: Presence::Optional,
            meaning: "How many packets one clear to send lets through; all when left out.",
            applies: Applies::Receive,
        },
        transport::configured::timeout(
            Applies::Both,
            Some(DEFAULT_TIMEOUT),
            "How long a peer that stops mid-transfer is waited on.",
        ),
    ],
};

/// This technology's schemes, as [`transport::Configured::SCHEMES`] defines
/// them.
pub const SCHEMES: &[&str] = &["j1939"];
