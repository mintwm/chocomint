use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
            KeyState, KeyboardKeyEvent, Keycode, PointerAxisEvent, PointerButtonEvent,
            PointerMotionEvent,
        },
        session::Session,
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::{FilterResult, Keysym},
        pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::compositor::{backend::Backend, state::App, udev::UdevData, window::WinitBackend};

impl<B: Backend + SpecialActions> App<B> {
    pub fn handle_input_event<I: InputBackend>(&mut self, input: InputEvent<I>)
    where
        I::Device: 'static,
    {
        match input {
            InputEvent::PointerMotion { event } => {
                self.input_state.cursor.location += event.delta();
                self.input_state.cursor.location =
                    self.clamp_pointer_location(self.input_state.cursor.location);

                let location = self.input_state.cursor.location;
                let under = self.surface_under(location);

                let pointer = self.seat.get_pointer().unwrap();
                let serial = SERIAL_COUNTER.next_serial();

                pointer.relative_motion(
                    self,
                    under.clone(),
                    &RelativeMotionEvent {
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                );

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location,
                        serial,
                        time: event.time_msec(),
                    },
                );

                pointer.frame(self);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let output_geo = {
                    let space = &self.globals.lock().unwrap().space;
                    let output = space.outputs().next().unwrap();

                    space.output_geometry(output).unwrap()
                };

                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().unwrap();
                let under = self.surface_under(pos);

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event } => {
                let globals = self.globals.clone();
                let mut globals = globals.lock().unwrap();
                let pointer = self.seat.get_pointer().unwrap();
                let keyboard = self.seat.get_keyboard().unwrap();

                let serial = SERIAL_COUNTER.next_serial();

                let button = event.button_code();

                let button_state = event.state();

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    let location = self.input_state.cursor.location;

                    // Ищем окно и ПОВЕРХНОСТЬ под курсором
                    let under = globals.space.element_under(location).map(|(w, l)| {
                        // важно: берем поверхность с учетом локальных координат внутри окна
                        let surface = w
                            .surface_under(location - l.to_f64(), WindowSurfaceType::all())
                            .unwrap()
                            .0;
                        (w.clone(), surface)
                    });

                    if let Some((window, surface)) = under {
                        // 1. Поднимаем окно в Space
                        globals.space.raise_element(&window, true);

                        // 2. Активируем окно (важно для XDG Shell)
                        window.set_activated(true);

                        // 3. Устанавливаем фокус клавиатуры
                        keyboard.set_focus(
                            self,
                            Some(surface), // Передаем конкретную поверхность
                            serial,
                        );

                        // 4. Генерируем Configure события
                        window.toplevel().unwrap().send_configure();
                    } else {
                        // Если кликнули мимо — снимаем фокус
                        globals.space.elements().for_each(|window| {
                            window.set_activated(false);
                            window.toplevel().unwrap().send_configure();
                        });
                        keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                    }
                }

                // ВАЖНО: Перед кликом Smithay должен знать, где находится указатель
                // Обычно это делается в PointerMotion, но для надежности можно и тут:
                // pointer.motion(self, under.map(|(_, s)| (s, location)), ...);

                drop(globals);
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let horizontal_amount_v120 = event.amount_v120(Axis::Horizontal);
                let horizontal_amount = event
                    .amount(Axis::Horizontal)
                    .or_else(|| horizontal_amount_v120.map(|amt| amt * 15. / 120.))
                    .unwrap_or(0.0);
                let vertical_amount_v120 = event.amount_v120(Axis::Vertical);
                let vertical_amount = event
                    .amount(Axis::Vertical)
                    .or_else(|| vertical_amount_v120.map(|amt| amt * 15. / 120.))
                    .unwrap_or(0.0);

                let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
                if horizontal_amount != 0.0 {
                    frame = frame.relative_direction(
                        Axis::Horizontal,
                        event.relative_direction(Axis::Horizontal),
                    );
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(amount_v120) = horizontal_amount_v120 {
                        frame = frame.v120(Axis::Horizontal, amount_v120 as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.relative_direction(
                        Axis::Vertical,
                        event.relative_direction(Axis::Vertical),
                    );
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(amount_v120) = vertical_amount_v120 {
                        frame = frame.v120(Axis::Vertical, amount_v120 as i32);
                    }
                }
                if event.source() == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.input_state.cursor.get_pointer();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            InputEvent::Keyboard { event } => {
                let keyboard = self.seat.get_keyboard().unwrap();

                let serial = SERIAL_COUNTER.next_serial();
                keyboard.input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    event.time_msec(),
                    |data, _, handle| {
                        let keysym = handle.modified_sym();
                        data.backend.handle_tty_keys(event.state(), keysym)
                    },
                );

                if event.key_code() == Keycode::new(9) {
                    std::process::exit(0);
                }
                if event.key_code() == Keycode::new(10) && event.state() == KeyState::Released {
                    let mut cmd = std::process::Command::new("kitty");
                    cmd.spawn().unwrap();
                }
            }
            _ => {}
        }
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.globals()
            .space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            })
    }

    //TODO mutli monitor setup
    pub fn clamp_pointer_location(
        &self,
        mut raw_location: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        let output = self.output_state.outputs.keys().next().unwrap();
        let output_location = output.current_location().to_f64();
        let output_size = output.current_mode().unwrap().size.to_f64();

        if raw_location.x < output_location.x {
            raw_location.x = output_location.x;
        }

        if raw_location.y < output_location.y {
            raw_location.y = output_location.y;
        }

        if raw_location.x > output_size.w {
            raw_location.x = output_size.w;
        }

        if raw_location.y > output_size.h {
            raw_location.y = output_size.h;
        }

        raw_location
    }
}

pub trait SpecialActions {
    fn handle_tty_keys(&mut self, _state: KeyState, _keysym: Keysym) -> FilterResult<()> {
        FilterResult::Forward
    }
}

impl SpecialActions for WinitBackend {}

impl SpecialActions for UdevData {
    #[inline]
    fn handle_tty_keys(&mut self, state: KeyState, keysym: Keysym) -> FilterResult<()> {
        if state != KeyState::Pressed {
            return FilterResult::Forward;
        }

        let vt_num = match keysym {
            Keysym::XF86_Switch_VT_1 => Some(1),
            Keysym::XF86_Switch_VT_2 => Some(2),
            Keysym::XF86_Switch_VT_3 => Some(3),
            Keysym::XF86_Switch_VT_4 => Some(4),
            Keysym::XF86_Switch_VT_5 => Some(5),
            Keysym::XF86_Switch_VT_6 => Some(6),
            Keysym::XF86_Switch_VT_7 => Some(7),
            Keysym::XF86_Switch_VT_8 => Some(8),
            Keysym::XF86_Switch_VT_9 => Some(9),
            Keysym::XF86_Switch_VT_10 => Some(10),
            Keysym::XF86_Switch_VT_11 => Some(11),
            Keysym::XF86_Switch_VT_12 => Some(12),
            _ => None,
        };

        if let Some(vt) = vt_num {
            log::info!("Switching to VT {vt}");
            if let Err(err) = self.session.change_vt(vt) {
                log::error!("Failed to switch session: {err}");
            }
            return FilterResult::Intercept(());
        }

        FilterResult::Forward
    }
}
