#![no_std]
#![no_main]

use embassy_net::udp::PacketMetadata;
use embassy_net::IpAddress;
use embassy_net::IpEndpoint;
use embassy_time::Duration;
use embassy_time::Timer;
use heapless::Vec;

use defmt::info;

use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_ieee802154::csma::{CsmaConfig, CsmaStack};
use embassy_ieee802154::driver::Ieee802154Driver;
use embassy_ieee802154::radio::Radio as _;
use embassy_nrf::{
    bind_interrupts,
    peripherals::{RADIO, RNG},
    radio, rng,
};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use embassy_net::udp::UdpSocket;
use embassy_net::{Ipv6Address, Ipv6Cidr, Stack, StackResources};

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

    // We setup CSMA
    let csma_config = CsmaConfig::default();
    static CSMA_TASK: StaticCell<CsmaStack<Radio>> = StaticCell::new();
    let task: &'static _ = CSMA_TASK.init(CsmaStack::new(radio, csma_config));
    // Ask a driver, such that we can control the csma task, by requesting to transmit/receive frames
    let device: Ieee802154Driver<'static, _> = task.driver();

    // We spawn the task that will control the CSMA task
    unwrap!(spawner.spawn(ieee802154_task(task, p.RNG)));

    let addr = option_env!("ADDRESS").unwrap_or("1").parse().unwrap();
    let is_root: bool = option_env!("ROOT").unwrap_or("false").parse().unwrap();

    let this_addr = Ipv6Address::new(0xfd0e, 0, 0, 0, 0, 0, 0, addr);
    let rpl_config = embassy_net::RplConfig::new(embassy_net::RplModeOfOperation::StoringMode);
    if is_root {
        defmt::info!("Mote configured as root with address: {}", this_addr);
        rpl_config.add_root_config(embassy_net::RplRootConfig::new(
            embassy_net::RplInstanceId::Local(42),
            this_addr,
        ));
    } else {
        defmt::info!("Mote configured as normal mote with address: {}", this_addr);
    }
    let config = embassy_net::Config::ipv6_static(embassy_net::StaticConfigV6 {
        address: Ipv6Cidr::new(this_addr, 64),
        dns_servers: Vec::new(),
        gateway: None,
        rpl_config: None,
    });

    // Init network stack
    let seed: u64 = 10; // XXX this should be csprng
    static STACK_CONTAINER: StaticCell<Stack<Ieee802154Driver<'static, Radio>>> = StaticCell::new();
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
    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut rx_buffer = [0; 4096];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_buffer = [0; 4096];
    let mut buf = [0; 4096];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
    socket.bind(9400).unwrap();

    loop {
        // If we are 1 -> echo result back
        if addr == 1 {
            let (n, ep) = socket.recv_from(&mut buf).await.unwrap();
            if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                info!("ECHO (to {}): {}", ep, s);
            } else {
                info!("ECHO (to {}): bytearray len {}", ep, n);
            }
        } else {
            // If we are not 1 -> send UDP packet to 1
            let ep = IpEndpoint::new(IpAddress::v6(0xfd0e, 0, 0, 0, 0, 0, 0, 1), 9400);
            socket
                .send_to(b"Hey, how are you? Can you ping this back to me? Please?", ep)
                .await
                .unwrap();

            Timer::after(Duration::from_secs(10)).await;

            if socket.may_recv() {
                defmt::info!("Received some data: {}", socket.recv_from(&mut buf).await.unwrap());
            }
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
async fn net_task(stack: &'static Stack<Ieee802154Driver<'static, Radio>>) -> ! {
    stack.run().await
}
