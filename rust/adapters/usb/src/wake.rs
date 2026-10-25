//! Host permission and one pending input wake for the current USB suspension.
use crate::{CS, Io, Status, Topology};
use core::cell::Cell;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::{blocking_mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::{UsbDevice, driver::Driver};

pub(crate) struct Wake {
    armed: Mutex<CS, Cell<bool>>,
    suspended: Mutex<CS, Cell<Option<Instant>>>,
    requested: Signal<CS, (u64, Instant)>,
}
impl Wake {
    pub const fn new() -> Self {
        Self {
            armed: Mutex::new(Cell::new(false)),
            suspended: Mutex::new(Cell::new(None)),
            requested: Signal::new(),
        }
    }
    pub fn reset(&self) {
        self.arm(false);
        self.suspend(false);
    }
    pub fn arm(&self, enabled: bool) {
        self.armed.lock(|armed| armed.set(enabled));
        if !enabled {
            self.requested.reset();
        }
    }
    pub fn suspend(&self, suspended: bool) {
        self.requested.reset();
        self.suspended
            .lock(|since| since.set(suspended.then(Instant::now)));
    }
    pub fn request(&self, status: Status) {
        if status.configured
            && status.suspended
            && self.armed.lock(Cell::get)
            && let Some(since) = self.suspended.lock(Cell::get)
        {
            self.requested.signal((status.generation, since));
        }
    }
    async fn wait(&self) -> u64 {
        let (generation, since) = self.requested.wait().await;
        // Suspend detection follows at least 3 ms of idle. Waiting a further
        // 5 ms is conservative and keeps the USB 2.0 minimum independent of IRQ latency.
        Timer::at(since + Duration::from_millis(5)).await;
        generation
    }
    fn valid(&self, status: Status, generation: u64) -> bool {
        status.generation == generation
            && status.configured
            && status.suspended
            && self.armed.lock(Cell::get)
    }
}

/// Owns all device bus operations. Returning always requires the caller to
/// disable the device before dropping workers or rebuilding the descriptors.
pub(crate) async fn run<'d, D: Driver<'d>>(device: &mut UsbDevice<'d, D>, io: &Io) -> Topology {
    loop {
        if let Either::Second(topology) =
            select(device.run_until_suspend(), io.reconnect.wait()).await
        {
            return topology;
        }
        loop {
            match select3(device.wait_resume(), io.reconnect.wait(), io.wake.wait()).await {
                Either3::First(()) => break,
                Either3::Second(topology) => return topology,
                Either3::Third(generation) => {
                    if io.wake.valid(io.status(), generation) {
                        // CompleteBus waits for host frames after the pulse. If
                        // rebuild cancels it, run_until_rebuild disables the bus,
                        // which deasserts resume before any other USB operation.
                        match select(device.remote_wakeup(), io.reconnect.wait()).await {
                            Either::First(_) => {}
                            Either::Second(topology) => return topology,
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BusHandler;
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use embassy_usb::Handler;

    fn advance(ms: u64) {
        crate::tests::TICKS.with(|ticks| ticks.set(Duration::from_millis(ms).as_ticks()));
    }
    #[test]
    fn permission_suspend_configuration_and_idle_time_gate_one_pending_wake() {
        advance(100);
        let io = Io::new();
        let mut handler = BusHandler(&io);
        let mut cx = Context::from_waker(Waker::noop());
        handler.configured(true);
        handler.suspended(true);
        io.wake.request(io.status());
        assert!(!io.wake.requested.signaled());
        handler.remote_wakeup_enabled(true);
        handler.suspended(false);
        io.wake.request(io.status());
        assert!(!io.wake.requested.signaled());
        handler.suspended(true);
        for _ in 0..10 {
            io.wake.request(io.status());
        }
        let generation = io.status().generation;
        {
            let mut pending = pin!(io.wake.wait());
            assert!(pending.as_mut().poll(&mut cx).is_pending());
            advance(104);
            assert!(pending.as_mut().poll(&mut cx).is_pending());
            advance(105);
            assert_eq!(pending.as_mut().poll(&mut cx), Poll::Ready(generation));
        }
        assert!(io.wake.valid(io.status(), generation));
        assert!(!io.wake.requested.signaled());
        handler.remote_wakeup_enabled(false);
        assert!(!io.wake.valid(io.status(), generation));
        io.wake.request(io.status());
        assert!(!io.wake.requested.signaled());
        handler.configured(false);
        handler.remote_wakeup_enabled(true);
        handler.suspended(true);
        io.wake.request(io.status());
        assert!(!io.wake.requested.signaled());
    }
    #[test]
    fn reset_disable_and_rebuild_clear_permission_and_stale_requests() {
        for transition in 0..3 {
            let io = Io::new();
            let mut handler = BusHandler(&io);
            handler.configured(true);
            handler.remote_wakeup_enabled(true);
            handler.suspended(true);
            let generation = io.status().generation;
            io.wake.request(io.status());
            assert!(io.wake.requested.signaled());
            match transition {
                0 => handler.reset(),
                1 => handler.enabled(false),
                _ => io.set_topology(Topology::default()),
            }
            assert!(!io.wake.requested.signaled());
            assert!(!io.wake.valid(io.status(), generation));
            handler.configured(true);
            handler.suspended(true);
            io.wake.request(io.status());
            assert!(!io.wake.requested.signaled());
        }
    }
    #[cfg(feature = "gamepad")]
    #[test]
    fn priority_observes_native_buttons_with_suspended_output_enumeration() {
        use cordial_core::{
            bluetooth::{Descriptor, Event, InputReport},
            link::ServiceId,
        };
        use embassy_futures::block_on;
        let io = Io::new();
        let (mut manager, store, mut radio) = crate::tests::support::setup();
        let device = manager.find(77).unwrap();
        let link = manager
            .connect(device, true, 30_000, None, &mut radio)
            .unwrap()
            .unwrap();
        manager
            .connected(
                link,
                alloc::vec![
                    Descriptor::from_slice(ServiceId(0), crate::gamepad::REPORT_DESCRIPTOR)
                        .unwrap()
                ],
                64,
                0,
            )
            .unwrap();
        let mut app = crate::tests::application();
        app.manager = manager;
        let mut shared = crate::owner::Shared { app, store, radio };
        let mut priority = crate::owner::Priority::new(&io);
        BusHandler(&io).configured(true);
        BusHandler(&io).remote_wakeup_enabled(true);
        BusHandler(&io).suspended(true);
        for (buttons, axis, expected) in [
            (1, 100, false),
            (1, 101, false),
            (0, 101, false),
            (2, 102, true),
            (2, 103, false),
        ] {
            let report = crate::gamepad::Report::encode(buttons, &[axis; 8], 0xff, None);
            shared.radio.events.push_back(Event::Input(
                InputReport::new(link, ServiceId(0), 0, report.as_bytes()).unwrap(),
            ));
            block_on(priority.pass(&mut shared, 1));
            assert_eq!(io.wake.requested.signaled(), expected);
            io.wake.requested.reset();
            assert!(!io.status().input_ready());
        }
    }
}
