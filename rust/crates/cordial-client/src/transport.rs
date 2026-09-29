//! Desktop serial adapter. Enumeration reads USB metadata without opening ports.
use serde::Serialize;
use std::{
    io::{self, Read, Write},
    time::Duration,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PortInfo {
    pub port: String,
    pub serial: String,
}

pub fn ports() -> io::Result<Vec<PortInfo>> {
    let found: Vec<_> = serialport::available_ports()?
        .into_iter()
        .filter_map(|port| match port.port_type {
            serialport::SerialPortType::UsbPort(info) if is_cordial(&info) => Some(PortInfo {
                port: port.port_name,
                serial: info.serial_number.unwrap_or_default(),
            }),
            _ => None,
        })
        .collect();
    Ok(unique_ports(found))
}
fn is_cordial(info: &serialport::UsbPortInfo) -> bool {
    info.vid == 0xcafe && info.pid == 0x4014 && info.manufacturer.as_deref() == Some("Cordial")
}
fn unique_ports(mut found: Vec<PortInfo>) -> Vec<PortInfo> {
    // macOS exposes callout and dial-in names for the same USB serial service.
    let callouts: std::collections::BTreeSet<_> = found
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

/// The same byte stream/session controls are supplied by offline test adapters.
pub trait Transport: Read + Write + Send {
    fn set_dtr(&mut self, enabled: bool) -> io::Result<()>;
    fn clear_input(&mut self) -> io::Result<()>;
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()>;
    fn try_clone(&self) -> io::Result<Box<dyn Transport>>;
}

struct Serial(Box<dyn serialport::SerialPort>);
impl Read for Serial {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for Serial {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl Transport for Serial {
    fn set_dtr(&mut self, enabled: bool) -> io::Result<()> {
        self.0
            .write_data_terminal_ready(enabled)
            .map_err(Into::into)
    }
    fn clear_input(&mut self) -> io::Result<()> {
        self.0
            .clear(serialport::ClearBuffer::Input)
            .map_err(Into::into)
    }
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.0.set_timeout(timeout).map_err(Into::into)
    }
    fn try_clone(&self) -> io::Result<Box<dyn Transport>> {
        Ok(Box::new(Serial(self.0.try_clone()?)))
    }
}

pub fn open(port: &str) -> io::Result<Box<dyn Transport>> {
    // The library acquires the OS's exclusive serial lock before configuring
    // the port. Its Unix flock also prevents aliases from opening a second session.
    let serial = serialport::new(port, 115200)
        .dtr_on_open(false)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|error| io::Error::other(format!("open {port}: {error}")))?;
    Ok(Box::new(Serial(serial)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_requires_cordial_usb_metadata() {
        let expected = serialport::UsbPortInfo {
            vid: 0xcafe,
            pid: 0x4014,
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
            serial: "same-or-missing-serial".into(),
        })
        .collect();
        let found = unique_ports(ports);
        assert_eq!(
            found.iter().map(|p| p.port.as_str()).collect::<Vec<_>>(),
            ["/dev/cu.usbmodem1", "/dev/cu.usbmodem2", "/dev/ttyACM0"]
        );
    }
}
