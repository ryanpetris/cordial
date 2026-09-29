use crate::{
    client::{Client, Envelope, Error, Request, Result, Wait},
    controller::{Auth, Event, Notice, Pending, Phase, SessionId, State},
    view::{self, View},
};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::*,
    info::DeviceInfo,
    messages::{
        self, Capabilities, Command, Empty, Enabled, Message, Prompt, SettingChunk, Status,
    },
    payloads::{
        AdapterSettings, DeviceResult, DeviceUnpaired, HeartbeatResult, MonitorResult, ReadyResult,
    },
    settings::{Setting, SettingKey},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) type Sink = Arc<dyn Fn(Event) + Send + Sync>;
struct PendingInfo {
    row: Pending,
    background: bool,
}
struct Data {
    view: View,
    capabilities: Capabilities,
    status: Status,
    candidates: BTreeMap<CandidateId, messages::Candidate>,
    prior_candidates: Option<BTreeMap<CandidateId, messages::Candidate>>,
    scan: Option<RequestId>,
    pending: BTreeMap<RequestId, PendingInfo>,
    auth: Option<Auth>,
    monitor: bool,
    want_monitor: bool,
    ready: bool,
    waiting: bool,
    ready_error: Option<Error>,
    ready_floor: u64,
    /// A readiness wait is in progress. A terminal report after the wait
    /// ended locally is ignored: the startup snapshot and monitoring never
    /// ran for it, so the session stays unready until reopened.
    ready_attempt: bool,
    processed: u64,
    /// Status counts and capacity estimates have no events of their own.
    status_stale: bool,
}
pub(crate) struct Session {
    pub client: Client,
    pub id: SessionId,
    pub port: String,
    data: Mutex<Data>,
    processed: Condvar,
    snapshots: Mutex<()>,
    settings: Mutex<()>,
    info: Mutex<()>,
    monitor: Mutex<()>,
    refresh_running: AtomicBool,
    stopping: AtomicBool,
    listener_done: AtomicBool,
    shutdown_guard: Mutex<()>,
    sink: Sink,
}
impl Session {
    pub fn new(client: Client, id: SessionId, port: String, sink: Sink) -> Arc<Self> {
        let status = client.status();
        let capabilities = client.capabilities();
        Arc::new(Self {
            client,
            id,
            port,
            data: Mutex::new(Data {
                view: View::new(status.revision, status.host_platform, status.name.clone()),
                ready_floor: status.revision,
                ready_attempt: false,
                capabilities,
                status,
                candidates: BTreeMap::new(),
                prior_candidates: None,
                scan: None,
                pending: BTreeMap::new(),
                auth: None,
                monitor: false,
                want_monitor: false,
                ready: false,
                waiting: false,
                ready_error: None,
                processed: 0,
                status_stale: false,
            }),
            processed: Condvar::new(),
            snapshots: Mutex::new(()),
            settings: Mutex::new(()),
            info: Mutex::new(()),
            monitor: Mutex::new(()),
            refresh_running: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            listener_done: AtomicBool::new(false),
            shutdown_guard: Mutex::new(()),
            sink,
        })
    }
    pub fn listen(self: &Arc<Self>) {
        let this = self.clone();
        thread::spawn(move || {
            while !this.stopping.load(Ordering::Acquire) {
                let result = match this.client.next(Duration::from_millis(50)) {
                    Ok(Some(envelope)) => this.incoming(envelope),
                    Ok(None) => continue,
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    this.client.fail(error.clone());
                    this.processed.notify_all();
                    if !this.stopping.load(Ordering::Acquire) {
                        this.phase(Phase::Lost(error));
                    }
                    break;
                }
            }
            this.listener_done.store(true, Ordering::Release);
            this.processed.notify_all();
        });
    }
    pub fn phase(&self, phase: Phase) {
        (self.sink)(Event::Connection {
            session: self.id,
            port: self.port.clone(),
            phase,
        });
    }
    pub fn notice(&self, notice: Notice) {
        (self.sink)(Event::Notice {
            session: self.id,
            notice,
        });
    }
    pub fn state(&self) -> State {
        let data = self.data.lock().unwrap();
        let mut status = data.status.clone();
        status.host_platform = data.view.platform;
        status.name = data.view.name.clone();
        let available = self.client.error().is_none() && !self.stopping.load(Ordering::Acquire);
        let auth = data
            .auth
            .as_ref()
            .filter(|a| a.expires > Instant::now())
            .cloned()
            .map(|mut a| {
                a.expires_in_ms = a
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(u32::MAX.into()) as u32;
                a
            });
        State {
            session: self.id,
            port: self.port.clone(),
            capabilities: data.capabilities.clone(),
            status,
            devices: data.view.named_devices(),
            candidates: data.candidates.values().cloned().collect(),
            pending: if available {
                data.pending.values().map(|p| p.row.clone()).collect()
            } else {
                Vec::new()
            },
            auth: if available { auth } else { None },
            available,
            current: data.view.valid,
            monitor: data.monitor,
            saturated: data.candidates.len() >= data.status.limits.scan_candidates,
            revision: data.view.revision,
            ready: data.ready,
            waiting: data.waiting,
            ready_error: data.ready_error.clone(),
            settings: data.view.settings(),
            info: data.view.infos(),
        }
    }
    pub fn hide_candidate(&self, id: &CandidateId) {
        self.data.lock().unwrap().candidates.remove(id);
    }
    pub fn scan(&self) -> Option<RequestId> {
        self.data.lock().unwrap().scan
    }
    pub fn start(
        &self,
        command: Command,
        internal: bool,
        background: bool,
        wait: &Wait,
    ) -> Result<Request> {
        let name = command.name();
        let target = target(&command);
        let mut data = self.data.lock().unwrap();
        if let Some(reason) = data.capabilities.unsupported(&command) {
            return Err(Error::new(format!("{name}: {reason}")));
        }
        let transport = match &command {
            Command::Connect(args) => data.view.devices.get(&args.device_id).map(|d| d.transport),
            Command::Pair(args) => data.candidates.get(&args.candidate_id).map(|c| c.transport),
            _ => None,
        };
        if transport.is_some_and(|t| !data.capabilities.supports_transport(t)) {
            return Err(Error::new(
                "this adapter does not support the device's Bluetooth transport",
            ));
        }
        if name == "discovery.scan" && data.scan.is_some() {
            return Err(Error::new("discovery is already running"));
        }
        let device_id = match &command {
            Command::Pair(_) => None,
            _ => target
                .as_ref()
                .filter(|s| data.view.devices.contains_key(&DeviceId((*s).clone())))
                .map(|s| DeviceId(s.clone())),
        };
        // The reader cannot process a prompt/completion before we record its request.
        let request = self.client.start(command, internal, wait)?;
        data.pending.insert(
            request.id,
            PendingInfo {
                row: Pending {
                    id: request.id,
                    device_id,
                    command: name,
                    target: target.clone(),
                },
                background,
            },
        );
        if name == "discovery.scan" {
            data.scan = Some(request.id);
            data.prior_candidates = Some(std::mem::take(&mut data.candidates));
        }
        drop(data);
        if matches!(name, "pairing.start" | "device.connect") {
            self.notice(Notice::RequestPending {
                id: request.id,
                command: name,
                target: target.unwrap_or_default(),
            });
        }
        Ok(request)
    }
    pub fn wait(&self, request: Request, wait: &Wait) -> Result<Vec<Envelope>> {
        let result = self.client.wait(request, wait);
        let sequence = match &result {
            Ok(rows) => rows.last().map(|m| m.sequence),
            Err(error) => error.responses.last().map(|m| m.sequence),
        };
        if let Some(sequence) = sequence {
            self.wait_processed(sequence, wait)?;
        }
        result
    }
    pub fn call(&self, command: Command, internal: bool, wait: &Wait) -> Result<Vec<Envelope>> {
        let reply = match &command {
            Command::PairReply(args) => Some((args.request_id, args.prompt_id.clone())),
            _ => None,
        };
        let mut attempt = 0;
        let result = loop {
            let result = self.wait(self.start(command.clone(), internal, false, wait)?, wait);
            if attempt < 2
                && matches!(command, Command::Devices(_))
                && result
                    .as_ref()
                    .err()
                    .and_then(|e| e.wire.as_ref())
                    .is_some_and(|e| e.code == ErrorCode::Busy)
            {
                attempt += 1;
                wait.check()?;
                continue;
            }
            break result;
        };
        if result.is_ok()
            && let Some((id, prompt)) = reply
        {
            let mut data = self.data.lock().unwrap();
            if data
                .auth
                .as_ref()
                .is_some_and(|a| a.request == id && a.prompt.prompt_id == prompt)
            {
                data.auth = None;
            }
        }
        result
    }
    fn wait_processed(&self, sequence: u64, wait: &Wait) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        while data.processed < sequence {
            wait.check()?;
            // The listener drains messages already received before reporting unplug.
            if self.stopping.load(Ordering::Acquire) {
                return Err(Error::new("control session closed"));
            }
            let (next, _) = self
                .processed
                .wait_timeout(data, Duration::from_millis(20))
                .unwrap();
            data = next;
            if self.client.error().is_some()
                && data.processed < sequence
                && self.listener_done.load(Ordering::Acquire)
            {
                return Err(self.client.error().unwrap());
            }
        }
        Ok(())
    }
    pub fn ready(self: &Arc<Self>, wait: &Wait) -> Result<()> {
        {
            let mut data = self.data.lock().unwrap();
            data.ready_floor = data.status.revision;
            data.ready_attempt = true;
        }
        let result = self.call(Command::Ready(Empty {}), true, wait);
        let mut data = self.data.lock().unwrap();
        data.ready_attempt = false;
        if let Err(error) = result {
            data.ready = false;
            data.waiting = false;
            data.ready_error = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }
    pub fn refresh(&self, wait: &Wait) -> Result<()> {
        let _guard = self.snapshots.lock().unwrap();
        self.data.lock().unwrap().status_stale = false;
        self.call(Command::Status(Empty {}), true, wait)?;
        let (epoch, limit) = {
            let data = self.data.lock().unwrap();
            (data.view.epoch, data.status.limits.saved_devices)
        };
        let rows = self.call(
            Command::Devices(messages::DeviceFilter::default()),
            true,
            wait,
        )?;
        self.data
            .lock()
            .unwrap()
            .view
            .install_devices(&rows, epoch, limit)
            .inspect_err(|error| {
                self.client.fail(error.clone());
            })?;
        self.sync_info(wait)
    }
    /// Reads the snapshots of devices whose information is missing or not
    /// current. A device the adapter can't report on is retried only after
    /// the next resynchronization.
    fn sync_info(&self, wait: &Wait) -> Result<()> {
        let needed = self.data.lock().unwrap().view.info_needed();
        for id in needed {
            match self.info(&id, false, true, wait) {
                Err(error) if error.wire.is_some() => {}
                other => other?,
            }
        }
        Ok(())
    }
    /// Reads a device's information snapshot, after asking the device for
    /// fresh values with `refresh`, and installs it.
    pub fn info(&self, id: &DeviceId, refresh: bool, internal: bool, wait: &Wait) -> Result<()> {
        let _guard = self.info.lock().unwrap();
        let epoch = {
            let mut data = self.data.lock().unwrap();
            data.view.info_tried(id);
            data.view.settings_epoch
        };
        let device = messages::DeviceRef {
            device_id: id.clone(),
        };
        let rows = self.call(
            if refresh {
                Command::DeviceInfoRefresh(device)
            } else {
                Command::DeviceInfo(device)
            },
            internal,
            wait,
        )?;
        let info = rows
            .last()
            .ok_or_else(|| Error::new("adapter returned no device information"))?
            .decode::<DeviceInfo>()?;
        self.data
            .lock()
            .unwrap()
            .view
            .install_info(id, info, epoch)
            .inspect_err(|error| {
                self.client.fail(error.clone());
            })
    }
    fn refresh_status(&self, wait: &Wait) -> Result<()> {
        let _guard = self.snapshots.lock().unwrap();
        self.data.lock().unwrap().status_stale = false;
        self.call(Command::Status(Empty {}), true, wait).map(drop)
    }
    pub fn monitoring(&self, enabled: bool, wait: &Wait) -> Result<()> {
        let _guard = self.monitor.lock().unwrap();
        self.call(Command::Monitor(Enabled { enabled }), true, wait)?;
        self.data.lock().unwrap().want_monitor = enabled;
        if enabled {
            self.refresh(wait)?;
        }
        Ok(())
    }
    fn renew_monitor(&self, wait: &Wait) -> Result<()> {
        let _guard = self.monitor.lock().unwrap();
        {
            let data = self.data.lock().unwrap();
            if !data.want_monitor || data.monitor {
                return Ok(());
            }
        }
        self.call(Command::Monitor(Enabled { enabled: true }), true, wait)?;
        self.refresh(wait)
    }
    pub fn settings(&self, id: &DeviceId, features: bool, wait: &Wait) -> Result<()> {
        let _guard = self.settings.lock().unwrap();
        let (epoch, limit, choices) = {
            let data = self.data.lock().unwrap();
            (
                data.view.settings_epoch,
                if features {
                    data.status.limits.hidpp_features
                } else {
                    data.status.limits.hidpp_settings
                },
                data.status.limits.hidpp_setting_choices,
            )
        };
        let device = messages::DeviceRef {
            device_id: id.clone(),
        };
        let rows = self.call(
            if features {
                Command::Features(device)
            } else {
                Command::Settings(device)
            },
            false,
            wait,
        )?;
        let result = {
            let mut data = self.data.lock().unwrap();
            if features {
                data.view.install_features(id, &rows, epoch, limit)
            } else {
                data.view.install_settings(id, &rows, epoch, limit, choices)
            }
        };
        result.inspect_err(|error| {
            self.client.fail(error.clone());
        })
    }
    pub fn setting(&self, id: &DeviceId, key: SettingKey, wait: &Wait) -> Result<Setting> {
        let state = self.state();
        if let Some(cache) = state.settings.get(id)
            && cache.current
            && let Some(setting) = cache.settings.iter().find(|s| s.key == key)
        {
            return Ok(setting.clone());
        }
        let rows = self.call(
            Command::SettingsGet(messages::SettingRef {
                device_id: id.clone(),
                key: serde_json::to_value(key).unwrap().as_str().unwrap().into(),
            }),
            false,
            wait,
        )?;
        Ok(rows.last().unwrap().decode::<SettingChunk>()?.setting)
    }
    pub fn stop_scan(&self, wait: &Wait) -> Result<bool> {
        let Some(id) = self.scan() else {
            return Ok(false);
        };
        match self.call(
            Command::Cancel(messages::RequestRef { request_id: id }),
            false,
            wait,
        ) {
            Err(error)
                if error
                    .wire
                    .as_ref()
                    .is_some_and(|w| w.code == ErrorCode::NotPending) => {}
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        let mut data = self.data.lock().unwrap();
        while data.scan == Some(id) {
            wait.check()?;
            if let Some(error) = self.client.error() {
                return Err(error);
            }
            let (next, _) = self
                .processed
                .wait_timeout(data, Duration::from_millis(20))
                .unwrap();
            data = next;
        }
        Ok(true)
    }
    pub fn shutdown(&self) {
        let _guard = self.shutdown_guard.lock().unwrap();
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.client.shutdown();
        self.processed.notify_all();
    }
    fn incoming(self: &Arc<Self>, envelope: Envelope) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let mut notices = Vec::new();
        let mut waiting = false;
        let mut refresh = false;
        let mut background = false;
        if let Some(name) = envelope.event() {
            data.view.event(&envelope)?;
            match name {
                "local.events_lost" => {
                    #[derive(Deserialize)]
                    struct Lost {
                        dropped: u64,
                    }
                    let lost: Lost = envelope.decode()?;
                    notices.push(Notice::Skipped(lost.dropped));
                }
                "device.unpaired" => {
                    let row: DeviceUnpaired = envelope.decode()?;
                    view::valid_id(&row.device_id.0)?;
                    data.status_stale = true;
                }
                "device.paired" | "device.connected" | "device.changed" | "device.disconnected" => {
                    data.status_stale = true;
                }
                "discovery.result" => {
                    let candidate: messages::Candidate = envelope.decode()?;
                    view::valid_id(&candidate.candidate_id.0)?;
                    if candidate.name.as_ref().is_some_and(|s| s.len() > 128) {
                        return Err(Error::new("invalid discovery result"));
                    }
                    let request = match &envelope.message {
                        Message::Event { request_id, .. } => *request_id,
                        _ => None,
                    };
                    if request == data.scan && request.is_some() {
                        data.prior_candidates = None;
                        if data.candidates.contains_key(&candidate.candidate_id)
                            || data.candidates.len() < data.status.limits.scan_candidates
                        {
                            data.candidates
                                .insert(candidate.candidate_id.clone(), candidate);
                        }
                    }
                }
                "pairing.prompt" | "pairing.display" => {
                    let prompt: Prompt = envelope.decode()?;
                    view::valid_id(&prompt.prompt_id)?;
                    if prompt.expires_in_ms > 180_000 {
                        return Err(Error::new("invalid authentication event"));
                    }
                    if let Message::Event {
                        request_id: Some(request),
                        ..
                    } = &envelope.message
                        && data
                            .pending
                            .get(request)
                            .is_some_and(|p| p.row.command == "pairing.start")
                    {
                        data.auth = Some(Auth {
                            request: *request,
                            expires: Instant::now()
                                + Duration::from_millis(prompt.expires_in_ms.into()),
                            expires_in_ms: prompt.expires_in_ms,
                            prompt,
                            display: name == "pairing.display",
                        });
                    }
                }
                _ => {}
            }
        } else if let Message::Response {
            id,
            ok,
            done,
            error,
            ..
        } = &envelope.message
        {
            let completed = if *done { data.pending.remove(id) } else { None };
            if *done {
                background = completed.as_ref().is_some_and(|p| p.background);
                if data.auth.as_ref().is_some_and(|a| a.request == *id) {
                    data.auth = None;
                }
                if data.scan == Some(*id) {
                    if !ok
                        && error.as_ref().is_some_and(|e| {
                            matches!(
                                e.code,
                                ErrorCode::Busy
                                    | ErrorCode::InvalidArgs
                                    | ErrorCode::HeartbeatRequired
                                    | ErrorCode::RadioUnavailable
                                    | ErrorCode::UnknownCommand
                            )
                        })
                        && let Some(prior) = data.prior_candidates.take()
                    {
                        data.candidates = prior;
                    }
                    data.prior_candidates = None;
                    data.scan = None;
                }
            }
            let command = envelope.command.unwrap_or("");
            if command == "adapter.wait_ready" && data.ready_attempt {
                if !ok {
                    data.waiting = false;
                    data.ready_error = envelope.data().err();
                } else {
                    match (done, envelope.decode::<ReadyResult<Box<Status>>>()?) {
                        (false, ReadyResult::Initializing) => {
                            data.waiting = true;
                            waiting = true;
                        }
                        (true, ReadyResult::Ready { status }) => {
                            validate_identity(&data.capabilities, &data.status, &status)?;
                            if status.revision < data.ready_floor
                                || !status.radio_ready
                                || !status.storage_ready
                            {
                                return Err(Error::new(
                                    "invalid or stale adapter readiness status",
                                ));
                            }
                            data.view.adapter(
                                status.revision,
                                status.host_platform,
                                status.name.clone(),
                            );
                            data.status = *status;
                            data.ready = true;
                            data.waiting = false;
                            data.ready_error = None;
                        }
                        _ => return Err(Error::new("invalid readiness report")),
                    }
                }
            }
            if *ok {
                match command {
                    "adapter.status" => {
                        let status: Status = envelope.decode()?;
                        validate_identity(&data.capabilities, &data.status, &status)?;
                        data.view.adapter(
                            status.revision,
                            status.host_platform,
                            status.name.clone(),
                        );
                        data.status = status;
                    }
                    "adapter.platform.set" | "adapter.name.set" => {
                        let row: AdapterSettings = envelope.decode()?;
                        view::valid_revision(row.revision)?;
                        data.view.adapter(row.revision, row.host_platform, row.name);
                    }
                    "session.monitor.set" => {
                        let enabled: MonitorResult = envelope.decode()?;
                        data.monitor = enabled.enabled;
                    }
                    "session.heartbeat" => {
                        let heartbeat: HeartbeatResult = envelope.decode()?;
                        if data.monitor && !heartbeat.monitor {
                            data.monitor = false;
                            data.view.lose();
                            notices.push(Notice::MonitorExpired);
                        }
                        refresh |= data.want_monitor && !heartbeat.monitor;
                    }
                    "device.unpair" if *done => {
                        if let Some(target) = completed.as_ref().and_then(|p| p.row.target.as_ref())
                        {
                            data.view.forget_settings(&DeviceId(target.clone()));
                        }
                    }
                    "pairing.start" if *done => {
                        let row: DeviceResult = envelope.decode()?;
                        view::validate_device(&row.device)?;
                    }
                    _ => {}
                }
                if (*done
                    && matches!(
                        command,
                        "hidpp.setting.get" | "hidpp.setting.set" | "hidpp.setting.forget"
                    ))
                    || (!done && matches!(command, "hidpp.setting.refresh" | "hidpp.setting.apply"))
                {
                    let row: SettingChunk = envelope.decode()?;
                    view::valid_id(&row.device_id.0)?;
                    view::valid_revision(row.revision)?;
                    view::validate_setting(&row.setting, data.status.limits.hidpp_setting_choices)?;
                    data.view
                        .put_setting(&row.device_id, row.revision, row.setting);
                }
            }
            if *done
                && matches!(
                    command,
                    "pairing.start"
                        | "device.connect"
                        | "device.disconnect"
                        | "device.unpair"
                        | "device.enabled.set"
                        | "device.trusted.set"
                        | "device.blocked.set"
                        | "device.hidpp.set"
                        | "adapter.platform.set"
                        | "hidpp.setting.set"
                        | "hidpp.setting.forget"
                        | "hidpp.setting.refresh"
                        | "hidpp.setting.apply"
                )
            {
                if data.monitor {
                    data.status_stale = true;
                } else {
                    data.view.lose();
                }
                refresh = true;
            }
        }
        refresh |= data.ready && data.monitor && !data.view.valid && envelope.event().is_some();
        refresh |= data.ready && data.monitor && data.status_stale;
        refresh |= data.ready && data.monitor && !data.view.info_needed().is_empty();
        drop(data);
        for notice in notices {
            self.notice(notice);
        }
        self.notice(Notice::Message {
            envelope: envelope.clone(),
            background,
        });
        if waiting {
            self.phase(Phase::Waiting);
        }
        {
            let mut data = self.data.lock().unwrap();
            data.processed = data.processed.max(envelope.sequence);
        }
        self.processed.notify_all();
        if refresh {
            self.schedule_refresh();
        }
        Ok(())
    }
    fn schedule_refresh(self: &Arc<Self>) {
        if self.refresh_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        thread::spawn(move || {
            loop {
                if this.stopping.load(Ordering::Acquire) || this.client.error().is_some() {
                    break;
                }
                let (ready, renew, need, stale, info) = {
                    let data = this.data.lock().unwrap();
                    (
                        data.ready,
                        data.want_monitor && !data.monitor,
                        !data.view.valid,
                        data.status_stale,
                        data.monitor && !data.view.info_needed().is_empty(),
                    )
                };
                if !ready || (!renew && !need && !stale && !info) {
                    break;
                }
                let wait = Wait::timeout(Duration::from_secs(5));
                let result = if renew {
                    this.renew_monitor(&wait)
                } else if need {
                    this.refresh(&wait)
                } else if stale {
                    this.refresh_status(&wait)
                } else {
                    this.sync_info(&wait)
                };
                if let Err(error) = result {
                    if this.stopping.load(Ordering::Acquire) || this.client.error().is_some() {
                        break;
                    }
                    this.notice(Notice::RefreshFailed(error));
                    for _ in 0..20 {
                        if this.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            this.refresh_running.store(false, Ordering::Release);
            // An invalidation may have observed the occupied flag after our last
            // check. Recheck after releasing it; swap still admits only one worker.
            let need = {
                let data = this.data.lock().unwrap();
                data.ready
                    && (!data.view.valid
                        || data.status_stale
                        || (data.want_monitor && !data.monitor)
                        || (data.monitor && !data.view.info_needed().is_empty()))
            };
            if need && !this.stopping.load(Ordering::Acquire) && this.client.error().is_none() {
                this.schedule_refresh();
            }
        });
    }
}
fn validate_identity(capabilities: &Capabilities, known: &Status, status: &Status) -> Result<()> {
    crate::client::validate_status(status, capabilities)?;
    if known.adapter_id != status.adapter_id
        || known.boot_id != status.boot_id
        || known.session_id != status.session_id
    {
        return Err(Error::new(
            "adapter restarted or control session changed; reopen it",
        ));
    }
    Ok(())
}
fn target(command: &Command) -> Option<String> {
    let id = match command {
        Command::Info(a)
        | Command::DeviceInfo(a)
        | Command::DeviceInfoRefresh(a)
        | Command::Disconnect(a)
        | Command::Unpair(a)
        | Command::Features(a)
        | Command::Settings(a)
        | Command::SettingsRefresh(a)
        | Command::SettingsApply(a) => &a.device_id,
        Command::Connect(a) => &a.device_id,
        Command::Hidpp(a) | Command::DeviceEnabled(a) => &a.device_id,
        Command::DeviceTrusted(a) => &a.device_id,
        Command::DeviceBlocked(a) => &a.device_id,
        Command::SettingsGet(a) | Command::SettingsForget(a) => &a.device_id,
        Command::SettingsSet(a) => &a.device_id,
        Command::Pair(a) => return Some(a.candidate_id.0.clone()),
        _ => return None,
    };
    Some(id.0.clone())
}
