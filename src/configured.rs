//! How a J1939 node is configured: the CAN interface as the address, the
//! node's own address and what it sends as its settings, which the
//! declaration holds (`declaration.rs`) and this reads through (ADR-0064,
//! amendment 2026-09-26).

use std::sync::Arc;

use can_bus::Bus;
use transport::Configured;
use transport::error::Result;
use xcore::settings::{Read, Settings};

use crate::{J1939Transport, declaration};

impl Configured for J1939Transport {
    const SETTINGS: &'static Settings = declaration::SETTINGS;
    const SCHEMES: &'static [&'static str] = declaration::SCHEMES;

    fn timeout_kept(&mut self) -> Option<&mut dyn transport::configured::TimeoutKept> {
        Some(&mut self.timeout)
    }

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
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_PRIORITY, DEFAULT_TIMEOUT, PROPRIETARY_B};
    use can_bus::loopback::Session;
    use xcore::settings::{Applies, Given};

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
        // The Location's timeout is what `open` hands the node once it is
        // built (`Configured::finished`), here on the simulated bus by its own
        // constructor, so it holds where a build opens the kernel's.
        let opened =
            J1939Transport::on(can_bus::simulated("can0"), &read).finished(&read, Applies::Send);
        assert_eq!(opened.timeout, std::time::Duration::from_millis(250));
        // Left out, the Location waits the default it declares.
        let unsaid = declared.read(Applies::Receive, &given[..1]).expect("read");
        let timeout = unsaid.optional_duration(transport::configured::TIMEOUT);
        assert_eq!(timeout, Some(DEFAULT_TIMEOUT));
        let node = J1939Transport::on(Session::fresh().near, &unsaid);
        assert_eq!(node.timeout, DEFAULT_TIMEOUT);

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
