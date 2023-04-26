#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::gpio::Pin;
use embassy_rp::{gpio, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Timer};
use gpio::{AnyPin, Input, Level, Output, Pull};
use {defmt_rtt as _, panic_probe as _};

#[derive(Clone, Copy, Format)]
enum ButtonEvent {
    Pressed(Button),
    Released(Button),
}

#[derive(Clone, Copy, Format)]
enum Button {
    Red,
    Blue,
}

static CHANNEL: Channel<CriticalSectionRawMutex, ButtonEvent, 1> = Channel::new();

const DEBOUNCE_DELAY: u64 = 50;

#[embassy_executor::task(pool_size = 2)]
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
        info!("Button released");
        sender.send(ButtonEvent::Released(button_id)).await;
        Timer::after(Duration::from_millis(DEBOUNCE_DELAY)).await;
    }
}

#[embassy_executor::task]
async fn output_manager(
    mut red_led: Output<'static, peripherals::PIN_16>,
    mut blue_led: Output<'static, peripherals::PIN_17>,
    receiver: Receiver<'static, CriticalSectionRawMutex, ButtonEvent, 1>,
) {
    loop {
        match receiver.recv().await {
            ButtonEvent::Pressed(Button::Red) => red_led.set_high(),
            ButtonEvent::Released(Button::Red) => red_led.set_low(),
            ButtonEvent::Pressed(Button::Blue) => blue_led.set_high(),
            ButtonEvent::Released(Button::Blue) => blue_led.set_low(),
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let red_led = Output::new(p.PIN_16, Level::Low);
    let blue_led = Output::new(p.PIN_17, Level::Low);
    let red_button = Input::new(p.PIN_15.degrade(), Pull::Down);
    let blue_button = Input::new(p.PIN_14.degrade(), Pull::Down);
    let sender = CHANNEL.sender();
    let receiver = CHANNEL.receiver();

    spawner
        .spawn(button_watcher(red_button, Button::Red, sender.clone()))
        .unwrap();
    spawner
        .spawn(button_watcher(blue_button, Button::Blue, sender.clone()))
        .unwrap();
    spawner
        .spawn(output_manager(red_led, blue_led, receiver))
        .unwrap();
}
