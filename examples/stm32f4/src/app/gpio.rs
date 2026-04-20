use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::{peripherals, Peri};
use embassy_time::Timer;

const HIGH_MILLIS: u64 = 200;
const LOW_MILLIS: u64 = 200;
const PD15_KEEPALIVE_MILLIS: u64 = 1000;

pub struct UsartPins {
    pub uart1: Peri<'static, peripherals::USART1>,
    pub uart1_rx: Peri<'static, peripherals::PA10>,
    pub uart1_tx: Peri<'static, peripherals::PA9>,
    pub uart3: Peri<'static, peripherals::USART3>,
    pub uart3_rx: Peri<'static, peripherals::PD9>,
    pub uart3_tx: Peri<'static, peripherals::PD8>,
}

pub fn gpio_init(spawner: &Spawner, p: embassy_stm32::Peripherals) -> UsartPins {
    // GPIO configuration lives here (no PE9 argument required from main).
    // Fixed pin: PE9
    let embassy_stm32::Peripherals {
        PE9,
        PD15,
        USART1,
        PA10,
        PA9,
        USART3,
        PD9,
        PD8,
        ..
    } = p;

    let pe9 = Output::new(PE9, Level::High, Speed::Low);
    spawner.spawn(blink_1hz(pe9).unwrap());

    // Fixed pin: PD15, output mode, default high.
    // Keep ownership in a task so lifecycle matches PE9 task style.
    let mut pd15 = Output::new(PD15, Level::High, Speed::Low);
    // spawner.spawn(hold_high(pd15).unwrap());
    pd15.set_high();
    Timer::after_millis(1000);
    pd15.set_low();

    UsartPins {
        uart1: USART1,
        uart1_rx: PA10,
        uart1_tx: PA9,
        uart3: USART3,
        uart3_rx: PD9,
        uart3_tx: PD8,
    }
}

#[embassy_executor::task]
async fn blink_1hz(mut pin: Output<'static>) {
    loop {
        pin.set_high();
        Timer::after_millis(HIGH_MILLIS).await;
        pin.set_low();
        Timer::after_millis(LOW_MILLIS).await;
    }
}
