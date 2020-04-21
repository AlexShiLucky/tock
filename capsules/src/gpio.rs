//! Provides userspace applications with access to GPIO pins.
//!
//! GPIOs are presented through a driver interface with synchronous commands
//! and a callback for interrupts.
//!
//! This capsule takes an array of pins to expose as generic GPIOs.
//! Note that this capsule is used for general purpose GPIOs. Pins that are
//! attached to LEDs or buttons are generally wired directly to those capsules,
//! not through this capsule as an intermediary.
//!
//! Usage
//! -----
//!
//! ```rust
//! let gpio_pins = static_init!(
//!     [&'static sam4l::gpio::GPIOPin; 4],
//!     [&sam4l::gpio::PB[14],
//!      &sam4l::gpio::PB[15],
//!      &sam4l::gpio::PB[11],
//!      &sam4l::gpio::PB[12]]);
//! let gpio = static_init!(
//!     capsules::gpio::GPIO<'static, sam4l::gpio::GPIOPin>,
//!     capsules::gpio::GPIO::new(gpio_pins));
//! for pin in gpio_pins.iter() {
//!     pin.set_client(gpio);
//! }
//! ```
//!
//! Syscall Interface
//! -----------------
//!
//! - Stability: 2 - Stable
//!
//! ### Commands
//!
//! All GPIO operations are synchronous.
//!
//! Commands control and query GPIO information, namely how many GPIOs are
//! present, the GPIO direction and state, and whether they should interrupt.
//!
//! ### Subscribes
//!
//! The GPIO interface provides only one callback, which is used for pins that
//! have had interrupts enabled.

/// Syscall driver number.
use crate::driver;
pub const DRIVER_NUM: usize = driver::NUM::Gpio as usize;

use kernel::hil::gpio;
use kernel::{AppId, Callback, Driver, Grant, ReturnCode};

pub struct GPIO<'a> {
    /// List of pins provided to the GPIO Driver capsule for userspace.
    pins: &'a [&'a dyn gpio::InterruptValuePin],

    /// Grant region for each app stores:
    /// 1. an optional callback
    /// 2. a bit array for which pins this app has selected a rising edge interrupt.
    /// 3. a bit array for which pins this app has selected a falling edge interrupt.
    apps: Grant<(Option<Callback>, u32, u32)>,
}

impl<'a> GPIO<'a> {
    pub fn new(
        pins: &'a [&'a dyn gpio::InterruptValuePin],
        grant: Grant<(Option<Callback>, u32, u32)>,
    ) -> GPIO<'a> {
        for (i, pin) in pins.iter().enumerate() {
            pin.set_value(i as u32);
        }
        GPIO {
            pins: pins,
            apps: grant,
        }
    }

    fn configure_input_pin(&self, pin_num: u32, config: usize) -> ReturnCode {
        let pin = self.pins[pin_num as usize];
        pin.make_input();
        match config {
            0 => {
                pin.set_floating_state(gpio::FloatingState::PullNone);
                ReturnCode::SUCCESS
            }
            1 => {
                pin.set_floating_state(gpio::FloatingState::PullUp);
                ReturnCode::SUCCESS
            }
            2 => {
                pin.set_floating_state(gpio::FloatingState::PullDown);
                ReturnCode::SUCCESS
            }
            _ => ReturnCode::ENOSUPPORT,
        }
    }
}

impl<'a> gpio::ClientWithValue for GPIO<'a> {
    fn fired(&self, pin_num: u32) {
        // read the value of the pin
        let pins = self.pins.as_ref();
        let pin_state = pins[pin_num as usize].read();

        // schedule callback with the pin number and value
        self.apps.each(|app| {
            match pin_state {
                true => {
                    // Pin is high, so this was a rising edge interrupt.
                    if (app.1 & (1 << pin_num)) > 0 {
                        app.0
                            .map(|mut cb| cb.schedule(pin_num as usize, pin_state as usize, 0));
                    }
                }
                false => {
                    // Pin is low, so this was a falling edge interrupt.
                    if (app.2 & (1 << pin_num)) > 0 {
                        app.0
                            .map(|mut cb| cb.schedule(pin_num as usize, pin_state as usize, 0));
                    }
                }
            }
        });
    }
}

impl<'a> Driver for GPIO<'a> {
    /// Subscribe to GPIO pin events.
    ///
    /// ### `subscribe_num`
    ///
    /// - `0`: Subscribe to interrupts from all pins with interrupts enabled.
    ///        The callback signature is `fn(pin_num: usize, pin_state: bool)`
    fn subscribe(
        &self,
        subscribe_num: usize,
        callback: Option<Callback>,
        app_id: AppId,
    ) -> ReturnCode {
        match subscribe_num {
            // subscribe to all pin interrupts (no affect or reliance on
            // individual pins being configured as interrupts)
            0 => self
                .apps
                .enter(app_id, |app, _| {
                    (**app).0 = callback;
                    ReturnCode::SUCCESS
                })
                .unwrap_or_else(|err| err.into()),

            // default
            _ => ReturnCode::ENOSUPPORT,
        }
    }

    /// Query and control pin values and states.
    ///
    /// Each byte of the `data` argument is treated as its own field.
    /// For all commands, the lowest order halfword is the pin number (`pin`).
    /// A few commands use higher order bytes for purposes documented below.
    /// If the higher order bytes are not used, they must be set to `0`.
    ///
    /// Other data bytes:
    ///
    ///   - `pin_config`: An internal resistor setting.
    ///                   Set to `0` for a pull-up resistor.
    ///                   Set to `1` for a pull-down resistor.
    ///                   Set to `2` for none.
    ///   - `irq_config`: Interrupt configuration setting.
    ///                   Set to `0` to interrupt on either edge.
    ///                   Set to `1` for rising edge.
    ///                   Set to `2` for falling edge.
    ///
    /// ### `command_num`
    ///
    /// - `0`: Number of pins.
    /// - `1`: Enable output on `pin`.
    /// - `2`: Set `pin`.
    /// - `3`: Clear `pin`.
    /// - `4`: Toggle `pin`.
    /// - `5`: Enable input on `pin` with `pin_config` in 0x00XX00000
    /// - `6`: Read `pin` value.
    /// - `7`: Configure interrupt on `pin` with `irq_config` in 0x00XX00000
    /// - `8`: Disable interrupt on `pin`.
    /// - `9`: Disable `pin`.
    fn command(&self, command_num: usize, data1: usize, data2: usize, appid: AppId) -> ReturnCode {
        let pins = self.pins.as_ref();
        let pin = data1;
        match command_num {
            // number of pins
            0 => ReturnCode::SuccessWithValue {
                value: pins.len() as usize,
            },

            // enable output
            1 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    pins[pin].make_output();
                    ReturnCode::SUCCESS
                }
            }

            // set pin
            2 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    pins[pin].set();
                    ReturnCode::SUCCESS
                }
            }

            // clear pin
            3 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    pins[pin].clear();
                    ReturnCode::SUCCESS
                }
            }

            // toggle pin
            4 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    pins[pin].toggle();
                    ReturnCode::SUCCESS
                }
            }

            // enable and configure input
            5 => {
                let pin_config = data2;
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    self.configure_input_pin(pin as u32, pin_config)
                }
            }

            // read input
            6 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    let pin_state = pins[pin].read();
                    ReturnCode::SuccessWithValue {
                        value: pin_state as usize,
                    }
                }
            }

            // configure interrupts on pin
            // (no affect or reliance on registered callback)
            7 => {
                let irq_config = data2;
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    // Record the interrupt being used for this app.
                    self.apps
                        .enter(appid, |cntr, _| {
                            match irq_config {
                                0 => {
                                    // Both falling and rising.
                                    cntr.1 |= 1 << pin;
                                    cntr.2 |= 1 << pin;
                                }
                                1 => {
                                    // rising
                                    cntr.1 |= 1 << pin;
                                    cntr.2 &= !(1 << pin);
                                }
                                2 => {
                                    // falling
                                    cntr.2 |= 1 << pin;
                                    cntr.1 &= !(1 << pin);
                                }
                                _ => {}
                            }

                            // Always do either interrupt in case one app wants
                            // falling and another app wants rising.
                            pins[pin].enable_interrupts(gpio::InterruptEdge::EitherEdge);
                            ReturnCode::SUCCESS
                        })
                        .unwrap_or_else(|err| err.into())
                }
            }

            // disable interrupts on pin, also disables pin
            // (no affect or reliance on registered callback)
            8 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    let _ = self.apps.enter(appid, |cntr, _| {
                        cntr.1 &= !(1 << pin);
                        cntr.2 &= !(1 << pin);
                    });

                    let in_use = self.apps.iter().any(|app| {
                        app.enter(|cntr, _| (cntr.1 & (1 << pin)) > 0 || (cntr.2 & (1 << pin)) > 0)
                    });

                    if !in_use {
                        pins[pin].disable_interrupts();
                        pins[pin].deactivate_to_low_power();
                    }
                    ReturnCode::SUCCESS
                }
            }

            // disable pin
            9 => {
                if pin >= pins.len() {
                    ReturnCode::EINVAL /* impossible pin */
                } else {
                    pins[pin].deactivate_to_low_power();
                    ReturnCode::SUCCESS
                }
            }

            // default
            _ => ReturnCode::ENOSUPPORT,
        }
    }
}
