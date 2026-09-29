#![allow(dead_code)]
use cordial_client::{
    client::{Client, Wait},
    transport::Transport,
};
use cordial_protocol::{
    codec,
    messages::{self, Capabilities, Capability, Command, Message, Request, Status},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

#[derive(Clone)]
pub enum WriteStep {
    Bytes(usize),
    Error(io::ErrorKind),
    Zero,
}
struct Port {
    writes: Arc<Mutex<VecDeque<WriteStep>>>,
    stream: TcpStream,
    dtr: Arc<Mutex<Vec<bool>>>,
    opened: Sender<()>,
}
impl Read for Port {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}
impl Write for Port {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.writes.lock().unwrap().pop_front();
        match next {
            Some(WriteStep::Bytes(n)) => self.stream.write(&bytes[..n.min(bytes.len())]),
            Some(WriteStep::Error(kind)) => Err(io::Error::from(kind)),
            Some(WriteStep::Zero) => Ok(0),
            None => self.stream.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
impl Transport for Port {
    fn set_dtr(&mut self, enabled: bool) -> io::Result<()> {
        self.dtr.lock().unwrap().push(enabled);
        if enabled {
            let _ = self.opened.send(());
        }
        Ok(())
    }
    fn clear_input(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.set_write_timeout(Some(timeout))
    }
    fn try_clone(&self) -> io::Result<Box<dyn Transport>> {
        Ok(Box::new(Self {
            stream: self.stream.try_clone()?,
            writes: self.writes.clone(),
            dtr: self.dtr.clone(),
            opened: self.opened.clone(),
        }))
    }
}
pub struct Firmware {
    pub writes: Arc<Mutex<VecDeque<WriteStep>>>,
    writer: Arc<Mutex<TcpStream>>,
    pub requests: Receiver<Request>,
    pub dtr: Arc<Mutex<Vec<bool>>>,
    /// Every request's wire name, in arrival order.
    pub log: Arc<Mutex<Vec<String>>>,
}
impl Firmware {
    pub fn receive(&self) -> Request {
        self.requests.recv_timeout(Duration::from_secs(2)).unwrap()
    }
    pub fn send(&self, message: &impl serde::Serialize) {
        self.writer
            .lock()
            .unwrap()
            .write_all(&codec::encode(message).unwrap())
            .unwrap();
    }
    pub fn reply(&self, request: &Request, result: Value) {
        self.send(&Message::<Value, ()>::success(request.id, result, true));
    }
    pub fn event(
        &self,
        name: &str,
        id: Option<cordial_protocol::identifiers::RequestId>,
        data: Value,
    ) {
        self.send(&Message::<(), Value>::event(name.into(), id, data));
    }
    pub fn disconnect(&self) {
        let _ = self.writer.lock().unwrap().shutdown(Shutdown::Both);
    }
}
impl Drop for Firmware {
    fn drop(&mut self) {
        self.disconnect();
    }
}
pub fn status() -> Status {
    serde_json::from_value(json!({
        "protocol":1,"firmware_version":"0.0.0","hardware_config":"test","radio_backend":"pico-sdk-cyw43","hardware_digest":"test",
        "adapter_id":"test","build_profile":"development","boot_id":"boot1","session_id":"boot1-1",
        "limits":{"max_line_bytes":4096,"max_pending_requests":4,"saved_devices":8,"active_connections":4,
            "scan_candidates":32,"hidpp_settings":32,"hidpp_saved_settings":16,"hidpp_sensors":2,
            "hidpp_firmware_entities":2,"hidpp_setting_choices":16,"hidpp_features":256},
        "counts":{"saved":0,"paired":0,"preferred_enabled":0,"enabled":0,"connected":0},
        "capacity":{"enabled":[{"transports":["classic","ble"],"limit":7,"enabled":0}],
            "pairing":[{"transport":"classic","available":true,"reason":null,"estimated_additional":16},
                {"transport":"ble","available":true,"reason":null,"estimated_additional":16}]},"revision":0,"name": "Test adapter", "host_platform":"linux","monitor":false,
        "radio_ready":true,"storage_ready":true,
        "heartbeat":{"interval_ms":5000,"timeout_ms":15000,"remaining_ms":15000},"pending":[]
    })).unwrap()
}
/// Every capability, as current development firmware advertises.
pub fn capabilities() -> Capabilities {
    Capabilities(vec![
        Capability::Classic,
        Capability::Ble,
        Capability::Debug,
        Capability::StorageManagement,
    ])
}
pub fn connect() -> (Client, Firmware) {
    connect_options(true, status(), json!(capabilities())).unwrap()
}
/// Like `connect`, but device.info requests reach the test instead of
/// receiving an empty snapshot.
pub fn connect_info() -> (Client, Firmware) {
    connect_with_info(true, status(), json!(capabilities()), false).unwrap()
}
pub fn manual_monitor() -> (Client, Firmware) {
    connect_options(false, status(), json!(capabilities())).unwrap()
}
pub fn connect_with_status(status: Status) -> (Client, Firmware) {
    connect_options(false, status, json!(capabilities())).unwrap()
}
pub fn connect_with(status: Status, capabilities: Capabilities) -> (Client, Firmware) {
    connect_options(false, status, json!(capabilities)).unwrap()
}
/// Opens with any capabilities result, including malformed ones.
pub fn try_connect(
    status: Status,
    capabilities: Value,
) -> Result<(Client, Firmware), cordial_client::client::Error> {
    connect_options(false, status, capabilities)
}
fn connect_options(
    auto_monitor: bool,
    advertised: Status,
    capabilities: Value,
) -> Result<(Client, Firmware), cordial_client::client::Error> {
    connect_with_info(auto_monitor, advertised, capabilities, true)
}
fn connect_with_info(
    auto_monitor: bool,
    advertised: Status,
    capabilities: Value,
    auto_info: bool,
) -> Result<(Client, Firmware), cordial_client::client::Error> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (remote, _) = listener.accept().unwrap();
    host.set_nodelay(true).unwrap();
    remote.set_nodelay(true).unwrap();
    let reader = remote.try_clone().unwrap();
    let writer = Arc::new(Mutex::new(remote));
    let output = writer.clone();
    let (opened, ready) = mpsc::channel();
    let (tx, requests) = mpsc::channel();
    let log = Arc::new(Mutex::new(Vec::new()));
    let names = log.clone();
    thread::spawn(move || {
        if ready.recv().is_err() {
            return;
        }
        output
            .lock()
            .unwrap()
            .write_all(b"old fragment\n\n")
            .unwrap();
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else {
                break;
            };
            let request = codec::decode_request(line.as_bytes()).unwrap();
            names
                .lock()
                .unwrap()
                .push(request.command.name().to_owned());
            let result = match &request.command {
                Command::Capabilities(_) => capabilities.clone(),
                Command::Status(_) => serde_json::to_value(&advertised).unwrap(),
                Command::Heartbeat(_) => json!({"timeout_ms":15000,"monitor":false}),
                Command::Monitor(args) if auto_monitor => {
                    json!({"enabled":args.enabled,"revision":0})
                }
                Command::Cancel(_) => json!({}),
                // Nothing reported: the snapshot the client fetches for each device.
                Command::DeviceInfo(args) if auto_info => {
                    json!({"revision":0,"device_id":args.device_id,"fields":[]})
                }
                _ => {
                    if tx.send(request).is_err() {
                        break;
                    }
                    continue;
                }
            };
            let message = Message::<Value, ()>::success(request.id, result, true);
            if output
                .lock()
                .unwrap()
                .write_all(&codec::encode(&message).unwrap())
                .is_err()
            {
                break;
            }
        }
    });
    let dtr = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(VecDeque::new()));
    let client = Client::connect(
        Box::new(Port {
            stream: host,
            writes: writes.clone(),
            dtr: dtr.clone(),
            opened,
        }),
        &Wait::timeout(Duration::from_secs(2)),
    )?;
    Ok((
        client,
        Firmware {
            writes,
            writer,
            requests,
            dtr,
            log,
        },
    ))
}
/// Limits status capacity to these transports and returns matching
/// capabilities, with the development capabilities.
pub fn set_transports(
    s: &mut Status,
    transports: Vec<cordial_protocol::identifiers::Transport>,
) -> Capabilities {
    use cordial_protocol::identifiers::Transport;
    s.capacity.enabled.retain_mut(|c| {
        c.transports.retain(|t| transports.contains(t));
        !c.transports.is_empty()
    });
    s.capacity
        .pairing
        .retain(|p| transports.contains(&p.transport));
    for t in &transports {
        if s.pairing(*t).is_none() {
            s.capacity.pairing.push(messages::PairingCapacity {
                transport: *t,
                available: true,
                reason: None,
                estimated_additional: 16,
            });
        }
    }
    let mut caps: Vec<Capability> = transports
        .iter()
        .map(|t| match t {
            Transport::Classic => Capability::Classic,
            Transport::Ble => Capability::Ble,
        })
        .collect();
    caps.extend([Capability::Debug, Capability::StorageManagement]);
    Capabilities(caps)
}
