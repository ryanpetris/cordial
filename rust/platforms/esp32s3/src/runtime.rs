//! One Embassy executor inside the platform's existing FreeRTOS task. Wakes
//! use native task notifications, including when USB interrupts wake a future.
use critical_section::Mutex;
use embassy_executor::{Spawner, raw::Executor};
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;
use esp_idf_sys::{self as sys, platform};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    task::Waker,
};

struct Critical;
critical_section::set_impl!(Critical);
unsafe impl critical_section::Impl for Critical {
    unsafe fn acquire() -> critical_section::RawRestoreState {
        unsafe { platform::cordial_esp_critical_enter() };
    }
    unsafe fn release(_: critical_section::RawRestoreState) {
        unsafe { platform::cordial_esp_critical_exit() };
    }
}

#[unsafe(no_mangle)]
fn __pender(context: *mut ()) {
    unsafe { platform::cordial_esp_wake(context.cast()) };
}

struct Time {
    queue: Mutex<RefCell<Queue>>,
    handle: Mutex<Cell<sys::esp_timer_handle_t>>,
}
embassy_time_driver::time_driver_impl!(static TIME: Time = Time {
    queue: Mutex::new(RefCell::new(Queue::new())),
    handle: Mutex::new(Cell::new(std::ptr::null_mut())),
});
// Timer handles are used only under the platform critical section.
unsafe impl Sync for Time {}
unsafe impl Send for Time {}

impl Time {
    fn update(&self, cs: critical_section::CriticalSection<'_>) {
        let now = self.now();
        let next = self.queue.borrow(cs).borrow_mut().next_expiration(now);
        let handle = self.handle.borrow(cs).get();
        unsafe {
            // An already-fired one-shot is inactive; stopping it is harmless.
            sys::esp_timer_stop(handle);
            if next != u64::MAX {
                assert_eq!(
                    sys::esp_timer_start_once(handle, next.saturating_sub(now).max(1)),
                    sys::ESP_OK
                );
            }
        }
    }
}
impl Driver for Time {
    fn now(&self) -> u64 {
        unsafe { sys::esp_timer_get_time() as u64 }
    }
    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            if self.queue.borrow(cs).borrow_mut().schedule_wake(at, waker) {
                self.update(cs);
            }
        });
    }
}
unsafe extern "C" fn timer(_: *mut c_void) {
    critical_section::with(|cs| TIME.update(cs));
}

/// Takes ownership of this FreeRTOS task; call once from application startup.
pub fn run(init: impl FnOnce(Spawner)) -> ! {
    let args = sys::esp_timer_create_args_t {
        callback: Some(timer),
        arg: std::ptr::null_mut(),
        dispatch_method: sys::esp_timer_dispatch_t_ESP_TIMER_TASK,
        name: c"cordial".as_ptr(),
        skip_unhandled_events: false,
    };
    let mut handle = std::ptr::null_mut();
    assert_eq!(
        unsafe { sys::esp_timer_create(&args, &mut handle) },
        sys::ESP_OK
    );
    critical_section::with(|cs| TIME.handle.borrow(cs).set(handle));
    let executor = Box::leak(Box::new(Executor::new(unsafe {
        platform::cordial_esp_current_task().cast()
    })));
    init(executor.spawner());
    loop {
        unsafe {
            executor.poll();
            platform::cordial_esp_wait();
        }
    }
}
