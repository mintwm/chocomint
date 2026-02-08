use plugin_engine::{
    UntypedPluginBinding,
    context::ExecutionContext,
    impl_untyped_plugin_binding,
    table::CapabilityProvider,
    wasm::{Linker, bindgen},
};
use slotmap::KeyData;

use crate::compositor::api::{
    CompositorContext, CompositorGlobals, PluginContextType, WindowKey,
    general::fusion::compositor::{
        types,
        wm_imports::{self, WindowId},
    },
};

bindgen!({
    path: "../../specs/compositor",
    world: "compositor",
});

pub struct GeneralCapabilityProvider;
impl CapabilityProvider for GeneralCapabilityProvider {
    type Inner = CompositorContext;

    fn link_functions(&self, linker: &mut Linker<ExecutionContext<Self::Inner>>) {
        Compositor::add_to_linker::<_, CompositorContext>(linker, |state| &mut state.inner)
            .unwrap();
    }

    fn create_bindings(
        &self,
        store: &mut wasmtime::Store<ExecutionContext<Self::Inner>>,
        component: &wasmtime::component::Component,
        linker: &Linker<ExecutionContext<Self::Inner>>,
    ) -> Box<dyn UntypedPluginBinding> {
        Box::new(Compositor::instantiate(&mut *store, component, linker).unwrap())
    }
}

impl_untyped_plugin_binding!(Compositor);

impl CompositorContext {
    #[inline]
    fn compositor(&self) -> std::sync::MutexGuard<'_, CompositorGlobals> {
        self.variants
            .get(&PluginContextType::Compositor)
            .unwrap()
            .compositor()
    }

    #[inline]
    fn compositor_mut(&mut self) -> std::sync::MutexGuard<'_, CompositorGlobals> {
        self.variants
            .get_mut(&PluginContextType::Compositor)
            .unwrap()
            .compositor_mut()
    }
}

impl wm_imports::Host for CompositorContext {
    fn get_elements(&mut self) -> Vec<WindowId> {
        let compositor = self.compositor_mut();
        compositor
            .mapped_windows
            .keys()
            .map(|key| WindowId {
                inner: key.0.as_ffi(),
            })
            .collect()
    }

    fn set_window_size(&mut self, window: WindowId, width: i32, height: i32) {
        let mut compositor = self.compositor_mut();
        let mapped_window = compositor
            .mapped_windows
            .get_mut(WindowKey(KeyData::from_ffi(window.inner)))
            .unwrap();

        mapped_window.set_window_size(width, height);
    }

    fn set_window_pos(&mut self, window: WindowId, x: i32, y: i32) {
        let mut compositor = self.compositor_mut();
        let mapped_window = compositor
            .mapped_windows
            .get(WindowKey(KeyData::from_ffi(window.inner)))
            .unwrap();

        let window = mapped_window.window().clone();
        compositor.space.map_element(window, (x, y), false);
    }

    fn get_output_size(&mut self) -> (i32, i32) {
        let compositor = self.compositor();
        if let Some(output) = compositor.space.outputs().next() {
            let geometry = compositor
                .space
                .output_geometry(output)
                .expect("Output not in space");
            (geometry.size.w, geometry.size.h)
        } else {
            panic!("TODO!")
        }
    }
}

impl types::Host for CompositorContext {}
