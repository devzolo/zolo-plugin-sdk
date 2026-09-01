use zolo_plugin::value::PluginValue;

#[test]
fn test_type_names() {
    assert_eq!(PluginValue::Nil.type_name(), "nil");
    assert_eq!(PluginValue::Bool(true).type_name(), "bool");
    assert_eq!(PluginValue::Number(1.0).type_name(), "number");
    assert_eq!(PluginValue::Integer(1).type_name(), "integer");
    assert_eq!(PluginValue::String("hi".into()).type_name(), "string");
    assert_eq!(PluginValue::Handle(0).type_name(), "handle");
}

#[test]
// 3.14 below is a plain decimal round-trip fixture, not an approximation
// of `f64::consts::PI`.
#[allow(clippy::approx_constant)]
fn test_conversions() {
    assert_eq!(PluginValue::Number(3.14).as_f64().unwrap(), 3.14);
    assert_eq!(PluginValue::Integer(42).as_i64().unwrap(), 42);
    assert_eq!(
        PluginValue::String("hello".into()).as_string().unwrap(),
        "hello"
    );
    assert_eq!(PluginValue::Bool(true).as_bool().unwrap(), true);
    assert_eq!(PluginValue::Handle(7).as_handle_id().unwrap(), 7);
}

#[test]
fn test_type_errors() {
    assert!(PluginValue::Nil.as_f64().is_err());
    assert!(PluginValue::Number(1.0).as_string().is_err());
    assert!(PluginValue::Bool(true).as_handle_id().is_err());
}

#[test]
fn test_truthiness() {
    assert!(!PluginValue::Nil.is_truthy());
    assert!(!PluginValue::Bool(false).is_truthy());
    assert!(PluginValue::Bool(true).is_truthy());
    assert!(PluginValue::Number(0.0).is_truthy());
    assert!(PluginValue::String("".into()).is_truthy());
}

#[test]
// 3.14 below is a plain decimal round-trip fixture, not an approximation
// of `f64::consts::PI`.
#[allow(clippy::approx_constant)]
fn test_from_impls() {
    assert_eq!(PluginValue::from(3.14_f64), PluginValue::Number(3.14));
    assert_eq!(PluginValue::from(42_i64), PluginValue::Integer(42));
    assert_eq!(PluginValue::from(true), PluginValue::Bool(true));
    assert_eq!(
        PluginValue::from("hello"),
        PluginValue::String("hello".into())
    );
}

#[test]
fn test_bytes_value() {
    let bytes = PluginValue::Bytes(vec![1, 2, 3, 4]);
    assert_eq!(bytes.type_name(), "bytes");
    assert_eq!(bytes.as_bytes().unwrap(), &[1, 2, 3, 4]);
    assert!(PluginValue::Nil.as_bytes().is_err());
}

#[test]
fn test_bytes_from() {
    let val = PluginValue::from(vec![0u8, 255]);
    assert_eq!(val, PluginValue::Bytes(vec![0, 255]));
}
