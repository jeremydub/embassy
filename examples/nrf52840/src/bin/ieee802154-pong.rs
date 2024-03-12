#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_ieee802154::csma::{CsmaConfig, CsmaStack};
use embassy_ieee802154::driver::Ieee802154Driver;
use embassy_net::driver::Driver;
use embassy_nrf::{
    bind_interrupts,
    gpio::{Level, Output, OutputDrive},
    peripherals::{RADIO, RNG},
    radio, rng,
};
use embassy_time::Timer;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

type Radio = embassy_nrf::radio::ieee802154::Radio<'static, RADIO>;

bind_interrupts!(struct Irqs {
    RADIO => radio::InterruptHandler<RADIO>;
    RNG => rng::InterruptHandler<RNG>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());
    let mut led = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);

    let radio = embassy_nrf::radio::ieee802154::Radio::new(p.RADIO, Irqs);

    let csma_config = CsmaConfig::default();
    static CSMA_TASK: StaticCell<CsmaStack<Radio>> = StaticCell::new();
    let task: &'static _ = CSMA_TASK.init(CsmaStack::new(radio, csma_config));
    let mut device: Ieee802154Driver<'static, _> = task.driver();

    loop {
        led.set_high();
        Timer::after_millis(300).await;
        led.set_low();
        Timer::after_millis(300).await;
    }
}

#[embassy_executor::task]
async fn ieee802154_task(csma: CsmaStack<Radio>, p_rng: RNG) -> ! {
    let rng = embassy_nrf::rng::Rng::new(p_rng, Irqs);
    let timer = embassy_time::Delay;
    csma.run(rng, timer).await
}
