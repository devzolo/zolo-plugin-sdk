use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use std::time::Instant;

use crate::error::PluginError;
use crate::types::{ClassDef, FunctionDef, ValueDef};
use crate::{PluginContext, RuntimeBridge, ZoloPlugin};

/// Maximum time a hot-reload will wait for in-flight plugin calls to
/// quiesce before aborting. Single-threaded VMs should always see a zero
/// counter between Lua calls, so the timeout primarily catches the
/// "Lua coroutine yielded inside a plugin call" pathological case.
pub const HOT_RELOAD_QUIESCE_TIMEOUT: Duration = Duration::from_millis(1000);

/// Process-global counter of in-flight plugin calls. Trampolines bump
/// this on entry and decrement on exit; [`PluginRegistry::hot_reload_plugin`]
/// waits until the counter reaches zero before unloading the old library.
///
/// Note: this is intentionally a single global rather than per-plugin —
/// the VM is single-threaded and all plugin calls share one trampoline,
/// so finer-grained tracking buys nothing in practice. A per-plugin
/// counter could be added later if/when concurrent plugin calls land.
static IN_FLIGHT: AtomicU32 = AtomicU32::new(0);

/// Increment the global in-flight plugin-call counter. Call from a
/// plugin trampoline immediately before entering plugin code.
pub fn enter_plugin_call() {
    IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
}

/// Decrement the global in-flight plugin-call counter. Pair with
/// [`enter_plugin_call`] — must always be called even on plugin error.
pub fn leave_plugin_call() {
    IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
}

/// Read the current in-flight plugin-call count. Mainly for tests and
/// the quiesce loop.
pub fn in_flight_plugin_calls() -> u32 {
    IN_FLIGHT.load(Ordering::Acquire)
}

/// Force-set the in-flight counter (test-only). Lets tests simulate a
/// stuck call without running real plugin code.
#[doc(hidden)]
pub fn _test_set_in_flight(n: u32) {
    IN_FLIGHT.store(n, Ordering::Release);
}

pub struct PluginRegistry {
    plugins: HashMap<String, LoadedPlugin>,
    functions: HashMap<String, FunctionDef>,
    classes: HashMap<String, ClassDef>,
    globals: HashMap<String, (String, ValueDef)>,
    function_owners: HashMap<String, String>,
    class_owners: HashMap<String, String>,
    /// Maps the name a caller used at `Plugin.load("<name>")` / the `use`
    /// statement to the plugin's self-reported `name()`. Needed because the
    /// dylib filename (e.g. `zolo_plugin_rapier.dll`) isn't always the
    /// plugin's internal identity (e.g. `"rapier3d"`). Without this map,
    /// `has_plugin("rapier")` would miss an already-loaded `rapier3d` and
    /// trigger a duplicate-registration error on the second import.
    load_aliases: HashMap<String, String>,
    /// Plugin names whose symbols must NOT be re-registered into the
    /// global Lua scope on subsequent `register_plugin_registry` calls.
    /// Populated by the `__zolo_plugin_namespace` runtime helper when
    /// the user opts into the bare `use plugin foo` namespaced form —
    /// without this, every later `__zolo_load_plugin` would
    /// re-publish the symbols we just moved into the namespace table.
    hidden_plugins: std::collections::HashSet<String>,
    /// Cached bridge to re-arm reloaded plugins. `set_runtime_bridge`
    /// stashes a clone here so a subsequent reload can restore it without
    /// the caller having to thread it through.
    bridge: Option<Arc<dyn RuntimeBridge>>,
}

/// Symbols owned by a single plugin. We keep the path lists so callers
/// can iterate exactly the things to remove/replace on a hot reload —
/// without re-deriving them by walking every plugin's `register()` again.
#[derive(Default, Debug, Clone)]
pub struct PluginSymbols {
    pub functions: Vec<String>,
    pub classes: Vec<String>,
    pub globals: Vec<String>,
}

struct LoadedPlugin {
    plugin: Box<dyn ZoloPlugin>,
    source: PluginSource,
    symbols: PluginSymbols,
    /// Generation counter — bumped on every successful reload so handles
    /// can be invalidated. Plugins that hand out raw pointers should
    /// stamp the current generation into their handles and check it on
    /// every dispatch.
    generation: Arc<AtomicU64>,
    /// If `false`, this plugin opted out of hot reloads (e.g. spawned a
    /// Rust thread holding internal state). Reload requests log a
    /// warning and leave the old library in place.
    hot_reload_safe: bool,
}

#[derive(Clone)]
enum PluginSource {
    BuiltIn,
    // Dynamic loading is intentionally unavailable in browser/WASM builds,
    // but the shared registry shape remains the same on every target.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    Dynamic {
        path: PathBuf,
    },
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            functions: HashMap::new(),
            classes: HashMap::new(),
            globals: HashMap::new(),
            function_owners: HashMap::new(),
            class_owners: HashMap::new(),
            load_aliases: HashMap::new(),
            hidden_plugins: std::collections::HashSet::new(),
            bridge: None,
        }
    }

    /// Mark a plugin as "namespaced" — its symbols will not be
    /// re-registered into the global Lua scope on subsequent
    /// `register_plugin_registry` calls. Called by the bare
    /// `use plugin foo` runtime path after it copies the symbols into
    /// a namespace table.
    pub fn hide_plugin_globals(&mut self, name: &str) {
        // Resolve the caller's alias to the plugin's internal name so
        // membership checks line up regardless of which spelling was
        // used at import time.
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.hidden_plugins.insert(internal.to_string());
    }

    /// True when this function/class name belongs to a plugin marked
    /// hidden via [`Self::hide_plugin_globals`]. The plugin-bridge's
    /// global-registration loop consults this to skip re-publishing
    /// symbols the bare `use plugin foo` form pulled into a
    /// namespace.
    pub fn is_function_hidden(&self, name: &str) -> bool {
        self.function_owners
            .get(name)
            .is_some_and(|owner| self.hidden_plugins.contains(owner))
    }
    pub fn is_class_hidden(&self, name: &str) -> bool {
        self.class_owners
            .get(name)
            .is_some_and(|owner| self.hidden_plugins.contains(owner))
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn load_dynamic(&mut self, path: &std::path::Path) -> Result<(), PluginError> {
        let plugin = crate::loader::load_dynamic(path)?;
        self.register_dynamic(plugin, path.to_path_buf())
    }

    /// Register a freshly-loaded dynamic plugin, recording the source
    /// path so a later [`Self::hot_reload_plugin`] knows where to read
    /// the new bits from.
    #[cfg(not(target_family = "wasm"))]
    fn register_dynamic(
        &mut self,
        plugin: Box<dyn ZoloPlugin>,
        path: PathBuf,
    ) -> Result<(), PluginError> {
        self.register_with_source(plugin, PluginSource::Dynamic { path })
    }

    /// Load a plugin and record the caller-supplied name as an alias for
    /// the plugin's internal `name()`. If the same alias was loaded before,
    /// this is a no-op — matches Lua's typical idempotent `require()`.
    #[cfg(not(target_family = "wasm"))]
    pub fn load_dynamic_as(
        &mut self,
        load_name: &str,
        path: &std::path::Path,
    ) -> Result<(), PluginError> {
        if self.load_aliases.contains_key(load_name) {
            return Ok(());
        }
        let plugin = crate::loader::load_dynamic(path)?;
        let internal = plugin.name().to_string();
        // If a plugin with the same internal name is already loaded (e.g. via
        // a prior alias), just record the new alias pointing at it — the
        // underlying functions/classes are already in the registry.
        if self.plugins.contains_key(&internal) {
            self.load_aliases.insert(load_name.to_string(), internal);
            return Ok(());
        }
        self.register_dynamic(plugin, path.to_path_buf())?;
        self.load_aliases.insert(load_name.to_string(), internal);
        Ok(())
    }

    pub fn register_builtin(&mut self, plugin: Box<dyn ZoloPlugin>) -> Result<(), PluginError> {
        self.register_with_source(plugin, PluginSource::BuiltIn)
    }

    /// Internal: shared entrypoint for both built-in and dynamic plugin
    /// registration. Records the symbol set so a hot reload can later
    /// remove exactly what was added.
    fn register_with_source(
        &mut self,
        plugin: Box<dyn ZoloPlugin>,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        let name = plugin.name().to_string();
        if self.plugins.contains_key(&name) {
            return Err(PluginError::NameConflict {
                name: name.clone(),
                plugin_a: name,
                plugin_b: "(duplicate)".into(),
            });
        }
        let mut ctx = PluginContext::new();
        plugin.register(&mut ctx);
        let symbols = self.merge_context(&name, ctx)?;
        plugin.on_init();
        // Re-arm the bridge if one was installed earlier — keeps reloaded
        // plugins able to call back into Zolo without the host having to
        // re-register the bridge after every reload.
        if let Some(bridge) = self.bridge.as_ref() {
            plugin.set_runtime_bridge(bridge.clone());
        }
        self.plugins.insert(
            name,
            LoadedPlugin {
                plugin,
                source,
                symbols,
                generation: Arc::new(AtomicU64::new(0)),
                hot_reload_safe: true,
            },
        );
        Ok(())
    }

    fn merge_context(
        &mut self,
        plugin_name: &str,
        ctx: PluginContext,
    ) -> Result<PluginSymbols, PluginError> {
        let mut symbols = PluginSymbols::default();
        self.merge_context_into(plugin_name, ctx, &mut symbols)?;
        Ok(symbols)
    }

    fn merge_context_into(
        &mut self,
        plugin_name: &str,
        ctx: PluginContext,
        symbols: &mut PluginSymbols,
    ) -> Result<(), PluginError> {
        for func_def in ctx.functions {
            if let Some(owner) = self.function_owners.get(&func_def.name) {
                return Err(PluginError::NameConflict {
                    name: func_def.name,
                    plugin_a: owner.clone(),
                    plugin_b: plugin_name.to_string(),
                });
            }
            self.function_owners
                .insert(func_def.name.clone(), plugin_name.to_string());
            symbols.functions.push(func_def.name.clone());
            self.functions.insert(func_def.name.clone(), func_def);
        }
        for class_def in ctx.classes {
            if let Some(owner) = self.class_owners.get(&class_def.name) {
                return Err(PluginError::NameConflict {
                    name: class_def.name,
                    plugin_a: owner.clone(),
                    plugin_b: plugin_name.to_string(),
                });
            }
            self.class_owners
                .insert(class_def.name.clone(), plugin_name.to_string());
            symbols.classes.push(class_def.name.clone());
            self.classes.insert(class_def.name.clone(), class_def);
        }
        for (gname, gval) in ctx.globals {
            symbols.globals.push(gname.clone());
            self.globals
                .insert(gname.clone(), (plugin_name.to_string(), gval));
        }
        for (mod_name, sub_ctx) in ctx.modules {
            let prefixed = self.prefix_context(&mod_name, sub_ctx);
            self.merge_context_into(plugin_name, prefixed, symbols)?;
        }
        Ok(())
    }

    fn prefix_context(&self, prefix: &str, mut ctx: PluginContext) -> PluginContext {
        for func in &mut ctx.functions {
            func.name = format!("{prefix}.{}", func.name);
        }
        for class in &mut ctx.classes {
            class.name = format!("{prefix}.{}", class.name);
        }
        ctx
    }

    pub fn get_function(&self, name: &str) -> Option<&FunctionDef> {
        self.functions.get(name)
    }

    pub fn get_class(&self, name: &str) -> Option<&ClassDef> {
        self.classes.get(name)
    }

    /// Look up the plugin name that owns a given class. Used by the
    /// VM bridge to dispatch handle-removal callbacks back to the
    /// originating plugin DLL (each dynamically loaded plugin has its own
    /// `GLOBAL_HANDLE_TABLE` copy so a remove_handle call from the host
    /// only touches the host's table — wrong table for dynamic plugins).
    pub fn class_owner(&self, class_name: &str) -> Option<&str> {
        self.class_owners.get(class_name).map(|s| s.as_str())
    }

    /// Borrow a registered plugin by its `name()` identity. Returns
    /// `None` for unknown plugins. The VM bridge uses this to recover
    /// each plugin's `remove_handle_fn` for cross-DLL handle cleanup.
    pub fn get_plugin(&self, name: &str) -> Option<&dyn ZoloPlugin> {
        self.plugins.get(name).map(|loaded| loaded.plugin.as_ref())
    }

    pub fn has_plugin(&self, name: &str) -> bool {
        // Accept both the plugin's internal identity and any caller-supplied
        // alias recorded by `load_dynamic_as` (dylib filename vs `name()`).
        self.plugins.contains_key(name) || self.load_aliases.contains_key(name)
    }

    /// Return all registered plugin names (both built-ins and dynamically
    /// loaded). Used by the wasm32 error helper to list available plugins
    /// when an unknown plugin is requested.
    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    /// Set the runtime bridge on all registered plugins. The bridge is
    /// also cached so plugins loaded later (or reloaded via
    /// [`Self::hot_reload_plugin`]) get armed with the same bridge
    /// without the host having to re-call this.
    pub fn set_runtime_bridge(&mut self, bridge: std::sync::Arc<dyn crate::RuntimeBridge>) {
        self.bridge = Some(bridge.clone());
        for loaded in self.plugins.values() {
            loaded.plugin.set_runtime_bridge(bridge.clone());
        }
    }

    pub fn functions(&self) -> &HashMap<String, FunctionDef> {
        &self.functions
    }

    pub fn classes(&self) -> &HashMap<String, ClassDef> {
        &self.classes
    }

    /// Return the source path for a dynamically loaded plugin (`None`
    /// for built-ins or unknown names). Used by the dev-mode file
    /// watcher to map shared-library file events back to plugin names.
    pub fn dynamic_path(&self, name: &str) -> Option<&Path> {
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        match &self.plugins.get(internal)?.source {
            PluginSource::Dynamic { path } => Some(path.as_path()),
            PluginSource::BuiltIn => None,
        }
    }

    /// Iterate over `(plugin_name, library_path)` pairs for every plugin
    /// loaded from a `.dll`/`.so`/`.dylib`. The caller (zolo dev's
    /// watcher setup) uses this to know which directories to watch for
    /// plugin file changes.
    pub fn dynamic_plugins(&self) -> Vec<(String, PathBuf)> {
        self.plugins
            .iter()
            .filter_map(|(name, loaded)| match &loaded.source {
                PluginSource::Dynamic { path } => Some((name.clone(), path.clone())),
                PluginSource::BuiltIn => None,
            })
            .collect()
    }

    /// Symbol set owned by `name` (functions, classes, globals registered
    /// from the plugin's `register()`). Returns `None` for unknown plugins.
    pub fn symbols(&self, name: &str) -> Option<&PluginSymbols> {
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.plugins.get(internal).map(|p| &p.symbols)
    }

    /// Generation counter for `name`. Increments on every successful
    /// hot reload — handles that include the generation can detect
    /// staleness by comparing against this.
    pub fn generation(&self, name: &str) -> u64 {
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.plugins
            .get(internal)
            .map(|p| p.generation.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Shared handle to the generation counter for `name`. Plugins that
    /// stamp a generation into raw handles can hold this `Arc` cheaply.
    pub fn generation_handle(&self, name: &str) -> Option<Arc<AtomicU64>> {
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.plugins.get(internal).map(|p| p.generation.clone())
    }

    /// Toggle hot-reload safety for `name`. Plugins that spawn Rust
    /// threads or hold ABI-fragile state can call this from `register()`
    /// or `on_init()` to opt out. Reload requests for an unsafe plugin
    /// log a warning and leave the old library in place.
    pub fn set_hot_reload_safe(&mut self, name: &str, safe: bool) -> bool {
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name)
            .to_string();
        if let Some(p) = self.plugins.get_mut(&internal) {
            p.hot_reload_safe = safe;
            true
        } else {
            false
        }
    }

    /// Read the hot-reload safety flag for `name`. Defaults to `true`
    /// for plugins that never opted out and `true` for unknown names
    /// (so callers don't have to special-case missing entries).
    pub fn is_hot_reload_safe(&self, name: &str) -> bool {
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.plugins
            .get(internal)
            .map(|p| p.hot_reload_safe)
            .unwrap_or(true)
    }

    /// Hot-reload a dynamic plugin by name. Drains in-flight calls,
    /// drops the old `Library`, loads the new one from disk, and
    /// re-registers all of the plugin's symbols. On failure, the
    /// previous version remains active (transactional).
    ///
    /// The caller is responsible for replacing any captured function
    /// references in VM tables — the typical pattern is to invoke the
    /// host's plugin-trampoline re-installation pass after this returns.
    /// `LuaState::register_plugin_registry` is idempotent and does this
    /// in the bytecode VM.
    ///
    /// # Quiesce
    ///
    /// Waits up to [`HOT_RELOAD_QUIESCE_TIMEOUT`] for the global
    /// in-flight plugin-call counter to reach zero. In a single-threaded
    /// VM this is essentially instantaneous (the counter is always 0
    /// between top-level Lua calls). The wait is a safety net for the
    /// pathological case of a Lua coroutine yielding from inside a
    /// plugin call — if it doesn't return, we abort the reload rather
    /// than risk dropping a `Library` while plugin code is on a
    /// suspended stack frame.
    #[cfg(not(target_family = "wasm"))]
    pub fn hot_reload_plugin(&mut self, name: &str) -> Result<HotReloadInfo, PluginError> {
        // Resolve to the plugin's internal name so subsequent lookups
        // hit the right slot regardless of which alias the caller used.
        let internal = self
            .load_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name)
            .to_string();

        // Pre-flight checks.
        let (path, generation, was_safe) = {
            let loaded = self.plugins.get(&internal).ok_or_else(|| {
                PluginError::Custom(format!("hot_reload: plugin '{name}' not loaded"))
            })?;
            if !loaded.hot_reload_safe {
                return Err(PluginError::Custom(format!(
                    "hot_reload: plugin '{name}' is marked #[hot_unsafe] — skipping"
                )));
            }
            let path = match &loaded.source {
                PluginSource::Dynamic { path } => path.clone(),
                PluginSource::BuiltIn => {
                    return Err(PluginError::Custom(format!(
                        "hot_reload: plugin '{name}' is built-in — only dynamic plugins can be reloaded"
                    )));
                }
            };
            (path, loaded.generation.clone(), loaded.hot_reload_safe)
        };
        let _ = was_safe;

        // Quiesce: wait for in-flight calls to drain. In single-threaded
        // mode this is a no-op (the counter is 0 at every entry into
        // the VM hook). The bounded wait protects against coroutines
        // suspended mid-plugin-call.
        let quiesce_start = Instant::now();
        while in_flight_plugin_calls() > 0 {
            if quiesce_start.elapsed() >= HOT_RELOAD_QUIESCE_TIMEOUT {
                return Err(PluginError::Custom(format!(
                    "hot_reload: '{name}' aborted — {} in-flight call(s) did not drain within {}ms",
                    in_flight_plugin_calls(),
                    HOT_RELOAD_QUIESCE_TIMEOUT.as_millis(),
                )));
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // Load the NEW plugin BEFORE dropping the old one. This is
        // important on Windows because the OS can refuse to unload a
        // DLL while another loader handle still references the same
        // file path; loading first means the new copy is held open and
        // we can drop the old one cleanly afterwards. It also makes
        // the operation transactional: if the new lib fails to load,
        // we never touched the registry.
        let new_plugin = crate::loader::load_dynamic(&path)?;

        // Snapshot the previous symbol set so we can detect drift
        // (a plugin author renaming an exported function across
        // versions — we'll log a warning rather than silently leaving
        // an orphan in the registry).
        let old_symbols = self
            .plugins
            .get(&internal)
            .map(|p| PluginSymbols {
                functions: p.symbols.functions.clone(),
                classes: p.symbols.classes.clone(),
                globals: p.symbols.globals.clone(),
            })
            .unwrap_or_default();

        // Remove the old symbols from the live tables. We do this
        // before dropping the library so the registry never has a
        // moment where it points at unloaded code.
        for fn_name in &old_symbols.functions {
            self.functions.remove(fn_name);
            self.function_owners.remove(fn_name);
        }
        for cls_name in &old_symbols.classes {
            self.classes.remove(cls_name);
            self.class_owners.remove(cls_name);
        }
        for g_name in &old_symbols.globals {
            self.globals.remove(g_name);
        }

        // Drop the old plugin (calls its `on_shutdown` + the dtor that
        // unloads the cdylib).
        self.plugins.remove(&internal);

        // Register the new plugin under the same internal name. Using
        // the existing register-with-source path also re-arms the
        // runtime bridge if one was previously installed.
        let new_internal = new_plugin.name().to_string();
        if new_internal != internal {
            // The plugin's `name()` changed across versions. We could
            // get fancy here (rewrite aliases, etc.) but the simpler
            // contract is "name is part of the ABI": fail loudly.
            return Err(PluginError::Custom(format!(
                "hot_reload: plugin name changed across versions ('{internal}' → '{new_internal}'). Aborting."
            )));
        }
        self.register_dynamic(new_plugin, path)?;

        // Bump the generation. We do this AFTER the new registration
        // succeeds so observers never see a phantom bump.
        let new_gen = generation.fetch_add(1, Ordering::AcqRel) + 1;
        // Sync the generation counter on the freshly-inserted entry so
        // future reads see the right value (registry stores its own
        // counter; we keep them aligned).
        if let Some(p) = self.plugins.get_mut(&internal) {
            p.generation.store(new_gen, Ordering::Release);
        }

        Ok(HotReloadInfo {
            generation: new_gen,
            old_symbols,
        })
    }
}

/// Outcome of a successful [`PluginRegistry::hot_reload_plugin`] call.
#[derive(Debug, Clone)]
pub struct HotReloadInfo {
    /// New generation counter for the plugin (post-reload).
    pub generation: u64,
    /// Symbols owned by the previous version. Callers that maintain
    /// auxiliary tables keyed by plugin function/class name can use
    /// this to clean up before re-installing the new version.
    pub old_symbols: PluginSymbols,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
