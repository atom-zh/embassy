use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::{peripherals, Peri};
use embassy_time::Timer;

const HIGH_MILLIS: u64 = 1000;
const LOW_MILLIS: u64 = 1000;

struct GpioTaskPins {
    pe9: Output<'static>,
    pd13: Output<'static>,
}

pub struct UsartPins {
    pub uart1: Peri<'static, peripherals::USART1>,
    pub uart1_rx: Peri<'static, peripherals::PA10>,
    pub uart1_tx: Peri<'static, peripherals::PA9>,
    pub uart3: Peri<'static, peripherals::USART3>,
    pub uart3_rx: Peri<'static, peripherals::PD9>,
    pub uart3_tx: Peri<'static, peripherals::PD8>,
}

pub fn gpio_init(spawner: &Spawner, p: embassy_stm32::Peripherals) -> UsartPins {
    let embassy_stm32::Peripherals {
        PE9,
        PD13,
        USART1,
        PA10,
        PA9,
        USART3,
        PD9,
        PD8,
        ..
    } = p;

    let pe9 = Output::new(PE9, Level::High, Speed::Low);
    let pd13 = Output::new(PD13, Level::Low, Speed::Low);
    let gpio_pins = GpioTaskPins { pe9, pd13 };
    spawner.spawn(gpio_task(gpio_pins).unwrap());

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
async fn gpio_task(mut pins: GpioTaskPins) {
    // Keep PE9 blinking at 1Hz and pulse PD13 high for the first second only.
    pins.pd13.set_high();
    Timer::after_millis(HIGH_MILLIS).await;
    pins.pd13.set_low();

    loop {
        pins.pe9.set_low();
        Timer::after_millis(LOW_MILLIS).await;
        pins.pe9.set_high();
        Timer::after_millis(HIGH_MILLIS).await;
    }
}
