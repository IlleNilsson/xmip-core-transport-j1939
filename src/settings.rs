//! What a J1939 Location says beyond its address, declared once and read
//! through (ADR-0064, amendment 2026-09-26).

use std::sync::Arc;

use can_bus::Bus;
use transport::Configured;
use transport::error::Result;
use xcore::settings::{Applies, Fixed, Kind, Presence, Read, Setting, Settings};

use crate::J1939Transport;
use crate::identifier::{DEFAULT_PRIORITY, GLOBAL, MAX_PGN, PROPRIETARY_B};

impl Configured for J1939Transport {
    /// The address is the CAN interface, `can0`, the node sits on; the
    /// settings are the node's own address and what it sends.
    const SETTINGS: &'static Settings = &Settings {
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
            Setting {
                name: "timeout",
                kind: Kind::Duration,
                presence: Presence::Optional,
                meaning: "How long a peer that stops mid-transfer is waited on.",
                applies: Applies::Both,
            },
        ],
    };

    fn configured(address: &str, settings: &Read) -> Result<Self> {
        Ok(Self::on(can_bus::open_bus(address)?, settings))
    }
}

impl J1939Transport {
    /// This node on `bus`, both directions on the one node, as the settings
    /// read say.
    pub(crate) fn on(bus: Arc<dyn Bus>, settings: &Read) -> Self {
        // The declaration holds every integer within its range.
        let byte = |name| {
            settings
                .optional_integer(name)
                .map(|n| u8::try_from(n).unwrap_or(0))
        };
        let source = byte("source").unwrap_or(0);
        let mut node = Self::new(Arc::clone(&bus), bus, source);
        // Both are the Send side's, so a Send Location has both and a
        // Receive Location neither.
        if let (Some(pgn), Some(destination)) =
            (settings.optional_integer("pgn"), byte("destination"))
        {
            node = node.addressed_to(u32::try_from(pgn).unwrap_or(0), destination);
        }
        if let Some(priority) = byte("priority") {
            node = node.at_priority(priority);
        }
        if let Some(block) = byte("block") {
            node = node.clearing(block);
        }
        match settings.optional_duration("timeout") {
            Some(timeout) => node.timing_out_after(timeout),
            None => node,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use can_bus::loopback::Session;
    use std::time::Duration;
    use xcore::settings::Given;

    #[test]
    fn j1939_declares_its_settings_and_reads_through_them() {
        let declared = J1939Transport::SETTINGS;
        assert!(declared.problems().is_empty(), "{:?}", declared.problems());

        let given = [
            ("source".to_string(), Given::Integer(0x21)),
            ("destination".to_string(), Given::Integer(0x10)),
            ("timeout".to_string(), Given::Text("250ms".to_string())),
        ];
        let read = declared.read(Applies::Send, &given).expect("read");
        let node = J1939Transport::on(Session::fresh().near, &read);
        assert_eq!(node.source, 0x21);
        assert_eq!((node.pgn, node.destination), (PROPRIETARY_B, 0x10));
        assert_eq!(node.priority, DEFAULT_PRIORITY);
        assert_eq!(node.timeout, Duration::from_millis(250));

        let Err(refused) = J1939Transport::open("can0", Applies::Receive, &given[..2]) else {
            panic!("a Receive Location reads no destination");
        };
        let Err(refused_missing) = J1939Transport::open("can0", Applies::Send, &[]) else {
            panic!("source is required");
        };
        assert!(refused.message.contains("\"destination\""), "{refused}");
        assert!(
            refused_missing.message.contains("\"source\""),
            "{refused_missing}"
        );
    }
}
