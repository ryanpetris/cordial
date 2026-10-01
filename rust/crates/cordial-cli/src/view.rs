//! The session's view of the Dongle, built from responses and events in the order the Dongle
//! sent them. Every record arrives whole, so each one replaces what the view held.
use cordial_protocol::{self as p, event, request::Command, response};
use std::collections::{BTreeMap, BTreeSet};

pub type SessionId = u64;

/// A command this session is running, for progress shown beside its target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pending {
    pub command: &'static str,
    pub target: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub session: SessionId,
    pub port: String,
    pub status: p::Status,
    /// Saved devices in the Dongle's order.
    pub devices: Vec<p::Device>,
    /// Candidates of the current scan, in the order found.
    pub candidates: Vec<p::Candidate>,
    /// The transports of the running scan, from its start until scan_done.
    pub scanning: Option<Vec<p::Transport>>,
    /// How the last scan ended.
    pub last_scan: Option<p::ScanDone>,
    /// The latest pairing event.
    pub pairing: Option<p::Pairing>,
    /// Each device's settings, once listed or reported.
    pub settings: BTreeMap<String, Vec<p::Setting>>,
    /// Each device's warnings, once listed or reported.
    pub warnings: BTreeMap<String, Vec<p::DeviceWarning>>,
    pub pending: Vec<Pending>,
    /// The connection is open.
    pub available: bool,
    /// The saved devices have been listed since the Dongle became ready.
    pub loaded: bool,
    /// Candidates hidden locally until the next scan.
    pub hidden: BTreeSet<String>,
}

impl State {
    pub fn device(&self, id: &str) -> Option<&p::Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn candidate(&self, id: &str) -> Option<&p::Candidate> {
        self.candidates.iter().find(|c| c.id == id)
    }

    /// The device's settings, or none when they have not been listed.
    pub fn settings_of(&self, id: &str) -> &[p::Setting] {
        self.settings.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn warnings_of(&self, id: &str) -> &[p::DeviceWarning] {
        self.warnings.get(id).map_or(&[], Vec::as_slice)
    }

    /// Whether this session runs `command`, for `target` when one is given.
    pub fn pending_for(&self, command: &str, target: &str) -> bool {
        self.pending.iter().any(|p| {
            p.command == command && (target.is_empty() || p.target.as_deref() == Some(target))
        })
    }

    pub fn ready(&self) -> bool {
        self.available && self.status.ready && self.loaded
    }

    fn put_device(&mut self, device: p::Device) {
        match self.devices.iter_mut().find(|d| d.id == device.id) {
            Some(d) => *d = device,
            None => self.devices.push(device),
        }
    }

    fn remove_device(&mut self, id: &str) {
        self.devices.retain(|d| d.id != id);
        self.settings.remove(id);
        self.warnings.remove(id);
    }

    /// Applies an event.
    pub fn event(&mut self, kind: &event::Kind) {
        match kind {
            event::Kind::Adapter(status) => self.status = status.clone(),
            event::Kind::Device(device) => self.put_device(device.clone()),
            event::Kind::DeviceRemoved(removed) => self.remove_device(&removed.id),
            event::Kind::Settings(s) => {
                self.settings.insert(s.device.clone(), s.settings.clone());
            }
            event::Kind::Warnings(w) => {
                self.warnings.insert(w.device.clone(), w.warnings.clone());
            }
            event::Kind::ScanFound(c) => match self.candidates.iter_mut().find(|o| o.id == c.id) {
                Some(o) => *o = c.clone(),
                None => self.candidates.push(c.clone()),
            },
            event::Kind::ScanDone(done) => {
                self.scanning = None;
                self.last_scan = Some(*done);
            }
            event::Kind::Pairing(pairing) => self.pairing = Some(pairing.clone()),
        }
    }

    /// Applies the records a response carries. An accepted scan or pairing starts here, so
    /// events before its response still belong to the earlier one.
    pub fn response(&mut self, request: &p::Request, response: &p::Response) {
        let accepted = !matches!(response.result, Some(response::Result::Error(_)));
        match &request.command {
            Some(Command::StartScan(scan)) if accepted => self.scan_started(
                scan.transports
                    .iter()
                    .filter_map(|t| p::Transport::try_from(*t).ok())
                    .collect(),
            ),
            Some(Command::StartPairing(_)) if accepted => self.pairing = None,
            _ => {}
        }
        match &response.result {
            Some(response::Result::Status(status)) => self.status = status.clone(),
            Some(response::Result::Device(device)) => self.put_device(device.clone()),
            Some(response::Result::Devices(list)) => {
                let ids: BTreeSet<&str> = list.devices.iter().map(|d| d.id.as_str()).collect();
                self.settings.retain(|id, _| ids.contains(id.as_str()));
                self.warnings.retain(|id, _| ids.contains(id.as_str()));
                self.devices = list.devices.clone();
            }
            Some(response::Result::Settings(s)) if self.device(&s.device).is_some() => {
                self.settings.insert(s.device.clone(), s.settings.clone());
            }
            Some(response::Result::Warnings(w)) if self.device(&w.device).is_some() => {
                self.warnings.insert(w.device.clone(), w.warnings.clone());
            }
            _ => {}
        }
    }

    /// A new scan replaces earlier candidates, except one a pairing holds.
    pub fn scan_started(&mut self, transports: Vec<p::Transport>) {
        let held = self
            .pairing
            .as_ref()
            .filter(|p| crate::model::pairing_running(p))
            .map(|p| p.candidate.clone());
        self.candidates.retain(|c| Some(&c.id) == held.as_ref());
        self.hidden.clear();
        self.last_scan = None;
        self.scanning = Some(transports);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p::{pairing, request::Command};

    fn accepted(command: Command) -> (p::Request, p::Response) {
        (
            p::Request {
                command: Some(command),
            },
            p::Response::default(),
        )
    }

    #[test]
    fn a_scan_starts_at_its_response() {
        let mut st = State::default();
        st.event(&event::Kind::ScanFound(p::Candidate {
            id: "c_1".into(),
            ..Default::default()
        }));
        st.scanning = Some(vec![p::Transport::Ble]);
        // The earlier scan's end arrives before the new scan's response.
        st.event(&event::Kind::ScanDone(p::ScanDone::default()));
        let (request, response) = accepted(Command::StartScan(p::StartScan {
            transports: vec![p::Transport::Ble as i32],
            seconds: 10,
        }));
        st.response(&request, &response);
        assert_eq!(st.scanning, Some(vec![p::Transport::Ble]));
        assert!(st.candidates.is_empty() && st.last_scan.is_none());
    }

    #[test]
    fn a_refused_scan_keeps_the_earlier_state() {
        let mut st = State {
            last_scan: Some(p::ScanDone::default()),
            ..Default::default()
        };
        let request = p::Request {
            command: Some(Command::StartScan(p::StartScan::default())),
        };
        let response = p::Response {
            result: Some(response::Result::Error(p::Error::default())),
        };
        st.response(&request, &response);
        assert!(st.scanning.is_none() && st.last_scan.is_some());
    }

    #[test]
    fn an_accepted_pairing_forgets_the_previous_result() {
        let mut st = State::default();
        st.event(&event::Kind::Pairing(p::Pairing {
            candidate: "c_1".into(),
            step: Some(pairing::Step::Failed(p::ErrorCode::Timeout as i32)),
        }));
        let (request, response) = accepted(Command::StartPairing(p::StartPairing {
            candidate: "c_1".into(),
        }));
        st.response(&request, &response);
        assert!(st.pairing.is_none());
    }
}
