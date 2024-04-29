#![no_std]
#![no_main]

use core::str::FromStr;

use embassy_futures::select::select;
use embassy_ieee802154::config::Channel;
use embassy_net::udp::PacketMetadata;
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
    let mut radio = embassy_nrf::radio::ieee802154::Radio::new(p.RADIO, Irqs);
    // and request the address given by the manufacturer
    let hardware_addr = radio.ieee802154_address();

    // Weaken the radio if configured
    if option_env!("WEAKEN")
        .map(|weaken| weaken.parse::<bool>().unwrap_or(false))
        .unwrap_or(false)
    {
        radio.set_transmission_power(-12);
    }

    // We setup CSMA
    let mut csma_config = CsmaConfig::default();
    csma_config.channel = match option_env!("CHANNEL") {
        Some("11") => Channel::_11,
        Some("12") => Channel::_12,
        Some("13") => Channel::_13,
        Some("14") => Channel::_14,
        Some("15") => Channel::_15,
        Some("16") => Channel::_16,
        Some("17") => Channel::_17,
        Some("18") => Channel::_18,
        Some("19") => Channel::_19,
        Some("20") => Channel::_20,
        Some("21") => Channel::_21,
        Some("22") => Channel::_22,
        Some("23") => Channel::_23,
        Some("24") => Channel::_24,
        Some("25") => Channel::_25,
        Some("26") => Channel::_26,
        _ => Channel::_26,
    }; // Change channel, so we do not have interference with other networks by default

    static CSMA_TASK: StaticCell<CsmaStack<Radio>> = StaticCell::new();
    let task: &'static _ = CSMA_TASK.init(CsmaStack::new(radio, csma_config));
    // Ask a driver, such that we can control the csma task, by requesting to transmit/receive frames
    let device: Ieee802154Driver<'static, _> = task.driver();

    // We spawn the task that will control the CSMA task
    unwrap!(spawner.spawn(ieee802154_task(task, p.RNG)));

    let addr = option_env!("ADDRESS")
        .map(|addr| addr.parse::<u16>().unwrap())
        .unwrap_or(((hardware_addr[6] as u16) << 8) | (hardware_addr[7] as u16));
    let is_root: bool = option_env!("ROOT").unwrap_or("false").parse().unwrap();

    let this_addr = Ipv6Address::new(0xfd0e, 0, 0, 0, 0, 0, 0, addr);
    let mut rpl_config = embassy_net::RplConfig::new(embassy_net::RplModeOfOperation::StoringMode);
    if is_root {
        defmt::info!("Mote configured as root with address: {}", this_addr);
        rpl_config = rpl_config.add_root_config(embassy_net::RplRootConfig::new(
            embassy_net::RplInstanceId::Local(42),
            this_addr,
        ));
    } else {
        defmt::info!("Mote configured as normal mote with address: {}", this_addr);
    }

    let multicast_addresses = Vec::from_iter(
        option_env!("MULTICAST_ADDRESSES")
            .iter()
            .flat_map(|addrs| addrs.split(','))
            .filter_map(|addr| Ipv6Address::from_str(addr).ok())
            .inspect(|addr| defmt::info!("Subscribing to multicast address: {}", addr)),
    );
    let config = embassy_net::Config::ipv6_static(embassy_net::StaticConfigV6 {
        address: Ipv6Cidr::new(this_addr, 64),
        multicast_addresses,
        dns_servers: Vec::new(),
        gateway: None,
        rpl_config: Some(rpl_config),
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
        // If we are not 1 -> send UDP packet to 1
        let send_to = option_env!("SEND_TO")
            .and_then(|addr| Ipv6Address::from_str(addr).ok())
            .unwrap_or(Ipv6Address::new(0xfd0e, 0, 0, 0, 0, 0, 0, 1));
        defmt::info!("Sending something to {}", send_to);

        let ep = IpEndpoint::new(send_to.into(), 9400);
        socket
            .send_to(b"Hey, how are you? Can you ping this back to me? Please?", ep)
            .await
            .unwrap();

        select(
            async {
                defmt::info!("Received some data: {}", socket.recv_from(&mut buf).await.unwrap());
            },
            Timer::after(Duration::from_millis(5000)),
        )
        .await;
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
