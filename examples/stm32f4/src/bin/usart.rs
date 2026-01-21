#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedUart, BufferedInterruptHandler, Config};
use embassy_stm32::{bind_interrupts, peripherals};
use embedded_io_async::Write;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USART1 => BufferedInterruptHandler<peripherals::USART1>;
});

type UsartBus = Mutex<NoopRawMutex, BufferedUart<'static>>;

static USART_BUS: StaticCell<UsartBus> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let mut config = Config::default();
    config.baudrate = 115200;

    static TX_BUF_CELL: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF_CELL: StaticCell<[u8; 256]> = StaticCell::new();

    let tx_buf = TX_BUF_CELL.init([0u8; 256]);
    let rx_buf = RX_BUF_CELL.init([0u8; 256]);

    let usart = BufferedUart::new(p.USART1, p.PA10, p.PA9, tx_buf, rx_buf, Irqs, config).unwrap();

    let usart_bus = USART_BUS.init(Mutex::new(usart));

    spawner.spawn(usart_task_a(usart_bus).unwrap());
    spawner.spawn(usart_task_b(usart_bus).unwrap());

    loop {
        Timer::after_secs(60).await;
    }
}

#[embassy_executor::task]
async fn usart_task_a(usart_bus: &'static UsartBus) {
    loop {
        let mut guard = usart_bus.lock().await;
        unwrap!(guard.write(b"[Task A] Hello from task A!\r\n").await);
        drop(guard);
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn usart_task_b(usart_bus: &'static UsartBus) {
    loop {
        let mut guard = usart_bus.lock().await;
        unwrap!(guard.write(b"[Task B] Hello from task B!\r\n").await);
        drop(guard);
        Timer::after_secs(1).await;
    }
}
