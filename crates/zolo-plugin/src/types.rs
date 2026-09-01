use crate::error::PluginError;
use crate::value::PluginValue;

pub type NativePluginFn = fn(args: &[PluginValue]) -> Result<PluginValue, PluginError>;

#[derive(Debug, Clone)]
pub enum TypeHint {
    Number,
    Integer,
    String,
    Bytes,
    Bool,
    Nil,
    Handle(std::string::String),
    Table,
    Function,
    Any,
    Union(Vec<TypeHint>),
    Optional(Box<TypeHint>),
}

impl TypeHint {
    /// Convert this type hint to Zolo type syntax for `.d.zolo` files.
    ///
    /// Primitives use Zolo's canonical lowercase spelling (`int`, `float`,
    /// `str`, `bool`, `nil`, `bytes`) so generated declarations match the
    /// hand-authored prelude/std style. `Any` keeps its capital `A` (that is
    /// the spelling the core declarations use). `Table` and `Function` are
    /// opaque to the generator — it has no shape/arity info — so they fall
    /// back to the most precise *recognized* types: a string-keyed map and
    /// the dynamic `Any` respectively. Hand-authored declarations refine
    /// these to exact shapes (`[Any]`, `fn(T) -> U`, …).
    pub fn to_zolo_type(&self) -> String {
        match self {
            TypeHint::Number => "float".to_string(),
            TypeHint::Integer => "int".to_string(),
            TypeHint::String => "str".to_string(),
            TypeHint::Bytes => "bytes".to_string(),
            TypeHint::Bool => "bool".to_string(),
            TypeHint::Nil => "nil".to_string(),
            TypeHint::Handle(name) => name.clone(),
            TypeHint::Table => "{str: Any}".to_string(),
            TypeHint::Function => "Any".to_string(),
            TypeHint::Any => "Any".to_string(),
            TypeHint::Union(variants) => variants
                .iter()
                .map(TypeHint::to_zolo_type)
                .collect::<Vec<_>>()
                .join(" | "),
            TypeHint::Optional(inner) => match inner.as_ref() {
                TypeHint::Union(variants) => {
                    let mut rendered = variants
                        .iter()
                        .map(TypeHint::to_zolo_type)
                        .collect::<Vec<_>>();
                    rendered.push("nil".to_string());
                    rendered.join(" | ")
                }
                _ => format!("{}?", inner.to_zolo_type()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TypeHint;

    #[test]
    fn binary_and_union_hints_emit_precise_zolo_types() {
        assert_eq!(TypeHint::Bytes.to_zolo_type(), "bytes");
        assert_eq!(
            TypeHint::Union(vec![TypeHint::String, TypeHint::Bytes]).to_zolo_type(),
            "str | bytes"
        );
        assert_eq!(
            TypeHint::Optional(Box::new(TypeHint::Union(vec![
                TypeHint::String,
                TypeHint::Bytes,
            ])))
            .to_zolo_type(),
            "str | bytes | nil"
        );
    }
}

#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: std::string::String,
    pub type_hint: TypeHint,
}

#[derive(Debug, Clone)]
pub struct FnSignature {
    pub params: Vec<ParamDef>,
    pub returns: TypeHint,
}

#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: std::string::String,
    pub func: NativePluginFn,
    pub sig: FnSignature,
}

#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: std::string::String,
    pub constructor: Option<NativePluginFn>,
    pub constructor_sig: Option<FnSignature>,
    pub methods: Vec<(std::string::String, NativePluginFn, FnSignature)>,
    pub static_methods: Vec<(std::string::String, NativePluginFn, FnSignature)>,
    pub finalizer: Option<fn(u64)>,
}

#[derive(Debug, Clone)]
pub enum ValueDef {
    Number(f64),
    Integer(i64),
    String(std::string::String),
    Bool(bool),
    Nil,
}
