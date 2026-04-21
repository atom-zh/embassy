use core::sync::atomic::{AtomicU32, Ordering};
use core::fmt::Write as FmtWrite;

use defmt::*;
use embassy_executor::Spawner;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Config, ConfigV4, Ipv4Address, Ipv4Cidr, Stack, StackResources};
use embassy_net_ppp::Runner as PppRunner;
use embassy_stm32::usart::Error as UartError;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::{BufRead, ErrorType, Read, Write};
use heapless::{String, Vec};
use static_cell::StaticCell;

use super::usart::{Uart3Rx, Uart3Tx};

const SERVER_IP: Ipv4Address = Ipv4Address::new(47, 103, 151, 216);
const SERVER_PORT: u16 = 8089;

// 按实际运营商修改 APN。
const APN: &str = "CMNET";
// 若运营商要求PPP认证，请填写用户名/密码。
const PPP_USERNAME: &[u8] = b"";
const PPP_PASSWORD: &[u8] = b"";

static PPP_STATE: StaticCell<embassy_net_ppp::State<4, 4>> = StaticCell::new();
static NET_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
const UART3_LOG_CHUNK: usize = 96;
const UART3_LOG_QUEUE_DEPTH: usize = 64;
const LOG_UART3_PPP_BINARY: bool = false;

#[derive(Copy, Clone)]
struct Uart3LogFrame {
    is_tx: bool,
    len: usize,
    data: [u8; UART3_LOG_CHUNK],
}

static UART3_LOG_CH: Channel<ThreadModeRawMutex, Uart3LogFrame, UART3_LOG_QUEUE_DEPTH> = Channel::new();
static UART3_LOG_DROP_COUNT: AtomicU32 = AtomicU32::new(0);
static UART3_OVERRUN_COUNT: AtomicU32 = AtomicU32::new(0);

fn enqueue_uart3_log(is_tx: bool, bytes: &[u8]) {
    for chunk in bytes.chunks(UART3_LOG_CHUNK) {
        let mut frame = Uart3LogFrame {
            is_tx,
            len: chunk.len(),
            data: [0; UART3_LOG_CHUNK],
        };
        frame.data[..chunk.len()].copy_from_slice(chunk);
        if UART3_LOG_CH.try_send(frame).is_err() {
            let dropped = UART3_LOG_DROP_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped % 100 == 0 {
                warn!("uart3 log queue full, dropped={} frames", dropped);
            }
        }
    }
}

fn log_uart3_tx_bytes(bytes: &[u8]) {
    if !LOG_UART3_PPP_BINARY && looks_like_ppp_binary(bytes) {
        return;
    }
    enqueue_uart3_log(true, bytes);
}

fn log_uart3_rx_bytes(bytes: &[u8]) {
    if !LOG_UART3_PPP_BINARY && looks_like_ppp_binary(bytes) {
        return;
    }
    enqueue_uart3_log(false, bytes);
}

fn log_uart3_chars(tag: &str, bytes: &[u8]) {
    info!("{} {} bytes", tag, bytes.len());

    if looks_like_ppp_binary(bytes) {
        let mut preview: String<128> = String::new();
        for &b in bytes.iter().take(48) {
            match b {
                b'\r' => {
                    let _ = preview.push_str("\\r");
                }
                b'\n' => {
                    let _ = preview.push_str("\\n");
                }
                b'\t' => {
                    let _ = preview.push_str("\\t");
                }
                0x20..=0x7e => {
                    let _ = preview.push(b as char);
                }
                _ => {
                    let _ = preview.push('.');
                }
            }
        }
        info!("{} PPP binary payload, preview: \"{}\"", tag, preview.as_str());
        return;
    }

    for chunk in bytes.chunks(96) {
        let mut line: String<384> = String::new();
        for &b in chunk {
            match b {
                b'\r' => {
                    let _ = line.push_str("\\r");
                }
                b'\n' => {
                    let _ = line.push_str("\\n");
                }
                b'\t' => {
                    let _ = line.push_str("\\t");
                }
                0x20..=0x7e => {
                    let _ = line.push(b as char);
                }
                _ => {
                    let _ = line.push_str("\\x");
                    let _ = line.push(hex_nibble((b >> 4) & 0x0F));
                    let _ = line.push(hex_nibble(b & 0x0F));
                }
            }
        }
        info!("{} chars: \"{}\"", tag, line.as_str());
    }
}

fn looks_like_ppp_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    let mut ppp_markers = 0usize;
    let mut printable = 0usize;
    for &b in bytes {
        if matches!(b, 0x7E | 0x7D | 0xFF | 0x03 | 0xC0 | 0x21) {
            ppp_markers += 1;
        }
        if matches!(b, b'\r' | b'\n' | b'\t' | 0x20..=0x7e) {
            printable += 1;
        }
    }

    let printable_ratio_low = printable * 100 < bytes.len() * 70;
    let has_ppp_signature = ppp_markers >= 3;
    has_ppp_signature || printable_ratio_low
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

pub fn init(spawner: &Spawner, uart3_tx: &'static Uart3Tx, uart3_rx: &'static Uart3Rx) {
    let ppp_state = PPP_STATE.init(embassy_net_ppp::State::<4, 4>::new());
    let (device, ppp_runner) = embassy_net_ppp::new(ppp_state);

    let seed = 0x5EED_1234_9ABC_DEF0u64;
    let (stack, net_runner) = embassy_net::new(
        device,
        Config::default(),
        NET_RESOURCES.init(StackResources::new()),
        seed,
    );

    spawner.spawn(unwrap!(net_task(net_runner)));
    spawner.spawn(unwrap!(ppp_task(stack, ppp_runner, uart3_tx, uart3_rx)));
    spawner.spawn(unwrap!(tcp_client_task(stack)));
    spawner.spawn(unwrap!(ppp_monitor_task(stack)));
    spawner.spawn(unwrap!(uart3_log_task()));
}

#[embassy_executor::task]
async fn ppp_monitor_task(stack: Stack<'static>) -> ! {
    loop {
        info!(
            "ppp monitor: link_up={} config_up={}",
            stack.is_link_up(),
            stack.is_config_up()
        );
        Timer::after_secs(5).await;
    }
}

#[embassy_executor::task]
async fn uart3_log_task() -> ! {
    loop {
        let frame = UART3_LOG_CH.receive().await;
        if frame.is_tx {
            log_uart3_chars("uart3 tx", &frame.data[..frame.len]);
        } else {
            log_uart3_chars("uart3 rx", &frame.data[..frame.len]);
        }
        // Yield to avoid starving PPP tasks when log volume is high.
        Timer::after_millis(0).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, embassy_net_ppp::Device<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn ppp_task(
    stack: Stack<'static>,
    mut runner: PppRunner<'static>,
    uart3_tx: &'static Uart3Tx,
    uart3_rx: &'static Uart3Rx,
) -> ! {
    loop {
        let ppp_cfg = embassy_net_ppp::Config {
            username: PPP_USERNAME,
            password: PPP_PASSWORD,
        };

        info!("ppp: start AT prepare");
        if let Err(e) = modem_prepare_and_dial(uart3_tx, uart3_rx).await {
            warn!("ppp: dial failed: {}", e);
            Timer::after_secs(5).await;
            continue;
        }

        info!("ppp: CONNECT received, entering PPP");
        let port = Uart3PppPort::new(uart3_tx, uart3_rx);
        let run_res = with_timeout(
            Duration::from_secs(120),
            runner.run(port, ppp_cfg, |ipv4| {
                let Some(addr) = ipv4.address else {
                    warn!("ppp: no IPv4 address from peer");
                    return;
                };

                let mut dns_servers = Vec::new();
                for s in ipv4.dns_servers.iter().flatten() {
                    let _ = dns_servers.push(*s);
                }

                stack.set_config_v4(ConfigV4::Static(embassy_net::StaticConfigV4 {
                    address: Ipv4Cidr::new(addr, 0),
                    gateway: None,
                    dns_servers,
                }));
                info!("ppp: ipv4 up, addr={}", addr);
            }),
        )
        .await;

        match run_res {
            Ok(r) => warn!("ppp: runner exited: {:?}", r),
            Err(_) => warn!("ppp: runner timeout (120s), restart link"),
        }
        let _ = at_send_line(uart3_tx, "+++").await;
        Timer::after_secs(2).await;
        let _ = at_send_line(uart3_tx, "ATH").await;
        Timer::after_secs(2).await;
    }
}

#[embassy_executor::task]
async fn tcp_client_task(stack: Stack<'static>) -> ! {
    let mut rx_buf = [0u8; 2048];
    let mut tx_buf = [0u8; 2048];
    let mut rbuf = [0u8; 256];
    let mut connect_attempt: u32 = 0;

    loop {
        connect_attempt = connect_attempt.wrapping_add(1);
        info!("tcp: wait config up, attempt={}", connect_attempt);
        stack.wait_config_up().await;
        info!("tcp: config is up, start connect");

        let mut sock = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
        sock.set_timeout(Some(Duration::from_secs(10)));

        let endpoint = (SERVER_IP, SERVER_PORT);
        info!("tcp: connecting {}:{} (attempt={})", SERVER_IP, SERVER_PORT, connect_attempt);
        match sock.connect(endpoint).await {
            Ok(()) => {
                info!("tcp: connected to whitelist server");
                info!("tcp: local endpoint={:?}", sock.local_endpoint());
                info!("tcp: remote endpoint={:?}", sock.remote_endpoint());
                info!("tcp: socket state={:?}", sock.state());
            }
            Err(e) => {
                warn!("tcp: connect failed: {:?}", e);
                Timer::after_secs(3).await;
                continue;
            }
        }

        let hello = b"STM32F4 PPP online\r\n";
        if let Err(e) = sock.write_all(hello).await {
            warn!("tcp: hello write failed: {:?}", e);
            Timer::after_secs(2).await;
            continue;
        }
        info!("tcp: sent hello {} bytes", hello.len());

        let mut seq: u32 = 0;

        loop {
            seq = seq.wrapping_add(1);
            let mut msg: String<64> = String::new();
            let _ = FmtWrite::write_fmt(&mut msg, core::format_args!("PING seq={}\r\n", seq));

            if let Err(e) = sock.write_all(msg.as_bytes()).await {
                warn!("tcp: write failed: {:?}", e);
                break;
            }
            info!("tcp: sent {} bytes: \"{}\"", msg.len(), msg.as_str());
            info!("tcp: post-write state={:?}", sock.state());

            match with_timeout(Duration::from_secs(2), sock.read(&mut rbuf)).await {
                Ok(Ok(0)) => {
                    warn!("tcp: peer closed");
                    break;
                }
                Ok(Ok(n)) => {
                    info!("tcp: rx {} bytes", n);
                    log_uart3_chars("tcp app rx", &rbuf[..n]);
                }
                Ok(Err(e)) => {
                    warn!("tcp: read error: {:?}", e);
                    break;
                }
                Err(_) => {
                    info!("tcp: read timeout (2s), keepalive continue");
                }
            }

            Timer::after_secs(3).await;
        }

        Timer::after_secs(2).await;
    }
}

struct Uart3PppPort {
    tx: &'static Uart3Tx,
    rx: &'static Uart3Rx,
    rx_buf: [u8; 2048],
    rx_pos: usize,
    rx_len: usize,
}

impl Uart3PppPort {
    fn new(tx: &'static Uart3Tx, rx: &'static Uart3Rx) -> Self {
        Self {
            tx,
            rx,
            rx_buf: [0; 2048],
            rx_pos: 0,
            rx_len: 0,
        }
    }
}

impl ErrorType for Uart3PppPort {
    type Error = embassy_stm32::usart::Error;
}

impl Write for Uart3PppPort {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        log_uart3_tx_bytes(buf);
        let mut tx = self.tx.lock().await;
        tx.write(buf).await?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Read for Uart3PppPort {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let available = self.fill_buf().await?;
        if available.is_empty() || buf.is_empty() {
            return Ok(0);
        }

        let n = core::cmp::min(available.len(), buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for Uart3PppPort {
    async fn fill_buf(&mut self) -> Result<&[u8], Self::Error> {
        if self.rx_pos < self.rx_len {
            return Ok(&self.rx_buf[self.rx_pos..self.rx_len]);
        }

        loop {
            let n = {
                let mut rx = self.rx.lock().await;
                match rx.read_until_idle(&mut self.rx_buf).await {
                    Ok(n) => n,
                    Err(UartError::Overrun) => {
                        let c = UART3_OVERRUN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                        if c % 50 == 0 {
                            warn!("uart3 rx overrun in PPP, count={}", c);
                        }
                        0
                    }
                    Err(e) => return Err(e),
                }
            };

            if n == 0 {
                Timer::after_millis(1).await;
                continue;
            }

            log_uart3_rx_bytes(&self.rx_buf[..n]);

            self.rx_pos = 0;
            self.rx_len = n;
            return Ok(&self.rx_buf[..self.rx_len]);
        }
    }

    fn consume(&mut self, amt: usize) {
        self.rx_pos = core::cmp::min(self.rx_pos + amt, self.rx_len);
        if self.rx_pos >= self.rx_len {
            self.rx_pos = 0;
            self.rx_len = 0;
        }
    }
}

async fn modem_prepare_and_dial(uart3_tx: &'static Uart3Tx, uart3_rx: &'static Uart3Rx) -> Result<(), &'static str> {
    if !at_sync(uart3_tx, uart3_rx).await {
        return Err("AT sync failed (modem may still be in data mode)");
    }

    at_expect(uart3_tx, uart3_rx, "ATE0", "OK", 1500)
        .await
        .map_err(|_| "ATE0 failed")?;
    at_expect(uart3_tx, uart3_rx, "AT+CMEE=2", "OK", 1500)
        .await
        .map_err(|_| "AT+CMEE=2 failed")?;
    at_expect(uart3_tx, uart3_rx, "AT+CPIN?", "READY", 3000)
        .await
        .map_err(|_| "AT+CPIN? not READY")?;

    wait_registered(uart3_tx, uart3_rx).await?;

    let mut apn_cmd: heapless::String<64> = heapless::String::new();
    let _ = core::fmt::write(&mut apn_cmd, format_args!("AT+CGDCONT=1,\"IP\",\"{}\"", APN));
    at_expect(uart3_tx, uart3_rx, apn_cmd.as_str(), "OK", 2000)
        .await
        .map_err(|_| "AT+CGDCONT failed")?;

    // 某些网络需要先激活 PDP 上下文。
    let _ = at_expect(uart3_tx, uart3_rx, "AT+CGACT=1,1", "OK", 8000).await;

    // 首选ATD拨号；失败则尝试CGDATA进入PPP数据态。
    if at_expect(uart3_tx, uart3_rx, "ATD*99***1#", "CONNECT", 30_000).await.is_err() {
        at_expect(uart3_tx, uart3_rx, "AT+CGDATA=\"PPP\",1", "CONNECT", 30_000)
            .await
            .map_err(|_| "PPP enter data mode failed (ATD/CGDATA)")?;
    }
    Ok(())
}

async fn at_sync(uart3_tx: &'static Uart3Tx, uart3_rx: &'static Uart3Rx) -> bool {
    // 1) Normal AT probing.
    for _ in 0..3 {
        if at_expect(uart3_tx, uart3_rx, "AT", "OK", 1500).await.is_ok() {
            return true;
        }
        Timer::after_millis(300).await;
    }

    // 2) Try escape from PPP/data mode using guard time ++++ guard time.
    Timer::after_millis(1200).await;
    {
        log_uart3_tx_bytes(b"+++");
        let mut tx = uart3_tx.lock().await;
        if tx.write(b"+++").await.is_err() {
            return false;
        }
    }
    Timer::after_millis(1200).await;

    // 3) Re-probe AT.
    for _ in 0..4 {
        if at_expect(uart3_tx, uart3_rx, "AT", "OK", 2000).await.is_ok() {
            return true;
        }
        Timer::after_millis(300).await;
    }

    false
}

async fn wait_registered(uart3_tx: &'static Uart3Tx, uart3_rx: &'static Uart3Rx) -> Result<(), &'static str> {
    let start = Instant::now();
    while Instant::now() - start < Duration::from_secs(60) {
        if at_query_contains(uart3_tx, uart3_rx, "AT+CEREG?", &[",1", ",5"], 2000).await {
            return Ok(());
        }
        Timer::after_secs(1).await;
    }
    Err("network register timeout")
}

async fn at_expect(
    uart3_tx: &'static Uart3Tx,
    uart3_rx: &'static Uart3Rx,
    cmd: &str,
    expected: &str,
    timeout_ms: u64,
) -> Result<(), &'static str> {
    at_send_line(uart3_tx, cmd).await.map_err(|_| "at write failed")?;
    if wait_rx_contains(uart3_rx, &[expected, "ERROR"], timeout_ms)
        .await
        .is_some_and(|s| s == expected)
    {
        Ok(())
    } else {
        Err("at expect failed")
    }
}

async fn at_query_contains(
    uart3_tx: &'static Uart3Tx,
    uart3_rx: &'static Uart3Rx,
    cmd: &str,
    expected_any: &[&str],
    timeout_ms: u64,
) -> bool {
    if at_send_line(uart3_tx, cmd).await.is_err() {
        return false;
    }

    let mut patterns: heapless::Vec<&str, 8> = heapless::Vec::new();
    for e in expected_any {
        let _ = patterns.push(*e);
    }
    let _ = patterns.push("ERROR");

    wait_rx_contains(uart3_rx, patterns.as_slice(), timeout_ms).await.is_some()
}

async fn at_send_line(uart3_tx: &'static Uart3Tx, cmd: &str) -> Result<(), embassy_stm32::usart::Error> {
    log_uart3_tx_bytes(cmd.as_bytes());
    log_uart3_tx_bytes(b"\r\n");
    let mut tx = uart3_tx.lock().await;
    tx.write(cmd.as_bytes()).await?;
    tx.write(b"\r\n").await?;
    Ok(())
}

async fn wait_rx_contains<'a>(uart3_rx: &'static Uart3Rx, patterns: &'a [&'a str], timeout_ms: u64) -> Option<&'a str> {
    let start = Instant::now();
    let mut buf = [0u8; 256];
    let mut acc: heapless::Vec<u8, 512> = heapless::Vec::new();

    while Instant::now() - start < Duration::from_millis(timeout_ms) {
        let n = {
            let mut rx = uart3_rx.lock().await;
            match with_timeout(Duration::from_millis(600), rx.read_until_idle(&mut buf)).await {
                Ok(Ok(n)) => n,
                Ok(Err(UartError::Overrun)) => {
                    let c = UART3_OVERRUN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    if c % 50 == 0 {
                        warn!("uart3 rx overrun in AT phase, count={}", c);
                    }
                    0
                }
                Ok(Err(_)) => return None,
                Err(_) => 0,
            }
        };

        if n == 0 {
            continue;
        }

        log_uart3_rx_bytes(&buf[..n]);

        for &b in &buf[..n] {
            if acc.len() >= acc.capacity() {
                acc.remove(0);
            }
            let _ = acc.push(b);
        }

        for p in patterns {
            if contains_subseq(acc.as_slice(), p.as_bytes()) {
                return Some(*p);
            }
        }
    }

    None
}

fn contains_subseq(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}
