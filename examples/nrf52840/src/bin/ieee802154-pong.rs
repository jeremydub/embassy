#![no_std]
#![no_main]

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::Poll;

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_futures::select;
use embassy_ieee802154::csma::{CsmaConfig, CsmaStack};
use embassy_ieee802154::driver::Ieee802154Driver;
use embassy_ieee802154::frame::{Address, Frame, FrameBuilder};
use embassy_ieee802154::radio::Radio as _;
use embassy_net::driver::{Driver, RxToken, TxToken};
use embassy_nrf::{
    bind_interrupts,
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
async fn main(spawner: Spawner) {
    // We configure the peripherals to use an external clock -> important for radio
    let mut config = embassy_nrf::config::Config::default();
    config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;

    let p = embassy_nrf::init(config);

    // We setup the radio
    let radio = embassy_nrf::radio::ieee802154::Radio::new(p.RADIO, Irqs);
    // and request the address given by the manufacturer
    let hardware_addr = radio.ieee802154_address();
    info!("Using LL address: {:?}", hardware_addr);

    // We setup CSMA
    let csma_config = CsmaConfig::default();
    static CSMA_TASK: StaticCell<CsmaStack<Radio>> = StaticCell::new();
    let task: &'static _ = CSMA_TASK.init(CsmaStack::new(radio, csma_config));
    // Ask a driver, such that we can control the csma task, by requesting to transmit/receive frames
    let device: RefCell<Ieee802154Driver<'static, _>> = RefCell::new(task.driver());

    // We spawn the task that will control the CSMA task
    unwrap!(spawner.spawn(ieee802154_task(task, p.RNG)));

    // Here we start the actual application, using only the raw driver
    let sequence_number = RefCell::new(10u8);
    select::select(
        async {
            loop {
                // Every second, we transmit a broadcast frame, with our address as the source
                let frame_repr = {
                    let mut sequence_number = sequence_number.borrow_mut();
                    let frame = FrameBuilder::new_data(b"who knows?")
                        .set_dst_address(Address::BROADCAST)
                        .set_src_address(Address::Extended(hardware_addr))
                        .set_dst_pan_id(0xffff)
                        .set_src_pan_id(0xffff)
                        .set_sequence_number(*sequence_number)
                        .finalize()
                        .unwrap();
                    *sequence_number = sequence_number.wrapping_add(1); // TODO: may not be zero
                    frame
                };

                // Then we request the frame to be transmitted
                poll_fn(|cx| match device.borrow_mut().transmit(cx) {
                    Some(tx_token) => {
                        tx_token.consume(frame_repr.buffer_len(), |buf| {
                            defmt::debug!("New buffer being sent");
                            let mut frame = Frame::new_unchecked(buf);
                            frame_repr.emit(&mut frame);
                        });

                        Poll::Ready(())
                    }
                    None => Poll::Pending,
                })
                .await;

                // We wait for the next second
                Timer::after_millis(1000).await;
            }
        },
        async {
            loop {
                // Here we wait for any incoming frames
                poll_fn(|cx| {
                    let mut device = device.borrow_mut();
                    let Some((rx, tx)) = device.receive(cx) else {
                        return Poll::Pending;
                    };
                    // A frame has arrived, let's open it
                    rx.consume(|buf| {
                        let Ok(frame) = Frame::new(&*buf) else {
                            defmt::error!("Malformed frame received: {}", buf);
                            return;
                        };
                        // Print some debug info about the received packet
                        defmt::info!(
                            "Received a good frame ({}) with payload: ({}) {}",
                            frame.sequence_number(),
                            frame.payload().map(|payload| payload.len()).unwrap_or(0),
                            frame
                                .payload()
                                .map(|payload| unsafe { core::str::from_utf8_unchecked(payload) })
                                .unwrap_or(""),
                        );

                        // Send a respons if it was a broadcast message
                        if let Some(addr) = frame.addressing() {
                            if addr
                                .dst_address()
                                .map(|dst_addr| dst_addr.is_broadcast())
                                .unwrap_or(false)
                            {
                                if let Some(src_addr) = addr.src_address() {
                                    let frame_repr = FrameBuilder::new_data(b"Hi, how are you :)")
                                        .set_dst_address(src_addr)
                                        .set_src_address(Address::Extended(hardware_addr))
                                        .set_dst_pan_id(0xffff)
                                        .set_src_pan_id(0xffff)
                                        .set_sequence_number({
                                            let mut sequence_number = sequence_number.borrow_mut();
                                            let old_number = *sequence_number;
                                            *sequence_number = sequence_number.wrapping_add(1);
                                            old_number
                                        })
                                        .finalize()
                                        .unwrap();

                                    // Consume and transmit the respons frame
                                    tx.consume(frame_repr.buffer_len(), |buf| {
                                        let mut frame = Frame::new_unchecked(buf);
                                        frame_repr.emit(&mut frame);
                                    })
                                }
                            }
                        }
                    });

                    Poll::Ready(())
                })
                .await;
            }
        },
    )
    .await;
}

/// Run CSMA in the background
#[embassy_executor::task]
async fn ieee802154_task(csma: &'static CsmaStack<Radio>, p_rng: RNG) -> ! {
    let rng = embassy_nrf::rng::Rng::new(p_rng, Irqs);
    let timer = embassy_time::Delay;
    csma.run(rng, timer).await
}
