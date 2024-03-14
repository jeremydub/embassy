#![no_std]
#![no_main]

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::Poll;
use defmt::info;

use defmt::unwrap;
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
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use embassy_net::udp::UdpSocket;
use embassy_net::{IpEndpoint, Ipv6Address, Ipv6Cidr, Stack, StackResources};
use heapless::Vec;

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

    // We setup CSMA
    let csma_config = CsmaConfig::default();
    static CSMA_TASK: StaticCell<CsmaStack<Radio>> = StaticCell::new();
    let task: &'static _ = CSMA_TASK.init(CsmaStack::new(radio, csma_config));
    // Ask a driver, such that we can control the csma task, by requesting to transmit/receive frames
    let device: Ieee802154Driver<'static, _> = task.driver();

    // We spawn the task that will control the CSMA task
    unwrap!(spawner.spawn(ieee802154_task(task, p.RNG)));

    let config = embassy_net::Config::ipv6_static(embassy_net::StaticConfigV6 {
        address: Ipv6Cidr::new(
            Ipv6Address::new(
                0xfd0e,
                0,
                0,
                0,
                0,
                0,
                0,
                option_env!("ADDRESS").unwrap_or("2").parse().unwrap(),
            ),
            64,
        ),
        dns_servers: Vec::new(),
        gateway: None,
    });

    // Init network stack
    let seed: u64 = 10; // XXX this should be csprng
    static STACK_CONTAINER: StaticCell<Stack<Device<'static, 'static>>> = StaticCell::new();
    static STACK_RESOURCES: StaticCell<StackResources<2>> = StaticCell::new();
    let stack = STACK_CONTAINER.init(Stack::new(
        device,
        config,
        STACK_RESOURCES.init(StackResources::<2>::new()),
        seed,
    ));

    // Launch network task
    unwrap!(spawner.spawn(net_task(stack)));

    info!("Network task initialized");

    // Then we can use it!
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    let msg = "Lorem ipsum dolor sit amet, consectetur adipiscing elit."; // Lorem ipsum dolor sit amet, consectetur adipiscing elit. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Lorem ipsum dolor sit amet, consectetur adipiscing elit.";

    loop {
        let mut socket = UdpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(120)));

        let remote_endpoint = IpEndpoint::from((Ipv6Address::new(0xfd0e, 0, 0, 0, 0, 0, 0, 1), 8000));

        info!("connecting...");
        let r = socket.connect(remote_endpoint).await;
        if let Err(e) = r {
            info!("connect error: {:?}", e);
            continue;
        }

        info!("connected!");
        loop {
            info!("sending \"{}\" to {}", msg, remote_endpoint);
            let r = socket.write_all(msg.as_bytes()).await;
            if let Err(e) = r {
                info!("write error: {:?}", e);
                break;
            }

            let _ = socket.flush().await;

            Timer::after(Duration::from_secs(5)).await;
        }
    }
}

/// Run CSMA in the background
#[embassy_executor::task]
async fn ieee802154_task(csma: &'static CsmaStack<Radio>, p_rng: RNG) -> ! {
    let rng = embassy_nrf::rng::Rng::new(p_rng, Irqs);
    let timer = embassy_time::Delay;
    csma.run(rng, timer).await
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<Device<'static, 'static>>) -> ! {
    stack.run().await
}
