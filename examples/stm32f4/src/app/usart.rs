use core::convert::Infallible;
use core::fmt::Write as FmtWrite;
use core::sync::atomic::{AtomicU32, Ordering};

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::dma;
use embassy_stm32::mode::Async;
use embassy_stm32::usart::{self, Config, Uart, UartRx, UartTx};
use embassy_stm32::{bind_interrupts, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, ThreadModeRawMutex};
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_time::{Instant, Timer};
use embedded_io_async::{ErrorType, Read as IoRead};
use heapless::String;
use static_cell::StaticCell;

bind_interrupts!(pub struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART3 => usart::InterruptHandler<peripherals::USART3>;
    DMA2_STREAM2 => dma::InterruptHandler<peripherals::DMA2_CH2>;
    DMA2_STREAM7 => dma::InterruptHandler<peripherals::DMA2_CH7>;
    DMA1_STREAM1 => dma::InterruptHandler<peripherals::DMA1_CH1>;
    DMA1_STREAM3 => dma::InterruptHandler<peripherals::DMA1_CH3>;
});

// UART1 is the existing console UART (USART1, PA9/PA10)
pub type Uart1Tx = Mutex<NoopRawMutex, UartTx<'static, Async>>;
pub type Uart1Rx = Mutex<NoopRawMutex, UartRx<'static, Async>>;

// UART3 is the new high-speed passthrough UART (USART3, PD8/PD9)
pub type Uart3Tx = Mutex<NoopRawMutex, UartTx<'static, Async>>;
pub type Uart3Rx = Mutex<NoopRawMutex, UartRx<'static, Async>>;

const UART1_BAUDRATE: u32 = 921_600;
const UART3_BAUDRATE: u32 = 921_600;

pub struct ShellChannelRx;

impl ErrorType for ShellChannelRx {
    type Error = Infallible;
}

impl IoRead for ShellChannelRx {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Block until at least one byte is available.
        buf[0] = UART1_SHELL_CH.receive().await;
        let mut n = 1;

        // Drain any additional bytes that are already queued.
        while n < buf.len() {
            match UART1_SHELL_CH.try_receive() {
                Ok(b) => {
                    buf[n] = b;
                    n += 1;
                }
                Err(_) => break,
            }
        }

        Ok(n)
    }
}

pub type Uart1ShellRx = Mutex<NoopRawMutex, ShellChannelRx>;

static UART1_TX: StaticCell<Uart1Tx> = StaticCell::new();
static UART1_RX: StaticCell<Uart1Rx> = StaticCell::new();

static UART3_TX: StaticCell<Uart3Tx> = StaticCell::new();
static UART3_RX: StaticCell<Uart3Rx> = StaticCell::new();

static UART1_SHELL_RX: StaticCell<Uart1ShellRx> = StaticCell::new();
static UART1_SHELL_CH: Channel<ThreadModeRawMutex, u8, 256> = Channel::new();
static UART1_TX_CH: Channel<ThreadModeRawMutex, u8, 512> = Channel::new();

#[allow(dead_code)]
static TASK_A_COUNT: AtomicU32 = AtomicU32::new(0);
#[allow(dead_code)]
static TASK_A_LAST_SECS: AtomicU32 = AtomicU32::new(0);
#[allow(dead_code)]
static TASK_B_COUNT: AtomicU32 = AtomicU32::new(0);
#[allow(dead_code)]
static TASK_B_LAST_SECS: AtomicU32 = AtomicU32::new(0);
#[allow(dead_code)]
static SHELL_COUNT: AtomicU32 = AtomicU32::new(0);
#[allow(dead_code)]
static SHELL_LAST_SECS: AtomicU32 = AtomicU32::new(0);

#[inline(never)]
fn debug_after_read_marker(n: usize) {
    // Another stable marker after UART read; good breakpoint target.
    cortex_m::asm::nop();
    core::hint::black_box(n);
}

fn uart1_try_enqueue_bytes(bytes: &[u8]) {
    for &b in bytes {
        let _ = UART1_TX_CH.try_send(b);
    }
}

fn init_uart1_mutexes(tx: UartTx<'static, Async>, rx: UartRx<'static, Async>) -> (&'static Uart1Tx, &'static Uart1Rx) {
    let uart1_tx = UART1_TX.init(Mutex::new(tx));
    let uart1_rx = UART1_RX.init(Mutex::new(rx));
    (uart1_tx, uart1_rx)
}

fn init_uart3_mutexes(tx: UartTx<'static, Async>, rx: UartRx<'static, Async>) -> (&'static Uart3Tx, &'static Uart3Rx) {
    let uart3_tx = UART3_TX.init(Mutex::new(tx));
    let uart3_rx = UART3_RX.init(Mutex::new(rx));
    (uart3_tx, uart3_rx)
}

/// Initialize the USART application tasks.
pub fn init(
    spawner: &Spawner,
    uart1: Peri<'static, peripherals::USART1>,
    uart1_rx: Peri<'static, peripherals::PA10>,
    uart1_tx: Peri<'static, peripherals::PA9>,
    uart1_tx_dma: Peri<'static, peripherals::DMA2_CH7>,
    uart1_rx_dma: Peri<'static, peripherals::DMA2_CH2>,
    uart3: Peri<'static, peripherals::USART3>,
    uart3_rx: Peri<'static, peripherals::PD9>,
    uart3_tx: Peri<'static, peripherals::PD8>,
    uart3_tx_dma: Peri<'static, peripherals::DMA1_CH3>,
    uart3_rx_dma: Peri<'static, peripherals::DMA1_CH1>,
    start: Instant,
) {
    // UART1 (console)
    let mut uart1_cfg = Config::default();
    // Lower console baudrate to avoid RX overrun during heavy load/debug sessions.
    uart1_cfg.baudrate = UART1_BAUDRATE;
    // Wake reader as soon as data arrives.
    uart1_cfg.eager_reads = Some(1);
    let uart1 = Uart::new(uart1, uart1_rx, uart1_tx, uart1_tx_dma, uart1_rx_dma, Irqs, uart1_cfg).unwrap();
    let (uart1_tx, uart1_rx) = uart1.split();
    let (uart1_tx, _uart1_rx) = init_uart1_mutexes(uart1_tx, uart1_rx);

    // UART3 (passthrough)
    let mut uart3_cfg = Config::default();
    uart3_cfg.baudrate = UART3_BAUDRATE;
    uart3_cfg.eager_reads = Some(1);
    let uart3 = Uart::new(uart3, uart3_rx, uart3_tx, uart3_tx_dma, uart3_rx_dma, Irqs, uart3_cfg).unwrap();
    let (uart3_tx, uart3_rx) = uart3.split();
    let (uart3_tx, uart3_rx) = init_uart3_mutexes(uart3_tx, uart3_rx);

    let uart1_shell_rx = UART1_SHELL_RX.init(Mutex::new(ShellChannelRx));

    // Existing tasks temporarily disabled.
    let _ = uart1_shell_rx;
    let _ = start;
    // spawner.spawn(usart_task_a(uart1_tx, start).unwrap());
    // spawner.spawn(usart_task_b(uart1_tx, start).unwrap());
    // spawner.spawn(shell_task(uart1_tx, uart1_shell_rx, start).unwrap());

    // PPP over UART3 + TCP client tasks.
    super::tcp::init(spawner, uart3_tx, uart3_rx);

    // Keep UART1 writer task available for local console output if needed.
    spawner.spawn(uart1_tx_worker(uart1_tx).unwrap());

    // UART3 passthrough tasks must stay disabled while PPP owns UART3.
    // spawner.spawn(uart1_to_uart3_passthrough(uart1_rx, uart3_tx).unwrap());
    // spawner.spawn(uart3_to_uart1_passthrough(uart3_rx).unwrap());
}

#[embassy_executor::task]
async fn usart_task_a(usart_tx: &'static Uart1Tx, start: Instant) {
    loop {
        let mut guard = usart_tx.lock().await;
        unwrap!(guard.write(b"[Task A] Hello from task A!\r\n").await);
        drop(guard);
        let uptime = (Instant::now() - start).as_secs() as u32;
        TASK_A_LAST_SECS.store(uptime, Ordering::Relaxed);
        TASK_A_COUNT.fetch_add(1, Ordering::Relaxed);
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn usart_task_b(usart_tx: &'static Uart1Tx, start: Instant) {
    loop {
        let mut guard = usart_tx.lock().await;
        unwrap!(guard.write(b"[Task B] Hello from task B!\r\n").await);
        drop(guard);
        let uptime = (Instant::now() - start).as_secs() as u32;
        TASK_B_LAST_SECS.store(uptime, Ordering::Relaxed);
        TASK_B_COUNT.fetch_add(1, Ordering::Relaxed);
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
pub async fn shell_task(usart_tx: &'static Uart1Tx, usart_rx: &'static Uart1ShellRx, start: Instant) {
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

                    let _ = guard.write(b"\r\n").await;

                    // Newline terminates the command line.
                    if line_len == 0 {
                        continue;
                    }

                    if let Ok(cmd) = core::str::from_utf8(&line_buf[..line_len]) {
                        let cmd = cmd.trim();

                        if cmd.eq_ignore_ascii_case("help") {
                            let _ = guard
                                .write(b"Commands:\r\n  status - show uptime\r\n  ps     - task status\r\n  help   - this message\r\n")
                                .await;
                        } else if cmd.eq_ignore_ascii_case("status") {
                            let uptime = (Instant::now() - start).as_secs();
                            let mut s: String<64> = String::new();
                            let _ = FmtWrite::write_fmt(&mut s, core::format_args!("Uptime: {} s\r\n", uptime));
                            let _ = guard.write(s.as_bytes()).await;
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
                            let _ = guard.write(out.as_bytes()).await;
                        } else if !cmd.is_empty() {
                            let _ = guard.write(b"Unknown command. Type 'help'\r\n").await;
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
                        let _ = guard.write(b"\x08 \x08").await;
                    }
                }

                _ => {
                    if line_len < line_buf.len() {
                        line_buf[line_len] = b;
                        line_len += 1;
                        let _ = guard.write(&[b]).await;
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

#[embassy_executor::task]
async fn uart1_to_uart3_passthrough(uart1_rx: &'static Uart1Rx, uart3_tx: &'static Uart3Tx) {
    let mut buf = [0u8; 64];
    loop {
        let n = {
            let mut guard = uart1_rx.lock().await;
            match guard.read_until_idle(&mut buf).await {
                Ok(n) => n,
                Err(_) => {
                    info!("uart1 read error");
                    Timer::after_millis(1).await;
                    continue;
                }
            }
        };
        debug_after_read_marker(10);
        if n == 0 {
            Timer::after_millis(1).await;
            continue;
        }

        // // Feed shell (best-effort, drop if full)
        // for &b in &buf[..n] {
        //     let _ = UART1_SHELL_CH.try_send(b);
        // }

        // Echo back to UART1 through single-writer queue (non-blocking to keep RX serviced).
        uart1_try_enqueue_bytes(&buf[..n]);

        // Forward to UART3
        let mut guard = uart3_tx.lock().await;
        let _ = guard.write(&buf[..n]).await;
        drop(guard);
    }
}

#[embassy_executor::task]
async fn uart1_tx_worker(uart1_tx: &'static Uart1Tx) {
    let mut out = [0u8; 64];
    loop {
        out[0] = UART1_TX_CH.receive().await;
        let mut n = 1;
        while n < out.len() {
            match UART1_TX_CH.try_receive() {
                Ok(b) => {
                    out[n] = b;
                    n += 1;
                }
                Err(_) => break,
            }
        }

        let mut guard = uart1_tx.lock().await;
        let _ = guard.write(&out[..n]).await;
    }
}

#[embassy_executor::task]
async fn uart3_to_uart1_passthrough(uart3_rx: &'static Uart3Rx) {
    let mut buf = [0u8; 64];
    let mut line_buf = [0u8; 256];
    let mut line_len: usize = 0;

    loop {
        let n = {
            let mut guard = uart3_rx.lock().await;
            match guard.read_until_idle(&mut buf).await {
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

        for &b in &buf[..n] {
            match b {
                b'\r' => {
                    // Bare CR is ignored as a separator.
                }
                b'\n' => {
                    // Handle LF and CRLF as line terminator.
                    if line_len > 0 {
                        if let Ok(s) = core::str::from_utf8(&line_buf[..line_len]) {
                            info!("uart3 line: {}", s);
                        } else {
                            info!("uart3 line: <non-utf8, {} bytes>", line_len);
                        }
                        line_len = 0;
                    }
                }
                _ => {
                    if line_len < line_buf.len() {
                        line_buf[line_len] = b;
                        line_len += 1;
                    } else {
                        info!("uart3 line too long, dropping");
                        line_len = 0;
                    }
                }
            }
        }

        // Forward to UART1 through single-writer queue (non-blocking to keep RX serviced).
        uart1_try_enqueue_bytes(&buf[..n]);
    }
}
