use zolo_plugin::prelude::*;
use zolo_plugin::registry::PluginRegistry;

struct TestPlugin;

impl ZoloPlugin for TestPlugin {
    fn name(&self) -> &str {
        "test"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn register(&self, ctx: &mut PluginContext) {
        ctx.add_function(
            "greet",
            test_greet,
            FnSignature {
                params: vec![ParamDef {
                    name: "name".into(),
                    type_hint: TypeHint::String,
                }],
                returns: TypeHint::String,
            },
        );
    }
}

fn test_greet(args: &[PluginValue]) -> Result<PluginValue, PluginError> {
    let name = args[0].as_string()?;
    Ok(PluginValue::String(format!("Hello, {name}!")))
}

#[test]
fn test_register_builtin() {
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(TestPlugin)).unwrap();
    assert!(registry.get_function("greet").is_some());
}

#[test]
fn test_plugin_listed() {
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(TestPlugin)).unwrap();
    assert!(registry.has_plugin("test"));
}

#[test]
fn test_name_conflict() {
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(TestPlugin)).unwrap();
    let result = registry.register_builtin(Box::new(TestPlugin));
    assert!(result.is_err());
}

#[test]
fn test_call_registered_function() {
    let mut registry = PluginRegistry::new();
    registry.register_builtin(Box::new(TestPlugin)).unwrap();
    let func = registry.get_function("greet").unwrap();
    let result = (func.func)(&[PluginValue::String("World".into())]).unwrap();
    assert_eq!(result, PluginValue::String("Hello, World!".into()));
}
