use crate::PluginError;

/// Field names used by the ABI v2 tagged-handle envelope.
///
/// A tagged handle deliberately remains a `PluginValue::Table` instead of a
/// new enum variant so plugins compiled against ABI v2 can exchange typed
/// handles across dynamic-library boundaries without changing the wire enum.
pub const TAGGED_HANDLE_ID_FIELD: &str = "__handle";
pub const TAGGED_HANDLE_CLASS_FIELD: &str = "__class";

#[derive(Debug, Clone, PartialEq)]
pub enum PluginValue {
    Nil,
    Bool(bool),
    Number(f64),
    Integer(i64),
    String(String),
    Handle(u64),
    Table(Vec<(PluginValue, PluginValue)>),
    Function(u64),
    Bytes(Vec<u8>),
}

impl PluginValue {
    /// Build the canonical ABI v2 envelope for a typed plugin handle.
    ///
    /// The `u64` is stored bit-for-bit in the existing signed `Integer` wire
    /// slot. `as_tagged_handle` reverses the cast, including ids above
    /// `i64::MAX`; no floating-point conversion is involved.
    pub fn tagged_handle(id: u64, class: impl Into<String>) -> Self {
        Self::Table(vec![
            (
                Self::String(TAGGED_HANDLE_ID_FIELD.into()),
                Self::Integer(id as i64),
            ),
            (
                Self::String(TAGGED_HANDLE_CLASS_FIELD.into()),
                Self::String(class.into()),
            ),
        ])
    }

    /// Decode the canonical ABI v2 `{ __handle, __class }` envelope.
    ///
    /// Field order is irrelevant and unrelated fields are allowed, matching
    /// how both the VM and native bridges treat instance wrapper tables.
    pub fn as_tagged_handle(&self) -> Option<(u64, &str)> {
        let Self::Table(entries) = self else {
            return None;
        };
        let id = entries.iter().find_map(|(key, value)| match (key, value) {
            (Self::String(key), Self::Integer(id)) if key == TAGGED_HANDLE_ID_FIELD => {
                Some(*id as u64)
            }
            _ => None,
        })?;
        let class = entries.iter().find_map(|(key, value)| match (key, value) {
            (Self::String(key), Self::String(class)) if key == TAGGED_HANDLE_CLASS_FIELD => {
                Some(class.as_str())
            }
            _ => None,
        })?;
        Some((id, class))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Bool(_) => "bool",
            Self::Number(_) => "number",
            Self::Integer(_) => "integer",
            Self::String(_) => "string",
            Self::Handle(_) => "handle",
            Self::Table(_) => "table",
            Self::Function(_) => "function",
            Self::Bytes(_) => "bytes",
        }
    }

    pub fn as_f64(&self) -> Result<f64, PluginError> {
        match self {
            Self::Number(n) => Ok(*n),
            Self::Integer(n) => Ok(*n as f64),
            other => Err(PluginError::TypeError {
                expected: "number".into(),
                got: other.type_name().into(),
                param: String::new(),
            }),
        }
    }

    pub fn as_i64(&self) -> Result<i64, PluginError> {
        match self {
            Self::Integer(n) => Ok(*n),
            Self::Number(n) => Ok(*n as i64),
            other => Err(PluginError::TypeError {
                expected: "integer".into(),
                got: other.type_name().into(),
                param: String::new(),
            }),
        }
    }

    pub fn as_string(&self) -> Result<&str, PluginError> {
        match self {
            Self::String(s) => Ok(s),
            other => Err(PluginError::TypeError {
                expected: "string".into(),
                got: other.type_name().into(),
                param: String::new(),
            }),
        }
    }

    pub fn as_bool(&self) -> Result<bool, PluginError> {
        match self {
            Self::Bool(b) => Ok(*b),
            other => Err(PluginError::TypeError {
                expected: "bool".into(),
                got: other.type_name().into(),
                param: String::new(),
            }),
        }
    }

    pub fn as_handle_id(&self) -> Result<u64, PluginError> {
        match self {
            Self::Handle(id) => Ok(*id),
            // Handles are pushed as integers through the VM bridge,
            // so accept integers as handle IDs too. Some VM paths
            // coerce small integers to Number (f64) — accept that as
            // well; u64 round-trips losslessly through f64 up to 2^53.
            Self::Integer(n) => Ok(*n as u64),
            Self::Number(n) => Ok(*n as u64),
            other => {
                other
                    .as_tagged_handle()
                    .map(|(id, _)| id)
                    .ok_or_else(|| PluginError::TypeError {
                        expected: "handle".into(),
                        got: other.type_name().into(),
                        param: String::new(),
                    })
            }
        }
    }

    pub fn as_table(&self) -> Result<&Vec<(PluginValue, PluginValue)>, PluginError> {
        match self {
            Self::Table(t) => Ok(t),
            other => Err(PluginError::TypeError {
                expected: "table".into(),
                got: other.type_name().into(),
                param: String::new(),
            }),
        }
    }

    pub fn as_bytes(&self) -> Result<&[u8], PluginError> {
        match self {
            Self::Bytes(b) => Ok(b),
            other => Err(PluginError::TypeError {
                expected: "bytes".into(),
                got: other.type_name().into(),
                param: String::new(),
            }),
        }
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Self::Nil | Self::Bool(false))
    }
}

impl From<f64> for PluginValue {
    fn from(v: f64) -> Self {
        Self::Number(v)
    }
}
impl From<i64> for PluginValue {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}
impl From<bool> for PluginValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<String> for PluginValue {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}
impl From<&str> for PluginValue {
    fn from(v: &str) -> Self {
        Self::String(v.to_string())
    }
}
impl From<Vec<u8>> for PluginValue {
    fn from(v: Vec<u8>) -> Self {
        Self::Bytes(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_handle_roundtrips_all_u64_bits() {
        let value = PluginValue::tagged_handle(u64::MAX, "OpaqueResource");
        assert_eq!(value.as_tagged_handle(), Some((u64::MAX, "OpaqueResource")));
        assert_eq!(value.as_handle_id().unwrap(), u64::MAX);
    }

    #[test]
    fn tagged_handle_fields_are_order_independent() {
        let value = PluginValue::Table(vec![
            (PluginValue::String("extra".into()), PluginValue::Bool(true)),
            (
                PluginValue::String(TAGGED_HANDLE_CLASS_FIELD.into()),
                PluginValue::String("GpuShader".into()),
            ),
            (
                PluginValue::String(TAGGED_HANDLE_ID_FIELD.into()),
                PluginValue::Integer(42),
            ),
        ]);
        assert_eq!(value.as_tagged_handle(), Some((42, "GpuShader")));
    }

    #[test]
    fn partial_envelopes_are_plain_tables() {
        let missing_class = PluginValue::Table(vec![(
            PluginValue::String(TAGGED_HANDLE_ID_FIELD.into()),
            PluginValue::Integer(7),
        )]);
        assert_eq!(missing_class.as_tagged_handle(), None);
    }
}
