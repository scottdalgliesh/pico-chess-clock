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

static CHANNEL: Channel<CriticalSectionRawMutex, ButtonEvent, 1> = Channel::new();

const DEBOUNCE_DELAY_MILLIS: u64 = 20;
const MINS_TO_MILLIS: i32 = 60 * 1000;
const DEFAULT_TURN_MILLIS: i32 = 10 * MINS_TO_MILLIS + 999; // offset by 999 millis to account for truncation
const MAX_TURN_MILLIS: i32 = 30 * MINS_TO_MILLIS;
const HOLD_TIME_SECS: u64 = 1;

/// Controls overall game (timer) state.
struct Game<'d, P1: Pin, P2: Pin> {
    phase: GameStatus,
    red_player: Player<'d, P1>,
    blue_player: Player<'d, P2>,
}

impl<'d, P1: Pin, P2: Pin> Game<'d, P1, P2> {
    /// Writes players' status (time remaining) to the provided LCD display.
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

    /// Reset all state to initiate a new game.
    fn reset(&mut self) {
        self.phase = GameStatus::PreGame;
        self.red_player.reset();
        self.blue_player.reset();
    }
}

/// Controls individual player state.
struct Player<'d, P: Pin> {
    millis_left: i32,
    is_active: bool,
    time_activated: Option<Instant>,
    led: Output<'d, P>,
}

impl<'d, P: Pin> Player<'d, P> {
    fn new(led: Output<'d, P>) -> Player<'d, P> {
        Player {
            millis_left: DEFAULT_TURN_MILLIS,
            is_active: false,
            time_activated: None,
            led,
        }
    }

    /// Reduce total player's turn time by the specified number of minutes.
    /// Used during the "pre-game" phase to select player time limits.
    fn decrement_time(&mut self, mins: i32) {
        let millis = mins * MINS_TO_MILLIS;
        if self.millis_left > millis {
            self.millis_left -= millis;
        } else {
            self.millis_left = MAX_TURN_MILLIS;
        }
    }

    /// Returns player's current time remaining as a formatted string.
    /// Format: [-]MM:SS
    fn formatted_time(&self) -> String<32> {
        let mut millis_left = self.millis_left.clone();
        if let Some(time_activated) = self.time_activated {
            millis_left -= Instant::now().duration_since(time_activated).as_millis() as i32;
        }
        let sign = if millis_left < 0 { "-" } else { "" };
        let mins = millis_left.abs() / (MINS_TO_MILLIS);
        let secs = millis_left.abs() % (MINS_TO_MILLIS) / 1000;
        let mut buf: String<32> = String::new();
        core::write!(&mut buf, "{}{:>02}:{:>02}", sign, mins, secs).unwrap();
        buf
    }

    /// Initiate player's turn.
    fn start_turn(&mut self) {
        if !self.is_active {
            self.is_active = true;
            self.time_activated = Some(Instant::now());
            self.led.set_high();
        }
    }

    /// End player's turn and update time remaining.
    fn end_turn(&mut self) {
        if self.is_active {
            self.is_active = false;
            if let Some(time_activated) = self.time_activated {
                self.millis_left -=
                    Instant::now().duration_since(time_activated).as_millis() as i32;
                self.time_activated = None;
            };
            self.led.set_low();
        }
    }

    /// Reset player's state to initiate a new game.
    fn reset(&mut self) {
        self.millis_left = DEFAULT_TURN_MILLIS;
        self.is_active = false;
        self.time_activated = None;
    }
}

#[derive(PartialEq)]
enum GameStatus {
    PreGame,
    Active,
    Paused,
}

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

/// Embassy task to monitor a given io port for user input.
/// Sends a message using the given sender for the following events:
/// * button pressed (i.e. signal high; instantaneous)
/// * button held (i.e. signal high; threshold set by const HOLD_TIME_SECS)
#[embassy_executor::task(pool_size = 3)]
async fn button_watcher(
    mut button: Input<'static, AnyPin>,
    button_id: Color,
    sender: Sender<'static, CriticalSectionRawMutex, ButtonEvent, 1>,
) {
    loop {
        button.wait_for_low().await;
        Timer::after(Duration::from_millis(DEBOUNCE_DELAY_MILLIS)).await;
        select(
            async {
                button.wait_for_high().await;
                sender.send(ButtonEvent::Pressed(button_id)).await;
            },
            async {
                Timer::after(Duration::from_secs(HOLD_TIME_SECS)).await;
                sender.send(ButtonEvent::Held(button_id)).await;
            },
        )
        .await;

        // monitor for continuous hold (repeated input)
        while button.is_low() {
            select(button.wait_for_high(), async {
                Timer::after(Duration::from_secs(HOLD_TIME_SECS)).await;
                sender.send(ButtonEvent::Held(button_id)).await;
            })
            .await;
        }
        Timer::after(Duration::from_millis(DEBOUNCE_DELAY_MILLIS)).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    //initialize IO
    let red_led = Output::new(p.PIN_5, Level::Low);
    let mut yellow_led = Output::new(p.PIN_9, Level::Low);
    let blue_led = Output::new(p.PIN_13, Level::Low);

    let red_button = Input::new(p.PIN_6.degrade(), Pull::Up);
    let yellow_button = Input::new(p.PIN_10.degrade(), Pull::Up);
    let blue_button = Input::new(p.PIN_14.degrade(), Pull::Up);

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

    'outer: loop {
        // Pre-game phase
        while game.phase == GameStatus::PreGame {
            game.display_string(&mut lcd);
            match receiver.receive().await {
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
                match receiver.receive().await {
                    ButtonEvent::Pressed(Color::Red) => game.blue_player.start_turn(),
                    ButtonEvent::Pressed(Color::Blue) => game.red_player.start_turn(),
                    ButtonEvent::Held(Color::Yellow) => {
                        game.reset();
                        yellow_led.set_low();
                        continue 'outer;
                    }
                    _ => continue,
                }
                game.phase = GameStatus::Active;
                yellow_led.set_low();
            }

            // active turn
            while game.phase == GameStatus::Active {
                game.display_string(&mut lcd);
                let mut game_reset_flag = false;
                select(
                    async {
                        match receiver.receive().await {
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
                            ButtonEvent::Held(Color::Yellow) => {
                                game.red_player.end_turn();
                                game.blue_player.end_turn();
                                game.reset();
                                yellow_led.set_low();
                                game_reset_flag = true;
                            }
                            _ => (),
                        };
                    },
                    Timer::after(Duration::from_millis(100)),
                )
                .await;
                if game_reset_flag {
                    continue 'outer;
                }
            }
        }
    }
}
