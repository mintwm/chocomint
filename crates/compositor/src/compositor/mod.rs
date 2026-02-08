pub mod api;
pub mod backend;
pub mod cursor;
pub mod data;
pub mod grabs;
pub mod input;
pub mod mapped;
pub mod output;
pub mod state;
pub mod udev;
pub mod window;

use std::sync::Arc;

use calloop::{LoopHandle, LoopSignal};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, PostAction};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use smithay::wayland::compositor::{CompositorClientState, CompositorState};

use wayland_server::backend::{ClientData, ClientId, DisconnectReason};

use crate::compositor::backend::Backend;
use crate::compositor::state::App;

pub fn init_compositor<B: Backend + 'static>(
    loop_handle: LoopHandle<'static, data::Data<B>>,
    signal: LoopSignal,
    backend: B,
) -> Result<data::Data<B>, Box<dyn std::error::Error>> {
    // Структура которая используется для хранения состояния композитора
    // и управления Бэкендом для отправки событий и получения запросов.
    let display: Display<App<B>> = Display::new()?;

    // Получаем DisplayHandle который будет использоваться для добавление и получения Wayland клиентов,
    // создания/отключения/удаления глобальных объектов, отправки событий и т.д.
    let dh = display.handle();

    // Wayland ListeningSocket который реализует calloop::EventSource и может быть использован в качестве источника в EventLoop.
    // Клиенты Wayland должны подключаться к этому сокету для получения событий и отправки запросов.
    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_os_string();

    println!("Socket: {}", socket_name.display());

    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    // Добавляем сокет Wayland к циклу событий
    // Цикл событий потребляет источник (сокет), затем замыкание, которые производит событие, метаданные и клиентские данные.
    // Событие в этом примере это UnixStream созданный сокетом,
    // без метаданных и клиентских данных которые были определены когда создали переменную event_loop
    loop_handle.insert_source(socket, |stream, (), data| {
        // Вставляем нового клиента в Display вместе с данными связанными с этим клиентом.
        // Это запустит управление клиентом через UnixStream
        data.display
            .insert_client(stream, Arc::new(ClientState::default()))
            .unwrap();
    })?;

    // Добавляем Display в цикл событий
    // Этот цикл событий может принять обобщенную структуру содержащую файловый дескриптор
    // который будет использоваться для генерации событий. Этот файловый дескриптор создается из winit ниже.
    // Нам только нужно читать (Interest::READ) файловый дескриптор, а Mode::Level будет следить за событиями
    // каждый раз когда цикл событий выполняет опрос.
    loop_handle.insert_source(
        Generic::new(
            display,
            Interest::READ,
            smithay::reexports::calloop::Mode::Level,
        ),
        |_, display, data| {
            // Отправка запросов, полученных от клиентов, на обратные вызовы для клиентов.
            // Обратные вызовам, возможно, понадобится доступ к текущему состоянию композитора, поэтому передаём его.
            unsafe {
                display.get_mut().dispatch_clients(&mut data.state).unwrap();
            }

            // Выше ListeningSocketSource обрабатывал цикл обработки событий, указывая PostAction.
            // Здесь мы реализуем наш собственный общий источник событий, поэтому мы должны вернуть
            // PostAction::Continue, чтобы сообщить циклу обработки событий о продолжении прослушивания событий.
            Ok(PostAction::Continue)
        },
    )?;

    // Создаем состояние нашего композитора и передаём все глобальные объекты к которым мы будем обращаться
    let state = App::init(&dh, backend, signal, loop_handle)?;

    // Данные хранящиеся в цикле событий, мы должны получать доступ к дисплею и состоянию композитора.
    let data = data::Data {
        display: dh.clone(),
        state,
    };

    Ok(data)
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl<B: Backend + 'static> AsMut<CompositorState> for App<B> {
    fn as_mut(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
}
