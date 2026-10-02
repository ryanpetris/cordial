//! Optional GATT services share the profile's ATT slot without delaying HID admission.
use super::*;
const PERIOD: u64 = 60_000;
const ENDPOINTS: usize = 32;
#[derive(Clone, Copy)]
pub(super) enum Action {
    Services(u16),
    Characteristics {
        start: u16,
        end: u16,
        instance: u8,
        uuid: u16,
    },
    Read(usize),
    Descriptors(usize),
    Subscribe(usize),
}
#[derive(Clone, Copy)]
struct Endpoint {
    c: Characteristic,
    instance: u8,
    cccd: u16,
    subscribed: bool,
}
#[derive(Default)]
pub(super) struct Information {
    tasks: VecDeque<Action>,
    endpoints: Vec<Endpoint>,
    current: Option<Action>,
    batteries: u8,
    due: u64,
    discovered: bool,
    last_characteristic: Option<usize>,
    malformed: bool,
    yield_once: bool,
    refresh_pending: bool,
    retry_discovery: bool,
    endpoint_start: usize,
}
impl Information {
    pub(super) fn busy(&self) -> bool {
        self.current.is_some() || !self.tasks.is_empty() || self.refresh_pending
    }
    /// The initial discovery pass has been queued and has finished.
    pub(super) fn settled(&self) -> bool {
        self.discovered && !self.busy()
    }
    pub(super) fn refresh(&mut self) {
        if self.busy() {
            self.refresh_pending = true;
            return;
        }
        if !self.discovered {
            self.tasks.extend([
                Action::Services(0x180f),
                Action::Services(0x180a),
                Action::Services(0x1800),
            ]);
            self.discovered = true;
        } else {
            self.tasks
                .extend((0..self.endpoints.len()).map(Action::Read));
        }
    }
}
impl<H: Host> Backend<H> {
    pub(super) fn information_poll(&mut self, link: &mut Link) {
        if !matches!(link.stage, Stage::Ready) || link.closing || link.pending.is_some() {
            return;
        }
        if core::mem::take(&mut link.info.yield_once) {
            return;
        }
        if link.info.current.is_none()
            && link.info.tasks.is_empty()
            && core::mem::take(&mut link.info.refresh_pending)
        {
            link.info.refresh();
        }
        let now = (self.now)();
        if !link.info.busy() && now >= link.info.due {
            if link.info.retry_discovery {
                link.info = Information::default();
            }
            if !link.info.discovered {
                link.info.refresh();
                return;
            } else {
                link.info.tasks.extend(
                    link.info
                        .endpoints
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| {
                            matches!(e.c.uuid, 0x2a19 | 0x2bed | 0x2bf0 | 0x2be9) && !e.subscribed
                        })
                        .map(|(i, _)| Action::Read(i)),
                );
            }
            link.info.due = now.saturating_add(PERIOD);
        }
        let Some(action) = link.info.tasks.front().copied() else {
            return;
        };
        let Ok(request) = self.sequence() else {
            return;
        };
        link.info.tasks.pop_front();
        link.info.current = Some(action);
        link.info.malformed = false;
        link.pending = Some(Pending {
            request,
            operation: Operation::Information,
            data: Vec::new(),
        });
        let token = link.token;
        let result = match action {
            Action::Services(uuid) => self.host.services(token, request, uuid),
            Action::Characteristics { start, end, .. } => {
                link.info.last_characteristic = None;
                link.info.endpoint_start = link.info.endpoints.len();
                self.host.characteristics(token, request, start, end)
            }
            Action::Read(i) => {
                self.host
                    .read(token, request, link.info.endpoints[i].c.value, false)
            }
            Action::Descriptors(i) => {
                let e = link.info.endpoints[i];
                if e.c.value < e.c.end {
                    self.host
                        .descriptors(token, request, e.c.value + 1, e.c.end)
                } else {
                    Err(Error::UnsupportedHid)
                }
            }
            Action::Subscribe(i) => {
                let e = link.info.endpoints[i];
                self.host.subscribe(
                    token,
                    request,
                    e.c.value,
                    e.cccd,
                    e.c.properties & 0x10 == 0,
                )
            }
        };
        if result.is_err() {
            self.information_complete(link, false);
        }
    }
    pub(super) fn information_event(&mut self, link: &mut Link, event: NativeEvent) {
        match event {
            NativeEvent::Service { start, end, .. } => {
                if let Some(Action::Services(uuid)) = link.info.current {
                    let instance = if uuid == 0x180f {
                        link.info.batteries
                    } else {
                        0
                    };
                    if start != 0
                        && end >= start
                        && link.info.tasks.len() < 8
                        && (uuid != 0x180f || instance < 4)
                    {
                        if uuid == 0x180f {
                            link.info.batteries += 1;
                        }
                        link.info.tasks.push_back(Action::Characteristics {
                            start,
                            end,
                            instance,
                            uuid,
                        });
                    }
                }
            }
            NativeEvent::Characteristic {
                declaration,
                value,
                properties,
                uuid,
                ..
            } => {
                if let Some(Action::Characteristics {
                    start,
                    end,
                    instance,
                    uuid: service,
                }) = link.info.current
                {
                    let boundary = declaration.unwrap_or(value);
                    if boundary < start || value <= boundary && declaration.is_some() || value > end
                    {
                        link.info.malformed = true;
                        return;
                    }
                    if let Some(i) = link.info.last_characteristic {
                        if boundary <= link.info.endpoints[i].c.value {
                            link.info.malformed = true;
                            return;
                        }
                        link.info.endpoints[i].c.end = boundary - 1;
                        link.info.last_characteristic = None;
                    }
                    let wanted = match service {
                        0x180f => matches!(uuid, 0x2a19 | 0x2bed | 0x2bf0 | 0x2be9),
                        0x1800 => matches!(uuid, 0x2a00 | 0x2a01),
                        0x180a => matches!(uuid, 0x2a24..=0x2a29 | 0x2a50),
                        _ => false,
                    };
                    if wanted && properties & 2 != 0 && link.info.endpoints.len() < ENDPOINTS {
                        let i = link.info.endpoints.len();
                        link.info.endpoints.push(Endpoint {
                            c: Characteristic {
                                value,
                                end,
                                properties,
                                uuid,
                            },
                            instance,
                            cccd: 0,
                            subscribed: false,
                        });
                        link.info.last_characteristic = Some(i);
                    }
                }
            }
            NativeEvent::Descriptor { handle, uuid, .. } => {
                if let Some(Action::Descriptors(i)) = link.info.current {
                    let e = link.info.endpoints[i];
                    if uuid == 0x2902 && e.cccd == 0 && handle > e.c.value && handle <= e.c.end {
                        link.info.endpoints[i].cccd = handle;
                    }
                    if uuid == 0x2904
                        && e.c.uuid == 0x2a19
                        && handle > e.c.value
                        && handle <= e.c.end
                        && link.info.endpoints.len() < ENDPOINTS
                    {
                        let index = link.info.endpoints.len();
                        link.info.endpoints.push(Endpoint {
                            c: Characteristic {
                                value: handle,
                                end: handle,
                                uuid,
                                properties: 2,
                            },
                            instance: e.instance,
                            cccd: 0,
                            subscribed: false,
                        });
                        link.info.tasks.push_back(Action::Read(index));
                    }
                }
            }
            NativeEvent::Data { offset, data, .. } => {
                let p = link.pending.as_mut().unwrap();
                if usize::from(offset) == p.data.len() && p.data.len() + data.bytes().len() <= 512 {
                    p.data.extend_from_slice(data.bytes());
                } else {
                    link.info.malformed = true;
                }
            }
            NativeEvent::Complete { result, .. } => self.information_complete(link, result.is_ok()),
            _ => {}
        }
    }
    fn information_complete(&mut self, link: &mut Link, success: bool) {
        let Some(action) = link.info.current.take() else {
            return;
        };
        let data = link.pending.take().unwrap().data;
        let success = success && !link.info.malformed;
        link.info.yield_once = true;
        if !success && matches!(action, Action::Services(_) | Action::Characteristics { .. }) {
            link.info.retry_discovery = true;
        }
        match action {
            Action::Services(0x180f) if success => {
                self.events.push_back(Event::Information {
                    success: true,
                    link: link.id,
                    uuid: 0x180f,
                    instance: 0,
                    bytes: alloc::vec![link.info.batteries].into_boxed_slice(),
                });
            }
            Action::Characteristics { start, end, .. } => {
                if !success {
                    link.info.endpoints.truncate(link.info.endpoint_start);
                }
                // Reads and subscriptions are optional.
                if success {
                    for (i, e) in link.info.endpoints.iter().enumerate() {
                        if e.c.value >= start && e.c.value <= end {
                            link.info.tasks.push_back(Action::Read(i));
                            if matches!(e.c.uuid, 0x2a19 | 0x2bed | 0x2bf0 | 0x2be9)
                                && !e.subscribed
                            {
                                link.info.tasks.push_back(Action::Descriptors(i));
                            }
                        }
                    }
                }
            }
            Action::Read(i) => {
                let e = link.info.endpoints[i];
                self.events.push_back(Event::Information {
                    success,
                    link: link.id,
                    uuid: e.c.uuid,
                    instance: e.instance,
                    bytes: if success {
                        data.into_boxed_slice()
                    } else {
                        Box::new([])
                    },
                });
            }
            Action::Subscribe(i) => {
                link.info.endpoints[i].subscribed = success;
            }
            Action::Descriptors(i)
                if success
                    && link.info.endpoints[i].cccd != 0
                    && link.info.endpoints[i].c.properties & 0x30 != 0 =>
            {
                link.info.tasks.push_back(Action::Subscribe(i));
            }
            _ => {}
        }
        link.info.due = (self.now)().saturating_add(PERIOD);
    }
    pub(super) fn information_notification(
        &mut self,
        link: &Link,
        handle: u16,
        bytes: &[u8],
    ) -> bool {
        if let Some(e) = link.info.endpoints.iter().find(|e| e.c.value == handle) {
            self.events.push_back(Event::Information {
                success: true,
                link: link.id,
                uuid: e.c.uuid,
                instance: e.instance,
                bytes: bytes.into(),
            });
            true
        } else {
            false
        }
    }
}
