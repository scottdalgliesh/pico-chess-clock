#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{gpio, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Timer};
use gpio::{Input, Level, Output, Pull};
use {defmt_rtt as _, panic_probe as _};

#[derive(Clone, Copy, Format)]
enum ButtonEvent {
    Pressed,
    Released,
}

static CHANNEL: Channel<CriticalSectionRawMutex, ButtonEvent, 1> = Channel::new();

#[embassy_executor::task]
async fn button_watcher(
    mut button: Input<'static, peripherals::PIN_15>,
    sender: Sender<'static, CriticalSectionRawMutex, ButtonEvent, 1>,
) {
    loop {
        let debounce_delay = 50;
        button.wait_for_high().await;
        sender.send(ButtonEvent::Pressed).await;
        Timer::after(Duration::from_millis(debounce_delay)).await;

        button.wait_for_low().await;
        info!("Button released");
        sender.send(ButtonEvent::Released).await;
        Timer::after(Duration::from_millis(debounce_delay)).await;
    }
}

#[embassy_executor::task]
async fn output_manager(
    mut led: Output<'static, peripherals::PIN_25>,
    reciever: Receiver<'static, CriticalSectionRawMutex, ButtonEvent, 1>,
) {
    loop {
        match reciever.recv().await {
            ButtonEvent::Pressed => led.set_high(),
            ButtonEvent::Released => led.set_low(),
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let led = Output::new(p.PIN_25, Level::Low);
    let button = Input::new(p.PIN_15, Pull::Down);
    let sender = CHANNEL.sender();
    let reciever = CHANNEL.receiver();

    spawner
        .spawn(button_watcher(button, sender.clone()))
        .unwrap();

    spawner
        .spawn(output_manager(led, reciever.clone()))
        .unwrap();
}
