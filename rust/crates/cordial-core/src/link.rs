//! A live HID connection. Backends copy submitted bytes before returning and
//! report completion with the same token; callbacks never borrow this owner.
use crate::model::{
    errors::{DeviceWarning, ErrorCode as Error, WarningCode},
    hidpp::ProtocolState,
    identifiers::{HostPlatform, NormalizationState, SettingsState},
    settings::SettingKey,
};
use crate::{
    bluetooth::ReportType,
    features::Engine,
    forward::Forwarder,
    hid::{self, Held, Input, Map, State},
    hidpp::Client,
    settings::Catalog,
};
use alloc::{boxed::Box, vec::Vec};

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

#[derive(Clone, Copy)]
struct IndicatorState {
    report: usize,
    kind: crate::bluetooth::ReportType,
    settled: Option<u8>,
    retry_at: u64,
    rearm: bool,
}
impl Default for IndicatorState {
    fn default() -> Self {
        Self {
            report: 0,
            kind: crate::bluetooth::ReportType::Output,
            settled: None,
            retry_at: 0,
            rearm: true,
        }
    }
}
pub struct Profile {
    pub service: ServiceId,
    map: Map,
    input: State,
    held: Held,
    indicators: Box<[IndicatorState]>,
    indicator_cache: hid::IndicatorCache,
    colors_ready: bool,
}
impl Profile {
    fn indicator(&self, report: usize) -> &IndicatorState {
        &self.indicators[report]
    }
    fn indicator_mut(&mut self, report: usize) -> &mut IndicatorState {
        &mut self.indicators[report]
    }
    pub fn compile(service: ServiceId, descriptor: &[u8]) -> Result<Self, Error> {
        let map = Map::compile(descriptor).map_err(|e| match e {
            hid::Error::Capacity => Error::Capacity,
            _ => Error::UnsupportedHid,
        })?;
        Self::from_map(service, map)
    }
    pub(crate) fn from_map(service: ServiceId, map: Map) -> Result<Self, Error> {
        let input = map.state().map_err(|_| Error::Capacity)?;
        let mut indicators = Vec::new();
        indicators
            .try_reserve_exact(map.indicator_reports().count())
            .map_err(|_| Error::Capacity)?;
        indicators.extend(
            map.indicator_reports()
                .map(|(report, kind)| IndicatorState {
                    report,
                    kind,
                    ..IndicatorState::default()
                }),
        );
        let indicator_cache = map.indicator_cache().map_err(|_| Error::Capacity)?;
        Ok(Self {
            indicators: indicators.into_boxed_slice(),
            indicator_cache,
            colors_ready: false,
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
    Leds {
        profile: usize,
        report: usize,
        target: u8,
        rearm: bool,
        unknown: u8,
        complete: bool,
    },
}
pub struct Output<'a> {
    pub id: WriteId,
    pub service: ServiceId,
    pub kind: crate::bluetooth::ReportType,
    /// None is an unnumbered report. Payload never includes the report ID.
    pub report_id: Option<u8>,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy)]
pub struct ReportRead {
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
    pub warnings: Vec<DeviceWarning>,
    profiles: Box<[Profile]>,
    vendor_service: Option<ServiceId>,
    enabled: bool,
    platform: HostPlatform,
    configure_pending: bool,
    activate_pending: bool,
    forward_pending: bool,
    normalization: NormalizationState,
    protocol: ProtocolState,
    sequence: u32,
    writing: Option<Writing>,
    write_deadline: u64,
    output: [u8; hid::REPORT_BYTES],
    led_read: Option<ReportRead>,
    led_read_needed: bool,
    led_read_for_state: bool,
    led_color_read: bool,
    led_feedback_cursor: usize,
    led_read_attempted: bool,
    led_base_valid: bool,
    led_base: [u8; hid::REPORT_BYTES],
    led_base_len: usize,
    led_retry: u64,
    sent_leds: Option<u8>,
    led_target: u8,
    led_profile: usize,
    led_report: usize,
    pub(crate) info_refresh_pending: bool,
    battery_read: Option<ReportRead>,
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
        let mut warnings = Vec::new();
        let warning_capacity: usize = profiles
            .iter()
            .map(|p| {
                p.map.limitations().len()
                    + p.map.indicator_field_count()
                    + p.indicators
                        .iter()
                        .map(|state| {
                            p.map
                                .indicator_locations_kind(state.report, state.kind, 31)
                                .count()
                                + 1
                        })
                        .sum::<usize>()
            })
            .sum();
        warnings
            .try_reserve_exact(warning_capacity)
            .map_err(|_| Error::Capacity)?;
        let mut vendor_service = None;
        let mut reports = 0;
        for (i, profile) in profiles.iter().enumerate() {
            if profiles[..i].iter().any(|p| p.service == profile.service) {
                return Err(Error::UnsupportedHid);
            }
            roles |= profile.map.roles;
            warnings.extend(profile.map.limitations().iter().map(|l| DeviceWarning {
                code: l.code,
                service: profile.service.0,
                report_id: profile.map.numbered.then_some(l.report_id),
                report_type: Some(match l.kind {
                    1 => ReportType::Output,
                    2 => ReportType::Feature,
                    _ => ReportType::Input,
                }),
                bit_offset: Some(l.bit_offset),
                usage_page: Some(l.usage_page),
                usage: Some(l.usage),
            }));
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
            protocol: ProtocolState::Unknown,
            sequence: 0,
            writing: None,
            write_deadline: 0,
            output: [0; hid::REPORT_BYTES],
            led_read: None,
            led_read_needed: false,
            led_read_for_state: false,
            led_color_read: false,
            led_feedback_cursor: 0,
            led_read_attempted: false,
            led_base_valid: false,
            led_base: [0; hid::REPORT_BYTES],
            led_base_len: 0,
            led_retry: 0,
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
    fn forward(&self, mut input: Input, forward: &mut Forwarder) -> Result<(), Error> {
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
            .input(self.id.slot as usize, {
                input.held = held;
                input
            })
            .map_err(|_| Error::InputOverflow)
    }
    fn hidpp_changed(&mut self, catalog: &mut Catalog) -> bool {
        let normalization_changed = self.normalization != self.client.status;
        let changed = normalization_changed || self.protocol != self.client.protocol;
        if normalization_changed && self.client.status == NormalizationState::Resetting {
            catalog.invalidate();
        }
        self.normalization = self.client.status;
        self.protocol = self.client.protocol;
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
        match client.protocol {
            ProtocolState::Detected { major, .. } => {
                Some(major >= 2 && client.feature_error().is_none())
            }
            ProtocolState::Unavailable => Some(false),
            _ => None,
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
        if !self.enabled && self.client.protocol.major() == 0 && self.client.quiesce() {
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
                && self.client.protocol.major() == 1
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
                self.forward(Input::default(), forward)?;
            }
            changed |= self.hidpp_changed(catalog);
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
        let numeric = profile.map.observe_numeric_indicators(
            &mut profile.indicator_cache,
            report_id,
            crate::bluetooth::ReportType::Input,
            payload,
            true,
        );
        let boolean = profile.map.observe_indicator_feedback(
            &mut profile.indicator_cache,
            report_id,
            crate::bluetooth::ReportType::Input,
            payload,
            true,
        );
        if numeric
            && profile
                .indicators
                .iter()
                .all(|state| state.settled == Some(self.led_target))
        {
            profile.map.capture_colors(&mut profile.indicator_cache, 31);
        }
        if numeric || boolean {
            for state in &mut profile.indicators {
                state.settled = None;
            }
            self.sent_leds = None;
            self.led_retry = 0;
        }
        profile.held = input.held;
        self.forward(input, forward)?;
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
            self.forward(Input::default(), forward)?;
            self.forward_pending = false;
        }
        if self.writing.is_some() && now >= self.write_deadline {
            return Err(Error::ConnectionFailed);
        }
        if self.client.tick(now) {
            self.forward(Input::default(), forward)?;
        }
        self.poll_legacy(catalog, now);
        let mut changed = self.settings.poll(catalog, &mut self.client, now);
        if self.configure_pending && self.client.quiesce() {
            self.configure_pending = false;
            self.client.configure(self.enabled, self.platform, now);
            self.forward(Input::default(), forward)?;
            self.activate_pending = true;
            changed = true;
        }
        if self.activate_pending && self.client.idle() && !self.settings.busy() {
            self.settings.activate(catalog, now)?;
            self.activate_pending = false;
            changed = true;
        }
        changed |= self.hidpp_changed(catalog);
        Ok(changed)
    }
    /// Call only when the backend has room to own a copy through completion.
    /// Do not call again until output_complete or connection teardown.
    pub fn output(&mut self, leds: u8, now: u64) -> Result<Option<Output<'_>>, Error> {
        if self.writing.is_some() || self.battery_read.is_some() || self.led_read.is_some() {
            return Ok(None);
        }
        if let Some(payload) = self.client.next_output(now) {
            let report = self.client.output_report();
            let length = if report == 0x10 { 6 } else { payload.len() };
            self.output[..length].copy_from_slice(&payload[..length]);
            let service = self.vendor_service.ok_or(Error::InternalError)?;
            return self
                .begin_write(
                    service,
                    crate::bluetooth::ReportType::Output,
                    Some(report),
                    length,
                    Writing::Hidpp,
                    now,
                )
                .map(Some);
        }
        let leds = leds & 0x1f;
        if self.sent_leds == Some(leds) && self.led_target == leds {
            return Ok(None);
        }
        if self.led_target != leds {
            self.sent_leds = None;
            self.led_feedback_cursor = 0;
            self.led_read_attempted = false;
            self.led_target = leds;
            self.led_profile = 0;
            self.led_report = 0;
            self.led_base_valid = false;
            self.led_read_needed = false;
            for p in &mut self.profiles {
                p.colors_ready = false;
                for state in &mut p.indicators {
                    state.rearm = true;
                    state.retry_at = 0;
                    state.settled = None;
                }
            }
            self.led_retry = 0;
        }
        if now < self.led_retry {
            return Ok(None);
        }
        while self.led_profile < self.profiles.len() {
            while self.led_report < self.profiles[self.led_profile].indicators.len() {
                let index = self.led_report;
                let p = &self.profiles[self.led_profile];
                let state = p.indicator(index);
                let report_index = state.report;
                let kind = state.kind;
                if p.indicator(index).settled == Some(leds) || now < p.indicator(index).retry_at {
                    self.advance_indicator();
                    continue;
                }
                if !p.colors_ready && p.map.color_reports().next().is_some() {
                    self.led_read_needed = true;
                    return Ok(None);
                }
                let rearm =
                    p.indicator(index).rearm && p.map.relative_indicators_kind(report_index, kind);
                let baseline = self
                    .led_base_valid
                    .then_some(&self.led_base[..self.led_base_len]);
                let result = p.map.indicator_report_scoped(
                    report_index,
                    kind,
                    leds,
                    &p.indicator_cache,
                    baseline,
                    rearm,
                    &mut self.output,
                );
                match result {
                    Ok(Some(encoding)) => {
                        if encoding.unknown != 0 && !self.led_read_attempted {
                            self.led_read_for_state = true;
                            self.led_read_needed = true;
                            return Ok(None);
                        }
                        let report_id = p.map.numbered.then_some(p.map.reports()[report_index].id);
                        let service = p.service;
                        let p = &mut self.profiles[self.led_profile];
                        p.map
                            .begin_numeric_write(&mut p.indicator_cache, report_index, kind);
                        return self
                            .begin_write(
                                service,
                                kind,
                                report_id,
                                encoding.length,
                                Writing::Leds {
                                    profile: self.led_profile,
                                    report: index,
                                    target: leds,
                                    rearm,
                                    unknown: encoding.unknown,
                                    complete: encoding.complete,
                                },
                                now,
                            )
                            .map(Some);
                    }
                    Err(hid::IndicatorFailure {
                        reason:
                            reason @ (hid::IndicatorError::ReadRequired
                            | hid::IndicatorError::StateUnknown),
                        ..
                    }) if !self.led_read_attempted => {
                        self.led_read_for_state = reason == hid::IndicatorError::StateUnknown;
                        self.led_read_needed = true;
                        return Ok(None);
                    }
                    Err(failure) => {
                        let code = match failure.reason {
                            hid::IndicatorError::ReadRequired => WarningCode::IndicatorReadFailed,
                            hid::IndicatorError::StateUnknown => WarningCode::IndicatorStateUnknown,
                            hid::IndicatorError::ArrayCapacity => WarningCode::IndicatorArrayFull,
                            hid::IndicatorError::RelativeArray => {
                                WarningCode::IndicatorRelativeSelectorUnsupported
                            }
                            hid::IndicatorError::Range => WarningCode::IndicatorRangeUnsupported,
                            hid::IndicatorError::Buffered => {
                                WarningCode::BufferedIndicatorUnsupported
                            }
                            hid::IndicatorError::Mode => WarningCode::IndicatorModeUnsupported,
                            hid::IndicatorError::Nonlinear => {
                                WarningCode::IndicatorNonlinearUnsupported
                            }
                            hid::IndicatorError::Scale => WarningCode::IndicatorScaleUnsupported,
                        };
                        self.indicator_field_warning(
                            self.led_profile,
                            index,
                            code,
                            failure.bit_offset,
                            failure.usage,
                        );
                        self.profiles[self.led_profile].indicator_mut(index).settled = Some(leds);
                    }
                    Ok(None) => {}
                }
                self.advance_indicator();
            }
            self.led_profile += 1;
            self.led_report = 0;
        }
        let retry = self
            .profiles
            .iter()
            .flat_map(|p| p.indicators.iter())
            .filter(|s| s.settled != Some(leds) && s.retry_at != 0)
            .map(|s| s.retry_at)
            .min();
        if let Some(retry) = retry {
            self.led_retry = retry;
        } else {
            self.led_retry = 0;
            if self
                .profiles
                .iter()
                .flat_map(|p| p.indicators.iter())
                .all(|s| s.settled == Some(leds))
            {
                self.sent_leds = Some(leds);
            }
        }
        self.led_profile = 0;
        self.led_report = 0;
        self.led_feedback_cursor = 0;
        self.led_read_attempted = false;
        Ok(None)
    }
    fn advance_indicator(&mut self) {
        self.led_report += 1;
        self.led_base_valid = false;
        self.led_feedback_cursor = 0;
        self.led_read_attempted = false;
    }
    fn indicator_warning(&mut self, profile: usize, report: usize, code: WarningCode) {
        self.indicator_field_warning(profile, report, code, None, None);
    }
    fn indicator_field_warning(
        &mut self,
        profile: usize,
        report: usize,
        code: WarningCode,
        bit_offset: Option<u16>,
        usage: Option<u32>,
    ) {
        let p = &self.profiles[profile];
        let r = &p.map.reports()[p.indicator(report).report];
        let warning = DeviceWarning {
            code,
            service: p.service.0,
            report_id: p.map.numbered.then_some(r.id),
            report_type: Some(p.indicator(report).kind),
            bit_offset,
            usage_page: Some(usage.map_or(8, |u| (u >> 16) as u16)),
            usage: usage.map(|u| u as u16),
        };
        self.warnings.retain(|w| {
            matches!(
                w.code,
                WarningCode::NumericSelectorUnsupported
                    | WarningCode::PointerSelectorUnsupported
                    | WarningCode::BufferedInputUnsupported
            ) || w.service != warning.service
                || w.report_id != warning.report_id
                || w.report_type != warning.report_type
                || w.bit_offset != warning.bit_offset
                || w.usage_page != warning.usage_page
                || w.usage != warning.usage
        });
        self.warnings.push(warning);
    }
    fn read_indicator_warning(&mut self, request: ReportRead, code: WarningCode) {
        self.warnings.retain(|w| {
            !(matches!(
                w.code,
                WarningCode::IndicatorReadFailed
                    | WarningCode::IndicatorReadUnsupported
                    | WarningCode::IndicatorReportTooLarge
            ) && w.service == request.service.0
                && w.report_id == request.report_id
                && w.report_type == Some(request.kind)
                && w.bit_offset.is_none())
        });
        self.warnings.push(DeviceWarning {
            code,
            service: request.service.0,
            report_id: request.report_id,
            report_type: Some(request.kind),
            bit_offset: None,
            usage_page: Some(8),
            usage: None,
        });
    }
    fn clear_indicator_warning(&mut self, profile: usize, report: usize) {
        let p = &self.profiles[profile];
        let report_id = p
            .map
            .numbered
            .then_some(p.map.reports()[p.indicator(report).report].id);
        let report_type = Some(p.indicator(report).kind);
        self.warnings.retain(|w| {
            matches!(
                w.code,
                WarningCode::NumericSelectorUnsupported
                    | WarningCode::PointerSelectorUnsupported
                    | WarningCode::BufferedInputUnsupported
            ) || w.service != p.service.0
                || w.report_id != report_id
                || w.report_type != report_type
        });
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
            || self.client.protocol.major() != 1
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
                && self.client.protocol.major() == 1
                && (self.legacy_waiting || self.legacy_due == 0))
    }
    pub fn report_read(&mut self, catalog: &Catalog, now: u64) -> Option<ReportRead> {
        if self.led_read_needed
            && self.writing.is_none()
            && self.battery_read.is_none()
            && self.led_read.is_none()
            && now >= self.led_retry
        {
            let p = &mut self.profiles[self.led_profile];
            self.sequence = self.sequence.checked_add(1)?;
            let color = !p.colors_ready && p.map.color_reports().next().is_some();
            if color && self.led_feedback_cursor == 0 {
                p.indicator_cache.begin_colors();
            }
            let feedback = if color {
                p.map.color_reports().nth(self.led_feedback_cursor)
            } else {
                p.map.indicator_feedback().nth(self.led_feedback_cursor)
            };
            if color && feedback.is_none() {
                p.map.capture_colors(&mut p.indicator_cache, 31);
                p.colors_ready = true;
                self.led_feedback_cursor = 0;
                self.led_read_needed = false;
                return None;
            }
            self.led_color_read = color;
            if !color && feedback.is_none() && self.led_read_for_state {
                self.led_read_attempted = true;
                self.led_read_needed = false;
                return None;
            }
            self.led_feedback_cursor += 1;
            let state = p.indicator(self.led_report);
            let actual_id = p.map.reports()[state.report].id;
            let actual_kind = state.kind;
            let (report_id, kind) = feedback.unwrap_or((actual_id, actual_kind));
            if !color && report_id == actual_id && kind == actual_kind {
                self.led_read_attempted = true;
            }
            p.indicator_cache.begin_read();
            let request = ReportRead {
                id: WriteId {
                    link: self.id,
                    sequence: self.sequence,
                },
                service: p.service,
                kind,
                report_id: p.map.numbered.then_some(report_id),
            };
            self.led_read_needed = false;
            self.led_read = Some(request);
            return Some(request);
        }
        if !catalog.info.battery.standard_hid()
            || self.writing.is_some()
            || self.battery_read.is_some()
            || self.led_read.is_some()
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
            let request = ReportRead {
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
    pub fn report_read_complete(
        &mut self,
        id: WriteId,
        kind: crate::bluetooth::ReportType,
        result: Result<&crate::bluetooth::InputReport, Error>,
        catalog: &mut Catalog,
        now: u64,
    ) {
        if let Some(request) = self.led_read.filter(|r| r.id == id) {
            self.led_read = None;
            let color_read = core::mem::take(&mut self.led_color_read);
            let p = &mut self.profiles[self.led_profile];
            let expected = p
                .map
                .reports()
                .iter()
                .find(|r| r.id == request.report_id.unwrap_or(0))
                .map_or(0, |r| {
                    usize::from(
                        r.bits[match request.kind {
                            crate::bluetooth::ReportType::Input => 0,
                            crate::bluetooth::ReportType::Output => 1,
                            crate::bluetooth::ReportType::Feature => 2,
                        }],
                    )
                    .div_ceil(8)
                });
            let bytes = result
                .ok()
                .filter(|r| {
                    kind == request.kind
                        && r.link == id.link
                        && r.service == request.service
                        && r.report_id == request.report_id.unwrap_or(0)
                        && r.payload().len() >= expected
                })
                .map(|r| r.payload());
            if let Some(bytes) = bytes {
                self.warnings.retain(|w| {
                    !(matches!(
                        w.code,
                        WarningCode::IndicatorReadFailed
                            | WarningCode::IndicatorReadUnsupported
                            | WarningCode::IndicatorReportTooLarge
                    ) && w.service == request.service.0
                        && w.report_id == request.report_id
                        && w.report_type == Some(request.kind)
                        && w.bit_offset.is_none())
                });
                p.map.remember_indicator_values(
                    &mut p.indicator_cache,
                    request.report_id.unwrap_or(0),
                    kind,
                    bytes,
                );
                p.map.observe_numeric_indicators(
                    &mut p.indicator_cache,
                    request.report_id.unwrap_or(0),
                    kind,
                    bytes,
                    false,
                );
                if p.map.observe_indicator_feedback(
                    &mut p.indicator_cache,
                    request.report_id.unwrap_or(0),
                    kind,
                    bytes,
                    false,
                ) {
                    for state in &mut p.indicators {
                        state.settled = None;
                    }
                    self.sent_leds = None;
                }
                let state = p.indicator(self.led_report);
                if kind == state.kind
                    && request.report_id.unwrap_or(0) == p.map.reports()[state.report].id
                {
                    self.led_base[..bytes.len()].copy_from_slice(bytes);
                    self.led_base_len = bytes.len();
                    self.led_base_valid = true;
                }
            } else {
                let unsupported = matches!(result, Err(Error::UnsupportedHid));
                let permanent = unsupported || matches!(result, Err(Error::HidReportTooLarge));
                let code = if unsupported {
                    WarningCode::IndicatorReadUnsupported
                } else if matches!(result, Err(Error::HidReportTooLarge)) {
                    WarningCode::IndicatorReportTooLarge
                } else {
                    WarningCode::IndicatorReadFailed
                };
                let actual = p.indicator(self.led_report);
                let actual_read = !color_read
                    && request.kind == actual.kind
                    && request.report_id.unwrap_or(0) == p.map.reports()[actual.report].id;
                let cannot_write = actual_read
                    && p.map
                        .indicator_report_scoped(
                            actual.report,
                            actual.kind,
                            self.led_target,
                            &p.indicator_cache,
                            None,
                            false,
                            &mut self.output,
                        )
                        .is_err();
                self.read_indicator_warning(request, code);
                if !permanent || cannot_write {
                    let state = self.profiles[self.led_profile].indicator_mut(self.led_report);
                    if permanent {
                        state.settled = Some(self.led_target);
                    } else {
                        state.retry_at = now.saturating_add(1_000);
                    }
                    self.advance_indicator();
                }
            }
            return;
        }
        self.battery_read_complete(id, kind, result, catalog);
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
        kind: crate::bluetooth::ReportType,
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
            kind,
            report_id,
            payload: &self.output[..length],
        })
    }
    pub fn output_complete(
        &mut self,
        id: WriteId,
        result: Result<(), Error>,
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
                if self.client.tx_complete(result.is_ok(), now) {
                    self.forward(Input::default(), forward)?;
                }
            }
            Writing::Leds {
                profile,
                report,
                target,
                rearm,
                unknown,
                complete,
            } => {
                self.led_base_valid = false;
                if result.is_ok() {
                    if rearm {
                        self.profiles[profile].indicator_mut(report).rearm = false;
                    } else {
                        let p = &mut self.profiles[profile];
                        let state = *p.indicator(report);
                        p.map.remember_indicator_values(
                            &mut p.indicator_cache,
                            p.map.reports()[state.report].id,
                            state.kind,
                            &self.output,
                        );
                        let confirmed = p.map.numeric_written(
                            &mut p.indicator_cache,
                            state.report,
                            state.kind,
                            &self.output,
                            true,
                        );
                        p.map.observe_numeric_indicators(
                            &mut p.indicator_cache,
                            p.map.reports()[state.report].id,
                            state.kind,
                            &self.output,
                            false,
                        );
                        p.map.confirm_indicators(
                            &mut p.indicator_cache,
                            state.report,
                            state.kind,
                            target,
                            true,
                        );
                        let state = self.profiles[profile].indicator_mut(report);
                        state.settled = (complete && confirmed).then_some(target);
                        state.retry_at = 0;
                        state.rearm = true;
                        let p = &mut self.profiles[profile];
                        if p.indicators
                            .iter()
                            .all(|state| state.settled == Some(target))
                        {
                            p.map.capture_colors(&mut p.indicator_cache, target);
                        }
                        self.clear_indicator_warning(profile, report);
                        let p = &self.profiles[profile];
                        for (bit_offset, usage) in p.map.unknown_indicator_locations(
                            p.indicator(report).report,
                            p.indicator(report).kind,
                            unknown,
                            &p.indicator_cache,
                        ) {
                            self.warnings.push(DeviceWarning {
                                code: WarningCode::IndicatorStateUnknown,
                                service: p.service.0,
                                report_type: Some(p.indicator(report).kind),
                                report_id: p
                                    .map
                                    .numbered
                                    .then_some(p.map.reports()[p.indicator(report).report].id),
                                bit_offset: Some(bit_offset),
                                usage_page: Some((usage >> 16) as u16),
                                usage: Some(usage as u16),
                            });
                        }
                        if complete && confirmed {
                            self.advance_indicator();
                        } else if !confirmed {
                            self.led_feedback_cursor = 0;
                            self.led_read_attempted = false;
                        }
                    }
                } else {
                    let p = &mut self.profiles[profile];
                    let state = *p.indicator(report);
                    p.map.confirm_indicators(
                        &mut p.indicator_cache,
                        state.report,
                        state.kind,
                        target,
                        false,
                    );
                    p.map.numeric_written(
                        &mut p.indicator_cache,
                        state.report,
                        state.kind,
                        &self.output,
                        false,
                    );
                    let unsupported = matches!(
                        result,
                        Err(Error::UnsupportedHid | Error::HidReportTooLarge)
                    );
                    self.indicator_warning(
                        profile,
                        report,
                        if matches!(result, Err(Error::HidReportTooLarge)) {
                            WarningCode::IndicatorReportTooLarge
                        } else if unsupported {
                            WarningCode::IndicatorWriteUnsupported
                        } else {
                            WarningCode::IndicatorWriteFailed
                        },
                    );
                    if unsupported {
                        self.profiles[profile].indicator_mut(report).settled = Some(target);
                    }
                    self.profiles[profile].indicator_mut(report).retry_at =
                        now.saturating_add(1_000);
                    self.profiles[profile].indicator_mut(report).rearm = true;
                    self.advance_indicator();
                }
            }
        }
        Ok(self.hidpp_changed(catalog))
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
    use crate::model::{identifiers::Transport, info::InfoKey, settings::SettingValue};
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
