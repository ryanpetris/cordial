use crate::hid::{HIDPP_LONG, HIDPP_SHORT, Held};
use crate::model::{
    hidpp::ProtocolState,
    identifiers::{HostPlatform, NormalizationState},
    translation::control,
};

pub const PAYLOAD_BYTES: usize = 19;
pub const TIMEOUT_MS: u64 = 1500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    NoReports,
    ProtocolUnsupported,
    ResetUnavailable,
    ControlsUnavailable,
    Timeout,
    Transport,
    Device(u8),
    InvalidResponse,
}
impl Error {
    pub fn code(self) -> crate::model::errors::ErrorCode {
        use crate::model::errors::ErrorCode as C;
        match self {
            Self::NoReports => C::HidppReportsUnavailable,
            Self::ProtocolUnsupported => C::HidppProtocolUnsupported,
            Self::ResetUnavailable => C::HidppResetUnavailable,
            Self::ControlsUnavailable => C::HidppControlsUnavailable,
            Self::Timeout => C::HidppTimeout,
            Self::Transport => C::HidppTransportError,
            Self::Device(_) => C::HidppDeviceError,
            Self::InvalidResponse => C::HidppInvalidResponse,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Step {
    #[default]
    Idle,
    Protocol,
    ResetFeature,
    Reset,
    ControlsFeature,
    Count,
    Info,
    Reporting,
    Divert,
    Disable,
    Settings,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Response {
    bytes: [u8; 16],
    length: usize,
}
impl Response {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

pub struct Client {
    pub held: Held,
    pub status: NormalizationState,
    pub error: Option<Error>,
    pub protocol: ProtocolState,
    pub exchange_sent: bool,
    selected: u128,
    native_controls: [u128; 3],
    deadline: u64,
    cid: u16,
    reports: u8,
    platform: HostPlatform,
    active_platform: HostPlatform,
    reset_feature: u8,
    controls_feature: u8,
    controls_version: u8,
    step: Step,
    count: u8,
    index: u8,
    software_id: u8,
    request: [u8; PAYLOAD_BYTES],
    response: Option<Result<Response, Error>>,
    enabled: bool,
    queued: bool,
    waiting: bool,
    tx_busy: bool,
    reset_uncertain: bool,
}
impl Client {
    pub fn new(reports: u8) -> Self {
        Self {
            held: Held::default(),
            status: NormalizationState::Off,
            error: None,
            protocol: ProtocolState::Unknown,
            exchange_sent: false,
            selected: 0,
            native_controls: [0; 3],
            deadline: 0,
            cid: 0,
            reports,
            platform: HostPlatform::Linux,
            active_platform: HostPlatform::Linux,
            reset_feature: 0,
            controls_feature: 0,
            controls_version: 0,
            step: Step::Idle,
            count: 0,
            index: 0,
            software_id: 0,
            request: [0; PAYLOAD_BYTES],
            response: None,
            enabled: false,
            queued: false,
            waiting: false,
            tx_busy: false,
            reset_uncertain: false,
        }
    }
    /// Controls whose standard translation is already representable by native HID reports.
    pub fn native_controls(&mut self, masks: [u128; 3]) {
        self.native_controls = masks;
    }
    fn needs_translation(&self, index: usize, flags: u8) -> bool {
        let platform = match self.platform {
            HostPlatform::Linux => 0,
            HostPlatform::Windows => 1,
            HostPlatform::Mac => 2,
        };
        // Platform normalization uses its fixed activation policy. Other keyboard
        // mappings divert only when the descriptor cannot carry their standard input.
        index < crate::model::translation::NORMALIZED_CONTROLS
            || flags & 1 == 0
                && flags & 6 != 0
                && self.native_controls[platform] & (1 << index) == 0
    }
    fn clear_held(&mut self) -> bool {
        let changed = self.held != Held::default();
        self.held = Held::default();
        changed
    }
    fn fail(&mut self, error: Error) -> bool {
        if self.step == Step::Settings {
            self.response = Some(Err(error));
            self.queued = false;
            self.waiting = false;
            self.step = Step::Idle;
            return false;
        }
        if self.step == Step::Protocol && self.protocol.major() == 0 {
            self.protocol = ProtocolState::Error { code: error.code() };
        }
        let changed = (self.reset_uncertain || matches!(self.step, Step::Reset | Step::Disable))
            && self.clear_held();
        self.error = Some(error);
        self.status = if !self.enabled && self.step != Step::Disable {
            NormalizationState::Off
        } else if matches!(
            error,
            Error::NoReports
                | Error::ProtocolUnsupported
                | Error::ResetUnavailable
                | Error::ControlsUnavailable
        ) {
            NormalizationState::Unsupported
        } else {
            NormalizationState::Error
        };
        self.queued = false;
        self.waiting = false;
        self.step = Step::Idle;
        changed
    }
    fn request(&mut self, step: Step, feature: u8, function: u8, parameters: &[u8], now: u64) {
        self.request.fill(0);
        self.software_id = self.software_id % 15 + 1;
        self.request[0] = 0xff;
        self.request[1] = feature;
        self.request[2] = (function << 4) | self.software_id;
        self.request[3..3 + parameters.len()].copy_from_slice(parameters);
        self.step = step;
        self.queued = true;
        self.waiting = false;
        self.deadline = now + TIMEOUT_MS;
    }
    fn get_feature(&mut self, step: Step, feature: u16, now: u64) {
        self.request(step, 0, 0, &feature.to_be_bytes(), now);
    }
    pub fn idle(&self) -> bool {
        !self.queued
            && !self.waiting
            && !self.tx_busy
            && self.response.is_none()
            && self.step == Step::Idle
    }
    pub fn quiesce(&mut self) -> bool {
        if self.waiting || self.tx_busy {
            return false;
        }
        if self.queued && self.step == Step::Settings {
            self.fail(Error::Transport);
            return false;
        }
        if self.response.is_some() {
            return false;
        }
        // Unsent normalization requests stay valid until configure replaces them.
        true
    }
    pub fn exchange(&mut self, feature: u8, function: u8, parameters: &[u8], now: u64) -> bool {
        if self.feature_error().is_some() || !self.idle() || function > 15 || parameters.len() > 16
        {
            return false;
        }
        self.exchange_sent = false;
        self.request(Step::Settings, feature, function, parameters, now);
        true
    }
    pub fn register(&mut self, write: bool, register: u8, parameters: &[u8], now: u64) -> bool {
        if self.protocol.major() != 1 || !self.idle() || parameters.len() > 3 {
            return false;
        }
        self.exchange_sent = false;
        self.request(
            Step::Settings,
            if write { 0x80 } else { 0x81 },
            0,
            parameters,
            now,
        );
        self.request[2] = register;
        true
    }
    pub fn output_report(&self) -> u8 {
        if self.protocol.major() == 1 || self.reports & HIDPP_LONG == 0 {
            0x10
        } else {
            0x11
        }
    }
    pub fn response(&mut self) -> Option<Result<Response, Error>> {
        self.response.take()
    }
    /// Feature exchanges require a negotiated modern protocol and long reports.
    pub fn feature_error(&self) -> Option<crate::model::errors::ErrorCode> {
        use crate::model::errors::ErrorCode as C;
        match self.protocol {
            ProtocolState::Error { code } => Some(code),
            ProtocolState::Unavailable => Some(C::HidppReportsUnavailable),
            ProtocolState::Detected { major: 2.., .. } if self.reports & HIDPP_LONG != 0 => None,
            ProtocolState::Detected { major: 2.., .. } => Some(C::HidppReportsUnavailable),
            _ => Some(C::HidppProtocolUnsupported),
        }
    }
    fn reset(&mut self, step: Step, now: u64) {
        self.status = NormalizationState::Resetting;
        self.request(step, self.reset_feature, 1, &[0, 0], now);
    }
    fn probe(&mut self, now: u64) {
        self.error = None;
        if self.reports == 0 {
            self.protocol = ProtocolState::Unavailable;
            self.fail(Error::NoReports);
            return;
        }
        self.status = if self.enabled {
            NormalizationState::Probing
        } else {
            NormalizationState::Off
        };
        self.protocol = ProtocolState::Probing;
        self.request(Step::Protocol, 0, 1, &[0, 0, 0xa5], now);
    }
    fn normalize(&mut self, now: u64) {
        if self.protocol.major() == 0 {
            self.probe(now);
        } else if self.protocol.major() == 1 {
            self.fail(Error::ProtocolUnsupported);
        } else if self.reports & HIDPP_LONG == 0 {
            self.fail(Error::NoReports);
        } else if self.enabled {
            self.status = NormalizationState::Probing;
            self.get_feature(Step::ResetFeature, 0x0020, now);
        } else {
            self.status = NormalizationState::Off;
            self.step = Step::Idle;
        }
    }
    /// Reconfiguration requires `quiesce()` to succeed first. A settings owner
    /// must drain its response before this replaces the normalization sequence.
    pub fn configure(&mut self, enabled: bool, platform: HostPlatform, now: u64) -> bool {
        let before = self.held;
        if self.enabled == enabled && (!enabled || self.platform == platform) {
            self.platform = platform;
            if !enabled && self.protocol.major() == 0 && self.idle() {
                self.probe(now);
            }
            return self.held != before;
        }
        if self.step == Step::Reset && self.waiting {
            self.clear_held();
        }
        self.enabled = enabled;
        self.platform = platform;
        self.queued = false;
        self.waiting = false;
        self.error = None;
        if !enabled {
            self.selected = 0;
            self.clear_held();
            if self.reset_feature != 0 && self.controls_feature != 0 {
                self.reset(Step::Disable, now);
            } else {
                self.status = NormalizationState::Off;
                self.step = Step::Idle;
                if self.protocol.major() == 0 {
                    self.probe(now);
                }
            }
        } else {
            self.normalize(now);
        }
        self.held != before
    }
    pub fn tick(&mut self, now: u64) -> bool {
        if (self.queued || self.waiting) && now >= self.deadline {
            self.fail(Error::Timeout)
        } else {
            false
        }
    }
    /// Returns the long-report payload, excluding its report ID 0x11.
    pub fn next_output(&mut self, now: u64) -> Option<[u8; PAYLOAD_BYTES]> {
        if !self.queued || self.tx_busy || now >= self.deadline {
            return None;
        }
        self.queued = false;
        self.waiting = true;
        self.tx_busy = true;
        if self.step == Step::Settings {
            self.exchange_sent = true;
        }
        if matches!(self.step, Step::Reset | Step::Disable) {
            self.reset_uncertain = true;
        }
        self.deadline = now + TIMEOUT_MS;
        Some(self.request)
    }
    pub fn tx_complete(&mut self, success: bool, now: u64) -> bool {
        if !self.tx_busy {
            return false;
        }
        self.tx_busy = false;
        if !success && (self.waiting || self.queued) {
            self.fail(Error::Transport)
        } else {
            self.tick(now)
        }
    }
    fn next_control(&mut self, now: u64) {
        if self.index == self.count {
            self.status = NormalizationState::Active;
            self.step = Step::Idle;
        } else {
            let index = self.index;
            self.index += 1;
            self.request(Step::Info, self.controls_feature, 1, &[index], now);
        }
    }
    fn notification(&mut self, parameters: &[u8]) -> bool {
        let mut held = Held::default();
        let mut ended = false;
        for j in 0..4 {
            let cid = u16::from_be_bytes([parameters[2 * j], parameters[2 * j + 1]]);
            if cid == 0 {
                ended = true;
                continue;
            }
            if ended || (0..j).any(|k| parameters[2 * k..2 * k + 2] == parameters[2 * j..2 * j + 2])
            {
                return false;
            }
            let Some((index, translation)) = control(cid, self.active_platform) else {
                continue;
            };
            if self.selected & (1 << index) == 0 {
                continue;
            }
            if translation.key != 0 {
                held.keys[translation.key as usize / 8] |= 1 << (translation.key % 8);
            }
            if translation.consumer != 0 {
                // A notification has at most four controls, each with one consumer usage.
                held.consumer(translation.consumer)
                    .expect("translation exceeds report capacity");
            }
            if let Some(i) = crate::hid::SYSTEM_USAGES
                .iter()
                .position(|&u| u == translation.system)
            {
                held.system |= 1 << i;
            }
            held.keys[28] |= translation.modifiers;
        }
        let changed = held != self.held;
        self.held = held;
        changed
    }
    pub fn receive(&mut self, report_id: u8, payload: &[u8], now: u64) -> bool {
        let mut changed = self.tick(now);
        let valid = (report_id == 0x11 && self.reports & HIDPP_LONG != 0 && payload.len() == 19)
            || (report_id == 0x10 && self.reports & HIDPP_SHORT != 0 && payload.len() == 6);
        if !valid || payload[0] != 0xff {
            return changed;
        }
        if self.enabled
            && self.controls_feature != 0
            && payload[1] == self.controls_feature
            && payload[2] == 0
        {
            return (payload.len() >= 11 && self.notification(&payload[3..])) || changed;
        }
        if !self.waiting {
            return changed;
        }
        if matches!(payload[1], 0xff | 0x8f)
            && payload[2] == self.request[1]
            && payload[3] == self.request[2]
        {
            if self.step == Step::Protocol && payload[1] == 0x8f && payload[4] == 1 {
                self.protocol = ProtocolState::Detected { major: 1, minor: 0 };
                return self.fail(Error::ProtocolUnsupported) || changed;
            }
            return self.fail(Error::Device(payload[4])) || changed;
        }
        if payload[1..3] != self.request[1..3] {
            return changed;
        }
        let p = &payload[3..];
        let needed = match self.step {
            Step::Settings => 0,
            Step::Info | Step::Reporting => 5,
            Step::Divert if self.controls_version >= 4 => 6,
            Step::Divert => 5,
            _ => 3,
        };
        if p.len() < needed {
            return changed;
        }
        if self.step == Step::Protocol && p[2] != 0xa5 {
            return changed;
        }
        if matches!(self.step, Step::Reporting | Step::Divert)
            && u16::from_be_bytes([p[0], p[1]]) != self.cid
        {
            return changed;
        }
        self.waiting = false;
        match self.step {
            Step::Settings => {
                let mut bytes = [0; 16];
                bytes[..p.len()].copy_from_slice(p);
                self.response = Some(Ok(Response {
                    bytes,
                    length: p.len(),
                }));
                self.step = Step::Idle;
            }
            Step::Protocol => {
                if p[0] == 0 {
                    changed |= self.fail(Error::InvalidResponse);
                } else {
                    self.protocol = ProtocolState::Detected {
                        major: p[0],
                        minor: p[1],
                    };
                    self.normalize(now);
                }
            }
            Step::ResetFeature | Step::ControlsFeature => {
                let reset = self.step == Step::ResetFeature;
                if p[0] == 0 || p[0] == 0xff || p[1] & 0x60 != 0 {
                    changed |= self.fail(if reset {
                        Error::ResetUnavailable
                    } else {
                        Error::ControlsUnavailable
                    });
                } else if reset {
                    self.reset_feature = p[0];
                    self.get_feature(Step::ControlsFeature, 0x1b04, now);
                } else {
                    self.controls_feature = p[0];
                    self.controls_version = p[2];
                    self.reset(Step::Reset, now);
                }
            }
            Step::Reset => {
                self.reset_uncertain = false;
                changed |= self.clear_held();
                self.selected = 0;
                self.active_platform = self.platform;
                self.status = NormalizationState::Configuring;
                self.request(Step::Count, self.controls_feature, 0, &[], now);
            }
            Step::Count => {
                self.count = p[0];
                self.index = 0;
                self.next_control(now);
            }
            Step::Info => {
                self.cid = u16::from_be_bytes([p[0], p[1]]);
                if p[4] & 0x20 != 0
                    && p[4] & 0x80 == 0
                    && control(self.cid, self.platform)
                        .is_some_and(|(i, _)| self.needs_translation(i, p[4]))
                {
                    self.request(Step::Reporting, self.controls_feature, 2, &p[..2], now);
                } else {
                    self.next_control(now);
                }
            }
            Step::Reporting => {
                let Some((index, _)) = control(self.cid, self.platform) else {
                    return self.fail(Error::InvalidResponse) || changed;
                };
                self.selected |= 1 << index;
                if p[2] & 1 != 0 {
                    self.next_control(now);
                } else {
                    let cid = self.cid.to_be_bytes();
                    let parameters = [cid[0], cid[1], 3, 0, 0, 0];
                    self.request(
                        Step::Divert,
                        self.controls_feature,
                        3,
                        &parameters[..if self.controls_version >= 4 { 6 } else { 5 }],
                        now,
                    );
                }
            }
            Step::Divert => {
                if p[..needed] != self.request[3..3 + needed] {
                    changed |= self.fail(Error::InvalidResponse);
                } else {
                    self.next_control(now);
                }
            }
            Step::Disable => {
                self.reset_uncertain = false;
                self.status = NormalizationState::Off;
                self.step = Step::Idle;
                if self.protocol.major() == 0 {
                    self.probe(now);
                }
            }
            Step::Idle => {}
        }
        changed
    }
}
