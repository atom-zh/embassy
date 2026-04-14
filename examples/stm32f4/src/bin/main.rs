#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedUart, Config};
use embassy_time::{Instant, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

#[path = "app/usart.rs"]
mod usart;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Print a couple of identification registers as early as possible (before HAL init),
    // to help diagnose mismatched chip/feature selections.
    let cpuid = unsafe { core::ptr::read_volatile(0xE000_ED00 as *const u32) };
    info!("CPUID: 0x{:08x}", cpuid);

    let dbg_idcode = unsafe { core::ptr::read_volatile(0xE004_2000 as *const u32) };
    info!("DBGMCU_IDCODE: 0x{:08x}", dbg_idcode);

    let p = embassy_stm32::init(Default::default());

    let mut config = Config::default();
    config.baudrate = 115200;

    static TX_BUF_CELL: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF_CELL: StaticCell<[u8; 256]> = StaticCell::new();

    let tx_buf = TX_BUF_CELL.init([0u8; 256]);
    let rx_buf = RX_BUF_CELL.init([0u8; 256]);

    let usart = BufferedUart::new(p.USART1, p.PA10, p.PA9, tx_buf, rx_buf, usart::Irqs, config).unwrap();
    let start = Instant::now();

    // Split TX/RX so the shell can block on RX without starving the periodic writers.
    let (tx, rx) = usart.split();
    let (usart_tx, usart_rx) = usart::init_uart_mutexes(tx, rx);

    // Spawn application tasks (including shell).
    usart::init(&spawner, usart_tx, usart_rx, start);

    loop {
        Timer::after_secs(60).await;
    }
}
