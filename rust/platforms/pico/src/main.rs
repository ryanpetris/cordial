#![no_std]
#![no_main]

extern crate alloc;

include!(concat!(env!("OUT_DIR"), "/metadata.rs"));

use alloc::boxed::Box;
use cordial_btstack::{
    backend::{Backend, State},
    storage::{Handle, Storage},
    transport::Io,
};
use cordial_core::{
    application::{Application, Build},
    bluetooth::Event,
    storage::Error,
};
use cordial_pico::{DmaIrqs, board, radio, services, usb};
use cordial_usb::{
    Buffers, Usb,
    owner::{Owner, Shared},
};
use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_rp::flash::{Async, Flash};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Instant, Timer};

static USB_IO: cordial_usb::Io = cordial_usb::Io::new();
static RADIO_IO: Io = Io::new();
static RADIO_ADDRESS: Signal<CriticalSectionRawMutex, [u8; 6]> = Signal::new();
static READY: Signal<CriticalSectionRawMutex, bool> = Signal::new();
type Records = cordial_record_storage::Storage<
    Flash<'static, embassy_rp::peripherals::FLASH, Async, { board::FLASH_BYTES }>,
    { ((board::STORAGE_END - board::STORAGE_START) as usize / 4096) - 1 },
>;

// A bad layout remains unavailable, while status and development recovery work.
type Store = Result<Records, Error>;
type App = Owner<'static, Handle<'static, Store>, Backend<Store>>;

fn now() -> u64 {
    Instant::now().as_millis()
}
fn fatal() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    fatal()
}

// CYW43's public vendor command programs the preserved Bluetooth address.
unsafe extern "C" fn set_address(address: *const u8, command: *mut u8) {
    let address = unsafe { core::slice::from_raw_parts(address, 6) };
    let command = unsafe { core::slice::from_raw_parts_mut(command, 9) };
    command[..3].copy_from_slice(&[0x01, 0xfc, 0x06]);
    for (to, from) in command[3..].iter_mut().zip(address.iter().rev()) {
        *to = *from;
    }
}

#[embassy_executor::task]
async fn usb_task(
    driver: embassy_rp::Peri<'static, embassy_rp::peripherals::USB>,
    serial: &'static str,
    interfaces: u8,
) {
    let mut buffers = Buffers::new(&USB_IO);
    Usb::new(
        usb::driver(driver),
        serial,
        board::DEFAULT_ADAPTER_NAME,
        interfaces,
        &USB_IO,
        &mut buffers,
    )
    .run()
    .await;
}
#[embassy_executor::task]
async fn priority_task(owner: &'static App, state: &'static State<Store>) {
    let mut indicated = false;
    owner
        .priority(
            async |shared: &mut Shared<Handle<'static, Store>, Backend<Store>>| {
                let Shared { app, store, radio } = shared;
                // Management runs while the Bluetooth host completes initialization.
                if let Some(address) = RADIO_ADDRESS.try_take() {
                    let started = cordial_core::identity::Identity::initialize(
                        store,
                        address,
                        services::random_u64,
                    )
                    .await
                    .map_err(|_| cordial_core::model::errors::ErrorCode::StorageFailed)
                    .and_then(|_| radio.start(Some(address)));
                    if let Err(error) = started {
                        app.event(Event::Failed(error), store, radio, now()).await;
                    }
                }
                let ready = app.manager.radio_ready && app.manager.storage_ready;
                if ready != indicated {
                    READY.signal(ready);
                    indicated = ready;
                }
            },
            async || {
                select(state.changed(), Timer::after_millis(1)).await;
            },
            now,
        )
        .await
}
#[embassy_executor::task]
async fn secondary_task(owner: &'static App, interfaces: u8) {
    owner.secondary(interfaces, now).await
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    let mut config = embassy_rp::config::Config::default();
    config.clocks = board::clocks();
    let p = embassy_rp::init(config);
    let mut indicator = cordial_pico::indicator_pin!(p);
    unsafe {
        services::init_heap();
    }
    let mut flash = Flash::<_, Async, { board::FLASH_BYTES }>::new(p.FLASH, p.DMA_CH1, DmaIrqs);
    let identity = services::identity(&mut flash).expect("board identity");
    let serial = Box::leak(identity.adapter.clone().into_boxed_str());
    let mut records = Records::open_or_provision_blank(
        flash,
        board::STORAGE_START..board::STORAGE_END,
        board::STORAGE_IDENTITY,
    )
    .await;
    // USB enumerates once, with the saved configuration interfaces.
    let interfaces =
        cordial_usb::saved_interfaces(&mut records, board::PROFILE_MEMORY_BUDGET.is_some()).await;
    let store = Box::leak(Box::new(Storage::new(records)));
    let unique = identity.unique;
    let state = Box::leak(Box::new(
        State::new(store, &RADIO_IO, now, fatal).expect("radio state"),
    ));
    let chipset = Box::leak(Box::new(cordial_btstack::ffi::Chipset {
        name: c"CYW43".as_ptr(),
        init: None,
        next_command: None,
        set_baudrate: None,
        set_address: Some(set_address),
    }));
    // This is the only host instance and owner for the lifetime of the board.
    let backend = unsafe { Backend::new(state, Some(chipset)) };
    let app = Application::new(Build {
        development: cfg!(feature = "development"),
        version: FIRMWARE_VERSION,
        board: board::HARDWARE,
        default_adapter_name: board::DEFAULT_ADAPTER_NAME,
        adapter_id: identity.adapter,
        profile_memory_budget: board::PROFILE_MEMORY_BUDGET,
        #[cfg(feature = "development")]
        bootloader: Some(cordial_core::application::Bootloader {
            enter: services::bootloader,
        }),
        #[cfg(feature = "production")]
        bootloader: None,
    });
    let owner: &'static App =
        Box::leak(Box::new(Owner::new(&USB_IO, app, store.handle(), backend)));
    spawner.spawn(priority_task(owner, state).unwrap());
    spawner.spawn(secondary_task(owner, interfaces).unwrap());
    // Queue USB after storage reads, immediately before radio initialization.
    // The synchronous C init finishes before this executor can poll USB;
    // the async Embassy init yields and lets USB management run during startup.
    spawner.spawn(usb_task(p.USB, serial, interfaces).unwrap());
    let Some((mut control, mac)) = radio::init(
        spawner,
        p.PIO0,
        p.DMA_CH0,
        p.DMA_CH2,
        cordial_pico::radio_pins!(p),
        &RADIO_IO,
        unique,
    )
    .await
    else {
        RADIO_IO.fail();
        core::future::pending::<()>().await;
        unreachable!();
    };
    RADIO_ADDRESS.signal(services::bluetooth_address(mac, unique));
    if let Some((pin, active_low)) = board::RADIO_LED {
        control.led(pin, active_low).await;
    }
    loop {
        let ready = READY.wait().await;
        if let Some(led) = indicator.as_mut() {
            led.set_level((ready ^ board::MCU_LED_ACTIVE_LOW).into());
        }
        if let Some((pin, active_low)) = board::RADIO_LED {
            control.led(pin, ready ^ active_low).await;
        }
    }
}
