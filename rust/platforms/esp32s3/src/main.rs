include!(concat!(env!("OUT_DIR"), "/metadata.rs"));

use cordial_core::{
    application::{Application, Build},
    bluetooth::Event,
};
use cordial_esp32s3::{board, runtime, storage, usb};
use cordial_usb::{
    Buffers, Usb,
    owner::{Owner, Shared},
};
use embassy_time::{Instant, Timer};
use esp_idf_sys as sys;

type Store = Result<storage::Storage, cordial_core::storage::Error>;
type App = Owner<'static, radio::Records, radio::Radio>;
static USB_IO: cordial_usb::Io = cordial_usb::Io::new();
#[cfg(feature = "btstack")]
#[path = "radio/btstack.rs"]
mod radio;
#[cfg(feature = "esp-nimble")]
#[path = "radio/native.rs"]
mod radio;

fn now() -> u64 {
    Instant::now().as_millis()
}
fn random() -> u64 {
    unsafe { (u64::from(sys::esp_random()) << 32) | u64::from(sys::esp_random()) }
}
#[cfg(feature = "btstack")]
fn fatal() -> ! {
    std::process::abort()
}
#[cfg(feature = "development")]
fn bootloader() -> ! {
    unsafe { sys::platform::cordial_esp_bootloader() }
}

#[embassy_executor::task]
async fn usb_task(serial: &'static str, interfaces: u8) {
    let mut out = [0; 256];
    let mut buffers = Buffers::new(&USB_IO);
    Usb::new(
        usb::driver(&mut out).expect("USB peripheral"),
        serial,
        board::DEFAULT_ADAPTER_NAME,
        interfaces,
        &USB_IO,
        &mut buffers,
    )
    .run()
    .await;
}
/// Starts the radio, then runs the priority loop.
#[embassy_executor::task]
async fn priority_task(owner: &'static App, wake: radio::Wake, available: bool) {
    {
        let mut shared = owner.lock().await;
        let Shared { app, store, radio } = &mut *shared;
        let started = if available {
            let mut address = [0; 6];
            let read =
                unsafe { sys::esp_read_mac(address.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_BT) };
            if read != sys::ESP_OK {
                Err(cordial_core::model::errors::ErrorCode::RadioUnavailable)
            } else {
                cordial_core::identity::Identity::initialize(store, address, random)
                    .await
                    .map_err(|_| cordial_core::model::errors::ErrorCode::StorageFailed)
                    .and_then(|_| radio::start(radio, store))
            }
        } else {
            Err(cordial_core::model::errors::ErrorCode::StorageFailed)
        };
        if let Err(error) = started {
            app.event(Event::Failed(error), store, radio, now()).await;
        }
    }
    let mut indicator = cordial_esp32s3::indicator::Indicator::new(board::MCU_LED);
    owner
        .priority(
            async |shared: &mut Shared<radio::Records, radio::Radio>| {
                indicator.set(shared.app.manager.radio_ready && shared.app.manager.storage_ready);
            },
            async || {
                embassy_futures::select::select(radio::changed(wake), Timer::after_millis(1)).await;
            },
            now,
        )
        .await
}
#[embassy_executor::task]
async fn secondary_task(owner: &'static App, interfaces: u8) {
    owner.secondary(interfaces, now).await
}

fn main() {
    sys::link_patches();
    let mut unique = [0; 8];
    assert_eq!(
        unsafe {
            sys::esp_efuse_read_field_blob(
                core::ptr::addr_of_mut!(sys::ESP_EFUSE_OPTIONAL_UNIQUE_ID).cast(),
                unique.as_mut_ptr().cast(),
                64,
            )
        },
        sys::ESP_OK
    );
    let serial = Box::leak(format!("{:016X}", u64::from_le_bytes(unique)).into_boxed_str());
    let mut store = storage::open(
        board::STORAGE_IDENTITY,
        board::STORAGE_START..board::STORAGE_END,
    );
    // USB enumerates once, with the saved configuration interfaces. Flash reads complete
    // synchronously, so no executor is needed yet.
    let interfaces = embassy_futures::block_on(cordial_usb::saved_interfaces(
        &mut store,
        board::PROFILE_MEMORY_BUDGET.is_some(),
    ));
    let available = store.is_ok();
    let (radio, handle, wake) = radio::new(store);
    let app = Application::new(Build {
        development: cfg!(feature = "development"),
        version: FIRMWARE_VERSION,
        board: board::HARDWARE,
        default_adapter_name: board::DEFAULT_ADAPTER_NAME,
        adapter_id: serial.into(),
        profile_memory_budget: board::PROFILE_MEMORY_BUDGET,
        #[cfg(feature = "development")]
        bootloader: Some(cordial_core::application::Bootloader { enter: bootloader }),
        #[cfg(feature = "production")]
        bootloader: None,
    });
    let owner: &'static App = Box::leak(Box::new(Owner::new(&USB_IO, app, handle, radio)));
    runtime::run(|spawner| {
        spawner.spawn(usb_task(serial, interfaces).unwrap());
        radio::spawn(spawner);
        spawner.spawn(priority_task(owner, wake, available).unwrap());
        spawner.spawn(secondary_task(owner, interfaces).unwrap());
    });
}
