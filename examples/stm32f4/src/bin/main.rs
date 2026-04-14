#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};
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
    let start = Instant::now();

    // USART setup + task spawning lives in the USART module.
    // UART1: USART1 PA10(RX), PA9(TX)
    // UART3: USART3 PD9(RX), PD8(TX)
    usart::init(&spawner, p.USART1, p.PA10, p.PA9, p.USART3, p.PD9, p.PD8, start);

    loop {
        Timer::after_secs(60).await;
    }
}
