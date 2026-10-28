//! Host-assisted storage migration: which adapters this version of Cordial may use, the worker
//! that migrates one, and `cordial migrate --ipc` for the desktop app.
//!
//! The client library's worker does the whole migration. While it runs it owns every adapter
//! connection: callers close all their sessions, wait for their ports to be released and open
//! nothing until the worker reports verified success or explicit cancellation and has exited. A
//! failure leaves that ownership in place until the user cancels.
use crate::{controller::Command, error::Error};
use cordial_client::migration::{self, Outcome};
use cordial_protocol::{
    self as p,
    migration::{MigrationControl, MigrationEvent, migration_control, migration_event},
};
use prost::Message as _;
use std::{
    io::{self, Read, Write},
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

pub const UNSUPPORTED: &str = "the adapter uses a storage format this version of Cordial doesn't support. Update Cordial to use this adapter";
pub const FIRMWARE: &str =
    "this version of Cordial doesn't support the adapter's firmware. Update the adapter's firmware";
pub const REQUIRED: &str =
    "the adapter's saved data must be migrated before use. Use migrate to migrate it";
pub const BUSY: &str = "Cordial is migrating an adapter's saved data. Try again when it finishes";
pub const FAILED: &str = "Cordial couldn't migrate the adapter's saved data";
pub const UNKNOWN: &str = "Cordial couldn't confirm whether the adapter saved its migrated data";
pub const CANCELLED: &str = "the migration was cancelled";

/// Whether this version of Cordial may use an adapter, decided from its bootstrap status before
/// any other request.
#[derive(Clone, Debug)]
pub enum Gate {
    /// The adapter isn't waiting for a migration. Its readiness and errors apply as usual.
    Normal,
    /// The adapter's saved data must be migrated by the host first.
    Assist,
    /// The adapter is waiting for a migration, but its other errors stop Cordial from running
    /// one. Only status and restart work.
    Blocked(Error),
    /// Cordial can't use the adapter's storage, and says why.
    Unsupported(Error),
}

/// The marker the adapter reports from boot until restart while it waits for a migration.
fn marker(e: &p::Error) -> bool {
    e.code() == p::ErrorCode::StorageMigrationRequired
        && e.reason() == p::ErrorReason::Storage
        && e.outcome() == p::ErrorOutcome::NoEffect
        && e.related.is_empty()
}

/// Decides from the target format and the migration marker. The committed format is only shown.
pub fn gate(status: &p::Status) -> Gate {
    let target = status
        .information
        .as_ref()
        .and_then(|i| i.target_storage_format);
    if target.is_none() || target == Some(0) {
        return Gate::Unsupported(Error::new(FIRMWARE));
    }
    if migration::check_target(status).is_err() {
        return Gate::Unsupported(Error::new(UNSUPPORTED));
    }
    if !status
        .errors
        .iter()
        .any(|e| e.code() == p::ErrorCode::StorageMigrationRequired)
    {
        return Gate::Normal;
    }
    match status.errors.iter().find(|e| !marker(e)) {
        None => Gate::Assist,
        Some(e) => Gate::Blocked(Error::dongle(e.clone(), None)),
    }
}

/// Why `command` can't run while the adapter is in this state, or None when it may.
pub fn refusal(status: &p::Status, command: &Command) -> Option<Error> {
    match (gate(status), command) {
        (_, Command::Status) | (Gate::Normal, _) => None,
        (Gate::Assist | Gate::Blocked(_), Command::Restart) => None,
        (Gate::Assist, _) => Some(Error::new(REQUIRED)),
        (Gate::Blocked(e) | Gate::Unsupported(e), _) => Some(e),
    }
}

/// Migrates the adapter with an ID, starting at an optional port, until done or cancelled,
/// reporting each event. Tests substitute their own.
pub type Engine = Arc<
    dyn Fn(
            &str,
            Option<&str>,
            &AtomicBool,
            &mut dyn FnMut(MigrationEvent),
        ) -> migration::Result<Outcome>
        + Send
        + Sync,
>;

/// The client library's worker over real serial ports.
pub fn live() -> Engine {
    Arc::new(|id, port, cancelled, report| migration::migrate(id, port, cancelled, report))
}

/// Uses supplied connections and their raw observers for every migration session.
pub fn with_backend(
    connector: Arc<crate::controller::Connector>,
    ports: impl Fn() -> io::Result<Vec<cordial_client::serial::PortInfo>> + Send + Sync + 'static,
) -> Engine {
    Arc::new(move |id, port, cancelled, report| {
        migration::migrate_with_backend(
            id,
            port,
            cancelled,
            report,
            |port| connector(port, Box::new(|_| {})),
            &ports,
        )
    })
}

/// A failure as the user reads it: whether the outcome is known, then the worker's reason.
pub fn failure_text(e: &migration::Error) -> String {
    let what = if e.outcome_unknown { UNKNOWN } else { FAILED };
    format!("{what}: {}", e.reason)
}

/// Runs `engine` and reports its result; a worker that panics is a failure. A failure's outcome
/// is unknown when its last event says so.
fn run(
    engine: &Engine,
    id: &str,
    port: Option<&str>,
    cancelled: &AtomicBool,
    report: &mut dyn FnMut(MigrationEvent),
) -> migration::Result<Outcome> {
    let mut unknown = false;
    let mut report = |event: MigrationEvent| {
        if let Some(migration_event::Event::Failure(f)) = &event.event {
            unknown |= f.outcome_unknown;
        }
        report(event);
    };
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        engine(id, port, cancelled, &mut report)
    }))
    .unwrap_or_else(|_| {
        Err(migration::Error {
            reason: "the migration stopped unexpectedly".into(),
            outcome_unknown: true,
        })
    });
    result.map_err(|mut e| {
        e.outcome_unknown |= unknown;
        e
    })
}

/// A migration running on its own thread.
pub struct Worker {
    cancelled: Arc<AtomicBool>,
}

impl Worker {
    /// Starts migrating; `events` sees each event, then `done` the result once the worker has
    /// closed its connection and is about to exit.
    pub fn start(
        engine: &Engine,
        id: String,
        port: Option<String>,
        mut events: impl FnMut(MigrationEvent) + Send + 'static,
        done: impl FnOnce(migration::Result<Outcome>) + Send + 'static,
    ) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (engine, flag) = (engine.clone(), cancelled.clone());
        thread::spawn(move || {
            done(run(&engine, &id, port.as_deref(), &flag, &mut events));
        });
        Self { cancelled }
    }

    /// Asks the worker to stop; its result follows.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// The largest control or event frame, in bytes after the length.
pub const MAX_FRAME: usize = 65_536;

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// Reads one length-prefixed frame, or None at the end of input between frames. The length is
/// checked before anything is allocated for the frame.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    let mut got = 0;
    while got < length.len() {
        match input.read(&mut length[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(invalid("the control input ended inside a frame")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let length = u32::from_le_bytes(length) as usize;
    if !(1..=MAX_FRAME).contains(&length) {
        return Err(invalid(&format!(
            "control frame length {length} is out of range"
        )));
    }
    let mut frame = vec![0; length];
    input.read_exact(&mut frame).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => invalid("the control input ended inside a frame"),
        _ => e,
    })?;
    Ok(Some(frame))
}

/// Writes one length-prefixed frame and flushes it.
pub fn write_frame(out: &mut dyn Write, message: &impl prost::Message) -> io::Result<()> {
    let bytes = message.encode_to_vec();
    if !(1..=MAX_FRAME).contains(&bytes.len()) {
        return Err(invalid("an event doesn't fit in a frame"));
    }
    out.write_all(&(bytes.len() as u32).to_le_bytes())?;
    out.write_all(&bytes)?;
    out.flush()
}

/// Reads one control message.
fn read_control(input: &mut impl Read) -> Result<migration_control::Action, String> {
    match read_frame(input) {
        Ok(Some(frame)) => MigrationControl::decode(&*frame)
            .ok()
            .and_then(|c| c.action)
            .ok_or_else(|| "the control input held an invalid message".to_owned()),
        Ok(None) => Err("the control input ended".to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// Why an IPC migration stops before its worker finishes; the first reason wins.
enum Stop {
    /// The parent sent an explicit cancellation.
    Cancel,
    /// The control input or event output failed, or the process was interrupted.
    Lost(String),
}

#[derive(Clone)]
struct Stopper {
    stop: Arc<Mutex<Option<Stop>>>,
    cancelled: Arc<AtomicBool>,
}

impl Stopper {
    fn stop(&self, why: Stop) {
        self.stop.lock().unwrap().get_or_insert(why);
        self.cancelled.store(true, Ordering::Release);
    }

    fn explicit(&self) -> bool {
        matches!(*self.stop.lock().unwrap(), Some(Stop::Cancel))
    }

    fn lost(&self) -> Option<String> {
        match &*self.stop.lock().unwrap() {
            Some(Stop::Lost(why)) => Some(why.clone()),
            _ => None,
        }
    }
}

/// Lets a signal handler stop an IPC migration. Interruption is a failure, not a cancellation.
#[derive(Clone)]
pub struct Interrupt(Stopper);

impl Interrupt {
    pub fn interrupt(&self) {
        self.0.stop(Stop::Lost("interrupted".into()));
    }
}

/// `cordial migrate --ipc`: reads one start and an optional cancellation from `input` and writes
/// only framed events to `out`. Succeeds only when the worker verified the migrated adapter and
/// closed it; the error otherwise is for diagnostics.
pub struct Ipc {
    stopper: Stopper,
}

impl Default for Ipc {
    fn default() -> Self {
        Self {
            stopper: Stopper {
                stop: Arc::new(Mutex::new(None)),
                cancelled: Arc::new(AtomicBool::new(false)),
            },
        }
    }
}

impl Ipc {
    pub fn interrupter(&self) -> Interrupt {
        Interrupt(self.stopper.clone())
    }

    pub fn run(
        &self,
        mut input: impl Read + Send + 'static,
        out: &mut dyn Write,
        engine: &Engine,
    ) -> Result<(), Error> {
        let fail = |out: &mut dyn Write, reason: String| {
            let _ = write_frame(
                out,
                &MigrationEvent {
                    event: Some(migration_event::Event::Failure(
                        p::migration::MigrationFailure {
                            reason: reason.clone(),
                            outcome_unknown: false,
                        },
                    )),
                },
            );
            Err(Error::new(reason))
        };
        let start = match read_control(&mut input) {
            Ok(migration_control::Action::Start(start)) if !start.adapter_id.is_empty() => start,
            Ok(migration_control::Action::Start(_)) => {
                return fail(out, "the start named no adapter".into());
            }
            Ok(migration_control::Action::Cancel(_)) => {
                return fail(out, "the control input cancelled before a start".into());
            }
            Err(why) => return fail(out, why),
        };
        let stopper = self.stopper.clone();
        thread::spawn(move || {
            let why = match read_control(&mut input) {
                Ok(migration_control::Action::Cancel(_)) => Stop::Cancel,
                Ok(migration_control::Action::Start(_)) => {
                    Stop::Lost("the control input sent a second start".into())
                }
                Err(why) => Stop::Lost(why),
            };
            stopper.stop(why);
        });
        let port = (!start.port_hint.is_empty()).then_some(start.port_hint.as_str());
        // Why writing an event failed; nothing more is written after it.
        let mut broken: Option<String> = None;
        let mut report = |mut event: MigrationEvent| {
            // Only the parent's explicit cancellation is reported as one.
            if matches!(event.event, Some(migration_event::Event::Cancelled(_)))
                && !self.stopper.explicit()
            {
                let reason = self.stopper.lost().unwrap_or_else(|| CANCELLED.into());
                event.event = Some(migration_event::Event::Failure(
                    p::migration::MigrationFailure {
                        reason,
                        outcome_unknown: false,
                    },
                ));
            }
            if broken.is_none()
                && let Err(e) = write_frame(out, &event)
            {
                let why = format!("the event output failed: {e}");
                broken = Some(why.clone());
                self.stopper.stop(Stop::Lost(why));
            }
        };
        let result = run(
            engine,
            &start.adapter_id,
            port,
            &self.stopper.cancelled,
            &mut report,
        );
        match result {
            // The parent may not have read the success, or no longer listens: verified success
            // alone doesn't make the exit successful.
            Ok(Outcome::Success) => match broken.or_else(|| self.stopper.lost()) {
                Some(why) => Err(Error::new(why)),
                None => Ok(()),
            },
            Ok(Outcome::Cancelled) => Err(Error::new(
                self.stopper.lost().unwrap_or_else(|| CANCELLED.into()),
            )),
            Err(e) => Err(Error::new(failure_text(&e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_protocol::migration::{MigrationCancel, MigrationStart};
    use std::{
        io::Cursor,
        sync::mpsc::{self, Receiver, Sender},
        time::Duration,
    };

    /// Input that delivers its bytes one at a time, then stays open until its sender is dropped.
    struct Trickle {
        bytes: Receiver<Vec<u8>>,
        pending: Vec<u8>,
    }

    impl Read for Trickle {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            while self.pending.is_empty() {
                match self.bytes.recv() {
                    Ok(bytes) => self.pending = bytes,
                    Err(_) => return Ok(0),
                }
            }
            out[0] = self.pending.remove(0);
            Ok(1)
        }
    }

    fn trickle() -> (Sender<Vec<u8>>, Trickle) {
        let (tx, bytes) = mpsc::channel();
        (
            tx,
            Trickle {
                bytes,
                pending: Vec::new(),
            },
        )
    }

    fn framed(action: migration_control::Action) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(
            &mut out,
            &MigrationControl {
                action: Some(action),
            },
        )
        .unwrap();
        out
    }

    fn start(id: &str) -> Vec<u8> {
        framed(migration_control::Action::Start(MigrationStart {
            adapter_id: id.into(),
            port_hint: "/dev/ttyACM0".into(),
        }))
    }

    fn cancel() -> Vec<u8> {
        framed(migration_control::Action::Cancel(MigrationCancel {}))
    }

    fn event(event: migration_event::Event) -> MigrationEvent {
        MigrationEvent { event: Some(event) }
    }

    /// An engine that reports progress, then succeeds, or waits for cancellation when `wait`.
    fn engine(wait: bool, called: Arc<AtomicBool>) -> Engine {
        Arc::new(move |id, port, cancelled, report| {
            assert_eq!((id, port), ("0123456789ABCDEF", Some("/dev/ttyACM0")));
            called.store(true, Ordering::SeqCst);
            report(event(migration_event::Event::Progress(Default::default())));
            while wait && !cancelled.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            if cancelled.load(Ordering::Acquire) {
                report(event(migration_event::Event::Cancelled(Default::default())));
                return Ok(Outcome::Cancelled);
            }
            report(event(migration_event::Event::Success(Default::default())));
            Ok(Outcome::Success)
        })
    }

    /// Every event written, checking that the output holds nothing but whole frames.
    fn events(out: &[u8]) -> Vec<migration_event::Event> {
        let mut input = Cursor::new(out);
        let mut events = Vec::new();
        while let Some(frame) = read_frame(&mut input).unwrap() {
            events.push(MigrationEvent::decode(&*frame).unwrap().event.unwrap());
        }
        events
    }

    fn run(
        input: impl Read + Send + 'static,
        wait: bool,
    ) -> (Result<(), Error>, Vec<migration_event::Event>, bool) {
        let called = Arc::new(AtomicBool::new(false));
        let mut out = Vec::new();
        let result = Ipc::default().run(input, &mut out, &engine(wait, called.clone()));
        (result, events(&out), called.load(Ordering::SeqCst))
    }

    #[test]
    fn a_fragmented_start_migrates_and_writes_only_framed_events() {
        let (tx, input) = trickle();
        tx.send(start("0123456789ABCDEF")).unwrap();
        let (result, events, _) = run(input, false);
        result.unwrap();
        assert!(matches!(
            events[..],
            [
                migration_event::Event::Progress(_),
                migration_event::Event::Success(_)
            ]
        ));
        drop(tx);
    }

    #[test]
    fn bad_control_input_fails_before_the_worker_starts() {
        let mut oversize = (MAX_FRAME as u32 + 1).to_le_bytes().to_vec();
        oversize.extend([0; 8]);
        let mut truncated = start("0123456789ABCDEF");
        truncated.pop();
        let mut garbage = 3u32.to_le_bytes().to_vec();
        garbage.extend([0xff; 3]);
        for input in [
            Vec::new(),
            vec![0, 0],
            0u32.to_le_bytes().to_vec(),
            u32::MAX.to_le_bytes().to_vec(),
            oversize,
            truncated,
            garbage,
            cancel(),
            start(""),
        ] {
            let (result, events, called) = run(Cursor::new(input.clone()), false);
            assert!(result.is_err(), "{input:?}");
            assert!(!called, "{input:?}");
            assert!(
                matches!(events[..], [migration_event::Event::Failure(_)]),
                "{input:?}: {events:?}"
            );
        }
    }

    #[test]
    fn only_an_explicit_cancel_is_reported_as_cancelled() {
        let (tx, input) = trickle();
        tx.send(start("0123456789ABCDEF")).unwrap();
        tx.send(cancel()).unwrap();
        let (result, events, _) = run(input, true);
        assert_eq!(result.unwrap_err().message, CANCELLED);
        assert!(matches!(
            events.last(),
            Some(migration_event::Event::Cancelled(_))
        ));
        drop(tx);

        let mut second = start("0123456789ABCDEF");
        second.extend(start("0123456789ABCDEF"));
        let mut torn = start("0123456789ABCDEF");
        torn.extend(&cancel()[..3]);
        let mut wrong = start("0123456789ABCDEF");
        wrong.extend(5u32.to_le_bytes());
        wrong.extend([0xff; 5]);
        // Ending the input, a second start, a torn or malformed frame all fail the migration.
        for input in [start("0123456789ABCDEF"), second, torn, wrong] {
            let (result, events, _) = run(Cursor::new(input), true);
            assert!(result.is_err());
            assert!(
                matches!(events.last(), Some(migration_event::Event::Failure(_))),
                "{events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, migration_event::Event::Cancelled(_)))
            );
        }
    }

    #[test]
    fn an_interrupt_is_a_failure() {
        let (tx, input) = trickle();
        tx.send(start("0123456789ABCDEF")).unwrap();
        let ipc = Ipc::default();
        let interrupt = ipc.interrupter();
        let called = Arc::new(AtomicBool::new(false));
        let seen = called.clone();
        thread::spawn(move || {
            while !seen.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
            interrupt.interrupt();
        });
        let mut out = Vec::new();
        let result = ipc.run(input, &mut out, &engine(true, called));
        assert_eq!(result.unwrap_err().message, "interrupted");
        assert!(matches!(
            events(&out).last(),
            Some(migration_event::Event::Failure(_))
        ));
        drop(tx);
    }

    /// Output that takes `frames` frames, then fails to write the next, or with `flush` to
    /// flush it.
    struct Failing {
        frames: usize,
        flush: bool,
    }

    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.frames > 0 || self.flush {
                Ok(bytes.len())
            } else {
                Err(io::Error::other("closed"))
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.frames == 0 {
                return Err(io::Error::other("closed"));
            }
            self.frames -= 1;
            Ok(())
        }
    }

    #[test]
    fn a_success_the_parent_may_not_have_read_fails() {
        for flush in [false, true] {
            let (tx, input) = trickle();
            tx.send(start("0123456789ABCDEF")).unwrap();
            let mut out = Failing { frames: 1, flush };
            let called = Arc::new(AtomicBool::new(false));
            let result = Ipc::default().run(input, &mut out, &engine(false, called));
            assert_eq!(
                result.unwrap_err().message,
                "the event output failed: closed"
            );
            drop(tx);
        }
    }

    /// An engine that writes its success, then returns it once the IPC migration has stopped.
    fn late() -> Engine {
        Arc::new(|_, _, cancelled, report| {
            report(event(migration_event::Event::Success(Default::default())));
            while !cancelled.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(Outcome::Success)
        })
    }

    #[test]
    fn control_input_failing_after_the_success_event_fails() {
        let mut out = Vec::new();
        let result = Ipc::default().run(Cursor::new(start("0123456789ABCDEF")), &mut out, &late());
        assert_eq!(result.unwrap_err().message, "the control input ended");
        assert!(matches!(
            events(&out)[..],
            [migration_event::Event::Success(_)]
        ));

        // An explicit cancellation that arrives too late leaves the verified success.
        let (tx, input) = trickle();
        tx.send(start("0123456789ABCDEF")).unwrap();
        tx.send(cancel()).unwrap();
        let mut out = Vec::new();
        Ipc::default().run(input, &mut out, &late()).unwrap();
        drop(tx);
    }

    #[test]
    fn a_failure_keeps_an_unknown_outcome_from_its_event() {
        let engine: Engine = Arc::new(|_, _, _, report| {
            report(event(migration_event::Event::Failure(
                p::migration::MigrationFailure {
                    reason: "write lost".into(),
                    outcome_unknown: true,
                },
            )));
            Err(migration::Error {
                reason: "write lost".into(),
                outcome_unknown: false,
            })
        });
        assert!(run_engine(&engine).is_err_and(|e| e.outcome_unknown));
        let panics: Engine = Arc::new(|_, _, _, _| panic!("worker bug"));
        assert!(run_engine(&panics).is_err_and(|e| e.outcome_unknown));
    }

    fn run_engine(engine: &Engine) -> migration::Result<Outcome> {
        super::run(engine, "id", None, &AtomicBool::new(false), &mut |_| {})
    }

    fn status(storage_format: Option<u32>, target: u32, errors: &[p::Error]) -> p::Status {
        p::Status {
            information: Some(p::AdapterInformation {
                storage_format,
                target_storage_format: Some(target),
                ..Default::default()
            }),
            errors: errors.to_vec(),
            ..Default::default()
        }
    }

    fn error(code: p::ErrorCode, outcome: p::ErrorOutcome) -> p::Error {
        p::Error {
            code: code as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: outcome as i32,
            ..Default::default()
        }
    }

    fn required() -> p::Error {
        error(
            p::ErrorCode::StorageMigrationRequired,
            p::ErrorOutcome::NoEffect,
        )
    }

    fn failed() -> p::Error {
        error(p::ErrorCode::StorageFailed, p::ErrorOutcome::Unspecified)
    }

    #[test]
    fn only_the_marker_on_a_supported_target_starts_a_migration() {
        use p::ErrorCode as C;
        // The committed format doesn't decide anything.
        for current in [None, Some(1), Some(2)] {
            assert!(matches!(gate(&status(current, 2, &[])), Gate::Normal));
            assert!(matches!(
                gate(&status(current, 2, &[failed()])),
                Gate::Normal
            ));
            assert!(matches!(
                gate(&status(current, 2, &[required()])),
                Gate::Assist
            ));
        }
        let radio = p::Error {
            reason: p::ErrorReason::Radio as i32,
            ..error(C::NotReady, p::ErrorOutcome::NoEffect)
        };
        let mut related = required();
        related.related = vec![failed()];
        let mut unknown = required();
        unknown.outcome = p::ErrorOutcome::Unknown as i32;
        for (errors, first) in [
            (vec![required(), radio.clone()], radio),
            (vec![failed(), required()], failed()),
            (vec![related.clone()], related),
            (vec![unknown.clone()], unknown),
        ] {
            assert!(matches!(
                gate(&status(Some(1), 2, &errors)),
                Gate::Blocked(e) if e.dongle == Some(first.clone())
            ));
        }
        for (s, why) in [
            (status(Some(2), 3, &[required()]), UNSUPPORTED),
            (status(Some(1), 1, &[]), UNSUPPORTED),
            (status(Some(2), 0, &[required()]), FIRMWARE),
            (p::Status::default(), FIRMWARE),
        ] {
            assert!(matches!(gate(&s), Gate::Unsupported(e) if e.message == why));
        }
    }

    #[test]
    fn absent_target_blocks_normal_commands_and_restart_despite_present_information() {
        for errors in [vec![], vec![required()]] {
            let mut s = status(Some(2), 2, &errors);
            s.ready = true;
            s.information.as_mut().unwrap().target_storage_format = None;
            assert!(matches!(gate(&s), Gate::Unsupported(e) if e.message == FIRMWARE));
            for command in [
                Command::Devices,
                Command::Files("/".into()),
                Command::Restart,
            ] {
                assert_eq!(refusal(&s, &command).unwrap().message, FIRMWARE);
            }
            assert!(refusal(&s, &Command::Status).is_none());
        }
    }

    #[test]
    fn restart_works_whenever_the_target_is_supported() {
        let restart = |s| refusal(&s, &Command::Restart);
        let devices = |s| refusal(&s, &Command::Devices);
        assert!(restart(status(Some(2), 2, &[])).is_none());
        assert!(restart(status(Some(2), 2, &[required()])).is_none());
        assert!(restart(status(None, 2, &[required(), failed()])).is_none());
        assert!(restart(status(Some(2), 3, &[])).is_some());
        assert!(devices(status(None, 2, &[failed()])).is_none());
        assert_eq!(
            devices(status(Some(2), 2, &[required()])).unwrap().message,
            REQUIRED
        );
        assert!(devices(status(None, 2, &[required(), failed()])).is_some());
        assert!(refusal(&status(None, 3, &[]), &Command::Status).is_none());
    }
}
