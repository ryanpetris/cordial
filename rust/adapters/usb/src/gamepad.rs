//! Standard HID gamepads with numbered buttons and usage-preserving axes.

use core::cell::Cell;
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use embassy_sync::signal::Signal;
use embassy_usb::{
    Builder,
    class::hid::{self, ReportId, RequestHandler},
    driver::{Driver, EndpointError},
};

/// One report: 32 buttons, six signed axes, a hat, D-pad/Start/Select, and two unsigned triggers.
pub const REPORT_LEN: usize = 22;
/// The hat is clockwise from north, 0..=7, with 8 meaning released.
pub const REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, // Generic Desktop, Game Pad.
    0x05, 0x09, 0x19, 0x01, 0x29, 0x20, // Button usages 1..32.
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x20, 0x81, 0x02, 0x05, 0x01, 0x19, 0x30, 0x29,
    0x35, // X, Y, Z, Rx, Ry, Rz.
    0x16, 0x00, 0x80, 0x26, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x06, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00,
    0x25, 0x07, // Hat switch with null state.
    0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x75, 0x04, 0x95,
    0x01, 0x81, 0x03, 0x65, 0x00, 0x35, 0x00, 0x45, 0x00, // Unitless D-pad/Start/Select.
    0x09, 0x90, 0x09, 0x91, 0x09, 0x92, 0x09, 0x93, 0x09, 0x3d, 0x09, 0x3e, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x06, 0x81, 0x02, 0x75, 0x02, 0x95, 0x01, 0x81, 0x03, 0x05, 0x02, 0x09, 0xc5,
    0x09, 0xc4, // Simulation Brake and Accelerator.
    0x15, 0x00, 0x27, 0xff, 0xff, 0x00, 0x00, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0xc0,
];

/// Encoded input. Missing sticks center; missing triggers and hats release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Report([u8; REPORT_LEN]);

impl Report {
    pub const NEUTRAL: Self = Self([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0,
    ]);

    /// `axes` follows X/Y/Z/Rx/Ry/Rz/Brake/Accelerator, normalized to 0..=65535.
    /// Bit n of `valid_axes` enables axis n. Generic Desktop axes are signed
    /// on the wire; Brake and Accelerator remain unsigned and release at zero.
    pub fn encode(buttons: u32, axes: &[u16; 8], valid_axes: u8, hat: Option<u8>) -> Self {
        let mut report = Self::NEUTRAL;
        report.0[..4].copy_from_slice(&buttons.to_le_bytes());
        for (i, axis) in axes[..6].iter().enumerate() {
            if valid_axes & (1 << i) != 0 {
                let signed = (i32::from(*axis) - 32768) as i16;
                report.0[4 + i * 2..6 + i * 2].copy_from_slice(&signed.to_le_bytes());
            }
        }
        for (i, axis) in axes.iter().enumerate().skip(6) {
            if valid_axes & (1 << i) != 0 {
                let offset = 18 + (i - 6) * 2;
                report.0[offset..offset + 2].copy_from_slice(&axis.to_le_bytes());
            }
        }
        report.0[16] = hat.filter(|&h| h < 8).unwrap_or(8);
        report
    }

    /// Add explicit Generic Desktop controls without conflating them with
    /// numbered buttons or the hat. D-pad bits are Up, Down, Right, and Left.
    pub fn with_controls(mut self, dpad: u8, start: bool, select: bool) -> Self {
        self.0[17] = (dpad & 0x0f) | (u8::from(start) << 4) | (u8::from(select) << 5);
        self
    }

    pub fn as_bytes(&self) -> &[u8; REPORT_LEN] {
        &self.0
    }
}

impl Default for Report {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// Current report for control reads and the host's requested idle interval.
pub struct Reports {
    report: Mutex<CriticalSectionRawMutex, Cell<Report>>,
    idle_ms: Mutex<CriticalSectionRawMutex, Cell<u32>>,
    changed: Signal<CriticalSectionRawMutex, ()>,
}

impl Default for Reports {
    fn default() -> Self {
        Self::new()
    }
}

impl Reports {
    pub const fn new() -> Self {
        Self {
            report: Mutex::new(Cell::new(Report::NEUTRAL)),
            idle_ms: Mutex::new(Cell::new(u32::MAX)),
            changed: Signal::new(),
        }
    }

    /// Update control reads immediately, even if the interrupt endpoint is busy.
    pub fn set(&self, report: Report) {
        self.report.lock(|r| r.set(report));
        self.changed.signal(());
    }

    pub fn current(&self) -> Report {
        self.report.lock(Cell::get)
    }

    /// Workers repeat the current report at this interval; `u32::MAX` means changes only.
    pub fn idle_ms(&self) -> u32 {
        self.idle_ms.lock(Cell::get)
    }

    /// Wake the slot's worker for current input or a new host idle interval.
    pub async fn changed(&self) {
        self.changed.wait().await;
    }

    /// Restore HID idle policy after USB reset without losing held input.
    pub fn reset(&self) {
        self.idle_ms.lock(|idle| idle.set(u32::MAX));
        self.changed.signal(());
    }
}

struct Control<'d>(&'d Reports);

impl RequestHandler for Control<'_> {
    fn get_report(&mut self, id: ReportId, buf: &mut [u8]) -> Option<usize> {
        if id != ReportId::In(0) || buf.len() < REPORT_LEN {
            return None;
        }
        buf[..REPORT_LEN].copy_from_slice(self.0.current().as_bytes());
        Some(REPORT_LEN)
    }

    fn get_idle_ms(&mut self, id: Option<ReportId>) -> Option<u32> {
        matches!(id, None | Some(ReportId::In(0))).then(|| self.0.idle_ms())
    }

    fn set_idle_ms(&mut self, id: Option<ReportId>, duration_ms: u32) {
        if matches!(id, None | Some(ReportId::In(0))) {
            self.0.idle_ms.lock(|idle| idle.set(duration_ms));
            self.0.changed.signal(());
        }
    }
}

/// Storage borrowed by Embassy for the HID control handler.
pub struct State<'d> {
    hid: hid::State<'d>,
    control: Control<'d>,
}

impl<'d> State<'d> {
    pub const fn new(reports: &'d Reports) -> Self {
        Self {
            hid: hid::State::new(),
            control: Control(reports),
        }
    }
}

/// One gamepad interface and IN endpoint. Run each writer in its own bounded worker.
pub struct Gamepad<'d, D: Driver<'d>> {
    writer: hid::HidWriter<'d, D, REPORT_LEN>,
}

impl<'d, D: Driver<'d>> Gamepad<'d, D> {
    pub fn new(builder: &mut Builder<'d, D>, state: &'d mut State<'d>) -> Self {
        Self {
            writer: hid::HidWriter::new(
                builder,
                &mut state.hid,
                hid::Config {
                    report_descriptor: REPORT_DESCRIPTOR,
                    request_handler: Some(&mut state.control),
                    poll_ms: 1,
                    max_packet_size: REPORT_LEN as u16,
                    hid_subclass: hid::HidSubclass::No,
                    hid_boot_protocol: hid::HidBootProtocol::None,
                },
            ),
        }
    }

    pub async fn ready(&mut self) {
        self.writer.ready().await;
    }

    /// Send the current assignment's report, or `Report::NEUTRAL` for a vacant slot.
    /// The caller updates `Reports` and checks assignment generations before sending.
    pub async fn write(&mut self, report: Report) -> Result<(), EndpointError> {
        self.writer.write(report.as_bytes()).await
    }
}
