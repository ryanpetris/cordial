use bt_hci::{
    ControllerToHostPacket, HostToControllerPacket, PacketKind, WriteHci, transport::Transport,
};
use cordial_btstack::transport::{Io, PACKET_BYTES, Packet};
use embassy_futures::join::join;
use embassy_rp::{
    Peri, bind_interrupts,
    dma::Channel,
    gpio::{Level, Output, Pin},
    peripherals::{DMA_CH0, PIO0},
    pio::{InterruptHandler, Pio, PioPin},
};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});
// Divider 30 gives 2.08 MHz GSPI on Pico W and 2.5 MHz on RP2350.
// Faster tested clocks corrupted received Bluetooth data on Pico W.
const RADIO_PIO_CLOCK_DIVIDER: u8 = 30;
pub type Spi = cyw43_pio::PioSpi<'static, PIO0, 0>;

pub fn spi(
    pio: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
    pins: (
        Peri<'static, impl Pin>,
        Peri<'static, impl PioPin>,
        Peri<'static, impl PioPin>,
        Peri<'static, impl Pin>,
    ),
) -> (Output<'static>, Spi) {
    let (power, data, clock, cs) = pins;
    let mut pio = Pio::new(pio, Irqs);
    let spi = Spi::new(
        &mut pio.common,
        pio.sm0,
        RADIO_PIO_CLOCK_DIVIDER.into(),
        pio.irq0,
        Output::new(cs, Level::High),
        data,
        clock,
        Channel::new(dma, crate::DmaIrqs),
    );
    (Output::new(power, Level::Low), spi)
}

// BTstack already serializes HCI commands and ACL packets. Preserve those
// bytes through the bt-hci transport rather than maintaining command models.
struct RawPacket<'a, const KIND: u8>(&'a [u8]);
impl<const KIND: u8> WriteHci for RawPacket<'_, KIND> {
    fn size(&self) -> usize {
        self.0.len()
    }
    fn write_hci<W: embedded_io::Write>(&self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0)
    }
    async fn write_hci_async<W: embedded_io_async::Write>(
        &self,
        mut writer: W,
    ) -> Result<(), W::Error> {
        writer.write_all(self.0).await
    }
}
impl HostToControllerPacket for RawPacket<'_, 1> {
    const KIND: PacketKind = PacketKind::Cmd;
}
impl HostToControllerPacket for RawPacket<'_, 2> {
    const KIND: PacketKind = PacketKind::AclData;
}

async fn run(driver: &cyw43::bluetooth::BtDriver<'_>, io: &Io) {
    let send = async {
        loop {
            let packet = io.next_outbound().await;
            let result = match packet.kind {
                1 => driver.write(&RawPacket::<1>(packet.data())).await,
                2 => driver.write(&RawPacket::<2>(packet.data())).await,
                _ => unreachable!(),
            };
            io.finish_outbound(result.is_ok());
            if result.is_err() {
                io.fail();
                return;
            }
        }
    };
    let receive = async {
        // BtDriver writes the indicator and complete original packet here.
        let mut buffer = [0; PACKET_BYTES + 2];
        loop {
            let (kind, len) = match driver.read(&mut buffer).await {
                Ok(ControllerToHostPacket::Event(p)) => (4, 2 + p.data.len()),
                Ok(ControllerToHostPacket::Acl(p)) => (2, p.size()),
                Ok(ControllerToHostPacket::Sync(p)) => (3, p.size()),
                Ok(ControllerToHostPacket::Iso(p)) => (5, p.size()),
                Err(_) => {
                    io.fail();
                    return;
                }
            };
            let mut packet = Packet::empty();
            packet.kind = kind;
            packet.len = len as u16;
            packet.data_mut().copy_from_slice(&buffer[1..1 + len]);
            io.received(packet).await;
        }
    };
    join(send, receive).await;
}

#[cfg(feature = "firmware")]
#[embassy_executor::task]
async fn driver_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, Spi>>) {
    runner.run().await;
}
#[cfg(feature = "firmware")]
#[embassy_executor::task]
async fn transport_task(driver: cyw43::bluetooth::BtDriver<'static>, io: &'static Io) {
    run(&driver, io).await;
}

pub struct Control(cyw43::Control<'static>);
impl Control {
    pub async fn led(&mut self, pin: u8, value: bool) {
        self.0.gpio_set(pin, value).await;
    }
}

#[cfg(feature = "firmware")]
pub async fn init(
    spawner: embassy_executor::Spawner,
    pio: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
    _spare_dma: Peri<'static, embassy_rp::peripherals::DMA_CH2>,
    pins: (
        Peri<'static, impl Pin>,
        Peri<'static, impl PioPin>,
        Peri<'static, impl PioPin>,
        Peri<'static, impl Pin>,
    ),
    io: &'static Io,
    _unique: [u8; 8],
) -> Option<(Control, [u8; 6])> {
    let (power, spi) = spi(pio, dma, pins);
    static STATE: static_cell::StaticCell<cyw43::State> = static_cell::StaticCell::new();
    let (_net, bt, mut control, runner) = cyw43::new_with_bluetooth(
        STATE.init_with(cyw43::State::new),
        power,
        spi,
        cyw43::aligned_bytes!(env!("CYW43_WIFI")),
        cyw43::aligned_bytes!(env!("CYW43_BT")),
        cyw43::aligned_bytes!(env!("CYW43_NVRAM")),
    )
    .await;
    spawner.spawn(driver_task(runner).unwrap());
    spawner.spawn(transport_task(bt, io).unwrap());
    let address = control.address().await;
    Some((Control(control), address))
}
