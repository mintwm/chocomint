#![allow(unused)]

use std::{
    cell::RefCell,
    collections::HashMap,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard},
};

use ::drm::control::crtc;
use calloop::LoopHandle;
use fusion_socket_protocol::{
    CompositorRequest, ExitResponse, FUSION_CTL_SOCKET_DEFAULT, GetPluginListResponse,
    PingResponse, Plugin, RestartPluginResponse,
};
use slotmap::SlotMap;
use smithay::{
    backend::{
        drm::{self, DrmDeviceFd, DrmNode},
        renderer::{element::RenderElement, utils::on_commit_buffer_handler},
        session::libseat::LibSeatSession,
    },
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        PopupKind, PopupManager, Space, Window, find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{KeyboardHandle, XkbConfig},
        pointer::{CursorImageStatus, Focus, GrabStartData, PointerHandle},
    },
    output::Output,
    reexports::{
        calloop::LoopSignal,
        wayland_protocols::{
            wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
            xdg::shell::server::xdg_toplevel,
        },
        x11rb::protocol::xproto::RESIZE_REQUEST_EVENT,
    },
    utils::{Clock, Logical, Monotonic, Physical, Point, Rectangle, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        dmabuf::DmabufHandler,
        input_method::InputMethodHandler,
        output::{OutputHandler, OutputManagerState},
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler,
            XdgShellState, XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
    },
};
use smithay_drm_extras::drm_scanner;
use tracing::trace_span;
use wayland_server::{
    Client, DisplayHandle, Resource,
    backend::ObjectId,
    protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
};
use zip::unstable::stream;

use crate::compositor::{
    ClientState,
    api::{
        CompositorContext, CompositorContextFactory, CompositorGlobals, UnsafeCompositorGlobals,
        WindowKey,
        general::{Compositor, GeneralCapabilityProvider, fusion::compositor::types::WindowId},
    },
    backend::Backend,
    cursor::InputState,
    data,
    grabs::{
        MoveSurfaceGrab, ResizeSurfaceGrab,
        resize_grab::{self},
    },
    mapped::MappedWindow,
    output::OutputState,
    udev::UdevOutputState,
};

use plugin_engine::{
    InnerContextFactory, PluginEngine, loader::LoaderConfig, table::CapabilityWriteRules,
};

pub struct App<B: Backend + 'static> {
    pub loop_signal: LoopSignal,
    pub handle: LoopHandle<'static, data::Data<B>>,
    pub backend: B,
    pub display: DisplayHandle,

    pub socket: UnixListener,
    pub engine: PluginEngine<CompositorContext>,

    pub compositor_state: CompositorState,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,

    pub popups: PopupManager,

    pub globals: Arc<Mutex<CompositorGlobals>>,

    pub clock: Clock<Monotonic>,

    pub sleep: bool,

    //Input
    pub input_state: InputState<B>,
    pub output_state: OutputState,
}

impl<B: Backend> App<B> {
    pub fn globals(&self) -> MutexGuard<'_, CompositorGlobals> {
        let _span = trace_span!("globals_mutex").entered();
        self.globals.lock().unwrap()
    }

    pub fn exit(stream: &mut UnixStream) {
        let response_data = postcard::to_stdvec_cobs(&ExitResponse).unwrap();
        stream.write_all(&response_data).unwrap();
        std::process::exit(0);
    }

    fn ping(stream: &mut UnixStream) {
        let response_data = postcard::to_stdvec_cobs(&PingResponse).unwrap();
        stream.write_all(&response_data).unwrap();
    }

    fn get_plugin_list(&self, stream: &mut UnixStream) {
        let mut plugins = Vec::new();
        for plugin_id in self.engine.get_plugin_list() {
            let plugin = self.engine.get_plugin_env_by_id(&plugin_id).unwrap();
            plugins.push(Plugin {
                id: plugin_id.to_string(),
                name: plugin.manifest().name().to_string(),
                status: plugin.status().to_string(),
                version: plugin.manifest().version().to_string(),
            });
        }

        let response = GetPluginListResponse::Ok(plugins);
        let response_data = postcard::to_stdvec_cobs(&response).unwrap();
        stream.write_all(&response_data).unwrap();
    }

    fn restart_plugin(&mut self, plugin_id: &str, stream: &mut UnixStream) {
        let response = match self.engine.restart_plugin(plugin_id) {
            Ok(status) => RestartPluginResponse::Ok,
            Err(error) => match error {
                plugin_engine::Error::PluginNotFound(message) => {
                    RestartPluginResponse::Error(message)
                }
            },
        };

        let response_data = postcard::to_stdvec_cobs(&response).unwrap();
        stream.write_all(&response_data).unwrap();
    }

    pub fn handle_socket(&mut self) {
        match self.socket.accept() {
            Ok((mut stream, addr)) => {
                let mut buf = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    stream.read_exact(&mut byte).unwrap();
                    if byte[0] == 0x00 {
                        break;
                    }
                    buf.push(byte[0]);
                }

                match postcard::from_bytes_cobs::<CompositorRequest>(&mut buf).unwrap() {
                    CompositorRequest::Exit(_) => Self::exit(&mut stream),
                    CompositorRequest::Ping(_) => Self::ping(&mut stream),
                    CompositorRequest::GetPluginList(_) => self.get_plugin_list(&mut stream),
                    CompositorRequest::RestartPlugin(request) => {
                        self.restart_plugin(&request.plugin_id, &mut stream);
                    }
                }
            }
            Err(error) => {}
        }
    }
}

impl<B: Backend> App<B> {
    pub fn init(
        dh: &DisplayHandle,
        backend: B,
        loop_signal: LoopSignal,
        handle: LoopHandle<'static, data::Data<B>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Композитор нашего композитора
        let compositor_state = CompositorState::new::<Self>(dh);

        // Буфер общей памяти для разделения буферов с клиентами.
        // Например, wl_buffer использует wl_shm для создания общего буфера
        // который будет использоваться композитором для
        // доступа к содержимому поверхности клиента.
        let shm_state = ShmState::new::<Self>(dh, vec![]);

        // Вывод - это пространство которое композитор использует.
        // OutputManagerState говорит wl_output использовать xdg-output extension.
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(dh);

        // Используется для настольных приложений.
        // Определяется два типа Wayland поверхностей клиентов которые могут быть использованы.
        // "toplevel" (для приложений) и "popup" (для диалоговых окон, подсказок и т.д.)
        let xdg_shell_state = XdgShellState::new::<Self>(dh);

        // Seat - группа устройств ввод такие как клавиатуры, мыши и т.д. Это управляет состоянем Seat.
        let mut seat_state = SeatState::<Self>::new();

        // Пространство для назначения окон к нему.
        // Отслеживает окна и выводы.
        // Можно получить доступ через space.element() и space.outputs()
        //let space = Space::<Window>::default();

        // Управляет копированием/вставкой и перетакиванием (drag-and-drop) от устройств ввода
        let data_device_state = DataDeviceState::new::<Self>(dh);

        // Создаём новый Seat из состояния Seat и передаём ему имя.
        let mut seat: Seat<Self> = seat_state.new_wl_seat(dh, "fusion_wm");

        // Добавляем указатель (мышь, тачпад и т.д.)
        let pointer_handle = seat.add_pointer();

        let popups = PopupManager::default();

        let mut globals = Arc::new(Mutex::new(CompositorGlobals::new()));

        let factory = {
            CompositorContextFactory {
                globals: globals.clone(),
            }
        };

        // Настройка модулей
        let mut engine = PluginEngine::new(factory, LoaderConfig::default())?;
        engine.add_capability(
            "compositor.window",
            CapabilityWriteRules::SingleWrite,
            GeneralCapabilityProvider,
        );

        if std::fs::exists(FUSION_CTL_SOCKET_DEFAULT)? {
            std::fs::remove_file(FUSION_CTL_SOCKET_DEFAULT)?;
        }

        let socket = UnixListener::bind(FUSION_CTL_SOCKET_DEFAULT)?;
        socket.set_nonblocking(true)?;

        let input_state = InputState::new(&mut seat);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(dh);

        Ok(Self {
            compositor_state,
            data_device_state,
            seat_state,
            seat,
            shm_state,
            //space,
            output_manager_state,
            xdg_shell_state,
            popups,
            loop_signal,
            handle,
            backend,

            engine,
            globals,
            socket,
            display: dh.clone(),

            input_state,
            output_state: OutputState::default(),
            clock: Clock::new(),
            xdg_decoration_state,
            sleep: false,
        })
    }

    pub fn map_output(&mut self, output: &Output) {
        self.globals().space.map_output(output, (0, 0));
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let space = &self.globals.lock().unwrap().space;
        let Some(window) = space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &root)
        else {
            return;
        };

        let output = space.outputs().next().unwrap();
        let output_geo = space.output_geometry(output).unwrap();
        let window_geo = space.element_geometry(window).unwrap();

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

delegate_seat!(@<B: Backend> App<B>);
impl<B: Backend> SeatHandler for App<B> {
    type KeyboardFocus = WlSurface;

    type PointerFocus = WlSurface;

    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut smithay::input::SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, status: CursorImageStatus) {
        self.input_state.cursor.set_icon(status);
        //TODO redraw
    }
}

delegate_compositor!(@<B: Backend + 'static> App<B>);
impl<B: Backend + 'static> CompositorHandler for App<B> {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        let space = &mut self.globals.lock().unwrap().space;
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
        }

        handle_commit(&mut self.engine, &mut self.popups, space, surface);
        resize_grab::handle_commit(space, surface);
    }
}

delegate_output!(@<B: Backend + 'static> App<B>);
impl<B: Backend + 'static> OutputHandler for App<B> {}

impl<B: Backend + 'static> BufferHandler for App<B> {
    fn buffer_destroyed(&mut self, buffer: &wayland_server::protocol::wl_buffer::WlBuffer) {}
}

delegate_shm!(@<B: Backend + 'static> App<B>);
impl<B: Backend + 'static> ShmHandler for App<B> {
    fn shm_state(&self) -> &smithay::wayland::shm::ShmState {
        &self.shm_state
    }
}

delegate_xdg_shell!(@<B: Backend + 'static> App<B>);
impl<B: Backend + 'static> XdgShellHandler for App<B> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        println!("Toplevel request");
        let window = Window::new_wayland_window(surface.clone());
        let window_id = {
            let mut globals = self.globals.lock().unwrap();
            let space = &mut globals.space;

            space.map_element(window.clone(), (0, 0), true);
            globals
                .mapped_windows
                .insert(MappedWindow::new(window.clone()))
        };
        window.user_data().insert_if_missing(|| window_id);

        let mut bindings = self
            .engine
            .get_single_write_bindings::<Compositor>("compositor.window");
        let mut store = bindings.store();
        let now = std::time::Instant::now();
        bindings
            .fusion_compositor_wm_exports()
            .call_new_toplevel(&mut store, window_id.into())
            .unwrap();
        let elapsed = now.elapsed();
        log::warn!("New toplevel creation took {elapsed:?}");
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let window_id = {
            let mut globals = self.globals.lock().unwrap();
            let window = {
                let space = &mut globals.space;
                space
                    .elements()
                    .find(|w| w.toplevel().unwrap().wl_surface() == surface.wl_surface())
                    .cloned()
                    .unwrap()
            };

            let window_id = *window.user_data().get::<WindowKey>().unwrap();
            let mapped_window = globals.mapped_windows.remove(window_id).unwrap();
            let space = &mut globals.space;
            space.unmap_elem(mapped_window.window());
            window_id
        };

        let mut bindings = self
            .engine
            .get_single_write_bindings::<Compositor>("compositor.window");
        let mut store = bindings.store();

        bindings
            .fusion_compositor_wm_exports()
            .call_toplevel_destroyed(&mut store, window_id.into())
            .unwrap();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();

            let grab = {
                let space = &mut self.globals.lock().unwrap().space;
                let window = space
                    .elements()
                    .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                    .unwrap()
                    .clone();
                let initial_window_location = space.element_location(&window).unwrap();

                MoveSurfaceGrab {
                    start_data,
                    window,
                    initial_window_location,
                }
            };

            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();

        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();

            let grab = {
                let space = &self.globals.lock().unwrap().space;

                let window = space
                    .elements()
                    .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                    .unwrap()
                    .clone();
                let initial_window_location = space.element_location(&window).unwrap();
                let initial_window_size = window.geometry().size;

                surface.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Resizing);
                });

                surface.send_pending_configure();

                ResizeSurfaceGrab::start(
                    start_data,
                    window,
                    edges.into(),
                    Rectangle::new(initial_window_location, initial_window_size),
                )
            };

            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // TODO popup grabs
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        match configure {
            Configure::Toplevel(configure) => {
                let space = unsafe {
                    let ptr = &raw const self.globals().space;
                    &*ptr
                };

                if let Some(window) = space
                    .elements()
                    .find(|w| *w.toplevel().unwrap().wl_surface() == surface)
                {
                    let mapped_windows = &mut self.globals().mapped_windows;
                    let window_id = window.user_data().get::<WindowKey>().unwrap();
                    let mapped_window = mapped_windows.get_mut(*window_id).unwrap();
                    mapped_window.ack_configure(configure.serial);
                }
            }
            Configure::Popup(_) => { /* TODO */ }
        }
    }
}

impl<B: Backend + 'static> SelectionHandler for App<B> {
    type SelectionUserData = ();
}

impl<B: Backend + 'static> ClientDndGrabHandler for App<B> {}
impl<B: Backend + 'static> ServerDndGrabHandler for App<B> {}

delegate_data_device!(@<B: Backend + 'static> App<B>);
impl<B: Backend + 'static> DataDeviceHandler for App<B> {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

pub fn handle_commit(
    engine: &mut PluginEngine<CompositorContext>,
    popups: &mut PopupManager,
    space: &Space<Window>,
    surface: &WlSurface,
) {
    // Handle toplevel commits.
    if let Some(window) = space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
        .cloned()
    {
        let window_id = *window.user_data().get::<WindowKey>().unwrap();
        let mut bindings = engine.get_single_write_bindings::<Compositor>("compositor.window");
        let mut store = bindings.store();
        bindings
            .fusion_compositor_wm_exports()
            .call_on_commit(&mut store, window_id.into())
            .unwrap();

        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        if !initial_configure_sent {
            window.toplevel().unwrap().send_configure();
        }
    }

    // Handle popup commits.
    popups.commit(surface);
    if let Some(popup) = popups.find_popup(surface) {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                if !xdg.is_initial_configure_sent() {
                    // NOTE: This should never fail as the initial configure is always
                    // allowed.
                    xdg.send_configure().expect("initial configure failed");
                }
            }
            PopupKind::InputMethod(ref _input_method) => {}
        }
    }
}

fn check_grab<B: Backend + 'static>(
    seat: &Seat<App<B>>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<GrabStartData<App<B>>> {
    let pointer = seat.get_pointer()?;

    // Check that this surface has a click grab.
    if !pointer.has_grab(serial) {
        return None;
    }

    let start_data = pointer.grab_start_data()?;

    let (focus, _) = start_data.focus.as_ref()?;
    // If the focus was for a different surface, ignore the request.
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }

    Some(start_data)
}

use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;

//TODO plugin configuration
delegate_xdg_decoration!(@<B: Backend + 'static> App<B>);
impl<B: Backend> XdgDecorationHandler for App<B> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        //TODO: custom decorations;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ClientSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        //TODO: custom decorations;
        toplevel
            .with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ClientSide));
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = None;
        });
        toplevel.send_configure();
    }
}
