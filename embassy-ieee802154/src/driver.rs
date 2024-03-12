use core::marker::PhantomData;

use dot15d4::phy::{
    driver::{self, PacketBuffer},
    radio::{self, Radio},
};
use embassy_net_driver::{Capabilities, HardwareAddress, LinkState};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Receiver, Sender, TrySendError},
};

/// Driver for the `dot15d4` side
pub(crate) struct EmbassyDriver<'a> {
    pub(crate) tx: Receiver<'a, NoopRawMutex, PacketBuffer, 1>,
    pub(crate) rx: Sender<'a, NoopRawMutex, PacketBuffer, 1>,
}

impl driver::Driver for EmbassyDriver<'_> {
    async fn transmit(&self) -> PacketBuffer {
        self.tx.receive().await
    }

    async fn received(&self, buffer: PacketBuffer) {
        self.rx.send(buffer).await
    }

    /// Higher layers do not currently support error handling
    async fn error(&self, error: driver::Error) {
        #[cfg(feature = "defmt")]
        defmt::error!("IEEE 802.15.4 Error: {}", error);
    }
}

pub struct Ieee802154Driver<'a, RADIO> {
    pub(crate) rx: Option<PacketBuffer>,
    pub(crate) tx: PacketBuffer,
    pub(crate) tx_channel: Sender<'a, NoopRawMutex, PacketBuffer, 1>,
    pub(crate) rx_channel: Receiver<'a, NoopRawMutex, PacketBuffer, 1>,
    pub(crate) hardware_addr: [u8; 8],
    pub(crate) _radio: PhantomData<RADIO>,
}

impl<'ch, R: Radio> Ieee802154Driver<'ch, R> {
    fn poll(&mut self, cx: &mut core::task::Context) {
        self.poll_transmit(cx);
        self.poll_receive(cx);
    }

    fn poll_receive(&mut self, cx: &mut core::task::Context) {
        match &self.rx {
            // Clean rx buffer can be removed.
            Some(rx) if !rx.dirty => {
                self.rx = None;
            }
            // Dirty rx buffer means we cannot poll the channel
            Some(_rx) => {
                return;
            }
            // No rx buffer means we can poll the channel
            None => (),
        }

        if self.rx_channel.poll_ready_to_receive(cx).is_pending() {
            return;
        }

        match self.rx_channel.try_receive() {
            Ok(msg) => self.rx = Some(msg),
            Err(_e) => {
                #[cfg(feature = "defmt")]
                defmt::error!("channel is ready to receive, but did not return a msg")
            }
        }
    }

    fn poll_transmit(&mut self, cx: &mut core::task::Context) {
        if !self.tx.dirty {
            return;
        }

        if self.tx_channel.poll_ready_to_send(cx).is_pending() {
            return;
        }

        let msg = core::mem::take(&mut self.tx);
        if let Err(e) = self.tx_channel.try_send(msg) {
            match e {
                TrySendError::Full(msg) => {
                    // Put it back and retry.
                    #[cfg(feature = "defmt")]
                    defmt::error!("channel is ready to send, but is full");
                    self.tx = msg;
                }
            }
        }
    }
}

impl<'ch, RADIO: Radio> embassy_net_driver::Driver for Ieee802154Driver<'ch, RADIO>
where
    for<'a> RADIO::TxToken<'a>: From<&'a mut [u8]>,
    for<'b> RADIO::RxToken<'b>: From<&'b mut [u8]>,
{
    type RxToken<'a> = RxToken<'a, RADIO::RxToken<'a>>
    where
        Self: 'a;

    type TxToken<'a> = TxToken<'a, RADIO::TxToken<'a>>
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut core::task::Context) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.poll(cx);

        match (&mut self.rx, self.tx.dirty) {
            (Some(ref mut rx), false) if rx.dirty => {
                let rx = RxToken {
                    frame: rx,
                    _inner_token: PhantomData,
                };
                let tx = TxToken {
                    frame: &mut self.tx,
                    _inner_token: PhantomData,
                };
                Some((rx, tx))
            }
            _ => None,
        }
    }

    fn transmit(&mut self, cx: &mut core::task::Context) -> Option<Self::TxToken<'_>> {
        self.poll(cx);

        if self.tx.dirty {
            return None;
        }

        Some(TxToken {
            frame: &mut self.tx,
            _inner_token: PhantomData,
        })
    }

    fn link_state(&mut self, cx: &mut core::task::Context) -> LinkState {
        self.poll(cx);

        LinkState::Up
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::default();
        caps.max_transmission_unit = 125;
        caps.max_burst_size = Some(1);

        caps
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ieee802154(self.hardware_addr)
    }
}

pub struct TxToken<'a, T> {
    frame: &'a mut PacketBuffer,
    _inner_token: PhantomData<T>,
}

impl<'a, T> embassy_net_driver::TxToken for TxToken<'a, T>
where
    T: radio::TxToken + From<&'a mut [u8]>,
{
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.frame.dirty = true;
        let token = T::from(&mut self.frame.buffer[..]);
        token.consume(len, f)
    }
}

pub struct RxToken<'a, T> {
    frame: &'a mut PacketBuffer,
    _inner_token: PhantomData<T>,
}

impl<'a, T> embassy_net_driver::RxToken for RxToken<'a, T>
where
    T: radio::RxToken + From<&'a mut [u8]>,
{
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        assert!(self.frame.dirty, "Buffer should be new");
        let token = T::from(&mut self.frame.buffer[..]);
        let result = token.consume(f);
        self.frame.dirty = false;
        result
    }
}
