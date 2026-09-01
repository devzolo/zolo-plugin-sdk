//! Tests for `PluginRegistry::hot_reload_plugin` and the surrounding
//! quiesce / generation / opt-out machinery.
//!
//! Building and dropping real cdylib files inside a unit test is
//! flaky on Windows (the OS holds the DLL open even after `Library`
//! drops), so the bulk of the coverage targets the parts that don't
//! depend on real `libloading::Library` behaviour: the in-flight
//! counter, the registry's hot_reload_safe flag, and the
//! "built-in plugins can't be reloaded" guardrail.
//!
//! A separate end-to-end test that builds two versions of a real plugin
//! and swaps them lives behind the `cdylib-tests` cfg gate (off by
//! default) — see `test_plugin_reload_basic_cdylib`.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use zolo_plugin::prelude::*;
use zolo_plugin::registry::{
    _test_set_in_flight, HOT_RELOAD_QUIESCE_TIMEOUT, PluginRegistry, enter_plugin_call,
    in_flight_plugin_calls, leave_plugin_call,
};

/// Serialise tests that touch the global in-flight counter. `cargo test`
/// runs tests in parallel by default, and several tests here call
/// `_test_set_in_flight` — they must not race or one test's reset will
/// invalidate another's setup.
fn lock_counter() -> MutexGuard<'static, ()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    let mu = M.get_or_init(|| Mutex::new(()));
    // Recover from poisoning: the only way the lock gets poisoned is a
    // panic INSIDE another test, and we want subsequent tests to still
    // run (they would otherwise all fail on `unwrap`).
    mu.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct GreeterV1;

impl ZoloPlugin for GreeterV1 {
    fn name(&self) -> &str {
        "greeter_test"
    }
    fn version(&self) -> &str {
        "1.0.0"
    }
    fn register(&self, ctx: &mut PluginContext) {
        ctx.add_function(
            "greet",
            greet_v1,
            FnSignature {
                params: vec![],
                returns: TypeHint::String,
            },
        );
        ctx.add_function(
            "hello",
            hello_v1,
            FnSignature {
                params: vec![],
                returns: TypeHint::String,
            },
        );
    }
}

fn greet_v1(_args: &[PluginValue]) -> Result<PluginValue, PluginError> {
    Ok(PluginValue::String("v1".into()))
}

fn hello_v1(_args: &[PluginValue]) -> Result<PluginValue, PluginError> {
    Ok(PluginValue::String("hello-v1".into()))
}

/// In-flight counter starts at zero and tracks enter/leave correctly.
#[test]
fn in_flight_counter_tracks_calls() {
    let _g = lock_counter();
    // Reset for isolation — other tests may have run in the same process.
    _test_set_in_flight(0);
    assert_eq!(in_flight_plugin_calls(), 0);
    enter_plugin_call();
    assert_eq!(in_flight_plugin_calls(), 1);
    enter_plugin_call();
    assert_eq!(in_flight_plugin_calls(), 2);
    leave_plugin_call();
    leave_plugin_call();
    assert_eq!(in_flight_plugin_calls(), 0);
}

/// Built-in plugins cannot be hot-reloaded (no source path on disk).
#[test]
fn hot_reload_rejects_built_in_plugins() {
    let _g = lock_counter();
    _test_set_in_flight(0);
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(GreeterV1)).unwrap();

    let err = registry
        .hot_reload_plugin("greeter_test")
        .expect_err("built-in plugins should not be reloadable");
    let msg = format!("{err}");
    assert!(
        msg.contains("built-in"),
        "expected 'built-in' in error message, got: {msg}"
    );
}

/// Unknown plugin names return an error, not a panic.
#[test]
fn hot_reload_rejects_unknown_plugin() {
    let _g = lock_counter();
    _test_set_in_flight(0);
    let mut registry = PluginRegistry::new();
    let err = registry
        .hot_reload_plugin("does-not-exist")
        .expect_err("unknown plugin should error");
    assert!(format!("{err}").contains("not loaded"));
}

/// Marking a plugin `#[hot_unsafe]` (via `set_hot_reload_safe(false)`)
/// causes hot-reload to refuse the swap.
#[test]
fn hot_reload_skips_unsafe_plugins() {
    let _g = lock_counter();
    _test_set_in_flight(0);
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(GreeterV1)).unwrap();
    assert!(registry.set_hot_reload_safe("greeter_test", false));
    assert!(!registry.is_hot_reload_safe("greeter_test"));

    let err = registry
        .hot_reload_plugin("greeter_test")
        .expect_err("unsafe plugin should not be reloadable");
    assert!(format!("{err}").contains("hot_unsafe"));

    // Toggling back enables it again.
    registry.set_hot_reload_safe("greeter_test", true);
    assert!(registry.is_hot_reload_safe("greeter_test"));
}

/// Stuck in-flight calls force the quiesce loop to bail at the timeout
/// rather than hanging forever. We don't drive a real
/// `hot_reload_plugin` here (the built-in guard rejects it before
/// quiesce kicks in) — instead we replicate the inner loop and check
/// the bookkeeping is sound.
#[test]
fn hot_reload_aborts_when_in_flight_does_not_drain() {
    let _g = lock_counter();
    _test_set_in_flight(0);
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(GreeterV1)).unwrap();

    // Sanity: the production timeout is documented as ~1s. We don't
    // actually want to wait the full second here — instead we use a
    // short local timeout so the test stays fast.
    let local_timeout = Duration::from_millis(50);
    _test_set_in_flight(3);
    let start = Instant::now();
    while in_flight_plugin_calls() > 0 && start.elapsed() < local_timeout {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        start.elapsed() >= local_timeout,
        "quiesce loop bailed before timeout; counter is {} after {:?}",
        in_flight_plugin_calls(),
        start.elapsed(),
    );
    assert_eq!(
        in_flight_plugin_calls(),
        3,
        "the counter should be untouched (no real plugin call happened)"
    );
    // Production constant is sane.
    assert!(HOT_RELOAD_QUIESCE_TIMEOUT >= Duration::from_millis(100));
    _test_set_in_flight(0);
}

/// Generation handles are shared `Arc`s, so callers can hold a cheap
/// reference and observe future bumps without re-asking the registry.
#[test]
fn generation_handle_is_shared() {
    let _g = lock_counter();
    _test_set_in_flight(0);
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(GreeterV1)).unwrap();

    let handle = registry
        .generation_handle("greeter_test")
        .expect("plugin should be registered");
    assert_eq!(handle.load(Ordering::Acquire), 0);
    assert_eq!(registry.generation("greeter_test"), 0);

    // Manually bump (mirrors what hot_reload would do on a real
    // dynamic plugin). The handle observes the change.
    handle.fetch_add(1, Ordering::AcqRel);
    assert_eq!(handle.load(Ordering::Acquire), 1);
}

/// `symbols(name)` returns exactly the functions/classes/globals that
/// the plugin's `register()` added.
#[test]
fn symbols_records_registered_names() {
    let _g = lock_counter();
    _test_set_in_flight(0);
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(GreeterV1)).unwrap();
    let symbols = registry.symbols("greeter_test").expect("plugin loaded");
    let mut fns = symbols.functions.clone();
    fns.sort();
    assert_eq!(fns, vec!["greet".to_string(), "hello".to_string()]);
    assert!(symbols.classes.is_empty());
    assert!(symbols.globals.is_empty());
}

/// `dynamic_plugins()` and `dynamic_path()` return nothing for built-ins,
/// since hot-reload only applies to plugins loaded from a `.dll`/`.so`/`.dylib`.
#[test]
fn dynamic_path_returns_none_for_builtin() {
    let _g = lock_counter();
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(GreeterV1)).unwrap();
    assert!(registry.dynamic_path("greeter_test").is_none());
    assert!(registry.dynamic_plugins().is_empty());
}
