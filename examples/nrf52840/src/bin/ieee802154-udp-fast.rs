#![no_std]
#![no_main]

use embassy_net::udp::PacketMetadata;
use embassy_net::IpAddress;
use embassy_net::IpEndpoint;
use embassy_net::Runner;
use embassy_time::Duration;
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::Vec;

use defmt::info;

use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_ieee802154::{driver::Ieee802154Driver, radio::Radio as _, stack::RadioStack};
use embassy_nrf::{
    bind_interrupts,
    peripherals::{RADIO, RNG},
    radio, rng,
};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use embassy_net::udp::UdpSocket;
use embassy_net::{Ipv6Address, Ipv6Cidr};

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
    let _hardware_addr = radio.ieee802154_address();

    // We setup radio stack
    static RADIO_STACK: StaticCell<RadioStack<Radio>> = StaticCell::new();
    let radio_stack: &'static _ = RADIO_STACK.init(RadioStack::new(radio));
    // Ask a driver, such that we can control the radio task, by requesting to transmit/receive frames
    let driver: Ieee802154Driver<'static, _> = radio_stack.driver();

    // We spawn the task that will control the CSMA task
    unwrap!(spawner.spawn(ieee802154_task(radio_stack, p.RNG)));

    let addr = option_env!("ADDRESS").unwrap_or("1").parse().unwrap();
    let config = embassy_net::Config::ipv6_static(embassy_net::StaticConfigV6 {
        address: Ipv6Cidr::new(Ipv6Address::new(0xfd0e, 0, 0, 0, 0, 0, 0, addr), 64),
        dns_servers: Vec::new(),
        gateway: None,
    });

    // Init network stack
    let seed: u64 = 10; // XXX this should be csprng
    static NET_STACK_RESOURCES: StaticCell<embassy_net::StackResources<2>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        driver,
        config,
        NET_STACK_RESOURCES.init(embassy_net::StackResources::<2>::new()),
        seed,
    );

    // Launch network task
    unwrap!(spawner.spawn(net_task(runner)));

    info!("Network task initialized");

    // Then we can use it!
    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut rx_buffer = [0; 4096];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_buffer = [0; 4096];
    let mut buf = [0; 4096];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
    socket.bind(9400).unwrap();

    let mut tx_count = 0;
    let mut rx_count = 0;

    let timestamp = Instant::now();

    loop {
        // If we are 1 -> Wait for datagram
        if addr == 1 {
            let (_n, _ep) = socket.recv_from(&mut buf).await.unwrap();
            rx_count += 1;
            if rx_count % 100 == 0 {
                info!("Received {} packets", rx_count);
            }
        } else {
            // If we are not 1 -> Send datagram every 10 ms
            let ep = IpEndpoint::new(IpAddress::v6(0xfd0e, 0, 0, 0, 0, 0, 0, 1), 9400);
            // info!("Sending message");
            socket.send_to(b"Hello, World !", ep).await.unwrap();
            tx_count += 1;
            if tx_count % 100 == 0 {
                info!("Sent {} packets", tx_count);
            }
            // Schedule next transmission in 10ms (synchronized with transmission of first packet)
            Timer::at(timestamp + Duration::from_millis((tx_count + 1) * 10)).await;
        }
    }
}

/// Run Radio stack in the background
#[embassy_executor::task]
async fn ieee802154_task(radio_stack: &'static RadioStack<Radio>, p_rng: RNG) -> ! {
    let rng = embassy_nrf::rng::Rng::new(p_rng, Irqs);
    let timer = embassy_time::Delay;
    radio_stack.run(rng, timer).await
}

#[embassy_executor::task]
async fn net_task(mut stack: Runner<'static, Ieee802154Driver<'static, Radio>>) -> ! {
    stack.run().await
}
