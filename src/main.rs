#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::gpio;
use embassy_time::{Duration, Timer};
use gpio::{Input, Level, Output, Pull};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut led = Output::new(p.PIN_25, Level::Low);
    let mut button = Input::new(p.PIN_15, Pull::Down);
    let debounce_delay = 50;

    loop {
        button.wait_for_high().await;
        info!("Button pressed");
        led.set_high();
        Timer::after(Duration::from_millis(debounce_delay)).await;

        button.wait_for_low().await;
        info!("Button released");
        led.set_low();
        Timer::after(Duration::from_millis(debounce_delay)).await;
    }
}
