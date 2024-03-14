#![no_std]
#![no_main]

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::Poll;

use embassy_executor::Spawner;
use embassy_futures::select;
use embassy_ieee802154::csma::{CsmaConfig, CsmaStack};
use embassy_ieee802154::driver::Ieee802154Driver;
use embassy_ieee802154::frame::{Address, Frame, FrameBuilder};
use embassy_ieee802154::radio::Radio as _;
use embassy_net::driver::{Driver, TxToken};
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
    let hardware_addr = [0xab; 8];

    let csma_config = CsmaConfig::default();
    static CSMA_TASK: StaticCell<CsmaStack<Radio>> = StaticCell::new();
    let task: &'static _ = CSMA_TASK.init(CsmaStack::new(radio, csma_config));
    let device: RefCell<Ieee802154Driver<'static, _>> = RefCell::new(task.driver());

    let mut sequence_number: u8 = 10;
    loop {
        let frame_repr = FrameBuilder::new_data(b"who knows?")
            .set_dst_address(Address::BROADCAST)
            .set_src_address(Address::Extended(hardware_addr))
            .set_dst_pan_id(0xffff)
            .set_src_pan_id(0xffff)
            .set_sequence_number(sequence_number)
            .finalize()
            .unwrap();
        sequence_number += 1;

        select::select(
            poll_fn(|cx| match device.borrow_mut().transmit(cx) {
                Some(tx_token) => {
                    tx_token.consume(frame_repr.buffer_len(), |buf| {
                        defmt::debug!("New buffer being sent: {}", buf);
                        let mut frame = Frame::new_unchecked(buf);
                        frame_repr.emit(&mut frame);
                    });

                    Poll::Ready(())
                }
                None => Poll::Pending,
            }),
            poll_fn(|cx| match device.borrow_mut().receive(cx) {
                _ => Poll::<()>::Pending,
            }),
        )
        .await;
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
