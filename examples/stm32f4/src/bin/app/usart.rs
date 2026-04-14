use core::fmt::Write as FmtWrite;
use core::sync::atomic::{AtomicU32, Ordering};

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, BufferedUartRx, BufferedUartTx, Config};
use embassy_stm32::{bind_interrupts, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Instant, Timer};
use embedded_io_async::{Read as IoRead, Write as IoWrite};
use heapless::String;
use static_cell::StaticCell;

bind_interrupts!(pub struct Irqs {
    USART1 => BufferedInterruptHandler<peripherals::USART1>;
});

pub type UsartTx = Mutex<NoopRawMutex, BufferedUartTx<'static>>;
pub type UsartRx = Mutex<NoopRawMutex, BufferedUartRx<'static>>;

static USART_TX: StaticCell<UsartTx> = StaticCell::new();
static USART_RX: StaticCell<UsartRx> = StaticCell::new();

static TX_BUF_CELL: StaticCell<[u8; 256]> = StaticCell::new();
static RX_BUF_CELL: StaticCell<[u8; 256]> = StaticCell::new();

static TASK_A_COUNT: AtomicU32 = AtomicU32::new(0);
static TASK_A_LAST_SECS: AtomicU32 = AtomicU32::new(0);
static TASK_B_COUNT: AtomicU32 = AtomicU32::new(0);
static TASK_B_LAST_SECS: AtomicU32 = AtomicU32::new(0);
static SHELL_COUNT: AtomicU32 = AtomicU32::new(0);
static SHELL_LAST_SECS: AtomicU32 = AtomicU32::new(0);

fn init_uart_mutexes(
    tx: BufferedUartTx<'static>,
    rx: BufferedUartRx<'static>,
) -> (&'static UsartTx, &'static UsartRx) {
    let usart_tx = USART_TX.init(Mutex::new(tx));
    let usart_rx = USART_RX.init(Mutex::new(rx));
    (usart_tx, usart_rx)
}

/// Initialize the USART application tasks.
pub fn init(
    spawner: &Spawner,
    usart: Peri<'static, peripherals::USART1>,
    rx: Peri<'static, peripherals::PA10>,
    tx: Peri<'static, peripherals::PA9>,
    start: Instant,
) {
    let mut config = Config::default();
    config.baudrate = 115200;

    let tx_buf = TX_BUF_CELL.init([0u8; 256]);
    let rx_buf = RX_BUF_CELL.init([0u8; 256]);

    let usart = BufferedUart::new(usart, rx, tx, tx_buf, rx_buf, Irqs, config).unwrap();
    let (tx, rx) = usart.split();
    let (usart_tx, usart_rx) = init_uart_mutexes(tx, rx);

    spawner.spawn(usart_task_a(usart_tx, start).unwrap());
    spawner.spawn(usart_task_b(usart_tx, start).unwrap());
    spawner.spawn(shell_task(usart_tx, usart_rx, start).unwrap());
}

#[embassy_executor::task]
async fn usart_task_a(usart_tx: &'static UsartTx, start: Instant) {
    loop {
        let mut guard = usart_tx.lock().await;
        unwrap!(IoWrite::write(&mut *guard, b"[Task A] Hello from task A!\r\n").await);
        drop(guard);
        let uptime = (Instant::now() - start).as_secs() as u32;
        TASK_A_LAST_SECS.store(uptime, Ordering::Relaxed);
        TASK_A_COUNT.fetch_add(1, Ordering::Relaxed);
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn usart_task_b(usart_tx: &'static UsartTx, start: Instant) {
    loop {
        let mut guard = usart_tx.lock().await;
        unwrap!(IoWrite::write(&mut *guard, b"[Task B] Hello from task B!\r\n").await);
        drop(guard);
        let uptime = (Instant::now() - start).as_secs() as u32;
        TASK_B_LAST_SECS.store(uptime, Ordering::Relaxed);
        TASK_B_COUNT.fetch_add(1, Ordering::Relaxed);
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
pub async fn shell_task(usart_tx: &'static UsartTx, usart_rx: &'static UsartRx, start: Instant) {
    let mut line_buf = [0u8; 128];
    let mut line_len: usize = 0;
    let mut last_was_cr = false;

    // Small chunk reads are fine: `read()` may legitimately return short reads.
    let mut rx_chunk = [0u8; 32];
    loop {
        let n = {
            // Only hold RX lock while waiting for input.
            let mut guard = usart_rx.lock().await;
            match IoRead::read(&mut *guard, &mut rx_chunk).await {
                Ok(n) => n,
                Err(_) => {
                    Timer::after_millis(1).await;
                    continue;
                }
            }
        };

        if n == 0 {
            Timer::after_millis(1).await;
            continue;
        }

        // Echo + responses use TX lock.
        let mut guard = usart_tx.lock().await;
        for &b in &rx_chunk[..n] {
            match b {
                b'\r' | b'\n' => {
                    // Normalize CR/LF to a single newline on the wire.
                    if b == b'\n' && last_was_cr {
                        last_was_cr = false;
                        continue;
                    }
                    last_was_cr = b == b'\r';

                    let _ = IoWrite::write(&mut *guard, b"\r\n").await;

                    // Newline terminates the command line.
                    if line_len == 0 {
                        continue;
                    }

                    if let Ok(cmd) = core::str::from_utf8(&line_buf[..line_len]) {
                        let cmd = cmd.trim();

                        if cmd.eq_ignore_ascii_case("help") {
                            let _ = IoWrite::write(
                                &mut *guard,
                                b"Commands:\r\n  status - show uptime\r\n  ps     - task status\r\n  help   - this message\r\n",
                            )
                            .await;
                        } else if cmd.eq_ignore_ascii_case("status") {
                            let uptime = (Instant::now() - start).as_secs();
                            let mut s: String<64> = String::new();
                            let _ = FmtWrite::write_fmt(&mut s, core::format_args!("Uptime: {} s\r\n", uptime));
                            let _ = IoWrite::write(&mut *guard, s.as_bytes()).await;
                        } else if cmd.eq_ignore_ascii_case("ps") {
                            let now = (Instant::now() - start).as_secs() as u32;
                            let a_count = TASK_A_COUNT.load(Ordering::Relaxed);
                            let a_last = TASK_A_LAST_SECS.load(Ordering::Relaxed);
                            let b_count = TASK_B_COUNT.load(Ordering::Relaxed);
                            let b_last = TASK_B_LAST_SECS.load(Ordering::Relaxed);
                            let s_count = SHELL_COUNT.load(Ordering::Relaxed);
                            let s_last = SHELL_LAST_SECS.load(Ordering::Relaxed);

                            let mut out: String<256> = String::new();
                            let _ = FmtWrite::write_fmt(&mut out, core::format_args!("Uptime: {} s\r\n", now));
                            let _ = FmtWrite::write_fmt(
                                &mut out,
                                core::format_args!(
                                    "Task A: count={} last={}s ago\r\n",
                                    a_count,
                                    now.saturating_sub(a_last)
                                ),
                            );
                            let _ = FmtWrite::write_fmt(
                                &mut out,
                                core::format_args!(
                                    "Task B: count={} last={}s ago\r\n",
                                    b_count,
                                    now.saturating_sub(b_last)
                                ),
                            );
                            let _ = FmtWrite::write_fmt(
                                &mut out,
                                core::format_args!(
                                    "Shell : count={} last={}s ago\r\n",
                                    s_count,
                                    now.saturating_sub(s_last)
                                ),
                            );
                            let _ = IoWrite::write(&mut *guard, out.as_bytes()).await;
                        } else if !cmd.is_empty() {
                            let _ = IoWrite::write(&mut *guard, b"Unknown command. Type 'help'\r\n").await;
                        }
                    }

                    // Reset for next command.
                    line_len = 0;

                    let uptime = (Instant::now() - start).as_secs() as u32;
                    SHELL_LAST_SECS.store(uptime, Ordering::Relaxed);
                    SHELL_COUNT.fetch_add(1, Ordering::Relaxed);
                }

                // Backspace support (common terminals).
                8 | 127 => {
                    if line_len > 0 {
                        line_len -= 1;
                        // Erase the last character on the terminal.
                        let _ = IoWrite::write(&mut *guard, b"\x08 \x08").await;
                    }
                }

                _ => {
                    if line_len < line_buf.len() {
                        line_buf[line_len] = b;
                        line_len += 1;
                        let _ = IoWrite::write(&mut *guard, &[b]).await;
                    } else {
                        // Overflow: drop the current line to avoid confusing partial commands.
                        line_len = 0;
                    }
                }
            }
        }
        drop(guard);
    }
}
