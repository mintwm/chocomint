// Ты знаешь что такое безумие?

#![allow(clippy::redundant_closure_for_method_calls)]

use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::{DrmEventMetadata, DrmEventTime};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, RenderElement, RenderElementStates};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame, ImportAll, ImportMem, Renderer};
use smithay::delegate_dmabuf;
use smithay::desktop::space::space_render_elements;
use smithay::desktop::utils::{
    surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
};
use smithay::desktop::{Space, Window};
use smithay::output::Mode;
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{Monotonic, Physical, Rectangle};
use smithay::wayland::dmabuf::DmabufHandler;
use smithay::wayland::presentation::Refresh;
use smithay::{
    backend::{
        allocator::gbm::GbmBufferFlags,
        drm::{compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter},
    },
    desktop::utils::OutputPresentationFeedback,
    output::{Output, OutputModeSource, PhysicalProperties, Subpixel},
    utils::Size,
};
use std::time::Duration;
use std::{collections::HashMap, path::Path};
use tracing::trace_span;

use ::drm::{
    control::{self, ModeFlags, ModeTypeFlags, connector, crtc},
    node::NodeType,
};
use calloop::{LoopHandle, RegistrationToken};
use smithay::{
    backend::{
        allocator::{
            format::FormatSet,
            gbm::{GbmAllocator, GbmDevice},
        },
        drm::{self, DrmDevice, DrmDeviceFd, DrmEvent, DrmNode},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{ImportDma, gles::GlesRenderer},
        session::{Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    reexports::rustix::fs::OFlags,
    utils::DeviceFd,
    wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufState},
};
use smithay_drm_extras::{
    display_info,
    drm_scanner::{self, DrmScanEvent, DrmScanner},
};

use crate::compositor::output::RenderState;
use crate::compositor::{backend::Backend, state::App};

type GbmDrmCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    OutputPresentationFeedback,
    DrmDeviceFd,
>;

const SUPPORTED_COLOR_FORMATS: &[Fourcc] = &[Fourcc::Argb8888, Fourcc::Abgr8888];

#[derive(Debug)]
struct Surface {
    info: OutputInfo,
    compositor: GbmDrmCompositor,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct UdevOutputState {
    pub device_id: u64,
    pub crtc: crtc::Handle,
}

pub struct Device {
    id: u64,
    token: RegistrationToken,
    drm: drm::DrmDevice,
    drm_scanner: drm_scanner::DrmScanner,
    surfaces: HashMap<crtc::Handle, Surface>,
    gbm: GbmDevice<DrmDeviceFd>,
    gles: GlesRenderer,
    formats: FormatSet,
    render_node: DrmNode,
}

pub struct UdevData {
    pub session: LibSeatSession,
    pub libinput: Libinput,
    pub primary_node: DrmNode,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub device: Option<Device>,
}

impl Backend for UdevData {
    fn create_output(&self) -> smithay::output::Output {
        // Сообщает клиенту физические свойства выходных данных.
        // Мы полагаемся на winit для управления физическим устройством, поэтому точный размер/марка/модель не нужны.
        let physical_properties = smithay::output::PhysicalProperties {
            // Размер в милиметрах
            size: (0, 0).into(),
            // Как физические пиксели организованы (HorizontalRGB, VerticalBGR).
            // Оставляем неизвестным для обычных выходов.
            subpixel: smithay::output::Subpixel::Unknown,
            make: "mwm".into(),
            model: "udev".into(),
        };

        // Создаем новый вывод который является областью в пространстве композитора, которую можно использовать клиентами.
        // Обычно представляет собой монитор, используемый композитором.
        smithay::output::Output::new("udev".to_string(), physical_properties)
    }

    fn mode(&self) -> smithay::output::Mode {
        smithay::output::Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        }
    }
}

impl UdevData {
    pub fn init(handle: &LoopHandle<'_, super::data::Data<UdevData>>) -> UdevData {
        use smithay::backend::session::Event as SessionEvent;

        let (session, notify) = LibSeatSession::new().unwrap();

        let libinput_session = LibinputSessionInterface::from(session.clone());
        let mut libinput = Libinput::new_with_udev(libinput_session);
        libinput.udev_assign_seat(&session.seat()).unwrap();

        let input_backend = LibinputInputBackend::new(libinput.clone());
        handle
            .insert_source(input_backend, |mut event, (), data| {
                let _span = trace_span!("libinput_backend").entered();
                //state.handle_libinput_event(&mut event);
                data.state.handle_input_event(event);
            })
            .unwrap();

        handle
            .insert_source(notify, |event, (), data| match event {
                SessionEvent::PauseSession => {
                    data.state.sleep = true;
                    let backend = &mut data.state.backend;
                    backend.libinput.suspend();

                    let device = data.state.backend.device.as_mut().unwrap();
                    device.drm.pause();
                }
                SessionEvent::ActivateSession => {
                    data.state.sleep = false;
                    let backend = &mut data.state.backend;
                    backend.libinput.resume().unwrap();

                    let device = data.state.backend.device.as_mut().unwrap();
                    device.drm.activate(false).unwrap();
                }
            })
            .unwrap();

        let primary_node = primary_gpu(session.seat())
            .unwrap()
            .and_then(|x| {
                DrmNode::from_path(x)
                    .ok()?
                    .node_with_type(NodeType::Primary)?
                    .ok()
            })
            .unwrap_or_else(|| {
                all_gpus(session.seat())
                    .unwrap()
                    .into_iter()
                    .find_map(|x| DrmNode::from_path(x).ok())
                    .expect("No GPU!")
            });

        log::info!("Primary gpu: {primary_node:#?}");

        Self {
            session,
            primary_node,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            device: None,
            libinput,
        }
    }
}

pub fn init_udev(state: &mut App<UdevData>) {
    let udev_backend = UdevBackend::new(state.backend.session.seat()).unwrap();
    for (device_id, path) in udev_backend.device_list() {
        state.on_udev_event(UdevEvent::Added {
            device_id,
            path: path.to_owned(),
        });
    }

    state
        .handle
        .insert_source(udev_backend, |event, (), data| {
            data.state.on_udev_event(event);
        })
        .unwrap();
}

impl App<UdevData> {
    fn on_udev_event(&mut self, event: UdevEvent) {
        match event {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    self.device_added(device_id, node, &path);
                }
            }
            UdevEvent::Changed { device_id } => {
                self.device_changed(device_id);
            }
            UdevEvent::Removed { device_id } => {
                self.device_removed(device_id);
            }
        }
    }

    fn device_added(&mut self, device_id: u64, node: DrmNode, path: &Path) {
        if node != self.backend.primary_node {
            return;
        }

        let fd = self
            .backend
            .session
            .open(
                path,
                OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
            )
            .unwrap();

        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, drm_notifier) = drm::DrmDevice::new(fd.clone(), true).unwrap();
        let gbm = GbmDevice::new(fd).unwrap();

        // SAFETY: this project doesn't use an egl display outside of smithay
        let display = unsafe { EGLDisplay::new(gbm.clone()) }.unwrap();
        let egl_context = EGLContext::new(&display).unwrap();

        // SAFETY: the egl context is only active in this thread
        let mut gles = unsafe { GlesRenderer::new(egl_context) }.unwrap();
        //shaders::init(glow.borrow_mut());
        //gles.bind_wl_display(&self.display).unwrap();

        let egl_device = EGLDevice::device_for_display(gles.egl_context().display()).unwrap();
        let render_node = egl_device.try_get_render_node().unwrap().unwrap();

        let dmabuf_formats = gles.dmabuf_formats();
        let dmabuf_default_feedback =
            DmabufFeedbackBuilder::new(render_node.dev_id(), dmabuf_formats)
                .build()
                .unwrap();

        let dmabuf_global = self
            .backend
            .dmabuf_state
            .create_global_with_default_feedback::<App<UdevData>>(
                &self.display,
                &dmabuf_default_feedback,
            );

        self.backend.dmabuf_global = Some(dmabuf_global);

        let token = self
            .handle
            .insert_source(drm_notifier, move |event, metadata, data| match event {
                DrmEvent::VBlank(crtc) => {
                    let metadata = metadata.expect("vblank events must have metadata");
                    data.state.on_vblank(crtc, metadata);
                }
                DrmEvent::Error(error) => log::error!("drm error {error:?}"),
            })
            .unwrap();

        let formats = gles.egl_context().dmabuf_render_formats().clone();

        let output_device = Device {
            id: device_id,
            token,
            render_node,
            drm_scanner: DrmScanner::new(),
            drm,
            gbm,
            gles,
            formats,
            surfaces: HashMap::new(),
        };
        self.backend.device = Some(output_device);

        self.device_changed(device_id);
    }

    fn device_changed(&mut self, device_id: u64) {
        let Some(device) = &mut self.backend.device else {
            return;
        };

        if device.id != device_id {
            return;
        }

        for event in device
            .drm_scanner
            .scan_connectors(&device.drm)
            .expect("failed to scan connectors")
        {
            match event {
                DrmScanEvent::Connected {
                    connector,
                    crtc: Some(crtc),
                } => {
                    self.connector_connected(&connector, crtc);
                }
                DrmScanEvent::Disconnected {
                    connector,
                    crtc: Some(crtc),
                } => {
                    self.connector_disconnected(&connector, crtc);
                }
                _ => {}
            }
        }
    }

    fn device_removed(&mut self, device_id: u64) {
        let Some(device) = &mut self.backend.device else {
            return;
        };

        if device.id != device_id {
            return;
        }

        let crtcs: Vec<_> = device
            .drm_scanner
            .crtcs()
            .map(|(info, crtc)| (info.clone(), crtc))
            .collect();

        for (connector, crtc) in crtcs {
            self.connector_disconnected(&connector, crtc);
        }

        //TODO
    }

    fn connector_connected(&mut self, connector: &connector::Info, crtc: crtc::Handle) {
        println!("Connector connected");
        let device = self.backend.device.as_mut().unwrap();

        let output_info = output_info(&device.drm, connector);
        let mode = pick_mode(connector);

        let surface = device
            .drm
            .create_surface(crtc, mode, &[connector.handle()])
            .unwrap();

        let mut planes = surface.planes().clone();

        // overlay planes need to be cleared when switching vt to
        // avoid the windows getting stuck on the monitor when switching
        // to a compositor that doesn't clean overlay planes on activate
        // todo find a better way to do this
        planes.overlay.clear();

        let gbm_flags = GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT;
        let allocator = GbmAllocator::new(device.gbm.clone(), gbm_flags);

        let (physical_width, physical_height) = connector.size().unwrap_or((0, 0));

        let output = Output::new(
            output_info.connector.clone(),
            PhysicalProperties {
                size: Size::new(physical_width as i32, physical_height as i32),
                subpixel: Subpixel::Unknown,
                make: output_info.make.clone(),
                model: output_info.model.clone(),
            },
        );

        let wl_mode = Mode::from(mode);
        output.change_current_state(Some(wl_mode), None, None, None);
        output.set_preferred(wl_mode);

        output.user_data().insert_if_missing(|| output_info.clone());
        output.user_data().insert_if_missing(|| UdevOutputState {
            device_id: device.id,
            crtc,
        });

        let compositor = DrmCompositor::new(
            OutputModeSource::Auto(output.clone()),
            surface,
            Some(planes),
            allocator,
            GbmFramebufferExporter::new(device.gbm.clone(), Some(device.render_node)),
            SUPPORTED_COLOR_FORMATS.iter().copied(),
            device.formats.clone(),
            device.drm.cursor_size(),
            Some(device.gbm.clone()),
        )
        .unwrap();

        let surface = Surface {
            info: output_info,
            compositor,
        };
        let prev = device.surfaces.insert(crtc, surface);
        assert!(prev.is_none(), "crtc must not have already existed");

        self.map_output(&output);
        self.output_state.add_output(output);
    }

    fn connector_disconnected(&mut self, connector: &connector::Info, crtc: crtc::Handle) {
        log::info!("disconnecting connector {connector:?}");
        let device = self.backend.device.as_mut().unwrap();

        if device.surfaces.remove(&crtc).is_none() {
            log::info!("crtc wasn't enabled");
            return;
        }

        todo!()

        //let output = mayland
        //    .workspaces
        //    .udev_output(device.id, crtc)
        //    .unwrap()
        //    .clone();
        //mayland.remove_output(&output);
    }

    fn on_vblank(&mut self, crtc: crtc::Handle, meta: DrmEventMetadata) {
        let device = self.backend.device.as_mut().unwrap();
        let Some(surface) = device.surfaces.get_mut(&crtc) else {
            log::warn!("missing crtc {crtc:?} in vblank callback");
            return;
        };

        let presentation_time = match meta.time {
            DrmEventTime::Monotonic(time) => time,
            DrmEventTime::Realtime(_) => {
                // not supported
                Duration::ZERO
            }
        };

        match surface.compositor.frame_submitted() {
            Ok(Some(mut feedback)) => {
                let seq = meta.sequence;
                let flags = wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwClock
                    | wp_presentation_feedback::Kind::HwCompletion;

                let output = feedback.output().unwrap();
                let refresh = output
                    .current_mode()
                    .map(|mode| Duration::from_secs_f64(1_000f64 / f64::from(mode.refresh)))
                    .map_or(Refresh::Unknown, Refresh::Fixed);

                feedback.presented::<_, Monotonic>(
                    presentation_time,
                    refresh,
                    u64::from(seq),
                    flags,
                );
            }
            Ok(None) => {}
            Err(err) => {
                log::error!("error marking frame as submitted {err}");
            }
        }

        let output = self
            .output_state
            .udev_output(crtc, device.id)
            .unwrap()
            .clone();

        self.output_state.queue_render(&output);

        //mayland.send_frame_callbacks(&output);
    }

    pub fn render_all(&mut self) {
        if self.sleep {
            return;
        }

        let now = self.clock.now();
        let space = unsafe {
            let ptr = &raw const self.globals().space;
            &*ptr
        };

        let output_state = unsafe {
            let ptr = &raw const self.output_state;
            &*ptr
        };

        for (output, state) in &output_state.outputs {
            if *state != RenderState::Queued {
                continue;
            }

            self.render(output);

            space.elements().for_each(|window| {
                window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            });
        }
    }

    fn render(&mut self, output: &Output) {
        let space = unsafe {
            let ptr = &raw const self.globals().space;
            &*ptr
        };

        let device = self.backend.device.as_mut().unwrap();
        let elements: Vec<
            smithay::desktop::space::SpaceRenderElements<
                GlesRenderer,
                WaylandSurfaceRenderElement<GlesRenderer>,
            >,
        > = space_render_elements(&mut device.gles, std::slice::from_ref(space), output, 1.0)
            .unwrap();

        let udev_state = output.user_data().get::<UdevOutputState>().unwrap();
        let surface = device.surfaces.get_mut(&udev_state.crtc).unwrap();

        let mut elements = elements
            .into_iter()
            .map(TestRenderElement::from)
            .collect::<Vec<_>>();

        let output_scale = output.current_scale();
        let mut cursor = self
            .input_state
            .cursor
            .render_cursor(&mut device.gles, output_scale);
        if !cursor.is_empty() {
            let cursor: TestRenderElement<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>> =
                cursor.remove(0);
            elements.insert(0, cursor);
        }

        let debug_cursor = {
            let location = self.input_state.cursor.location;
            RectElement::new(
                (location.x as i32, location.y as i32),
                (6, 16),
                [0.0, 0.0, 1.0, 1.0],
            )
        };

        elements.insert(0, TestRenderElement::from(debug_cursor));

        let drm_compositor = &mut surface.compositor;
        match drm_compositor.render_frame(
            &mut device.gles,
            &elements,
            [0.8, 0.8, 0.8, 1.0],
            FrameFlags::DEFAULT,
        ) {
            Ok(render_output_res) => {
                if render_output_res.is_empty {
                    return;
                }

                let output_presentation_feedback =
                    presentation_feedback(space, output, &render_output_res.states);

                match drm_compositor.queue_frame(output_presentation_feedback) {
                    Ok(()) => {
                        self.output_state.wait_render_request(output);
                    }
                    Err(err) => log::error!("error queueing frame {err:?}"),
                }
            }
            Err(err) => {
                drm_compositor.reset_buffers();
                log::error!("error rendering frame {err:?}");
            }
        }
    }
}

fn presentation_feedback(
    space: &Space<Window>,
    output: &Output,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    let element_iter = space.elements_for_output(output);
    for window in element_iter {
        window.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, render_element_states)
            },
        );
    }

    output_presentation_feedback
}

#[derive(Debug, Clone)]
struct OutputInfo {
    connector: String,
    make: String,
    model: String,
    serial: Option<String>,
}

fn output_info(drm: &DrmDevice, connector: &connector::Info) -> OutputInfo {
    let info = display_info::for_connector(drm, connector.handle());

    let connector = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );
    let make = info
        .as_ref()
        .and_then(|info| info.make())
        .unwrap_or_else(|| "unknown".to_owned());
    let model = info
        .as_ref()
        .and_then(|info| info.model())
        .unwrap_or_else(|| "unknown".to_owned());
    let serial = info.as_ref().and_then(|info| info.serial());

    OutputInfo {
        connector,
        make,
        model,
        serial,
    }
}

fn pick_mode(connector: &connector::Info) -> control::Mode {
    // try to get a preferred mode
    if let Some(mode) = connector
        .modes()
        .iter()
        .filter(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .max_by_key(|m| (m.size(), m.vrefresh()))
    {
        return *mode;
    }

    // pick the highest quality one that's not interlaced
    if let Some(mode) = connector
        .modes()
        .iter()
        .filter(|mode| !mode.flags().contains(ModeFlags::INTERLACE))
        .max_by_key(|m| (m.size(), m.vrefresh()))
    {
        return *mode;
    }

    // just pick the highest quality one
    if let Some(mode) = connector
        .modes()
        .iter()
        .max_by_key(|m| (m.size(), m.vrefresh()))
    {
        return *mode;
    }

    // what
    panic!("no modes available for this output?");
}

delegate_dmabuf!(App<UdevData>);
impl DmabufHandler for App<UdevData> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.backend.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: smithay::wayland::dmabuf::ImportNotifier,
    ) {
        let device = self.backend.device.as_mut().unwrap();
        device.gles.import_dmabuf(&dmabuf, None).unwrap();
        notifier.successful::<Self>().unwrap();
    }
}

pub struct RectElement {
    id: Id,
    rect: Rectangle<i32, Physical>,
    color: [f32; 4],
}

impl RectElement {
    pub fn new(pos: (i32, i32), size: (i32, i32), color: [f32; 4]) -> Self {
        Self {
            id: Id::new(),
            rect: Rectangle::new(pos.into(), size.into()),
            color,
        }
    }
}

impl Element for RectElement {
    fn id(&self) -> &smithay::backend::renderer::element::Id {
        &self.id
    }

    fn current_commit(&self) -> smithay::backend::renderer::utils::CommitCounter {
        CommitCounter::default()
    }

    fn src(&self) -> Rectangle<f64, smithay::utils::Buffer> {
        Rectangle::default()
    }

    fn geometry(&self, _: smithay::utils::Scale<f64>) -> Rectangle<i32, Physical> {
        self.rect
    }
}

impl<R: Renderer> RenderElement<R> for RectElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: smithay::utils::Rectangle<f64, smithay::utils::Buffer>,
        dst: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
        damage: &[smithay::utils::Rectangle<i32, smithay::utils::Physical>],
        _opaque_regions: &[smithay::utils::Rectangle<i32, smithay::utils::Physical>],
    ) -> Result<(), R::Error> {
        frame.draw_solid(dst, damage, self.color.into())
    }
}

smithay::backend::renderer::element::render_elements! {
    pub TestRenderElement<R, E> where R: ImportAll + ImportMem;
    Cursor = MemoryRenderBufferRenderElement<R>,
    Surface = WaylandSurfaceRenderElement<R>,
    Space = smithay::desktop::space::SpaceRenderElements<R, E>,
    Rect = RectElement,
}
