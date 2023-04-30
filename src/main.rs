#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::fmt::Write;
use hd44780_driver::bus::DataBus;
use heapless::String;

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_rp::gpio::{self, Pin};
use embassy_rp::i2c::{self, Config};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::{Delay, Duration, Instant, Timer};
use gpio::{AnyPin, Input, Level, Output, Pull};
use hd44780_driver::HD44780;
use {defmt_rtt as _, panic_probe as _};

#[derive(Clone, Copy, Format)]
enum ButtonEvent {
    Pressed(Color),
    Held(Color),
}

#[derive(Clone, Copy, Format)]
enum Color {
    Red,
    Yellow,
    Blue,
}

struct Game<'d, P1: Pin, P2: Pin> {
    phase: GameStatus,
    red_player: Player<'d, P1>,
    blue_player: Player<'d, P2>,
}

impl<'d, P1: Pin, P2: Pin> Game<'d, P1, P2> {
    fn display_string<B: DataBus>(&self, lcd: &mut HD44780<B>) {
        let mut buf: String<64> = String::new();
        core::write!(
            &mut buf,
            "{:<8}{:>8}",
            self.red_player.formatted_time(),
            self.blue_player.formatted_time()
        )
        .unwrap();
        lcd.reset(&mut Delay).unwrap();
        lcd.write_str("Red         Blue", &mut Delay).unwrap();
        lcd.set_cursor_pos(40, &mut Delay).unwrap();
        lcd.write_str(&buf, &mut Delay).unwrap();
    }
}

#[derive(PartialEq)]
enum GameStatus {
    PreGame,
    Active,
    Paused,
}

struct Player<'d, P: Pin> {
    time_left: Duration,
    is_active: bool,
    time_activated: Option<Instant>,
    led: Output<'d, P>,
}

impl<'d, P: Pin> Player<'d, P> {
    fn new(led: Output<'d, P>) -> Player<'d, P> {
        Player {
            time_left: Duration::from_secs(DEFAULT_TURN_MINUTES * 60),
            is_active: false,
            time_activated: None,
            led,
        }
    }

    fn decrement_time(&mut self, mins: u64) {
        if self.time_left > Duration::from_secs(mins * 60) {
            self.time_left -= Duration::from_secs(mins * 60);
        } else {
            self.time_left = Duration::from_secs(MAX_TURN_MINUTES * 60);
        }
    }

    fn formatted_time(&self) -> String<32> {
        let mut time_left = self.time_left.clone();
        if let Some(time_activated) = self.time_activated {
            time_left -= Instant::now().duration_since(time_activated);
        }
        let mins = time_left.as_secs() / 60;
        let secs = time_left.as_secs() % 60;
        let mut buf: String<32> = String::new();
        core::write!(&mut buf, "{:>02}:{:>02}", mins, secs).unwrap();
        buf
    }

    fn start_turn(&mut self) {
        if !self.is_active {
            self.is_active = true;
            self.time_activated = Some(Instant::now());
            self.led.set_high();
        }
    }

    fn end_turn(&mut self) {
        if self.is_active {
            self.is_active = false;
            if let Some(time_activated) = self.time_activated {
                self.time_left -= Instant::now().duration_since(time_activated);
                self.time_activated = None;
            };
            self.led.set_low();
        }
    }
}

static CHANNEL: Channel<CriticalSectionRawMutex, ButtonEvent, 1> = Channel::new();

const DEBOUNCE_DELAY_MILLIS: u64 = 20;
const DEFAULT_TURN_MINUTES: u64 = 10;
const MAX_TURN_MINUTES: u64 = 30;
const HOLD_TIME_SECS: u64 = 1;

#[embassy_executor::task(pool_size = 3)]
async fn button_watcher(
    mut button: Input<'static, AnyPin>,
    button_id: Color,
    sender: Sender<'static, CriticalSectionRawMutex, ButtonEvent, 1>,
) {
    loop {
        // monitor for initial button press condition
        if button.is_low() {
            button.wait_for_high().await;
            sender.send(ButtonEvent::Pressed(button_id)).await;
            Timer::after(Duration::from_millis(DEBOUNCE_DELAY_MILLIS)).await;
        }

        // monitor for button held condition
        let time_pressed = Instant::now();
        select(
            Timer::after(Duration::from_secs(HOLD_TIME_SECS)),
            button.wait_for_low(),
        )
        .await;
        if Instant::now().duration_since(time_pressed).as_secs() >= HOLD_TIME_SECS {
            sender.send(ButtonEvent::Held(button_id)).await;
            Timer::after(Duration::from_millis(DEBOUNCE_DELAY_MILLIS)).await;
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    //initialize IO
    let red_led = Output::new(p.PIN_16, Level::Low);
    let mut yellow_led = Output::new(p.PIN_18, Level::Low);
    let blue_led = Output::new(p.PIN_17, Level::Low);

    let red_button = Input::new(p.PIN_15.degrade(), Pull::Down);
    let yellow_button = Input::new(p.PIN_13.degrade(), Pull::Down);
    let blue_button = Input::new(p.PIN_14.degrade(), Pull::Down);

    let i2c = i2c::I2c::new_blocking(p.I2C0, p.PIN_1, p.PIN_0, Config::default());
    let mut lcd = HD44780::new_i2c(i2c, 0x27, &mut Delay).unwrap();
    lcd.clear(&mut Delay).unwrap();

    let sender = CHANNEL.sender();
    let receiver = CHANNEL.receiver();

    spawner
        .spawn(button_watcher(red_button, Color::Red, sender.clone()))
        .unwrap();
    spawner
        .spawn(button_watcher(yellow_button, Color::Yellow, sender.clone()))
        .unwrap();
    spawner
        .spawn(button_watcher(blue_button, Color::Blue, sender.clone()))
        .unwrap();

    // initiate game
    let mut game = Game {
        phase: GameStatus::PreGame,
        red_player: Player::new(red_led),
        blue_player: Player::new(blue_led),
    };

    // Pre-game phase
    while game.phase == GameStatus::PreGame {
        game.display_string(&mut lcd);
        match receiver.recv().await {
            ButtonEvent::Pressed(Color::Red) => game.red_player.decrement_time(1),
            ButtonEvent::Held(Color::Red) => game.red_player.decrement_time(5),
            ButtonEvent::Pressed(Color::Blue) => game.blue_player.decrement_time(1),
            ButtonEvent::Held(Color::Blue) => game.blue_player.decrement_time(5),
            ButtonEvent::Pressed(Color::Yellow) => game.phase = GameStatus::Paused,
            _ => (),
        }
    }

    // game phase
    loop {
        // game paused
        while game.phase == GameStatus::Paused {
            yellow_led.set_high();
            game.display_string(&mut lcd);
            match receiver.recv().await {
                ButtonEvent::Pressed(Color::Red) => game.blue_player.start_turn(),
                ButtonEvent::Pressed(Color::Blue) => game.red_player.start_turn(),
                _ => continue,
            }
            game.phase = GameStatus::Active;
            yellow_led.set_low();
        }

        // active turn
        while game.phase == GameStatus::Active {
            game.display_string(&mut lcd);
            select(
                async {
                    match receiver.recv().await {
                        ButtonEvent::Pressed(Color::Red) => {
                            game.red_player.end_turn();
                            game.blue_player.start_turn();
                        }
                        ButtonEvent::Pressed(Color::Blue) => {
                            game.blue_player.end_turn();
                            game.red_player.start_turn();
                        }
                        ButtonEvent::Pressed(Color::Yellow) => {
                            game.red_player.end_turn();
                            game.blue_player.end_turn();
                            game.phase = GameStatus::Paused;
                        }
                        _ => (),
                    };
                },
                Timer::after(Duration::from_millis(100)),
            )
            .await;
        }
    }
}
