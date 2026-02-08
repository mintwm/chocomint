use smithay::{
    desktop::Window,
    utils::{Logical, Serial, Size},
    wayland::shell::xdg::{ToplevelState, ToplevelSurface},
};

pub struct MappedWindow {
    window: Window,
    has_pending_changes: bool,

    wait_ack_configure: bool,
    next_configure_serial: Option<Serial>,
}

impl MappedWindow {
    pub const fn new(window: Window) -> Self {
        Self {
            window,
            has_pending_changes: false,
            wait_ack_configure: false,
            next_configure_serial: None,
        }
    }

    pub fn set_window_size(&mut self, width: i32, height: i32) {
        let toplevel = self.window.toplevel().unwrap();
        toplevel.with_pending_state(|state| {
            state.size = Some(Size::<i32, Logical>::new(width, height));
        });

        self.has_pending_changes = true;
    }

    pub fn with_pending_state<F, T>(&mut self, f: F)
    where
        F: FnOnce(&mut ToplevelState) -> T,
    {
        let toplevel = self.toplevel();
        toplevel.with_pending_state(f);
        self.has_pending_changes = true;
    }

    pub fn send_pending_configure(&mut self) {
        if self.has_pending_changes && !self.wait_ack_configure {
            let toplevel = self.window.toplevel().unwrap();
            let serial = toplevel.send_pending_configure();
            if let Some(serial) = serial {
                self.next_configure_serial = Some(serial);
                self.wait_ack_configure = true;
                log::error!("wait for ack configure with serial: {serial:#?}");
            }
        }
    }

    pub fn ack_configure(&mut self, serial: Serial) {
        if !self.wait_ack_configure {
            log::error!("we don't wait ack with serial: {serial:#?}");
            return;
        }

        if let Some(inner_serial) = self.next_configure_serial {
            if inner_serial == serial {
                self.next_configure_serial = None;
                self.wait_ack_configure = false;
                log::error!("confirm serial: {serial:#?}");
            } else {
                log::error!("incorrect serial");
            }
        } else {
            log::error!("serial is none");
        }
    }

    pub fn toplevel(&self) -> &ToplevelSurface {
        self.window.toplevel().unwrap()
    }

    pub const fn window(&self) -> &Window {
        &self.window
    }
}
