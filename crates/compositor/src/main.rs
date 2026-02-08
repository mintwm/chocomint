#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
use std::time::Duration;

use tracing::trace_span;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
mod compositor;

use crate::compositor::{
    udev::{UdevData, init_udev},
    window::WinitBackend,
};
use smithay::reexports::calloop::EventLoop;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::new("warn")
        .add_directive("compositor=trace".parse().unwrap())
        .add_directive("calloop=trace".parse().unwrap());

    tracing_subscriber::registry()
        .with(filter) // Чтобы видеть логи в терминале
        .with(tracing_tracy::TracyLayer::default()) // Отправка данных в профайлер
        .init();

    run_udev()?;

    Ok(())
}

#[allow(unused)]
fn run_winit() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<compositor::data::Data<WinitBackend>> = EventLoop::try_new()?;

    let backend = WinitBackend::new().unwrap();
    let mut data =
        compositor::init_compositor(event_loop.handle(), event_loop.get_signal(), backend)?;
    event_loop.run(None, &mut data, |_| {})?;
    Ok(())
}

fn run_udev() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<compositor::data::Data<UdevData>> = EventLoop::try_new()?;

    let backend = UdevData::init(&event_loop.handle());
    let mut data =
        compositor::init_compositor(event_loop.handle(), event_loop.get_signal(), backend)?;

    init_udev(&mut data.state);

    event_loop.run(Duration::from_secs(10), &mut data, |data| {
        let span = trace_span!("event_loop").entered();
        span.in_scope(|| {
            let span = trace_span!("plugin_engine").entered();
            span.in_scope(|| {
                data.state.handle_socket();
                data.state.engine.load_packages();
            });
            drop(span);

            let span = trace_span!("render_all").entered();
            span.in_scope(|| {
                data.state.render_all();
            });
            drop(span);

            let span = trace_span!("send_pending_configure").entered();
            span.in_scope(|| {
                for mapped_window in data.state.globals().mapped_windows.values_mut() {
                    mapped_window.send_pending_configure();
                }
            });
            drop(span);

            data.display.flush_clients().unwrap();
        });
    })?;
    Ok(())
}
