use std::path::Path;

use crate::error::PluginError;
use crate::{PluginContext, ZoloPlugin};

/// Plugin ABI version. Bump whenever `PluginVTable` changes shape.
///
/// Version history:
/// * 1 — initial release: name/version/register/on_init/on_shutdown/
///   set_runtime_bridge/drop.
/// * 2 — added `remove_handle` so the host VM can call back into the
///   originating plugin's `GLOBAL_HANDLE_TABLE` (each dynamically loaded
///   plugin has its own copy of zolo-plugin's statics, so cross-DLL handle
///   cleanup requires the plugin to export its own remover). This lets the
///   bridge attach a sentinel `userdata` with `__gc` to every wrapped
///   handle, so handles are released by the VM's GC instead of leaking
///   when the plugin author forgets a `remove_handle` call.
/// * 3 — the shared allocator moved from the zolo_alloc_rt cdylib into the
///   host executable; plugins resolve zolo_rt_* from the exe at runtime, so
///   a v2 plugin (raw-dylib import of zolo_alloc_rt.dll) can no longer load.
pub const ZOLO_PLUGIN_ABI_VERSION: u32 = 3;

/// RAII guard that resets the DLL search directory when dropped.
#[cfg(target_os = "windows")]
struct DllDirGuard;

#[cfg(target_os = "windows")]
impl Drop for DllDirGuard {
    fn drop(&mut self) {
        unsafe {
            unsafe extern "system" {
                fn SetDllDirectoryW(path: *const u16) -> i32;
            }
            SetDllDirectoryW(std::ptr::null());
        }
    }
}

#[repr(C)]
pub struct PluginStr {
    pub ptr: *const u8,
    pub len: u64,
}

impl PluginStr {
    pub unsafe fn to_string(&self) -> String {
        let bytes = unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) };
        unsafe { std::str::from_utf8_unchecked(bytes) }.to_string()
    }
}

#[repr(C)]
pub struct PluginVTable {
    pub name: extern "C" fn(*mut u8) -> PluginStr,
    pub version: extern "C" fn(*mut u8) -> PluginStr,
    pub register: extern "C" fn(*mut u8, *mut PluginContext),
    pub on_init: extern "C" fn(*mut u8),
    pub on_shutdown: extern "C" fn(*mut u8),
    pub set_runtime_bridge: extern "C" fn(*mut u8, *mut u8),
    pub drop: extern "C" fn(*mut u8),
    /// Release a handle id from this plugin's own
    /// `zolo_plugin::handle::GLOBAL_HANDLE_TABLE`. Each dynamically loaded
    /// plugin has its own copy of that static (Rust links statics per-DLL),
    /// so the host cannot call the plugin's `remove_handle` directly — it
    /// has to dispatch through this trampoline so the right table is mutated.
    /// Returns `true` if a live entry was removed, `false` if the id was
    /// already gone (double-remove is safe — the wrapper userdata's `__gc`
    /// may fire after the plugin already explicitly released the handle).
    pub remove_handle: extern "C" fn(u64) -> bool,
}

pub struct DynamicPlugin {
    name: String,
    version: String,
    instance: *mut u8,
    vtable: PluginVTable,
    _lib: libloading::Library,
}

unsafe impl Send for DynamicPlugin {}
unsafe impl Sync for DynamicPlugin {}

impl ZoloPlugin for DynamicPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn register(&self, ctx: &mut PluginContext) {
        (self.vtable.register)(self.instance, ctx as *mut PluginContext);
    }

    fn on_init(&self) {
        (self.vtable.on_init)(self.instance);
    }

    fn on_shutdown(&self) {
        (self.vtable.on_shutdown)(self.instance);
    }

    fn remove_handle_fn(&self) -> Option<extern "C" fn(u64) -> bool> {
        Some(self.vtable.remove_handle)
    }

    fn set_runtime_bridge(&self, bridge: std::sync::Arc<dyn crate::RuntimeBridge>) {
        // Arc<dyn Trait> is a fat pointer — can't pass directly through FFI.
        // Box it to get a thin pointer: *mut Box<Arc<dyn RuntimeBridge>> → *mut u8.
        let boxed: Box<std::sync::Arc<dyn crate::RuntimeBridge>> = Box::new(bridge);
        let raw = Box::into_raw(boxed) as *mut u8;
        (self.vtable.set_runtime_bridge)(self.instance, raw);
    }
}

impl Drop for DynamicPlugin {
    fn drop(&mut self) {
        (self.vtable.on_shutdown)(self.instance);
        (self.vtable.drop)(self.instance);
    }
}

/// Entry registered by `export_plugin!` when a plugin is compiled with
/// `--cfg zolo_plugin_static` for static linking into the host executable.
pub struct StaticPluginDesc {
    pub vtable: extern "C" fn() -> PluginVTable,
    pub create: extern "C" fn() -> *mut u8,
}

#[cfg(not(target_family = "wasm"))]
#[linkme::distributed_slice]
pub static STATIC_PLUGINS: [StaticPluginDesc] = [..];

/// A plugin whose code is linked into this binary (no dylib). Same vtable
/// dispatch as `DynamicPlugin`, minus the `libloading::Library` keep-alive.
pub struct StaticPlugin {
    name: String,
    version: String,
    instance: *mut u8,
    vtable: PluginVTable,
}

unsafe impl Send for StaticPlugin {}
unsafe impl Sync for StaticPlugin {}

impl ZoloPlugin for StaticPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn register(&self, ctx: &mut PluginContext) {
        (self.vtable.register)(self.instance, ctx as *mut PluginContext);
    }

    fn on_init(&self) {
        (self.vtable.on_init)(self.instance);
    }

    fn on_shutdown(&self) {
        (self.vtable.on_shutdown)(self.instance);
    }

    fn remove_handle_fn(&self) -> Option<extern "C" fn(u64) -> bool> {
        Some(self.vtable.remove_handle)
    }

    fn set_runtime_bridge(&self, bridge: std::sync::Arc<dyn crate::RuntimeBridge>) {
        let boxed: Box<std::sync::Arc<dyn crate::RuntimeBridge>> = Box::new(bridge);
        let raw = Box::into_raw(boxed) as *mut u8;
        (self.vtable.set_runtime_bridge)(self.instance, raw);
    }
}

impl Drop for StaticPlugin {
    fn drop(&mut self) {
        (self.vtable.on_shutdown)(self.instance);
        (self.vtable.drop)(self.instance);
    }
}

/// Instantiate a statically linked plugin from its registry entry.
pub fn load_static(desc: &StaticPluginDesc) -> Box<dyn ZoloPlugin> {
    let vtable = (desc.vtable)();
    let instance = (desc.create)();
    let name = unsafe { (vtable.name)(instance).to_string() };
    let version = unsafe { (vtable.version)(instance).to_string() };
    Box::new(StaticPlugin {
        name,
        version,
        instance,
        vtable,
    })
}

pub fn load_dynamic(path: &Path) -> Result<Box<dyn ZoloPlugin>, PluginError> {
    // Pre-flight: plugins allocate through the zolo_rt_* symbols exported by
    // THIS process's executable. If they are missing, the plugin's first
    // allocation would abort the whole process with a raw stderr message —
    // fail here with a recoverable, explanatory error instead. Binaries get
    // the exports by enabling zolo-alloc's `host` feature and emitting
    // zolo-alloc-exports link args from build.rs (test binaries too:
    // "rustc-link-arg-tests").
    if !zolo_alloc::host_allocator_exported() {
        return Err(PluginError::LoadError(
            "this process does not export the zolo shared-allocator symbols (zolo_rt_*); \
             plugins can only be loaded by a host built with zolo-alloc's `host` feature \
             and the zolo-alloc-exports link args (see crates/zolo-alloc)"
                .into(),
        ));
    }

    // Canonicalize path so Windows can resolve DLL dependencies relative to it
    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // On Windows, temporarily set the DLL search directory to the plugin's
    // directory so dependencies (e.g., libcef.dll) can be found alongside it.
    #[cfg(target_os = "windows")]
    let _restore_dll_dir = if let Some(dir) = abs_path.parent() {
        unsafe {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = dir
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            unsafe extern "system" {
                fn SetDllDirectoryW(path: *const u16) -> i32;
            }
            SetDllDirectoryW(wide.as_ptr());
        }
        Some(DllDirGuard)
    } else {
        None
    };

    let lib = unsafe {
        libloading::Library::new(&abs_path).map_err(|e| {
            PluginError::LoadError(format!("failed to load {}: {e}", abs_path.display()))
        })?
    };

    let abi_version: u32 = unsafe {
        let sym = lib
            .get::<*const u32>(b"ZOLO_PLUGIN_ABI_VERSION")
            .map_err(|e| PluginError::LoadError(format!("missing ZOLO_PLUGIN_ABI_VERSION: {e}")))?;
        **sym
    };

    if abi_version != ZOLO_PLUGIN_ABI_VERSION {
        return Err(PluginError::LoadError(format!(
            "ABI version mismatch: plugin has {abi_version}, host expects {ZOLO_PLUGIN_ABI_VERSION}",
        )));
    }

    let vtable: PluginVTable = unsafe {
        let sym = lib
            .get::<extern "C" fn() -> PluginVTable>(b"zolo_plugin_vtable")
            .map_err(|e| PluginError::LoadError(format!("missing zolo_plugin_vtable: {e}")))?;
        sym()
    };

    let instance: *mut u8 = unsafe {
        let sym = lib
            .get::<extern "C" fn() -> *mut u8>(b"zolo_plugin_create")
            .map_err(|e| PluginError::LoadError(format!("missing zolo_plugin_create: {e}")))?;
        sym()
    };

    if instance.is_null() {
        return Err(PluginError::LoadError(
            "zolo_plugin_create returned null".into(),
        ));
    }

    let name = unsafe { (vtable.name)(instance).to_string() };
    let version = unsafe { (vtable.version)(instance).to_string() };

    Ok(Box::new(DynamicPlugin {
        name,
        version,
        instance,
        vtable,
        _lib: lib,
    }))
}
