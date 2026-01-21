#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedUart, BufferedInterruptHandler, Config};
use embassy_stm32::{bind_interrupts, peripherals};
use embedded_io_async::Write as IoWrite;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Timer, Instant};
use core::fmt::Write as FmtWrite;
use heapless::String;
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

    // spawn interactive shell task
    let start = Instant::now();
    spawner.spawn(shell_task(usart_bus, start).unwrap());

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

#[embassy_executor::task]
async fn shell_task(usart_bus: &'static UsartBus, start: Instant) {
    let mut line_buf = [0u8; 128];
    loop {
        // read available bytes into buffer via the trait impl on BufferedUart
        let n = {
            let mut guard = usart_bus.lock().await;
            match embedded_io_async::Read::read(&mut *guard, &mut line_buf).await {
                Ok(n) => n,
                Err(_) => { Timer::after_secs(0).await; continue; }
            }
        };

        if n == 0 { Timer::after_secs(0).await; continue; }

        // trim at newline or carriage return
        let mut end = n;
        if let Some(pos) = line_buf[..n].iter().position(|&b| b == b'\n' || b == b'\r') {
            end = pos;
        }

        if let Ok(cmd) = core::str::from_utf8(&line_buf[..end]) {
            let cmd = cmd.trim();
            if cmd.eq_ignore_ascii_case("help") {
                let mut guard = usart_bus.lock().await;
                let _ = guard.write(b"Commands:\r\n  status - show uptime\r\n  help - this message\r\n").await;
            } else if cmd.eq_ignore_ascii_case("status") {
                let uptime = (Instant::now() - start).as_secs();
                let mut s: String<64> = String::new();
                let _ = FmtWrite::write_fmt(&mut s, core::format_args!("Uptime: {} s\r\n", uptime));
                let mut guard = usart_bus.lock().await;
                let _ = guard.write(s.as_bytes()).await;
            } else if !cmd.is_empty() {
                let mut guard = usart_bus.lock().await;
                let _ = guard.write(b"Unknown command. Type 'help'\r\n").await;
            }
        }
    }
}
