use core::marker::PhantomData;

use dot15d4::{
    mac::{
        Error as MacError, {MacIndication, MacRequest},
    },
    phy::{
        radio::{self, Radio},
        FrameBuffer,
    },
    upper::UpperLayer,
};
use embassy_net_driver::{Capabilities, HardwareAddress, LinkState};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Receiver, Sender, TrySendError},
};

/// Driver for the `dot15d4` side
pub(crate) struct EmbassyUpperLayer<'a> {
    pub(crate) upper_tx: Receiver<'a, NoopRawMutex, MacRequest, 1>,
    pub(crate) upper_rx: Sender<'a, NoopRawMutex, MacIndication, 1>,
}

impl UpperLayer for EmbassyUpperLayer<'_> {
    async fn mac_request(&self) -> MacRequest {
        self.upper_tx.receive().await
    }

    async fn received_mac_indication(&self, indication: MacIndication) {
        self.upper_rx.send(indication).await
    }

    /// Higher layers do not currently support error handling
    async fn error(&self, error: MacError) {
        #[cfg(feature = "defmt")]
        defmt::error!("IEEE 802.15.4 Error: {}", error);
    }
}

pub struct Ieee802154Driver<'a, RADIO> {
    pub(crate) rx: Option<MacIndication>,
    pub(crate) tx: MacRequest,
    pub(crate) tx_channel: Sender<'a, NoopRawMutex, MacRequest, 1>,
    pub(crate) rx_channel: Receiver<'a, NoopRawMutex, MacIndication, 1>,
    pub(crate) hardware_addr: [u8; 8],
    pub(crate) _radio: PhantomData<RADIO>,
}

impl<'ch, R: Radio> Ieee802154Driver<'ch, R> {
    pub async fn submit_mac_request(&self, request: MacRequest) {
        self.tx_channel.send(request).await;
    }

    fn poll(&mut self, cx: &mut core::task::Context) {
        self.poll_transmit(cx);
        self.poll_receive(cx);
    }

    fn poll_receive(&mut self, cx: &mut core::task::Context) {
        if let Some(MacIndication::McpsData(indication)) = &self.rx {
            if !indication.buffer.dirty {
                self.rx = None;
            } else {
                // Dirty rx buffer means we cannot poll the channel
                return;
            }
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
        match &self.tx {
            MacRequest::McpsDataRequest(request) => {
                if !request.buffer.dirty {
                    return;
                }

                if self.tx_channel.poll_ready_to_send(cx).is_pending() {
                    return;
                }
            }
            _ => {
                return;
            }
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
    type RxToken<'a>
        = RxToken<'a, RADIO::RxToken<'a>>
    where
        Self: 'a;

    type TxToken<'a>
        = TxToken<'a, RADIO::TxToken<'a>>
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut core::task::Context) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.poll(cx);
        match (&mut self.rx, &mut self.tx) {
            (Some(MacIndication::McpsData(indication)), MacRequest::McpsDataRequest(request))
                if !request.buffer.dirty && indication.buffer.dirty =>
            {
                let rx = RxToken {
                    frame: &mut indication.buffer,
                    _inner_token: PhantomData,
                };
                let tx = TxToken {
                    frame: &mut request.buffer,
                    _inner_token: PhantomData,
                };
                Some((rx, tx))
            }
            _ => None,
        }
    }

    fn transmit(&mut self, cx: &mut core::task::Context) -> Option<Self::TxToken<'_>> {
        self.poll(cx);

        match &mut self.tx {
            MacRequest::McpsDataRequest(request) => {
                if request.buffer.dirty {
                    return None;
                }
                Some(TxToken {
                    frame: &mut request.buffer,
                    _inner_token: PhantomData,
                })
            }
            _ => None,
        }
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
    frame: &'a mut FrameBuffer,
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
    frame: &'a mut FrameBuffer,
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
