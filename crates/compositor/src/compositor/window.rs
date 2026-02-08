use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::winit::{WinitEvent, WinitEventLoop, WinitGraphicsBackend};
use smithay::desktop::space::render_output;
use smithay::output::Mode;
use smithay::utils::{Rectangle, Transform};
use smithay::{backend::renderer::gles::GlesRenderer, output};

use crate::compositor::backend::Backend;
use crate::compositor::data;
use crate::compositor::state::App;

pub struct WinitBackend {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub winit: WinitEventLoop,
}

impl WinitBackend {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (backend, winit) = smithay::backend::winit::init::<GlesRenderer>()?;
        Ok(Self { backend, winit })
    }

    pub fn bind(
        &mut self,
    ) -> (
        &mut smithay::backend::renderer::gles::GlesRenderer,
        smithay::backend::renderer::gles::GlesTarget<'_>,
    ) {
        self.backend.bind().unwrap()
    }

    pub fn backend(&mut self) -> &mut WinitGraphicsBackend<GlesRenderer> {
        &mut self.backend
    }
}

impl Backend for WinitBackend {
    fn create_output(&self) -> output::Output {
        // Сообщает клиенту физические свойства выходных данных.
        // Мы полагаемся на winit для управления физическим устройством, поэтому точный размер/марка/модель не нужны.
        let physical_properties = output::PhysicalProperties {
            // Размер в милиметрах
            size: (0, 0).into(),
            // Как физические пиксели организованы (HorizontalRGB, VerticalBGR).
            // Оставляем неизвестным для обычных выходов.
            subpixel: output::Subpixel::Unknown,
            make: "mwm".into(),
            model: "Winit".into(),
        };

        // Создаем новый вывод который является областью в пространстве композитора, которую можно использовать клиентами.
        // Обычно представляет собой монитор, используемый композитором.
        output::Output::new("winit".to_string(), physical_properties)
    }

    fn mode(&self) -> output::Mode {
        // Получаем размер окна winit
        let size = self.backend.window_size();

        // Определяем размер окна и частоту обновления в милигерцах
        output::Mode {
            size,
            // 60 fps
            refresh: 60_000,
        }
    }
}

pub fn run_winit(data: &mut data::Data<WinitBackend>) {
    // Create a timer and start time for the EventLoop.
    // TODO: Use ping for a tighter event loop.
    let start_time = std::time::Instant::now();
    let timer = Timer::immediate();

    let output = data.state.backend.create_output();
    let mode = data.state.backend.mode();

    // Клиенты могут получить доступ к глобальным объектам для получения физических свойств и состояния вывода.
    output.create_global::<App<WinitBackend>>(&data.display);
    // Устанавливаем состояние для использования winit.
    output.change_current_state(
        // Содержит размер/частоту обновления от winit.
        Some(mode),
        Some(Transform::Flipped180), // OpenGL ES texture?
        None,
        Some((0, 0).into()),
    );

    // Set the prefereed mode to use.
    output.set_preferred(mode);
    // Set the output of a space with coordinates for the upper left corner of the surface.
    data.state
        .globals
        .lock()
        .unwrap()
        .space
        .map_output(&output, (0, 0));

    // Tracks output for damaged elements allowing for the ability to redraw only what has been damaged.
    let mut output_damage_tracker = OutputDamageTracker::from_output(&output);
    data.state
        .handle
        .insert_source(timer, move |_, (), data| {
            let display = &mut data.display;
            let state = &mut data.state;

            //state.backend.dispatch_new_events(&mut output);
            let state_ptr: *mut App<WinitBackend> = state;

            unsafe {
                (*state_ptr)
                    .backend
                    .winit
                    .dispatch_new_events(|event| match event {
                        WinitEvent::Resized {
                            size,
                            scale_factor: _,
                        } => {
                            output.change_current_state(
                                Some(Mode {
                                    size,
                                    refresh: 60_000,
                                }),
                                None,
                                None,
                                None,
                            );
                        }
                        WinitEvent::Focus(_) => {}
                        WinitEvent::Input(input) => state.handle_input_event(input),
                        WinitEvent::CloseRequested => {
                            state.loop_signal.stop();
                        }
                        WinitEvent::Redraw => {
                            state.handle_socket();
                            state.engine.load_packages();
                        }
                    });
            }

            {
                let space = &state.globals.lock().unwrap().space;
                let (renderer, mut framebuffer) = state.backend.bind();

                render_output::<_, WaylandSurfaceRenderElement<GlesRenderer>, _, _>(
                    &output,
                    renderer,
                    &mut framebuffer,
                    1.0,
                    0,
                    [space],
                    &[],
                    &mut output_damage_tracker,
                    [0.8, 0.8, 0.8, 1.0],
                )
                .unwrap();
            }

            let size = state.backend.backend.window_size();
            let damage = Rectangle::from_size(size);
            state.backend.backend().submit(Some(&[damage])).unwrap();

            let space = &mut state.globals.lock().unwrap().space;
            space.elements().for_each(|window| {
                window.send_frame(
                    &output,
                    start_time.elapsed(),
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            });

            space.refresh();

            display.flush_clients().unwrap();

            TimeoutAction::ToDuration(Duration::from_millis(16))
        })
        .unwrap();
}
