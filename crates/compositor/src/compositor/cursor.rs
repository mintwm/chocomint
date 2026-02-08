use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::{ImportDmaWl, ImportEgl, ImportMem, ImportMemWl};
use smithay::input::pointer::CursorImageSurfaceData;
use smithay::output::Scale;
use smithay::utils::Transform;
use smithay::wayland::compositor::with_states;
use smithay::{
    backend::renderer::{
        Renderer,
        element::{RenderElement, memory::MemoryRenderBuffer},
    },
    input::pointer::CursorIcon,
};
use std::collections::HashMap;

use smithay::{
    input::{
        Seat,
        keyboard::{KeyboardHandle, XkbConfig},
        pointer::{CursorImageStatus, PointerHandle},
    },
    utils::{Logical, Point},
};
use xcursor::{
    CursorTheme,
    parser::{Image, parse_xcursor},
};

use crate::compositor::{backend::Backend, state::App, udev::TestRenderElement};

pub struct XCursor {
    inner: Vec<Image>,
}

pub struct Cursor<B: Backend> {
    pointer: PointerHandle<App<B>>,
    pub location: Point<f64, Logical>,
    status: CursorImageStatus,
    theme: CursorTheme,
    cache: HashMap<CursorIcon, XCursor>,
}

impl<B: Backend> Cursor<B> {
    pub fn get_pointer(&self) -> PointerHandle<App<B>> {
        self.pointer.clone()
    }

    pub fn set_icon(&mut self, status: CursorImageStatus) {
        self.status = status;
        if let CursorImageStatus::Named(icon) = &self.status {
            let icon_path = self.theme.load_icon(icon.name()).unwrap();
            let bytes = std::fs::read(icon_path).unwrap();
            let image = parse_xcursor(&bytes).unwrap();
            self.cache.insert(*icon, XCursor { inner: image });
        }
    }

    pub fn render_cursor<
        R: Renderer + ImportMem + ImportMemWl + ImportDmaWl + ImportEgl,
        E: RenderElement<R>,
    >(
        &self,
        renderer: &mut R,
        scale: Scale,
    ) -> Vec<TestRenderElement<R, E>>
    where
        <R as smithay::backend::renderer::RendererSuper>::TextureId: Send + Clone + 'static,
    {
        let location = self.location;
        //let location = self.location.to_f64().to_physical(1.0);
        match &self.status {
            CursorImageStatus::Hidden => vec![],
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .hotspot
                });

                let location_physical = location.to_f64().to_physical(scale.fractional_scale());
                let hotspot_physical = hotspot.to_f64().to_physical(scale.fractional_scale());
                let final_pos_f64 = location_physical - hotspot_physical;
                let final_pos = final_pos_f64.to_i32_round();

                render_elements_from_surface_tree::<R, WaylandSurfaceRenderElement<R>>(
                    renderer,
                    surface,
                    final_pos,
                    scale.fractional_scale(),
                    1.,
                    Kind::Cursor,
                )
                .into_iter()
                .map(TestRenderElement::<R, E>::from)
                .collect()
            }
            CursorImageStatus::Named(cursor_icon) => {
                let cursor = self.cache.get(cursor_icon).unwrap();
                let image = cursor.inner.first().unwrap();

                let buffer = MemoryRenderBuffer::from_slice(
                    &image.pixels_rgba,
                    Fourcc::Argb8888,
                    (image.width as i32, image.height as i32),
                    scale.integer_scale(),
                    Transform::Normal,
                    None,
                );

                let hotspot = Point::<i32, Logical>::new(image.xhot as i32, image.yhot as i32);
                let location = (location - hotspot.to_f64()).to_physical(scale.fractional_scale());

                let texture = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location,
                    &buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .unwrap();

                vec![TestRenderElement::from(texture)]
            }
        }
    }
}

pub struct InputState<B: Backend> {
    pub keyboard: KeyboardHandle<App<B>>,
    pub cursor: Cursor<B>,
}

impl<B: Backend> InputState<B> {
    pub fn new(seat: &mut Seat<App<B>>) -> Self {
        // Добавляем клавиатуру с частоток повтора и задержкой в миллисекундах.
        // Повтор - время повтора, задержка - как должно нужно ждать перез следующим повтором
        let keyboard = seat.add_keyboard(XkbConfig::default(), 200, 25).unwrap();
        let pointer = seat.add_pointer();

        let theme = CursorTheme::load("Adwaita");
        let mut cursor = Cursor {
            pointer,
            location: Point::default(),
            status: CursorImageStatus::default_named(),
            theme,
            cache: HashMap::default(),
        };

        cursor.set_icon(CursorImageStatus::default_named());

        Self { keyboard, cursor }
    }
}
