use core::{cell::Cell, marker::PhantomData, panic};

pub use dot15d4::csma::CsmaConfig;
use dot15d4::{
    csma::CsmaDevice,
    phy::{
        driver::PacketBuffer,
        radio::{Radio, RadioFrameMut},
    },
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embedded_hal_async::delay::DelayNs;
use rand_core::RngCore;

use crate::driver::{EmbassyDriver, Ieee802154Driver};

pub struct CsmaStack<RADIO> {
    config: CsmaConfig,
    tx: Channel<NoopRawMutex, PacketBuffer, 1>,
    rx: Channel<NoopRawMutex, PacketBuffer, 1>,
    radio: Cell<Option<RADIO>>,
    hardware_addr: [u8; 8],
}

impl<RADIO: Radio> CsmaStack<RADIO> {
    pub fn new(radio: RADIO, config: CsmaConfig) -> Self {
        let hardware_addr = radio.ieee802154_address();
        Self {
            config,
            tx: Channel::new(),
            rx: Channel::new(),
            radio: Cell::new(Some(radio)),
            hardware_addr,
        }
    }

    pub fn driver(&self) -> Ieee802154Driver<'_, RADIO> {
        Ieee802154Driver {
            rx: None,
            tx: PacketBuffer::default(),
            tx_channel: self.tx.sender(),
            rx_channel: self.rx.receiver(),
            hardware_addr: self.hardware_addr,
            _radio: PhantomData,
        }
    }
}

impl<RADIO: Radio> CsmaStack<RADIO>
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
        let driver = EmbassyDriver {
            tx: self.tx.receiver(),
            rx: self.rx.sender(),
        };
        let mut csma = CsmaDevice::new(radio, rng, driver, timer, self.config.clone());
        csma.run().await
    }
}
