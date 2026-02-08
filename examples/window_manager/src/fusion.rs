#![allow(clippy::cast_possible_truncation)]

use std::sync::Mutex;

use crate::{
    WindowManager,
    fusion::fusion::compositor::{
        types::WindowId,
        wm_imports::{get_output_size, set_window_pos, set_window_size},
    },
};

wit_bindgen::generate!({
    path: "../../specs/compositor",
    world: "compositor",
});

#[derive(Default)]
struct GlobalState {
    windows: Vec<WindowId>,
}

impl GlobalState {
    #[must_use]
    pub const fn new() -> Self {
        Self { windows: vec![] }
    }

    pub fn rearrange_windows(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        let (screen_width, screen_height) = get_output_size();
        let width_per_window = screen_width / self.windows.len() as i32;

        for (i, window) in self.windows.iter().enumerate() {
            let window = *window;
            let x_pos = i as i32 * width_per_window;
            let y_pos = 0;

            set_window_size(window, width_per_window, screen_height);
            set_window_pos(window, x_pos, y_pos);
        }
    }
}

static STATE: Mutex<GlobalState> = Mutex::new(GlobalState::new());

fn state<R>(f: impl FnOnce(&mut GlobalState) -> R) -> R {
    let mut wm = STATE.lock().unwrap();
    f(&mut wm)
}

impl exports::fusion::compositor::wm_exports::Guest for crate::WindowManager {
    fn new_toplevel(window: WindowId) {
        state(|wm| {
            wm.windows.push(window);
            wm.rearrange_windows();
        });
    }

    fn toplevel_destroyed(window: WindowId) {
        state(|wm| {
            wm.windows.retain(|&w| w.inner != window.inner);
            wm.rearrange_windows();
        });
    }

    fn rearrange_windows() {
        state(GlobalState::rearrange_windows);
    }

    fn on_commit(_: WindowId) {}
}

impl Guest for crate::WindowManager {
    fn stop() {}
}

export!(WindowManager);
