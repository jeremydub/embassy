use core::{cell::Cell, marker::PhantomData, panic};

use dot15d4::{
    mac::{MacIndication, MacRequest},
    phy::radio::{Radio, RadioFrameMut},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embedded_hal_async::delay::DelayNs;
use rand_core::RngCore;

use crate::driver::{EmbassyUpperLayer, Ieee802154Driver};

pub struct RadioStack<RADIO> {
    tx: Channel<NoopRawMutex, MacRequest, 1>,
    rx: Channel<NoopRawMutex, MacIndication, 1>,
    radio: Cell<Option<RADIO>>,
    hardware_addr: [u8; 8],
}

impl<RADIO: Radio> RadioStack<RADIO> {
    pub fn new(radio: RADIO) -> Self {
        let hardware_addr = radio.ieee802154_address();
        Self {
            tx: Channel::new(),
            rx: Channel::new(),
            radio: Cell::new(Some(radio)),
            hardware_addr,
        }
    }

    pub fn driver(&self) -> Ieee802154Driver<'_, RADIO> {
        Ieee802154Driver {
            rx: None,
            tx: Default::default(),
            tx_channel: self.tx.sender(),
            rx_channel: self.rx.receiver(),
            hardware_addr: self.hardware_addr,
            _radio: PhantomData,
        }
    }
}

impl<RADIO: Radio> RadioStack<RADIO>
where
    for<'a> RADIO::RadioFrame<&'a mut [u8]>: RadioFrameMut<&'a mut [u8]>,
    for<'a> RADIO::TxToken<'a>: From<&'a mut [u8]>,
{
    pub async fn run<Rng, TIMER>(&self, rng: Rng, timer: TIMER) -> !
    where
        Rng: RngCore,
        TIMER: DelayNs + Clone,
    {
        let Some(radio) = self.radio.take() else {
            panic!("Stack already running");
        };
        let upper = EmbassyUpperLayer {
            upper_tx: self.tx.receiver(),
            upper_rx: self.rx.sender(),
        };
        let mut device = dot15d4::Device::new(radio, rng, upper, timer);
        device.run().await
    }
}
