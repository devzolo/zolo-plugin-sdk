use std::path::PathBuf;
use std::sync::Arc;

pub mod embed;
pub mod error;
pub mod handle;
#[cfg(not(target_family = "wasm"))]
pub mod loader;
pub mod registry;
pub mod types;
pub mod value;
pub mod window_handle;

pub use error::PluginError;
pub use types::*;
pub use value::PluginValue;
pub use window_handle::WindowHandle;
pub use zolo_plugin_macros::*;

/// Re-export of the shared global allocator shim, used by `export_plugin!`.
/// Hidden from docs — it exists only so the macro can name `ZoloAlloc` through
/// `$crate` without the plugin crate needing a direct `zolo-alloc` dependency.
#[cfg(not(target_family = "wasm"))]
#[doc(hidden)]
pub use zolo_alloc as __zolo_alloc;

/// Re-export of `linkme` so `export_plugin!`'s static-link branch can register
/// a `StaticPluginDesc` in `loader::STATIC_PLUGINS` without the plugin crate
/// depending on `linkme` directly. The distributed-slice attributes name it
/// through `$crate::linkme` (plus `#[linkme(crate = $crate::linkme)]`).
#[cfg(not(target_family = "wasm"))]
pub use linkme;

// ── Caller source directory ──────────────────────────────────────────
// Set by the VM before calling a plugin function, so plugins can resolve
// relative paths (like "./routes/") relative to the calling script file,
// matching Node/Deno behavior.
//
// Uses an environment variable (`__ZOLO_CALLER_SOURCE_DIR`) because plugins
// are dynamically loaded DLLs that have their own copy of Rust statics.
// Env vars are process-global, so the VM and all plugin DLLs share the same value.

#[cfg(not(target_family = "wasm"))]
const CALLER_DIR_ENV: &str = "__ZOLO_CALLER_SOURCE_DIR";

// On wasm there are no dynamically loaded DLLs — every "plugin" is linked
// statically, so a plain thread_local is shared by definition. The env-var
// channel is not only unnecessary there: `std::env::remove_var` PANICS on
// wasm32-unknown-unknown ("cannot unset env vars on this platform"), which
// aborted the playground on the first builtin-plugin call.
#[cfg(target_family = "wasm")]
thread_local! {
    static CALLER_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Returns the directory of the script that called the current plugin function.
/// Plugins should use this to resolve relative paths like `./routes/`.
pub fn caller_source_dir() -> Option<PathBuf> {
    #[cfg(not(target_family = "wasm"))]
    {
        std::env::var(CALLER_DIR_ENV).ok().map(PathBuf::from)
    }
    #[cfg(target_family = "wasm")]
    {
        CALLER_DIR.with(|d| d.borrow().clone())
    }
}

/// Set the caller source directory. Called by the VM trampolines.
///
/// # Safety
/// Uses `std::env::set_var` which is unsafe in Rust 2024+ because env vars are
/// process-global. This is safe here because it's only called from the single-threaded
/// VM trampoline path, never concurrently.
pub fn set_caller_source_dir(dir: Option<PathBuf>) {
    #[cfg(not(target_family = "wasm"))]
    unsafe {
        match dir {
            Some(d) => std::env::set_var(CALLER_DIR_ENV, d.as_os_str()),
            None => std::env::remove_var(CALLER_DIR_ENV),
        }
    }
    #[cfg(target_family = "wasm")]
    {
        CALLER_DIR.with(|s| *s.borrow_mut() = dir);
    }
}

/// Resolve a path string: if it starts with `./` or `../`, resolve it relative
/// to the caller's script directory. Otherwise, return it as-is.
pub fn resolve_plugin_path(path: &str) -> PathBuf {
    if (path.starts_with("./")
        || path.starts_with("../")
        || path.starts_with(".\\")
        || path.starts_with("..\\"))
        && let Some(base) = caller_source_dir()
    {
        return base.join(path);
    }
    PathBuf::from(path)
}

/// Trait that allows plugins to call back into Zolo code.
/// Each runtime (VM and Native) implements this trait.
pub trait RuntimeBridge: Send + Sync {
    fn call_function(
        &self,
        func_id: u64,
        args: &[crate::PluginValue],
    ) -> Result<crate::PluginValue, crate::PluginError>;

    /// Compile and execute Zolo source code, returning the result.
    /// Used by plugins that need to dynamically load .zolo files (e.g. svelte +server.zolo).
    fn eval_code(
        &self,
        _source: &str,
        _name: &str,
    ) -> Result<crate::PluginValue, crate::PluginError> {
        Err(crate::PluginError::Custom(
            "eval_code not supported by this runtime".into(),
        ))
    }
}

pub trait ZoloPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn register(&self, ctx: &mut PluginContext);
    fn on_init(&self) {}
    fn on_shutdown(&self) {}
    fn set_runtime_bridge(&self, _bridge: Arc<dyn RuntimeBridge>) {}

    /// FFI trampoline that releases a handle id from *this* plugin's
    /// `GLOBAL_HANDLE_TABLE`. Defaults to `None` for built-in plugins
    /// (they share the host's static, so the host's own
    /// `zolo_plugin::handle::remove_handle` is the right call).
    /// Dynamically loaded plugins override this to return their vtable
    /// trampoline — required because each plugin DLL has its own copy of
    /// zolo-plugin's statics.
    fn remove_handle_fn(&self) -> Option<extern "C" fn(u64) -> bool> {
        None
    }
}

pub struct PluginContext {
    pub functions: Vec<FunctionDef>,
    pub classes: Vec<ClassDef>,
    pub globals: Vec<(String, ValueDef)>,
    pub modules: Vec<(String, PluginContext)>,
}

impl PluginContext {
    pub fn new() -> Self {
        Self {
            functions: Vec::new(),
            classes: Vec::new(),
            globals: Vec::new(),
            modules: Vec::new(),
        }
    }

    pub fn add_function(&mut self, name: &str, func: NativePluginFn, sig: FnSignature) {
        self.functions.push(FunctionDef {
            name: name.to_string(),
            func,
            sig,
        });
    }

    pub fn add_class(&mut self, def: ClassDef) {
        self.classes.push(def);
    }

    pub fn add_global(&mut self, name: &str, value: ValueDef) {
        self.globals.push((name.to_string(), value));
    }

    pub fn add_module(&mut self, name: &str, build: fn(&mut PluginContext)) {
        let mut sub = PluginContext::new();
        build(&mut sub);
        self.modules.push((name.to_string(), sub));
    }

    /// Serialize this plugin's API to `.d.zolo` declaration syntax.
    pub fn to_declarations(&self) -> String {
        let mut out = String::new();

        // Global constants
        for (name, value) in &self.globals {
            let val_str = match value {
                ValueDef::Number(n) => format!("{}", n),
                ValueDef::Integer(i) => format!("{}", i),
                ValueDef::String(s) => format!("\"{}\"", s),
                ValueDef::Bool(b) => format!("{}", b),
                ValueDef::Nil => "nil".to_string(),
            };
            out.push_str(&format!("const {} = {}\n", name, val_str));
        }
        if !self.globals.is_empty() {
            out.push('\n');
        }

        // Free functions. Some plugins (e.g. prometheus) register namespaced
        // APIs as dotted function names like `MetricRegistry.new` — there's
        // no first-class class syntax in their `register()` call. Group those
        // into synthetic `impl` blocks so the emission is valid Zolo.
        let mut synthetic_classes: std::collections::BTreeMap<String, Vec<(String, &FnSignature)>> =
            std::collections::BTreeMap::new();
        let mut free_functions: Vec<&FunctionDef> = Vec::new();
        for func in &self.functions {
            if let Some((cls, method)) = func.name.split_once('.') {
                synthetic_classes
                    .entry(cls.to_string())
                    .or_default()
                    .push((method.to_string(), &func.sig));
            } else {
                free_functions.push(func);
            }
        }
        for func in &free_functions {
            out.push_str(&format_fn_decl(&func.name, &func.sig, false, ""));
        }
        if !free_functions.is_empty() {
            out.push('\n');
        }

        // Synthetic impl blocks from dotted function names. Heuristic: the
        // first parameter's TypeHint matches the class name — treat as method
        // (emit `self`), otherwise emit as static. Constructors conventionally
        // named `new` with no matching first arg fall into the static bucket.
        for (cls, members) in &synthetic_classes {
            out.push_str(&format!("struct {}\n\n", cls));
            out.push_str(&format!("impl {} {{\n", cls));
            for (method, sig) in members {
                let is_self_method = sig
                    .params
                    .first()
                    .map(|p| matches!(&p.type_hint, TypeHint::Handle(h) if h == cls))
                    .unwrap_or(false);
                out.push_str(&format_fn_decl(method, sig, is_self_method, "    "));
            }
            out.push_str("}\n\n");
        }

        // Classes
        for class in &self.classes {
            out.push_str(&format!("struct {}\n\n", class.name));
            out.push_str(&format!("impl {} {{\n", class.name));

            // constructor (no self — static factory)
            if let Some(ref sig) = class.constructor_sig {
                out.push_str(&format_fn_decl("new", sig, false, "    "));
            }

            // instance methods (have self handle as first param)
            for (name, _, sig) in &class.methods {
                out.push_str(&format_fn_decl(name, sig, true, "    "));
            }

            // static methods
            for (name, _, sig) in &class.static_methods {
                out.push_str(&format_fn_decl(name, sig, false, "    "));
            }

            out.push_str("}\n\n");
        }

        // Sub-modules (recursive)
        for (mod_name, sub_ctx) in &self.modules {
            out.push_str(&format!("// module: {}\n", mod_name));
            out.push_str(&sub_ctx.to_declarations());
        }

        out
    }
}

/// Format a function declaration in .d.zolo syntax.
fn format_fn_decl(name: &str, sig: &FnSignature, is_method: bool, indent: &str) -> String {
    let mut params = Vec::new();
    if is_method {
        params.push("self".to_string());
    }
    let skip = if is_method { 1 } else { 0 };
    for param in sig.params.iter().skip(skip) {
        params.push(format!(
            "{}: {}",
            param.name,
            param.type_hint.to_zolo_type()
        ));
    }
    let ret = match &sig.returns {
        TypeHint::Nil => String::new(),
        other => format!(" -> {}", other.to_zolo_type()),
    };
    format!("{}fn {}({}){}\n", indent, name, params.join(", "), ret)
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new()
    }
}

pub mod prelude {
    pub use crate::error::PluginError;
    pub use crate::handle::PluginHandle;
    pub use crate::types::*;
    pub use crate::value::PluginValue;
    pub use crate::window_handle::WindowHandle;
    pub use crate::{
        PluginContext, RuntimeBridge, ZoloPlugin, caller_source_dir, resolve_plugin_path,
    };
    pub use std::sync::Arc;
}

/// Export the FFI entry points required for dynamic plugin loading.
///
/// Usage (in plugin crate root):
/// ```ignore
/// zolo_plugin::export_plugin!(MyPlugin);
/// ```
///
/// This generates `ZOLO_PLUGIN_ABI_VERSION`, `zolo_plugin_vtable()`,
/// and `zolo_plugin_create()` — the symbols that `loader::load_dynamic` expects.
#[macro_export]
macro_rules! export_plugin {
    ($plugin_ty:ty) => {
        // Everything lives in a wrapper module so we can `#[allow(unexpected_cfgs)]`
        // the `zolo_plugin_static` cfg. That cfg is injected via RUSTFLAGS only by
        // zolo-native's static-bundle builder; the ~197 plugin crates don't declare
        // it in their Cargo.toml, so without this the default cdylib build would emit
        // an `unexpected_cfgs` lint at every `export_plugin!` call site.
        // `pub` so the static bundle can reach `__zolo_static_link_anchor` by
        // path (`zolo_plugin_<name>::__zolo_plugin_export::__zolo_static_link_anchor`).
        // Keeping the item inside this `#[allow(unexpected_cfgs)]` module is what
        // suppresses the lint for the `zolo_plugin_static` cfg — a crate-root
        // re-export would sit outside the allow and warn in every cdylib build.
        #[allow(unexpected_cfgs)]
        pub mod __zolo_plugin_export {
            // `$plugin_ty` (and `Default`, etc.) are defined in the plugin crate
            // root; bring them into scope so the impls below resolve.
            use super::*;

            // ── Shared implementations (defined once, no exported symbols) ──
            // Both the dynamic cdylib entry points and the static-link registry
            // entry call through these, so the vtable/create bodies exist once.

            pub extern "C" fn __zolo_plugin_vtable_impl() -> $crate::loader::PluginVTable {
                extern "C" fn _name(ptr: *mut u8) -> $crate::loader::PluginStr {
                    let plugin = unsafe { &*(ptr as *const $plugin_ty) };
                    let name = $crate::ZoloPlugin::name(plugin);
                    $crate::loader::PluginStr {
                        ptr: name.as_ptr(),
                        len: name.len() as u64,
                    }
                }
                extern "C" fn _version(ptr: *mut u8) -> $crate::loader::PluginStr {
                    let plugin = unsafe { &*(ptr as *const $plugin_ty) };
                    let version = $crate::ZoloPlugin::version(plugin);
                    $crate::loader::PluginStr {
                        ptr: version.as_ptr(),
                        len: version.len() as u64,
                    }
                }
                extern "C" fn _register(ptr: *mut u8, ctx: *mut $crate::PluginContext) {
                    let plugin = unsafe { &*(ptr as *const $plugin_ty) };
                    let ctx = unsafe { &mut *ctx };
                    $crate::ZoloPlugin::register(plugin, ctx);
                }
                extern "C" fn _on_init(ptr: *mut u8) {
                    let plugin = unsafe { &*(ptr as *const $plugin_ty) };
                    $crate::ZoloPlugin::on_init(plugin);
                }
                extern "C" fn _on_shutdown(ptr: *mut u8) {
                    let plugin = unsafe { &*(ptr as *const $plugin_ty) };
                    $crate::ZoloPlugin::on_shutdown(plugin);
                }
                extern "C" fn _set_runtime_bridge(ptr: *mut u8, bridge_raw: *mut u8) {
                    let plugin = unsafe { &*(ptr as *const $plugin_ty) };
                    // Reconstruct the Box<Arc<dyn RuntimeBridge>> from the thin pointer,
                    // then unbox to get the Arc.
                    let bridge: std::sync::Arc<dyn $crate::RuntimeBridge> = unsafe {
                        *Box::from_raw(bridge_raw as *mut std::sync::Arc<dyn $crate::RuntimeBridge>)
                    };
                    $crate::ZoloPlugin::set_runtime_bridge(plugin, bridge);
                }
                extern "C" fn _drop(ptr: *mut u8) {
                    unsafe { drop(Box::from_raw(ptr as *mut $plugin_ty)) };
                }
                // ABI v2: each plugin DLL exposes its own `remove_handle` so the
                // host VM can release entries in *this* plugin's statics. Each
                // dynamically loaded plugin has its own copy of
                // `zolo_plugin::handle::GLOBAL_HANDLE_TABLE`, so the host cannot
                // call the plugin's `remove_handle` directly across the FFI
                // boundary — it has to dispatch through this trampoline.
                extern "C" fn _remove_handle(id: u64) -> bool {
                    $crate::handle::remove_handle(id)
                }
                $crate::loader::PluginVTable {
                    name: _name,
                    version: _version,
                    register: _register,
                    on_init: _on_init,
                    on_shutdown: _on_shutdown,
                    set_runtime_bridge: _set_runtime_bridge,
                    drop: _drop,
                    remove_handle: _remove_handle,
                }
            }

            pub extern "C" fn __zolo_plugin_create_impl() -> *mut u8 {
                let plugin = Box::new(<$plugin_ty>::default());
                Box::into_raw(plugin) as *mut u8
            }

            // ── Dynamic cdylib build (default) ──
            // Install the shared-heap allocator shim and export the fixed-name
            // entry points that loader::load_dynamic expects. Route this plugin
            // DLL's global allocator through the single heap owned by the HOST
            // EXECUTABLE: `ZoloAlloc` lazily resolves the zolo_rt_* symbols the
            // exe exports. Every plugin and the host then allocate and free from
            // ONE process-wide heap, so the ownership the plugin ABI transfers
            // across the DLL boundary (the `String`s/`Vec`s a plugin's
            // `register()` hands the host, and the `PluginValue`s every call
            // returns) can be freed on the other side without corrupting the
            // heap. Without this each DLL has its own allocator and cross-module
            // frees crash with an access violation. See crates/zolo-alloc.
            // A plugin crate's unit-test harness is an ordinary executable,
            // not a dynamically loaded DLL and not a Zolo host. Installing
            // the forwarding allocator there aborts before the first test
            // because the harness intentionally exports no `zolo_rt_*`
            // symbols. Tests stay on their process allocator; real cdylib
            // builds retain the shared-host allocator contract unchanged.
            #[cfg(all(not(test), not(target_family = "wasm"), not(zolo_plugin_static)))]
            #[global_allocator]
            static __ZOLO_GLOBAL_ALLOC: $crate::__zolo_alloc::ZoloAlloc =
                $crate::__zolo_alloc::ZoloAlloc;

            #[cfg(not(zolo_plugin_static))]
            #[unsafe(no_mangle)]
            pub static ZOLO_PLUGIN_ABI_VERSION: u32 = $crate::loader::ZOLO_PLUGIN_ABI_VERSION;

            #[cfg(not(zolo_plugin_static))]
            #[unsafe(no_mangle)]
            pub extern "C" fn zolo_plugin_vtable() -> $crate::loader::PluginVTable {
                __zolo_plugin_vtable_impl()
            }

            #[cfg(not(zolo_plugin_static))]
            #[unsafe(no_mangle)]
            pub extern "C" fn zolo_plugin_create() -> *mut u8 {
                __zolo_plugin_create_impl()
            }

            #[cfg(not(zolo_plugin_static))]
            #[unsafe(no_mangle)]
            pub extern "C" fn zolo_plugin_declarations() -> $crate::loader::PluginStr {
                use std::sync::LazyLock;
                static DECLS: LazyLock<String> = LazyLock::new(|| {
                    let plugin = <$plugin_ty>::default();
                    let mut ctx = $crate::PluginContext::new();
                    $crate::ZoloPlugin::register(&plugin, &mut ctx);
                    ctx.to_declarations()
                });
                $crate::loader::PluginStr {
                    ptr: DECLS.as_ptr(),
                    len: DECLS.len() as u64,
                }
            }

            // ── Static-link build (`zolo build --plugins=static`, cfg injected
            // via RUSTFLAGS by zolo-native's bundle builder) ──
            // No exported symbols (they would collide across plugins in one
            // binary) and no #[global_allocator] (the host exe already carries
            // the shared zolo-alloc heap). Register in the distributed slice so
            // the runtime can instantiate this plugin via loader::load_static.
            #[cfg(all(not(target_family = "wasm"), zolo_plugin_static))]
            #[$crate::linkme::distributed_slice($crate::loader::STATIC_PLUGINS)]
            #[linkme(crate = $crate::linkme)]
            static __ZOLO_STATIC_PLUGIN_DESC: $crate::loader::StaticPluginDesc =
                $crate::loader::StaticPluginDesc {
                    vtable: __zolo_plugin_vtable_impl,
                    create: __zolo_plugin_create_impl,
                };

            // Link anchor for the static bundle. `__ZOLO_STATIC_PLUGIN_DESC` is
            // passive — it is reachable only through the linkme distributed
            // slice, so a demand-driven link of the bundle staticlib never pulls
            // this crate's registration object into the final exe and the slice
            // ends up empty. The bundle root references this fn by Rust path,
            // creating an ordinary symbol dependency that drags the descriptor
            // in. Returning the descriptor's address keeps the reference from
            // being optimized away.
            #[cfg(all(not(target_family = "wasm"), zolo_plugin_static))]
            pub fn __zolo_static_link_anchor() -> *const () {
                &__ZOLO_STATIC_PLUGIN_DESC as *const _ as *const ()
            }
        }
    };
}
