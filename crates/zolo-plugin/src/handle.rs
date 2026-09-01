use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::error::PluginError;

pub struct PluginHandle<T: Send + Sync + 'static> {
    inner: Arc<HandleInner<T>>,
}

pub struct HandleInner<T> {
    resource: RwLock<T>,
    closed: AtomicBool,
    type_name: &'static str,
    _type_id: TypeId,
}

impl<T: Send + Sync + 'static> PluginHandle<T> {
    pub fn new(resource: T, type_name: &'static str) -> Self {
        Self {
            inner: Arc::new(HandleInner {
                resource: RwLock::new(resource),
                closed: AtomicBool::new(false),
                type_name,
                _type_id: TypeId::of::<T>(),
            }),
        }
    }

    pub fn get(&self) -> Result<RwLockReadGuard<'_, T>, PluginError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(PluginError::HandleClosed {
                type_name: self.inner.type_name.to_string(),
            });
        }
        self.inner
            .resource
            .read()
            .map_err(|_| PluginError::Custom(format!("{} lock poisoned", self.inner.type_name)))
    }

    pub fn get_mut(&self) -> Result<RwLockWriteGuard<'_, T>, PluginError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(PluginError::HandleClosed {
                type_name: self.inner.type_name.to_string(),
            });
        }
        self.inner
            .resource
            .write()
            .map_err(|_| PluginError::Custom(format!("{} lock poisoned", self.inner.type_name)))
    }

    pub fn close(&self) -> Result<(), PluginError> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Err(PluginError::HandleClosed {
                type_name: self.inner.type_name.to_string(),
            });
        }
        Ok(())
    }

    pub fn release(&self) {
        self.inner.closed.store(true, Ordering::Release);
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    pub fn type_name(&self) -> &'static str {
        self.inner.type_name
    }

    pub fn clone_inner(&self) -> Arc<dyn Any + Send + Sync> {
        self.inner.clone()
    }
}

impl<T: Send + Sync + 'static> Clone for PluginHandle<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

// --- HandleTable ---

pub struct HandleTable {
    handles: HashMap<u64, Arc<dyn Any + Send + Sync>>,
    next_id: u64,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn insert(&mut self, handle: Arc<dyn Any + Send + Sync>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, handle);
        id
    }

    pub fn get<T: Send + Sync + 'static>(&self, id: u64) -> Option<Arc<HandleInner<T>>> {
        self.handles
            .get(&id)
            .and_then(|h| h.clone().downcast::<HandleInner<T>>().ok())
    }

    pub fn remove(&mut self, id: u64) -> bool {
        self.handles.remove(&id).is_some()
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

// Global handle table
static GLOBAL_HANDLE_TABLE: LazyLock<RwLock<HandleTable>> =
    LazyLock::new(|| RwLock::new(HandleTable::new()));

/// Side-table mapping a handle id to the class name its owning plugin
/// associated with it (typically the `type_name` passed to
/// `PluginHandle::new`). Used by the VM bridge to wrap a bare handle into an
/// instance table with a metatable on the fly, so Zolo code can dispatch
/// methods on a handle received as a hook argument.
static HANDLE_CLASS_NAMES: LazyLock<RwLock<HashMap<u64, &'static str>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn register_handle(handle: Arc<dyn Any + Send + Sync>) -> u64 {
    let mut table = GLOBAL_HANDLE_TABLE.write().unwrap();
    table.insert(handle)
}

/// Register a handle and record its class name in one call. Prefer this over
/// `register_handle` when the caller has a `PluginHandle<T>` on hand so the
/// VM bridge can auto-wrap the handle for method dispatch in scripts.
pub fn register_handle_typed<T: Send + Sync + 'static>(handle: &PluginHandle<T>) -> u64 {
    let id = register_handle(handle.clone_inner());
    HANDLE_CLASS_NAMES
        .write()
        .unwrap()
        .insert(id, handle.type_name());
    id
}

/// Look up the class name associated with a handle id. Returns `None` if the
/// handle was registered via `register_handle` without a class name.
///
/// Note: when called from a dynamically loaded plugin DLL, this only sees
/// names registered *in the same DLL* — each shared object has its own copy
/// of `HANDLE_CLASS_NAMES`. To make a class name visible to the host VM
/// across the FFI boundary, return the handle in the
/// `PluginValue::Table([__handle, __class])` wire shape via
/// `register_handle_tagged`.
pub fn handle_class_name(id: u64) -> Option<&'static str> {
    HANDLE_CLASS_NAMES.read().unwrap().get(&id).copied()
}

/// Register a handle and return it as a `PluginValue::Table` with
/// `__handle` and `__class` fields — the wire format the VM trampolines
/// recognise as a method-dispatchable instance. Use this whenever a
/// plugin function returns a handle whose class differs from the calling
/// class (e.g. `wgpu.request_adapter()` returning a `GpuAdapter`), so
/// the VM attaches the *right* metatable. Cross-DLL safe — no static
/// state is required on the host side.
pub fn register_handle_tagged<T: Send + Sync + 'static>(
    handle: &PluginHandle<T>,
) -> crate::value::PluginValue {
    let id = register_handle_typed(handle);
    crate::value::PluginValue::tagged_handle(id, handle.type_name())
}

pub fn get_handle<T: Send + Sync + 'static>(id: u64) -> Option<PluginHandle<T>> {
    let table = GLOBAL_HANDLE_TABLE.read().unwrap();
    table.get::<T>(id).map(|inner| PluginHandle { inner })
}

pub fn remove_handle(id: u64) -> bool {
    let mut table = GLOBAL_HANDLE_TABLE.write().unwrap();
    HANDLE_CLASS_NAMES.write().unwrap().remove(&id);
    table.remove(id)
}

/// Number of live handles in this plugin's handle table.
///
/// Per-plugin: each dynamically loaded plugin has its own statics, so this
/// reports the size of the *calling* plugin's table — not a global count.
/// Useful as a leak-detection probe from inside a plugin (e.g. a wgpu
/// render loop can assert the count stays bounded across frames).
pub fn handle_count() -> usize {
    GLOBAL_HANDLE_TABLE.read().unwrap().len()
}
