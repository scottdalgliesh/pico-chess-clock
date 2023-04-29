#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::convert::AsRef;
use strum_macros::AsRefStr;

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::gpio::{self, Pin};
use embassy_rp::i2c::{self, Config};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::{Delay, Duration, Timer};
use gpio::{AnyPin, Input, Level, Output, Pull};
use hd44780_driver::HD44780;
use {defmt_rtt as _, panic_probe as _};

#[derive(Clone, Copy, Format)]
enum ButtonEvent {
    Pressed(Button),
    Released(Button),
}

#[derive(AsRefStr, Clone, Copy, Format)]
enum Button {
    Red,
    Yellow,
    Blue,
}

static CHANNEL: Channel<CriticalSectionRawMutex, ButtonEvent, 1> = Channel::new();

const DEBOUNCE_DELAY: u64 = 50;

#[embassy_executor::task(pool_size = 3)]
async fn button_watcher(
    mut button: Input<'static, AnyPin>,
    button_id: Button,
    sender: Sender<'static, CriticalSectionRawMutex, ButtonEvent, 1>,
) {
    loop {
        button.wait_for_high().await;
        sender.send(ButtonEvent::Pressed(button_id)).await;
        Timer::after(Duration::from_millis(DEBOUNCE_DELAY)).await;

        button.wait_for_low().await;
        sender.send(ButtonEvent::Released(button_id)).await;
        Timer::after(Duration::from_millis(DEBOUNCE_DELAY)).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut red_led = Output::new(p.PIN_16, Level::Low);
    let mut yellow_led = Output::new(p.PIN_18, Level::Low);
    let mut blue_led = Output::new(p.PIN_17, Level::Low);

    let red_button = Input::new(p.PIN_15.degrade(), Pull::Down);
    let yellow_button = Input::new(p.PIN_13.degrade(), Pull::Down);
    let blue_button = Input::new(p.PIN_14.degrade(), Pull::Down);

    let i2c = i2c::I2c::new_blocking(p.I2C0, p.PIN_1, p.PIN_0, Config::default());
    let mut lcd = HD44780::new_i2c(i2c, 0x27, &mut Delay).unwrap();
    lcd.reset(&mut Delay).unwrap();
    lcd.clear(&mut Delay).unwrap();

    let sender = CHANNEL.sender();
    let receiver = CHANNEL.receiver();

    spawner
        .spawn(button_watcher(red_button, Button::Red, sender.clone()))
        .unwrap();
    spawner
        .spawn(button_watcher(
            yellow_button,
            Button::Yellow,
            sender.clone(),
        ))
        .unwrap();
    spawner
        .spawn(button_watcher(blue_button, Button::Blue, sender.clone()))
        .unwrap();

    // manage outputs
    loop {
        let msg = receiver.recv().await;
        if let ButtonEvent::Pressed(button) = msg {
            lcd.reset(&mut Delay).unwrap();
            lcd.clear(&mut Delay).unwrap();
            lcd.write_str(button.as_ref(), &mut Delay).unwrap();
        }
        match msg {
            ButtonEvent::Pressed(Button::Red) => red_led.set_high(),
            ButtonEvent::Released(Button::Red) => red_led.set_low(),
            ButtonEvent::Pressed(Button::Yellow) => yellow_led.set_high(),
            ButtonEvent::Released(Button::Yellow) => yellow_led.set_low(),
            ButtonEvent::Pressed(Button::Blue) => blue_led.set_high(),
            ButtonEvent::Released(Button::Blue) => blue_led.set_low(),
        }
    }
}
