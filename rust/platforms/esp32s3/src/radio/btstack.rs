use crate::{Store, fatal, now};
use cordial_btstack::{
    backend::{Backend, State},
    storage::{Handle, Storage},
    transport::Io,
};
use cordial_esp32s3::controller;
use cordial_core::model::errors::ErrorCode as Error;

static IO: Io = Io::new();
pub type Radio = Backend<Store>;
pub type Records = Handle<'static, Store>;
/// What the priority loop waits on for radio activity.
pub type Wake = &'static State<Store>;
#[embassy_executor::task]
async fn transport() {
    controller::run(&IO).await;
}
pub fn spawn(spawner: embassy_executor::Spawner) {
    spawner.spawn(transport().unwrap());
}
pub fn new(store: Store) -> (Radio, Records, Wake) {
    let store = Box::leak(Box::new(Storage::new(store)));
    let state = Box::leak(Box::new(
        State::new(store, &IO, now, fatal).expect("Bluetooth owner"),
    ));
    (unsafe { Backend::new(state, None) }, store.handle(), state)
}
pub async fn changed(wake: Wake) {
    wake.changed().await;
}
pub fn start(radio: &mut Radio, _: &mut Records) -> Result<(), Error> {
    controller::initialize(&IO).map_err(|_| Error::RadioUnavailable)?;
    radio.start(None)
}
