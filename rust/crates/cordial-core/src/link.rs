//! A live HID connection. Backends copy submitted bytes before returning and
//! report completion with the same token; callbacks never borrow this owner.
use crate::{
    features::Engine,
    forward::Forwarder,
    hid::{self, Held, Input, Map, State},
    hidpp::Client,
    settings::Catalog,
};
use alloc::{boxed::Box, vec::Vec};
use cordial_protocol::{
    errors::ErrorCode as Error,
    identifiers::{HostPlatform, NormalizationState, SettingsState},
    settings::SettingKey,
};

/// The owner advances generation each time a connection slot is reused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkId {
    pub slot: u8,
    pub generation: u64,
}
/// Identifies a report map/service within one connection, not a vendor handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceId(pub u16);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteId {
    pub link: LinkId,
    pub sequence: u32,
}

pub struct Profile {
    pub service: ServiceId,
    map: Map,
    input: State,
    held: Held,
}
impl Profile {
    pub fn compile(service: ServiceId, descriptor: &[u8]) -> Result<Self, Error> {
        let map = Map::compile(descriptor).map_err(|e| match e {
            hid::Error::Capacity => Error::Capacity,
            _ => Error::UnsupportedHid,
        })?;
        let input = map.state().map_err(|_| Error::Capacity)?;
        Ok(Self {
            service,
            map,
            input,
            held: Held::default(),
        })
    }
}
#[derive(Clone, Copy)]
enum Writing {
    Hidpp,
    Leds,
}
pub struct Output<'a> {
    pub id: WriteId,
    pub service: ServiceId,
    /// None is an unnumbered report. Payload never includes the report ID.
    pub report_id: Option<u8>,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy)]
pub struct BatteryRead {
    pub id: WriteId,
    pub service: ServiceId,
    pub kind: crate::bluetooth::ReportType,
    pub report_id: Option<u8>,
}
pub struct Link {
    pub id: LinkId,
    pub client: Client,
    pub settings: Engine,
    pub roles: u8,
    /// Bit 0: unsupported HID fields; bit 1: unavailable lock-indicator output.
    pub warnings: u8,
    profiles: Box<[Profile]>,
    vendor_service: Option<ServiceId>,
    enabled: bool,
    platform: HostPlatform,
    configure_pending: bool,
    activate_pending: bool,
    forward_pending: bool,
    normalization: NormalizationState,
    sequence: u32,
    writing: Option<Writing>,
    write_deadline: u64,
    output: [u8; hid::REPORT_BYTES],
    max_output: usize,
    sent_leds: Option<u8>,
    led_target: u8,
    led_profile: usize,
    led_report: usize,
    pub(crate) info_refresh_pending: bool,
    battery_read: Option<BatteryRead>,
    battery_read_valid: bool,
    battery_cursor: usize,
    battery_due: u64,
    battery_initial: bool,
    legacy_stage: u8,
    legacy_waiting: bool,
    legacy_valid: bool,
    legacy_flags: [u8; 3],
    legacy_register: u8,
    legacy_due: u64,
}
impl Link {
    /// Profiles are complete before admission. Only live connections own parsed
    /// maps and per-report state; saved disconnected devices retain their catalog.
    pub fn new(
        id: LinkId,
        profiles: Vec<Profile>,
        max_output: usize,
        enabled: bool,
        platform: HostPlatform,
        catalog: &mut Catalog,
    ) -> Result<Self, Error> {
        if id.slot as usize >= hid::SOURCES
            || id.generation == 0
            || profiles.is_empty()
            || profiles.len() > hid::REPORTS
        {
            return Err(Error::UnsupportedHid);
        }
        let max_output = max_output.min(hid::REPORT_BYTES);
        let mut roles = 0;
        let mut warnings = 0;
        let mut vendor_service = None;
        let mut reports = 0;
        for (i, profile) in profiles.iter().enumerate() {
            if profiles[..i].iter().any(|p| p.service == profile.service) {
                return Err(Error::UnsupportedHid);
            }
            roles |= profile.map.roles;
            warnings |= u8::from(profile.map.ignored_fields);
            if vendor_service.is_none()
                && profile.map.hidpp_reports != 0
                && max_output
                    >= if profile.map.hidpp_reports & hid::HIDPP_LONG != 0 {
                        crate::hidpp::PAYLOAD_BYTES
                    } else {
                        6
                    }
            {
                vendor_service = Some(profile.service);
                reports = profile.map.hidpp_reports;
            }
            if profile.map.reports().iter().any(|r| {
                r.leds && (r.output_other || (r.bits[1] as usize).div_ceil(8) > max_output)
            }) {
                warnings |= 2;
            }
        }
        catalog.info.battery.hidpp_reports(reports != 0);
        catalog.connection(true, enabled);
        let mut client = Client::new(reports);
        client.status = if enabled {
            NormalizationState::Pending
        } else {
            NormalizationState::Off
        };
        let mut settings = Engine::default();
        settings.state = SettingsState::Pending;
        Ok(Self {
            id,
            client,
            settings,
            roles,
            warnings,
            profiles: profiles.into_boxed_slice(),
            vendor_service,
            enabled,
            platform,
            configure_pending: true,
            activate_pending: false,
            forward_pending: false,
            normalization: NormalizationState::Pending,
            sequence: 0,
            writing: None,
            write_deadline: 0,
            output: [0; hid::REPORT_BYTES],
            max_output,
            sent_leds: None,
            led_target: 0,
            led_profile: 0,
            led_report: 0,
            info_refresh_pending: false,
            battery_read: None,
            battery_read_valid: false,
            battery_cursor: 0,
            battery_due: 0,
            battery_initial: true,
            legacy_stage: 0,
            legacy_waiting: false,
            legacy_valid: false,
            legacy_flags: [0; 3],
            legacy_register: 0x0d,
            legacy_due: 0,
        })
    }
    fn forward(&self, motion: [i64; 4], forward: &mut Forwarder) -> Result<(), Error> {
        let mut held = Held::default();
        for profile in &self.profiles {
            held = held
                .union(&profile.held)
                .map_err(|_| Error::InputOverflow)?;
        }
        if self.enabled {
            held = held
                .union(&self.client.held)
                .map_err(|_| Error::InputOverflow)?;
        }
        forward
            .input(self.id.slot as usize, Input { held, motion })
            .map_err(|_| Error::InputOverflow)
    }
    fn normalization_changed(&mut self, catalog: &mut Catalog) -> bool {
        let changed = self.normalization != self.client.status;
        if changed && self.client.status == NormalizationState::Resetting {
            catalog.invalidate();
        }
        self.normalization = self.client.status;
        changed
    }
    pub fn reconfigure(&mut self, enabled: bool, platform: HostPlatform, catalog: &mut Catalog) {
        self.legacy_valid = false;
        self.legacy_stage = 0;
        self.legacy_due = 0;
        self.battery_read_valid = false;
        self.battery_cursor = 0;
        self.battery_due = 0;
        self.battery_initial = true;
        self.settings.cancel(if enabled {
            Error::Cancelled
        } else {
            Error::HidppDisabled
        });
        self.enabled = enabled;
        self.platform = platform;
        catalog.connection(true, enabled);
        catalog.invalidate();
        self.configure_pending = true;
        self.activate_pending = false;
        self.forward_pending = true;
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Whether the device answered the protocol probe as HID++ 2.0 or later
    /// and has usable long reports, once that is known. HID++ 1.0 and a device
    /// without usable HID++ reports answer false; an error reply, timeout or
    /// transport failure leaves it unknown.
    pub fn hidpp_found(&self) -> Option<bool> {
        let client = &self.client;
        if client.protocol[0] >= 2 {
            Some(true)
        } else if client.protocol[0] == 1 || client.error == Some(crate::hidpp::Error::NoReports) {
            Some(false)
        } else {
            None
        }
    }
    pub fn busy(&self) -> bool {
        self.configure_pending || self.activate_pending || self.settings.busy()
    }
    pub fn start_information(&mut self, catalog: &mut Catalog, now: u64) -> Result<(), Error> {
        self.battery_cursor = 0;
        self.battery_due = 0;
        self.battery_initial = true;
        self.legacy_due = 0;
        if self.vendor_service.is_none() || !self.enabled {
            return Ok(());
        }
        if self.busy() {
            return Err(Error::Busy);
        }
        self.settings.start_information(catalog, now)
    }
    pub fn start_settings(
        &mut self,
        catalog: &mut Catalog,
        apply: bool,
        key: Option<SettingKey>,
        explicit: bool,
        now: u64,
    ) -> Result<(), Error> {
        if self.busy() {
            return Err(Error::Busy);
        }
        // A manual read retries an earlier failed read-only protocol probe.
        if !self.enabled && self.client.protocol[0] == 0 && self.client.quiesce() {
            self.client.configure(false, self.platform, now);
        }
        self.settings
            .start(catalog, apply, key, explicit, false, now)
    }
    /// The backend already checked connection generation and supplies the real
    /// service/report ID. Unknown services cannot update another report map.
    pub fn input(
        &mut self,
        service: ServiceId,
        report_id: u8,
        payload: &[u8],
        catalog: &mut Catalog,
        forward: &mut Forwarder,
        now: u64,
    ) -> Result<bool, Error> {
        let profile = self
            .profiles
            .iter_mut()
            .find(|p| p.service == service)
            .ok_or(Error::ConnectionFailed)?;
        let vendor = (report_id == 0x10 && profile.map.hidpp_reports & hid::HIDPP_SHORT != 0)
            || (report_id == 0x11 && profile.map.hidpp_reports & hid::HIDPP_LONG != 0);
        if vendor {
            if Some(service) != self.vendor_service {
                return Ok(false);
            }
            if catalog.info.battery.vendor()
                && self.client.protocol[0] == 1
                && payload.len() >= 6
                && payload[0] == 0xff
                && matches!(payload[1], 7 | 0x0d)
            {
                legacy_notification(&mut catalog.info.battery, payload);
                if self.legacy_waiting && self.legacy_stage == 2 {
                    self.legacy_valid = false;
                    self.legacy_due = now.saturating_add(60_000);
                }
            }
            let mut changed =
                !self.configure_pending && self.settings.receive(catalog, report_id, payload, now);
            if self.client.receive(report_id, payload, now) {
                self.forward([0; 4], forward)?;
            }
            changed |= self.normalization_changed(catalog);
            return Ok(changed);
        }
        if catalog.info.battery.standard_hid() {
            if self.battery_read.is_some_and(|r| {
                r.service == service
                    && r.report_id.unwrap_or(0) == report_id
                    && r.kind == crate::bluetooth::ReportType::Input
            }) {
                self.battery_read_valid = false;
            }
            profile.map.battery(
                report_id,
                crate::bluetooth::ReportType::Input,
                payload,
                |instance, key, value| catalog.info.battery.hid_reading(instance, key, value),
            );
        }
        let input = match profile.map.decode(&mut profile.input, report_id, payload) {
            Ok(input) => input,
            Err(hid::Error::Rollover) => return Ok(false),
            Err(hid::Error::Overflow) => return Err(Error::InputOverflow),
            Err(_) => return Err(Error::ConnectionFailed),
        };
        profile.held = input.held;
        self.forward(input.motion, forward)?;
        Ok(false)
    }
    /// Poll even while a transport write is outstanding, so HID++ timeouts and
    /// cancellation progress without confusing a reply with write completion.
    pub fn poll(
        &mut self,
        catalog: &mut Catalog,
        forward: &mut Forwarder,
        now: u64,
    ) -> Result<bool, Error> {
        if self.forward_pending {
            self.forward([0; 4], forward)?;
            self.forward_pending = false;
        }
        if self.writing.is_some() && now >= self.write_deadline {
            return Err(Error::ConnectionFailed);
        }
        if self.client.tick(now) {
            self.forward([0; 4], forward)?;
        }
        self.poll_legacy(catalog, now);
        let mut changed = self.settings.poll(catalog, &mut self.client, now);
        if self.configure_pending && self.client.quiesce() {
            self.configure_pending = false;
            self.client.configure(self.enabled, self.platform, now);
            self.forward([0; 4], forward)?;
            self.activate_pending = true;
            changed = true;
        }
        if self.activate_pending && self.client.idle() && !self.settings.busy() {
            self.settings.activate(catalog, now)?;
            self.activate_pending = false;
            changed = true;
        }
        changed |= self.normalization_changed(catalog);
        Ok(changed)
    }
    /// Call only when the backend has room to own a copy through completion.
    /// Do not call again until output_complete or connection teardown.
    pub fn output(&mut self, leds: u8, now: u64) -> Result<Option<Output<'_>>, Error> {
        if self.writing.is_some() || self.battery_read.is_some() {
            return Ok(None);
        }
        if let Some(payload) = self.client.next_output(now) {
            let report = self.client.output_report();
            let length = if report == 0x10 { 6 } else { payload.len() };
            self.output[..length].copy_from_slice(&payload[..length]);
            let service = self.vendor_service.ok_or(Error::InternalError)?;
            return self
                .begin_write(service, Some(report), length, Writing::Hidpp, now)
                .map(Some);
        }
        let leds = leds & 0x1f;
        if self.sent_leds == Some(leds) {
            return Ok(None);
        }
        if self.led_target != leds {
            self.led_target = leds;
            self.led_profile = 0;
            self.led_report = 0;
        }
        while self.led_profile < self.profiles.len() {
            let p = &self.profiles[self.led_profile];
            while self.led_report < p.map.reports().len() {
                let index = self.led_report;
                self.led_report += 1;
                let Some(length) =
                    p.map
                        .led_report(index, leds, &mut self.output[..self.max_output])
                else {
                    continue;
                };
                let report_id = p.map.numbered.then_some(p.map.reports()[index].id);
                return self
                    .begin_write(p.service, report_id, length, Writing::Leds, now)
                    .map(Some);
            }
            self.led_profile += 1;
            self.led_report = 0;
        }
        self.sent_leds = Some(leds);
        self.led_profile = 0;
        self.led_report = 0;
        Ok(None)
    }
    fn poll_legacy(&mut self, catalog: &mut Catalog, now: u64) {
        if self.legacy_waiting {
            let Some(result) = self.client.response() else {
                return;
            };
            self.legacy_waiting = false;
            if !self.legacy_valid || !catalog.info.battery.vendor() {
                return;
            }
            match (self.legacy_stage, result) {
                (0, Ok(p)) if p.bytes().len() >= 3 => {
                    self.legacy_flags.copy_from_slice(&p.bytes()[..3]);
                    self.legacy_flags[0] |= 0x10;
                    self.legacy_stage = 1;
                }
                (0 | 1, _) => self.legacy_stage = 2,
                (2, Err(crate::hidpp::Error::Device(2))) if self.legacy_register == 0x0d => {
                    self.legacy_register = 7;
                }
                (2, result) => {
                    match result {
                        Ok(p) => legacy_battery(
                            &mut catalog.info.battery,
                            self.legacy_register,
                            p.bytes(),
                        ),
                        Err(_) => catalog.info.battery.vendor_reading(None, None),
                    }
                    self.legacy_due = now.saturating_add(60_000);
                }
                _ => {}
            }
        }
        if !catalog.info.battery.vendor()
            || self.client.protocol[0] != 1
            || self.configure_pending
            || self.activate_pending
            || self.settings.busy()
            || !self.client.idle()
            || now < self.legacy_due
        {
            return;
        }
        let register = if self.legacy_stage < 2 {
            0
        } else {
            self.legacy_register
        };
        self.legacy_valid = true;
        self.legacy_waiting = self.client.register(
            self.legacy_stage == 1,
            register,
            if self.legacy_stage == 1 {
                &self.legacy_flags
            } else {
                &[]
            },
            now,
        );
    }
    pub fn battery_busy(&self, catalog: &Catalog) -> bool {
        (catalog.info.battery.standard_hid()
            && (self.battery_read.is_some() || self.battery_initial))
            || (catalog.info.battery.vendor()
                && self.client.protocol[0] == 1
                && (self.legacy_waiting || self.legacy_due == 0))
    }
    pub fn battery_read(&mut self, catalog: &Catalog, now: u64) -> Option<BatteryRead> {
        if !catalog.info.battery.standard_hid()
            || self.writing.is_some()
            || self.battery_read.is_some()
            || !self.client.idle()
            || self.busy()
            || now < self.battery_due
        {
            return None;
        }
        loop {
            let candidate = self
                .profiles
                .iter()
                .flat_map(|p| {
                    p.map
                        .battery_reports()
                        .map(move |(id, kind)| (p.service, p.map.numbered.then_some(id), kind))
                })
                .nth(self.battery_cursor);
            self.battery_cursor += 1;
            let Some((service, report_id, kind)) = candidate else {
                self.battery_cursor = 0;
                self.battery_initial = false;
                self.battery_due = now.saturating_add(60_000);
                return None;
            };
            if !self.battery_initial && kind == crate::bluetooth::ReportType::Input {
                continue;
            }
            self.sequence = self.sequence.checked_add(1)?;
            let request = BatteryRead {
                id: WriteId {
                    link: self.id,
                    sequence: self.sequence,
                },
                service,
                kind,
                report_id,
            };
            self.battery_read = Some(request);
            self.battery_read_valid = true;
            return Some(request);
        }
    }
    pub fn battery_read_complete(
        &mut self,
        id: WriteId,
        kind: crate::bluetooth::ReportType,
        result: Result<&crate::bluetooth::InputReport, Error>,
        catalog: &mut Catalog,
    ) {
        let Some(request) = self.battery_read.filter(|r| r.id == id) else {
            return;
        };
        self.battery_read = None;
        if !self.battery_read_valid || !catalog.info.battery.standard_hid() || kind != request.kind
        {
            return;
        }
        let Some(p) = self.profiles.iter().find(|p| p.service == request.service) else {
            return;
        };
        let bytes = result
            .ok()
            .filter(|r| {
                r.link == id.link
                    && r.service == request.service
                    && r.report_id == request.report_id.unwrap_or(0)
            })
            .map_or(&[][..], |r| r.payload());
        p.map.battery(
            request.report_id.unwrap_or(0),
            kind,
            bytes,
            |instance, key, value| catalog.info.battery.hid_reading(instance, key, value),
        );
    }
    fn begin_write(
        &mut self,
        service: ServiceId,
        report_id: Option<u8>,
        length: usize,
        writing: Writing,
        now: u64,
    ) -> Result<Output<'_>, Error> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(Error::ConnectionFailed)?;
        self.writing = Some(writing);
        self.write_deadline = now.saturating_add(30_000);
        Ok(Output {
            id: WriteId {
                link: self.id,
                sequence: self.sequence,
            },
            service,
            report_id,
            payload: &self.output[..length],
        })
    }
    pub fn output_complete(
        &mut self,
        id: WriteId,
        success: bool,
        catalog: &mut Catalog,
        forward: &mut Forwarder,
        now: u64,
    ) -> Result<bool, Error> {
        if id.link != self.id || id.sequence != self.sequence {
            return Ok(false);
        }
        let Some(writing) = self.writing.take() else {
            return Ok(false);
        };
        match writing {
            Writing::Hidpp => {
                if self.client.tx_complete(success, now) {
                    self.forward([0; 4], forward)?;
                }
            }
            Writing::Leds if !success => self.warnings |= 2,
            Writing::Leds => {}
        }
        Ok(self.normalization_changed(catalog))
    }
    /// Call before dropping a link, including failed setup after input admission.
    /// The manager retains explicit job results until their terminal response.
    pub fn disconnected(&mut self, catalog: &mut Catalog, forward: &mut Forwarder) {
        self.settings.disconnected(catalog, &self.client);
        forward.remove(self.id.slot as usize);
        self.writing = None;
    }
}

fn legacy_notification(battery: &mut crate::battery::Battery, payload: &[u8]) {
    if payload.len() < 6 {
        return;
    }
    let data = if payload[1] == 7 {
        [payload[2], payload[3], 0]
    } else {
        [payload[3], 0, payload[4]]
    };
    legacy_battery(battery, payload[1], &data);
}

fn legacy_battery(battery: &mut crate::battery::Battery, register: u8, p: &[u8]) {
    if p.len() < 3 {
        battery.vendor_reading(None, None);
        return;
    }
    let (percent, charging) = if register == 0x0d {
        (
            (p[0] <= 100).then_some(p[0]),
            match p[2] & 0xf0 {
                0x30 | 0x90 => Some(false),
                0x50 => Some(true),
                _ => None,
            },
        )
    } else {
        (
            match p[0] {
                1 | 2 => Some(5),
                3 | 4 => Some(20),
                5 | 6 => Some(75),
                7 => Some(100),
                _ => None,
            },
            match p[1] {
                0 => Some(false),
                0x21 | 0x24 | 0x25 => Some(true),
                0x22 | 0x26 => Some(false),
                _ => None,
            },
        )
    };
    battery.vendor_reading(percent, charging);
}

#[cfg(test)]
mod battery_tests {
    use super::*;
    use cordial_protocol::{identifiers::Transport, info::InfoKey, settings::SettingValue};
    #[test]
    fn legacy_registers_and_notifications() {
        let mut b = crate::battery::Battery::default();
        b.configure(Transport::Classic, true);
        b.connection(true, true);
        for (status, expected) in [
            (0, Some(false)),
            (0x21, Some(true)),
            (0x22, Some(false)),
            (0x23, None),
            (0x24, Some(true)),
            (0x25, Some(true)),
            (0x26, Some(false)),
            (0x20, None),
        ] {
            legacy_battery(&mut b, 7, &[4, status, 0]);
            assert_eq!(
                b.field(InfoKey::BatteryPercent).value,
                SettingValue::Integer(20)
            );
            assert_eq!(
                b.field(InfoKey::BatteryCharging).value,
                expected.map_or(SettingValue::Null, SettingValue::Bool)
            );
        }
        legacy_notification(&mut b, &[0xff, 7, 6, 0x24, 0, 0]);
        assert_eq!(
            b.field(InfoKey::BatteryPercent).value,
            SettingValue::Integer(75)
        );
        assert_eq!(
            b.field(InfoKey::BatteryCharging).value,
            SettingValue::Bool(true)
        );
        legacy_notification(&mut b, &[0xff, 0x0d, 0, 51, 0x50, 0]);
        assert_eq!(
            b.field(InfoKey::BatteryPercent).value,
            SettingValue::Integer(51)
        );
        assert_eq!(
            b.field(InfoKey::BatteryCharging).value,
            SettingValue::Bool(true)
        );
    }
}
