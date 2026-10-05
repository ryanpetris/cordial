use crate::{Store, now};
use cordial_ble_hid::Backend;
use cordial_esp32s3::native::Native;
use cordial_core::model::errors::ErrorCode as Error;

pub type Radio = Backend<Native>;
pub type Records = Store;
/// What the priority loop waits on for radio activity.
pub type Wake = ();
pub fn spawn(_: embassy_executor::Spawner) {}
pub fn new(store: Store) -> (Radio, Records, Wake) {
    (
        Backend::new(Native::new().expect("Bluetooth owner"), now).expect("Bluetooth events"),
        store,
        (),
    )
}
pub async fn changed(_: Wake) {
    cordial_esp32s3::native::changed().await;
}
pub fn start(radio: &mut Radio, store: &mut Records) -> Result<(), Error> {
    let identity = embassy_futures::block_on(cordial_core::identity::Identity::load(store))
        .map_err(|_| Error::StorageFailed)?
        .ok_or(Error::StorageFailed)?;
    unsafe {
        esp_idf_sys::platform::cordial_ble_identity(identity.0[40..].as_ptr());
    }
    radio.start()
}
