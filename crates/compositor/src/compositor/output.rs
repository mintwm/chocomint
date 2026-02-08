use std::collections::HashMap;

use derive_more::Display;
use drm::control::crtc;
use smithay::output::Output;

use crate::compositor::udev::UdevOutputState;

#[derive(Default, Clone, Copy, PartialEq, Eq, Display)]
pub enum RenderState {
    #[default]
    Idle,
    Queued,
}

#[derive(Default)]
pub struct OutputState {
    pub outputs: HashMap<Output, RenderState>,
}

impl OutputState {
    pub fn add_output(&mut self, output: Output) {
        self.outputs.insert(output, RenderState::Queued);
    }

    //TODO
    #[allow(unused)]
    pub fn remove_output(&mut self, output: &Output) {
        self.outputs.remove(output).unwrap();
    }

    pub fn queue_render(&mut self, output: &Output) {
        let state = self.outputs.get_mut(output).unwrap();
        if let RenderState::Idle = *state {
            *state = RenderState::Queued;
        } else {
            unreachable!("Error. Incorrect draw state: {state}.")
        }
    }

    pub fn wait_render_request(&mut self, output: &Output) {
        let state = self.outputs.get_mut(output).unwrap();
        *state = RenderState::Idle;
        //if let RenderState::Idle = *state {
        //    *state = RenderState::Queued;
        //} else {
        //    unreachable!("Error. Incorrect draw state: {state}.")
        //}
    }

    pub fn udev_output(&mut self, crtc: crtc::Handle, device_id: u64) -> Option<&Output> {
        self.outputs.keys().find(|output| {
            let state = output.user_data().get::<UdevOutputState>().unwrap();
            state.crtc == crtc && state.device_id == device_id
        })
    }
}
