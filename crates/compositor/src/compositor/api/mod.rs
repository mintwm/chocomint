use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

use plugin_engine::{InnerContext, InnerContextFactory};
use slotmap::{KeyData, SlotMap, new_key_type};
use smithay::desktop::{Space, Window};
use wasmtime::component::HasData;

use crate::compositor::{api::general::fusion::compositor::types::WindowId, mapped::MappedWindow};

pub mod general;

// Compositor
// Audio
// Network

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginContextType {
    Compositor,
}

pub enum PluginContext {
    Compositor(Arc<Mutex<CompositorGlobals>>),
}

impl PluginContext {
    #[inline]
    pub fn compositor(&self) -> std::sync::MutexGuard<'_, CompositorGlobals> {
        match self {
            PluginContext::Compositor(compositor) => compositor.lock().unwrap(),
        }
    }

    #[inline]
    pub fn compositor_mut(&mut self) -> std::sync::MutexGuard<'_, CompositorGlobals> {
        match self {
            PluginContext::Compositor(compositor) => compositor.lock().unwrap(),
        }
    }
}

#[derive(Clone)]
pub struct UnsafeCompositorGlobals {
    inner: *mut CompositorGlobals,
}

impl core::ops::Deref for UnsafeCompositorGlobals {
    type Target = CompositorGlobals;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner }
    }
}

impl core::ops::DerefMut for UnsafeCompositorGlobals {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.inner }
    }
}

pub struct CompositorContextFactory {
    pub globals: Arc<Mutex<CompositorGlobals>>,
}

impl InnerContextFactory<CompositorContext> for CompositorContextFactory {
    fn generate(&self, _capabilities: &[String]) -> CompositorContext {
        let globals = self.globals.clone();
        let mut map = HashMap::new();
        map.insert(
            PluginContextType::Compositor,
            PluginContext::Compositor(globals),
        );

        CompositorContext { variants: map }
    }
}

new_key_type! {
    pub struct WindowKey;
}

impl From<WindowId> for WindowKey {
    #[inline]
    fn from(id: WindowId) -> Self {
        Self(KeyData::from_ffi(id.inner))
    }
}

impl From<WindowKey> for WindowId {
    #[inline]
    fn from(key: WindowKey) -> Self {
        WindowId {
            inner: key.0.as_ffi(),
        }
    }
}

pub struct CompositorGlobals {
    pub mapped_windows: SlotMap<WindowKey, MappedWindow>,
    pub space: Space<Window>,
}

impl CompositorGlobals {
    pub fn new() -> Self {
        Self {
            mapped_windows: SlotMap::default(),
            space: Space::default(),
        }
    }
}

pub struct CompositorContext {
    variants: HashMap<PluginContextType, PluginContext>,
}

impl HasData for CompositorContext {
    type Data<'a> = &'a mut CompositorContext;
}

fn get_config_dir() -> std::path::PathBuf {
    dirs::config_dir().unwrap().join("fusion")
}

impl InnerContext for CompositorContext {
    type Factory = CompositorContextFactory;

    fn config_path() -> std::path::PathBuf {
        let root = get_config_dir();
        root.join("config")
    }

    fn logs_path() -> std::path::PathBuf {
        let root = get_config_dir();
        root.join("logs")
    }

    fn plugins_path() -> std::path::PathBuf {
        let root = get_config_dir();
        root.join("plugins")
    }
}
