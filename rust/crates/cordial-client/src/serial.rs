//! USB serial ports: listing attached Dongles and opening a session on one.
use crate::{Connection, Received};
use cordial_protocol::{self as p, USB_MANUFACTURER, USB_PRODUCT_ID, USB_VENDOR_ID};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    sync::mpsc::Receiver,
    thread,
    time::Duration,
};

/// One attached Dongle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortInfo {
    /// The serial port's path or name, to pass to [`open`].
    pub port: String,
    /// The adapter ID, read from the USB serial number; empty when the port reports none.
    pub id: String,
}

/// Lists attached Dongles from USB metadata, without opening any port.
pub fn ports() -> io::Result<Vec<PortInfo>> {
    let found = serialport::available_ports()?
        .into_iter()
        .filter_map(|port| match port.port_type {
            serialport::SerialPortType::UsbPort(info) if is_cordial(&info) => Some(PortInfo {
                port: port.port_name,
                id: info.serial_number.unwrap_or_default(),
            }),
            _ => None,
        })
        .collect();
    Ok(unique_ports(found))
}

fn is_cordial(info: &serialport::UsbPortInfo) -> bool {
    info.vid == USB_VENDOR_ID
        && info.pid == USB_PRODUCT_ID
        && info.manufacturer.as_deref() == Some(USB_MANUFACTURER)
}

fn unique_ports(mut found: Vec<PortInfo>) -> Vec<PortInfo> {
    // macOS lists a callout and a dial-in name for the same USB serial port.
    let callouts: BTreeSet<_> = found
        .iter()
        .filter_map(|p| p.port.strip_prefix("/dev/cu.").map(str::to_owned))
        .collect();
    found.retain(|p| {
        p.port
            .strip_prefix("/dev/tty.")
            .is_none_or(|suffix| !callouts.contains(suffix))
    });
    found.sort_by(|a, b| a.port.cmp(&b.port));
    found.dedup_by(|a, b| a.port == b.port);
    found
}

/// Opens a Dongle's port and starts a session on it. Returns the connection and its events.
pub fn open(port: &str) -> io::Result<(Connection, Receiver<p::Event>)> {
    let (reader, writer) = prepare(port)?;
    Ok(Connection::new(reader, writer))
}

/// Opens a Dongle's port and starts a session that passes everything it receives to `handler`,
/// as [`Connection::with_handler`] does.
pub fn open_with_handler(
    port: &str,
    handler: impl FnMut(Received<'_>) + Send + 'static,
) -> io::Result<Connection> {
    let (reader, writer) = prepare(port)?;
    Ok(Connection::with_handler(reader, writer, handler))
}

/// Opens the port and starts a new session: DTR low long enough for the Dongle to see a
/// session end, stale input discarded, then DTR raised.
fn prepare(port: &str) -> io::Result<(Reader, Writer)> {
    // serialport takes the operating system's exclusive lock on the port before configuring
    // it, so a second client cannot open the same Dongle.
    let mut serial = serialport::new(port, 115_200)
        .dtr_on_open(false)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|error| io::Error::other(format!("open {port}: {error}")))?;
    serial.write_data_terminal_ready(false)?;
    thread::sleep(Duration::from_millis(60));
    serial.clear(serialport::ClearBuffer::Input)?;
    serial.write_data_terminal_ready(true)?;
    let reader = serial.try_clone()?;
    Ok((Reader(reader), Writer(serial)))
}

struct Reader(Box<dyn serialport::SerialPort>);

impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}

/// The writing half. Dropping it lowers DTR, which ends the session.
struct Writer(Box<dyn serialport::SerialPort>);

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.write_data_terminal_ready(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_requires_cordial_usb_metadata() {
        let expected = serialport::UsbPortInfo {
            vid: 0x1209,
            pid: 0xc0d1,
            manufacturer: Some("Cordial".into()),
            serial_number: Some("0123456789ABCDEF".into()),
            product: Some("Pico 2 W".into()),
        };
        assert!(is_cordial(&expected));
        for other in [
            serialport::UsbPortInfo {
                vid: 0x1234,
                ..expected.clone()
            },
            serialport::UsbPortInfo {
                pid: 0x4001,
                ..expected.clone()
            },
            serialport::UsbPortInfo {
                manufacturer: None,
                ..expected.clone()
            },
            serialport::UsbPortInfo {
                manufacturer: Some("Other".into()),
                ..expected.clone()
            },
            serialport::UsbPortInfo {
                manufacturer: Some("cordial".into()),
                ..expected.clone()
            },
        ] {
            assert!(!is_cordial(&other), "{other:?}");
        }
        assert!(is_cordial(&serialport::UsbPortInfo {
            product: None,
            serial_number: None,
            ..expected
        }));
    }

    #[test]
    fn mac_callout_aliases_count_once_without_merging_distinct_adapters() {
        let ports = [
            "/dev/tty.usbmodem1",
            "/dev/cu.usbmodem1",
            "/dev/cu.usbmodem2",
            "/dev/tty.usbmodem2",
            "/dev/ttyACM0",
        ]
        .into_iter()
        .map(|port| PortInfo {
            port: port.into(),
            id: "same-or-missing-serial".into(),
        })
        .collect();
        let found = unique_ports(ports);
        assert_eq!(
            found.iter().map(|p| p.port.as_str()).collect::<Vec<_>>(),
            ["/dev/cu.usbmodem1", "/dev/cu.usbmodem2", "/dev/ttyACM0"]
        );
    }
}
