//! Application-side packet pump. Only this owner touches application state.
use crate::{HidTx, Io, SerialRx, SerialTx, Status};
use cordial_core::{
    application::Application, bluetooth::Bluetooth, control::EmitError, storage::RecordStore,
};
use cordial_protocol::messages::Request;

pub struct Owner<'a> {
    io: &'a Io,
    seen: Status,
    hid_pending: Option<(u64, u64)>,
    hid_sequence: u64,
    serial_pending: bool,
    separator: bool,
    input: Option<SerialRx>,
    offset: usize,
    request: Option<Request>,
}
impl<'a> Owner<'a> {
    pub fn new(io: &'a Io) -> Self {
        Self {
            io,
            seen: Status::default(),
            hid_pending: None,
            hid_sequence: 0,
            serial_pending: false,
            separator: false,
            input: None,
            offset: 0,
            request: None,
        }
    }
    fn sync<B: Bluetooth>(&mut self, app: &mut Application<'_>, radio: &mut B, now: u64) -> Status {
        let status = self.io.status();
        if status.session != self.seen.session {
            app.session(status.serial_open(), radio, now);
            self.input = None;
            self.request = None;
            self.offset = 0;
            self.separator = status.serial_open();
            self.serial_pending = false;
        }
        if status.generation != self.seen.generation {
            self.hid_pending = None;
            app.manager.forward.resync();
        }
        app.manager.forward.enable(status.input_ready());
        self.seen = status;
        if !app.serial.active() {
            self.request = None;
        }
        status
    }

    pub async fn poll<S: RecordStore, B: Bluetooth>(
        &mut self,
        app: &mut Application<'_>,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let status = self.sync(app, radio, now);

        while let Ok(done) = self.io.hid_done.try_receive() {
            if self.hid_pending == Some((done.generation, done.sequence)) {
                self.hid_pending = None;
                if done.success {
                    app.manager.forward.complete();
                }
            }
        }
        while let Ok(done) = self.io.serial_done.try_receive() {
            if done.session == status.session {
                self.serial_pending = false;
                if let Some(token) = done.token {
                    app.serial.output_complete(token, done.length);
                } else if done.length != 0 {
                    self.separator = false;
                }
            }
        }
        // One USB packet per owner turn bounds serial work alongside input/radio.
        if self.input.is_none() {
            self.input = self.io.serial_rx.try_receive().ok();
        }
        if let Some(packet) = &self.input
            && (packet.session != status.session || !app.serial.active())
        {
            self.input = None;
            self.offset = 0;
        }
        loop {
            if let Some(request) = &self.request {
                let result = app.dispatch(request, store, radio, now).await;
                let blocked = matches!(result, Err(EmitError::Full));
                if !blocked {
                    self.request = None;
                }
                // Flash can yield while USB closes/reopens the port. Finish the
                // already accepted command, then discard that session's tail.
                if self.io.status() != status {
                    self.sync(app, radio, now);
                    return;
                }
                if blocked {
                    break;
                }
            }
            let Some(packet) = &self.input else {
                break;
            };
            let (read, request) = app
                .serial
                .feed(&packet.bytes[self.offset..packet.length], now);
            self.offset += read;
            self.request = request;
            if self.offset == packet.length {
                self.input = None;
                self.offset = 0;
            }
            if read == 0 {
                break;
            }
        }

        if status.input_ready()
            && self.hid_pending.is_none()
            && !self.io.hid_tx.is_full()
            && let Some(packet) = app.manager.forward.packet()
        {
            self.hid_sequence = self.hid_sequence.wrapping_add(1);
            let mut tx = HidTx {
                generation: status.generation,
                sequence: self.hid_sequence,
                bytes: [0; 33],
                length: 1 + packet.bytes().len(),
            };
            tx.bytes[0] = packet.id;
            tx.bytes[1..tx.length].copy_from_slice(packet.bytes());
            if self.io.hid_tx.try_send(tx).is_ok() {
                self.hid_pending = Some((status.generation, self.hid_sequence));
            }
        }
        if status.serial_open() && !self.serial_pending && !self.io.serial_tx.is_full() {
            let mut tx = SerialTx {
                session: status.session,
                token: None,
                bytes: [0; 63],
                length: 0,
            };
            if self.separator {
                tx.bytes[0] = b'\n';
                tx.length = 1;
            } else if let Some((token, bytes)) = app.serial.output(63, now) {
                tx.token = Some(token);
                tx.length = bytes.len();
                tx.bytes[..tx.length].copy_from_slice(bytes);
            }
            if tx.length != 0 {
                // Single producer, capacity checked above; no await in between.
                self.io.serial_tx.try_send(tx).ok().unwrap();
                self.serial_pending = true;
            }
        }
    }
}
