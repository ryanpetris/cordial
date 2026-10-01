include!(concat!(env!("OUT_DIR"), "/metadata.rs"));

use cordial_core::{
    application::{Application, Build},
    bluetooth::{Event, EventSource},
};
use cordial_esp32s3::{board, runtime, storage, usb};
use cordial_usb::{Buffers, Usb, owner::Owner};
use embassy_time::{Instant, Timer};
use esp_idf_sys as sys;

type Store = Result<storage::Storage, cordial_core::storage::Error>;
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
async fn usb_task(serial: &'static str) {
    let mut out = [0; 256];
    let mut buffers = Buffers::new(&USB_IO);
    Usb::new(
        usb::driver(&mut out).expect("USB peripheral"),
        serial,
        board::DEFAULT_ADAPTER_NAME,
        &USB_IO,
        &mut buffers,
    )
    .run()
    .await;
}
#[embassy_executor::task]
async fn owner_task(store: Store, serial: &'static str) {
    let available = store.is_ok();
    let (mut radio, mut handle) = radio::new(store);
    let mut app = Application::new(Build {
        development: cfg!(feature = "development"),
        version: FIRMWARE_VERSION,
        board: board::HARDWARE,
        default_adapter_name: board::DEFAULT_ADAPTER_NAME,
        adapter_id: serial.into(),
        #[cfg(feature = "development")]
        bootloader: Some(cordial_core::application::Bootloader { enter: bootloader }),
        #[cfg(feature = "production")]
        bootloader: None,
    });
    let started = if available {
        let mut address = [0; 6];
        let read =
            unsafe { sys::esp_read_mac(address.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_BT) };
        if read != sys::ESP_OK {
            Err(cordial_core::model::errors::ErrorCode::RadioUnavailable)
        } else {
            cordial_core::identity::Identity::initialize(&mut handle, address, random)
                .await
                .map_err(|_| cordial_core::model::errors::ErrorCode::StorageFailed)
                .and_then(|_| radio::start(&mut radio, &mut handle))
        }
    } else {
        Err(cordial_core::model::errors::ErrorCode::StorageFailed)
    };
    if let Err(error) = started {
        app.event(Event::Failed(error), &mut handle, &mut radio, now())
            .await;
    }
    let mut owner = Owner::new(&USB_IO);
    let mut indicator = cordial_esp32s3::indicator::Indicator::new(board::MCU_LED);
    loop {
        EventSource::poll(&mut radio).await;
        while let Some(event) = radio.next_event() {
            app.event(event, &mut handle, &mut radio, now()).await;
        }
        owner.poll(&mut app, &mut handle, &mut radio, now()).await;
        app.poll(&mut handle, &mut radio, USB_IO.status().leds, now())
            .await;
        indicator.set(app.manager.radio_ready && app.manager.storage_ready);
        embassy_futures::select::select3(USB_IO.changed(), radio.changed(), Timer::after_millis(1))
            .await;
    }
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
    let store = storage::open(
        board::STORAGE_IDENTITY,
        board::STORAGE_START..board::STORAGE_END,
    );
    runtime::run(|spawner| {
        spawner.spawn(usb_task(serial).unwrap());
        radio::spawn(spawner);
        spawner.spawn(owner_task(store, serial).unwrap());
    });
}
