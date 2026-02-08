use crate::{Session, XDG_SESSION_TYPE};

pub(super) enum SessionType {
    TTY,
    Wayland,
    Other(String),
}

pub(super) fn define_session_type(custom: Option<Session>) -> SessionType {
    if let Some(custom) = custom {
        return match custom {
            Session::Winit => SessionType::Wayland,
            Session::Udev => SessionType::TTY,
        };
    }

    let session_type = std::env::var(XDG_SESSION_TYPE).expect("XDG_SESSION_TYPE is not set");
    match session_type.as_str() {
        "tty" => SessionType::TTY,
        "wayland" => SessionType::Wayland,
        other => SessionType::Other(other.to_string()),
    }
}
