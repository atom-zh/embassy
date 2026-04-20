use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::{peripherals, Peri};
use embassy_time::Timer;

const HIGH_SECS: u64 = 1;
const LOW_SECS: u64 = 1;

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
        USART1,
        PA10,
        PA9,
        USART3,
        PD9,
        PD8,
        ..
    } = p;

    let pin = Output::new(PE9, Level::High, Speed::Low);
    spawner.spawn(blink_1hz(pin).unwrap());

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
        Timer::after_secs(HIGH_SECS).await;
        pin.set_low();
        Timer::after_secs(LOW_SECS).await;
    }
}
