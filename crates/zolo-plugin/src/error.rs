use std::fmt;

#[derive(Debug, Clone)]
pub enum PluginError {
    TypeError {
        expected: String,
        got: String,
        param: String,
    },
    ArityError {
        expected: usize,
        got: usize,
        function: String,
    },
    HandleClosed {
        type_name: String,
    },
    Custom(String),
    LoadError(String),
    NameConflict {
        name: String,
        plugin_a: String,
        plugin_b: String,
    },
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeError {
                expected,
                got,
                param,
            } => {
                write!(
                    f,
                    "type error: parameter '{param}' expected {expected}, got {got}"
                )
            }
            Self::ArityError {
                expected,
                got,
                function,
            } => {
                write!(
                    f,
                    "arity error: '{function}' expected {expected} args, got {got}"
                )
            }
            Self::HandleClosed { type_name } => {
                write!(f, "handle error: {type_name} handle already closed")
            }
            Self::Custom(msg) => write!(f, "plugin error: {msg}"),
            Self::LoadError(msg) => write!(f, "load error: {msg}"),
            Self::NameConflict {
                name,
                plugin_a,
                plugin_b,
            } => {
                write!(
                    f,
                    "name conflict: '{name}' registered by both '{plugin_a}' and '{plugin_b}'"
                )
            }
        }
    }
}

impl std::error::Error for PluginError {}
