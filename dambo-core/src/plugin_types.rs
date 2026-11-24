use http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt;

/// Maximum byte length for `namespace` and `key` identifiers (and `topic` names).
pub const MAX_NS_KEY_LEN: usize = 128;

/// Maximum serialized JSON payload byte length for a single KV entry (`256 KB`).
pub const MAX_VALUE_BYTES: usize = 256 * 1024;

/// Maximum entry count per `namedomain` within one store (`persist` or `memory`).
pub const MAX_KEYS_PER_STORE: usize = 10_000;

/// Maximum total serialized byte count per `namedomain` within one store (`16 MB`).
pub const MAX_BYTES_PER_STORE: usize = 16 * 1024 * 1024;

/// Maximum recursion depth for nested `list`/`struct` schemas and values.
pub const MAX_SCHEMA_DEPTH: usize = 16;

/// Unified error returned by Layer 1/Layer 2 platform operations and serialized as `{ "error": ... }`.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl PluginApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", message)
    }

    pub fn invalid_data_type(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "INVALID_DATA_TYPE", message)
    }

    pub fn type_mismatch(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "TYPE_MISMATCH", message)
    }

    pub fn invalid_enum_value(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "INVALID_ENUM_VALUE", message)
    }

    pub fn out_of_range(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "OUT_OF_RANGE", message)
    }

    pub fn struct_field_invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "STRUCT_FIELD_INVALID", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", message)
    }

    pub fn acl_denied(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "ACL_DENIED", message)
    }

    pub fn schema_locked(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "SCHEMA_LOCKED", message)
    }

    pub fn forbidden_origin(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "FORBIDDEN_ORIGIN", message)
    }

    pub fn plugin_not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "PLUGIN_NOT_FOUND", message)
    }

    pub fn key_not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "KEY_NOT_FOUND", message)
    }

    pub fn client_not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "CLIENT_NOT_FOUND", message)
    }

    pub fn quota_exceeded(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "QUOTA_EXCEEDED", message)
    }

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_REQUESTS", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", message)
    }

    /// Serialize into the canonical Layer 1 JSON error envelope.
    pub fn to_json_envelope(&self) -> serde_json::Value {
        let mut inner = serde_json::Map::new();
        inner.insert(
            "code".to_string(),
            serde_json::Value::String(self.code.to_string()),
        );
        inner.insert(
            "message".to_string(),
            serde_json::Value::String(self.message.clone()),
        );
        if let Some(ref details) = self.details {
            inner.insert("details".to_string(), details.clone());
        }
        serde_json::json!({ "error": serde_json::Value::Object(inner) })
    }
}

impl fmt::Display for PluginApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.code,
            self.status.as_u16(),
            self.message
        )
    }
}

impl std::error::Error for PluginApiError {}

/// Validates a `namespace` or `topic` identifier (`[A-Za-z0-9_.-]`, 1..=128 bytes, non-dot-segment).
pub fn is_valid_ns_or_topic(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NS_KEY_LEN
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Validates a data `namespace` name (excludes `*`, `:`, `/`, and control chars).
pub fn is_valid_namespace(ns: &str) -> bool {
    is_valid_ns_or_topic(ns)
}

/// Validates a plugin-owned `topic` name (same rules as namespace).
pub fn is_valid_topic_name(topic: &str) -> bool {
    is_valid_ns_or_topic(topic)
}

/// Validates a KV `key` (non-empty, `<= 128` bytes, no control chars, `\0`, `/`, or `.`/`..`).
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_NS_KEY_LEN
        && key != "."
        && key != ".."
        && !key.chars().any(|c| c.is_control() || c == '\0' || c == '/')
}

/// Normalized target category inside an ACL resource string (`persist:<ns>`, `memory:<ns>`, `topic:<name>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AclResourceKind {
    Persist,
    Memory,
    Topic,
}

impl AclResourceKind {
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Persist => "persist",
            Self::Memory => "memory",
            Self::Topic => "topic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParsedAclResource {
    pub kind: AclResourceKind,
    /// Target namespace/topic name or `"*"` for wildcard.
    pub scope: String,
}

impl ParsedAclResource {
    pub fn is_wildcard(&self) -> bool {
        self.scope == "*"
    }

    pub fn is_persist_config(&self) -> bool {
        self.kind == AclResourceKind::Persist && self.scope == "config"
    }

    pub fn canonical(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.scope)
    }

    pub fn wildcard(&self) -> String {
        format!("{}:*", self.kind.prefix())
    }
}

/// Parse and validate an ACL resource specifier (`persist:<ns|*>`, `memory:<ns|*>`, `topic:<name|*>`).
pub fn parse_acl_resource(raw: &str) -> Result<ParsedAclResource, PluginApiError> {
    let trimmed = raw.trim();
    let Some((prefix, scope)) = trimmed.split_once(':') else {
        return Err(PluginApiError::invalid_argument(format!(
            "Invalid ACL resource '{}': expected '<persist|memory|topic>:<scope>'",
            raw
        )));
    };
    let kind = match prefix {
        "persist" => AclResourceKind::Persist,
        "memory" => AclResourceKind::Memory,
        "topic" => AclResourceKind::Topic,
        _ => {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid ACL resource prefix '{}' in '{}'",
                prefix, raw
            )));
        }
    };
    if scope != "*" && !is_valid_ns_or_topic(scope) {
        return Err(PluginApiError::invalid_argument(format!(
            "Invalid ACL scope '{}' in '{}'",
            scope, raw
        )));
    }
    Ok(ParsedAclResource {
        kind,
        scope: scope.to_string(),
    })
}

/// Six orthogonal JSON-compatible data types supported by Dambo storage & memory.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    Number,
    String,
    Bool,
    Enum,
    List,
    Struct,
}

impl DataType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::String => "string",
            Self::Bool => "bool",
            Self::Enum => "enum",
            Self::List => "list",
            Self::Struct => "struct",
        }
    }
}

/// Option definition inside `DataMeta.options` for `enum` entries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum EnumOption {
    Plain(String),
    Labeled {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl EnumOption {
    pub fn value(&self) -> &str {
        match self {
            Self::Plain(v) => v.as_str(),
            Self::Labeled { value, .. } => value.as_str(),
        }
    }
}

/// Element schema for typed `list` entries (`meta.item`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ItemSchema {
    #[serde(rename = "type")]
    pub data_type: DataType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Box<DataMeta>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
}

/// Member field schema for typed `struct` entries (`meta.fields`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub data_type: DataType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Box<DataMeta>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Validation & fallback UI metadata attached to a typed entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DataMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<EnumOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Box<ItemSchema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, FieldSchema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Self-describing typed storage envelope `{ type, value, meta? }`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TypedEntry {
    #[serde(rename = "type")]
    pub data_type: DataType,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<DataMeta>,
}

/// Write payload accepted by `PUT .../keys/{key}` and `POST .../mset` entries.
/// If `data_type` and `meta` are both `None`, it operates in value-only `set` mode;
/// otherwise it operates in explicit schema `define` mode.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WriteEntryInput {
    #[serde(rename = "type", default)]
    pub data_type: Option<DataType>,
    pub value: serde_json::Value,
    #[serde(default)]
    pub meta: Option<DataMeta>,
}

impl WriteEntryInput {
    pub fn is_define_mode(&self) -> bool {
        self.data_type.is_some() || self.meta.is_some()
    }
}

/// Infer the platform `DataType` from a raw JSON value (rejecting top-level `null`).
pub fn infer_data_type(value: &serde_json::Value) -> Result<DataType, PluginApiError> {
    match value {
        serde_json::Value::Null => Err(PluginApiError::invalid_data_type(
            "Top-level null values are not allowed; use DELETE to clear a key",
        )),
        serde_json::Value::Number(n) => {
            if n.as_f64().is_some_and(f64::is_finite) {
                Ok(DataType::Number)
            } else {
                Err(PluginApiError::invalid_data_type(
                    "Non-finite numeric values are not supported",
                ))
            }
        }
        serde_json::Value::String(_) => Ok(DataType::String),
        serde_json::Value::Bool(_) => Ok(DataType::Bool),
        serde_json::Value::Array(_) => Ok(DataType::List),
        serde_json::Value::Object(_) => Ok(DataType::Struct),
    }
}

/// Validate that `(data_type, meta)` forms a coherent schema definition.
pub fn validate_schema_definition(
    data_type: DataType,
    meta: Option<&DataMeta>,
    depth: usize,
) -> Result<(), PluginApiError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(PluginApiError::invalid_argument(format!(
            "Schema nesting exceeds maximum depth of {}",
            MAX_SCHEMA_DEPTH
        )));
    }

    match data_type {
        DataType::Number => {
            if let Some(m) = meta {
                for (name, v) in [("min", m.min), ("max", m.max), ("step", m.step)] {
                    if let Some(num) = v
                        && !num.is_finite()
                    {
                        return Err(PluginApiError::invalid_argument(format!(
                            "Meta '{}' must be a finite number",
                            name
                        )));
                    }
                }
                if let (Some(min), Some(max)) = (m.min, m.max)
                    && min > max
                {
                    return Err(PluginApiError::invalid_argument(format!(
                        "Invalid number bounds: min ({}) > max ({})",
                        min, max
                    )));
                }
                if let Some(step) = m.step
                    && step <= 0.0
                {
                    return Err(PluginApiError::invalid_argument(
                        "Meta 'step' must be greater than 0",
                    ));
                }
                if let Some(ref fmt) = m.format
                    && fmt != "integer"
                    && fmt != "slider"
                {
                    return Err(PluginApiError::invalid_argument(format!(
                        "Unsupported number format '{}': expected 'integer' or 'slider'",
                        fmt
                    )));
                }
            }
        }
        DataType::String => {
            if let Some(m) = meta
                && let Some(ref fmt) = m.format
                && !matches!(fmt.as_str(), "color" | "textarea" | "url")
            {
                return Err(PluginApiError::invalid_argument(format!(
                    "Unsupported string format '{}': expected 'color', 'textarea', or 'url'",
                    fmt
                )));
            }
        }
        DataType::Bool => {}
        DataType::Enum => {
            let opts = meta.and_then(|m| m.options.as_ref()).ok_or_else(|| {
                PluginApiError::invalid_enum_value("Enum type requires non-empty meta.options")
            })?;
            if opts.is_empty() {
                return Err(PluginApiError::invalid_enum_value(
                    "Enum meta.options must contain at least one choice",
                ));
            }
            let mut seen = HashSet::new();
            for opt in opts {
                let v = opt.value();
                if v.is_empty() {
                    return Err(PluginApiError::invalid_enum_value(
                        "Enum option value must not be empty",
                    ));
                }
                if !seen.insert(v) {
                    return Err(PluginApiError::invalid_enum_value(format!(
                        "Duplicate enum option value '{}'",
                        v
                    )));
                }
            }
        }
        DataType::List => {
            if let Some(m) = meta
                && let Some(ref item) = m.item
            {
                validate_schema_definition(item.data_type, item.meta.as_deref(), depth + 1)?;
            }
        }
        DataType::Struct => {
            if let Some(m) = meta
                && let Some(ref fields) = m.fields
            {
                for (fname, fschema) in fields {
                    if fname.is_empty() {
                        return Err(PluginApiError::invalid_argument(
                            "Struct field name must not be empty",
                        ));
                    }
                    validate_schema_definition(
                        fschema.data_type,
                        fschema.meta.as_deref(),
                        depth + 1,
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn validate_url_string(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || s.starts_with('/')
}

fn is_integral_number(n: &serde_json::Number, as_f64: f64) -> bool {
    n.as_i64().is_some() || n.as_u64().is_some() || (as_f64.is_finite() && as_f64.fract() == 0.0)
}

/// Validate a JSON value against a declared `(data_type, meta)` schema.
pub fn validate_value_against(
    data_type: DataType,
    meta: Option<&DataMeta>,
    value: &serde_json::Value,
    depth: usize,
    path: &str,
) -> Result<(), PluginApiError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(PluginApiError::invalid_argument(format!(
            "Value at '{}' exceeds maximum nesting depth of {}",
            path, MAX_SCHEMA_DEPTH
        )));
    }
    if value.is_null() {
        return Err(PluginApiError::invalid_data_type(format!(
            "Value at '{}' cannot be null",
            path
        )));
    }

    match data_type {
        DataType::Number => {
            let serde_json::Value::Number(n) = value else {
                return Err(PluginApiError::type_mismatch(format!(
                    "Expected 'number' at '{}', got {}",
                    path,
                    json_kind_name(value)
                )));
            };
            let Some(num) = n.as_f64().filter(|v| v.is_finite()) else {
                return Err(PluginApiError::type_mismatch(format!(
                    "Non-finite number at '{}'",
                    path
                )));
            };
            if let Some(m) = meta {
                if m.format.as_deref() == Some("integer") && !is_integral_number(n, num) {
                    return Err(PluginApiError::out_of_range(format!(
                        "Value {} at '{}' must be an integer",
                        num, path
                    )));
                }
                if let Some(min) = m.min
                    && num < min
                {
                    return Err(PluginApiError::out_of_range(format!(
                        "Value {} at '{}' is less than minimum {}",
                        num, path, min
                    )));
                }
                if let Some(max) = m.max
                    && num > max
                {
                    return Err(PluginApiError::out_of_range(format!(
                        "Value {} at '{}' exceeds maximum {}",
                        num, path, max
                    )));
                }
            }
        }
        DataType::String => {
            let serde_json::Value::String(s) = value else {
                return Err(PluginApiError::type_mismatch(format!(
                    "Expected 'string' at '{}', got {}",
                    path,
                    json_kind_name(value)
                )));
            };
            if let Some(m) = meta {
                if let Some(max_len) = m.max_length {
                    let count = s.chars().count();
                    if count > max_len {
                        return Err(PluginApiError::out_of_range(format!(
                            "String length {} at '{}' exceeds max_length {}",
                            count, path, max_len
                        )));
                    }
                }
                if m.format.as_deref() == Some("url") && !validate_url_string(s) {
                    return Err(PluginApiError::invalid_argument(format!(
                        "String at '{}' must be an http(s):// URL or root-relative '/' path",
                        path
                    )));
                }
            }
        }
        DataType::Bool => {
            if !value.is_boolean() {
                return Err(PluginApiError::type_mismatch(format!(
                    "Expected 'bool' at '{}', got {}",
                    path,
                    json_kind_name(value)
                )));
            }
        }
        DataType::Enum => {
            let opts = meta
                .and_then(|m| m.options.as_ref())
                .filter(|o| !o.is_empty())
                .ok_or_else(|| {
                    PluginApiError::invalid_enum_value("Enum entry is missing meta.options")
                })?;
            let serde_json::Value::String(s) = value else {
                return Err(PluginApiError::type_mismatch(format!(
                    "Expected string for 'enum' at '{}', got {}",
                    path,
                    json_kind_name(value)
                )));
            };
            if !opts.iter().any(|opt| opt.value() == s) {
                return Err(PluginApiError::invalid_enum_value(format!(
                    "Value '{}' at '{}' is not in allowed enum options",
                    s, path
                ))
                .with_details(serde_json::json!({
                    "allowed": opts.iter().map(|o| o.value()).collect::<Vec<_>>()
                })));
            }
        }
        DataType::List => {
            let serde_json::Value::Array(items) = value else {
                return Err(PluginApiError::type_mismatch(format!(
                    "Expected 'list' at '{}', got {}",
                    path,
                    json_kind_name(value)
                )));
            };
            if let Some(m) = meta {
                if let Some(max_items) = m.max_items
                    && items.len() > max_items
                {
                    return Err(PluginApiError::out_of_range(format!(
                        "List length {} at '{}' exceeds max_items {}",
                        items.len(),
                        path,
                        max_items
                    )));
                }
                if let Some(ref item_schema) = m.item {
                    for (idx, elem) in items.iter().enumerate() {
                        let elem_path = format!("{}[{}]", path, idx);
                        if elem.is_null() {
                            if item_schema.nullable {
                                continue;
                            }
                            return Err(PluginApiError::type_mismatch(format!(
                                "List element at '{}' cannot be null",
                                elem_path
                            )));
                        }
                        validate_value_against(
                            item_schema.data_type,
                            item_schema.meta.as_deref(),
                            elem,
                            depth + 1,
                            &elem_path,
                        )?;
                    }
                }
            }
        }
        DataType::Struct => {
            let serde_json::Value::Object(map) = value else {
                return Err(PluginApiError::type_mismatch(format!(
                    "Expected 'struct' object at '{}', got {}",
                    path,
                    json_kind_name(value)
                )));
            };
            if let Some(m) = meta
                && let Some(ref fields) = m.fields
            {
                for (fname, fschema) in fields {
                    let child_path = format!("{}.{}", path, fname);
                    match map.get(fname) {
                        None => {
                            if !fschema.optional {
                                return Err(PluginApiError::struct_field_invalid(format!(
                                    "Missing required struct field '{}'",
                                    child_path
                                )));
                            }
                        }
                        Some(serde_json::Value::Null) => {
                            if !fschema.nullable && !fschema.optional {
                                return Err(PluginApiError::struct_field_invalid(format!(
                                    "Struct field '{}' cannot be null",
                                    child_path
                                )));
                            }
                        }
                        Some(field_val) => {
                            if let Err(err) = validate_value_against(
                                fschema.data_type,
                                fschema.meta.as_deref(),
                                field_val,
                                depth + 1,
                                &child_path,
                            ) {
                                return Err(PluginApiError::struct_field_invalid(format!(
                                    "Invalid struct field '{}': {}",
                                    child_path, err.message
                                ))
                                .with_details(serde_json::json!({
                                    "field": child_path,
                                    "cause_code": err.code
                                })));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn json_kind_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Validate and serialize size check helper returning serialized UTF-8 byte count.
pub fn serialize_and_check_entry_size(entry: &TypedEntry) -> Result<Vec<u8>, PluginApiError> {
    let bytes = serde_json::to_vec(entry)
        .map_err(|e| PluginApiError::internal(format!("Failed to serialize entry: {}", e)))?;
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(PluginApiError::quota_exceeded(format!(
            "Serialized entry size {} bytes exceeds maximum {} bytes",
            bytes.len(),
            MAX_VALUE_BYTES
        )));
    }
    Ok(bytes)
}

/// Parse a single default entry declared under `plugin.json` `"defaults"`.
/// Accepts either full Typed Entry objects (`{ type, value, meta? }`) or raw JSON literal shorthand.
pub fn parse_default_entry(raw: &serde_json::Value) -> Result<TypedEntry, PluginApiError> {
    if let serde_json::Value::Object(map) = raw {
        let only_envelope_keys = !map.is_empty()
            && map
                .keys()
                .all(|k| k == "type" || k == "value" || k == "meta");
        if only_envelope_keys
            && let Some(raw_type) = map.get("type")
            && serde_json::from_value::<DataType>(raw_type.clone()).is_ok()
        {
            if !map.contains_key("value") {
                return Err(PluginApiError::invalid_argument(
                    "Default typed entry must include 'value'",
                ));
            }
            let entry: TypedEntry = serde_json::from_value(raw.clone()).map_err(|e| {
                PluginApiError::invalid_argument(format!("Invalid default typed entry: {}", e))
            })?;
            validate_schema_definition(entry.data_type, entry.meta.as_ref(), 0)?;
            validate_value_against(entry.data_type, entry.meta.as_ref(), &entry.value, 0, "$")?;
            serialize_and_check_entry_size(&entry)?;
            return Ok(entry);
        }
    }

    let inferred = infer_data_type(raw)?;
    let entry = TypedEntry {
        data_type: inferred,
        value: raw.clone(),
        meta: None,
    };
    validate_value_against(entry.data_type, None, &entry.value, 0, "$")?;
    serialize_and_check_entry_size(&entry)?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_identifier_and_acl_resource_validation() {
        assert!(is_valid_namespace("scores"));
        assert!(is_valid_namespace("game24.v1_scores-top"));
        assert!(!is_valid_namespace(""));
        assert!(!is_valid_namespace("."));
        assert!(!is_valid_namespace(".."));
        assert!(!is_valid_namespace("*"));
        assert!(!is_valid_namespace("a:b"));
        assert!(!is_valid_namespace("a/b"));

        assert!(is_valid_key("uid:123"));
        assert!(is_valid_key("你好_世界"));
        assert!(!is_valid_key(""));
        assert!(!is_valid_key(".."));
        assert!(!is_valid_key("a/b"));
        assert!(!is_valid_key("bad\0key"));

        let res = parse_acl_resource("persist:scores").unwrap();
        assert_eq!(res.kind, AclResourceKind::Persist);
        assert_eq!(res.scope, "scores");
        assert!(!res.is_wildcard());
        assert_eq!(res.wildcard(), "persist:*");

        let cfg_res = parse_acl_resource("persist:config").unwrap();
        assert!(cfg_res.is_persist_config());

        let wild = parse_acl_resource("topic:*").unwrap();
        assert!(wild.is_wildcard());

        assert!(parse_acl_resource("p2p:all").is_err());
        assert!(parse_acl_resource("persist:bad/ns").is_err());
    }

    #[test]
    fn test_six_types_validation_formats_and_null_rules() {
        // 1. Top-level null forbidden
        assert_eq!(
            infer_data_type(&json!(null)).unwrap_err().code,
            "INVALID_DATA_TYPE"
        );

        // 2. Number with integer & slider bounds
        let num_meta = DataMeta {
            min: Some(1.0),
            max: Some(10.0),
            format: Some("integer".to_string()),
            ..Default::default()
        };
        validate_schema_definition(DataType::Number, Some(&num_meta), 0).unwrap();
        validate_value_against(DataType::Number, Some(&num_meta), &json!(5), 0, "$").unwrap();
        assert_eq!(
            validate_value_against(DataType::Number, Some(&num_meta), &json!(5.5), 0, "$")
                .unwrap_err()
                .code,
            "OUT_OF_RANGE"
        );
        assert_eq!(
            validate_value_against(DataType::Number, Some(&num_meta), &json!(11), 0, "$")
                .unwrap_err()
                .code,
            "OUT_OF_RANGE"
        );

        // 3. String url safety & max_length
        let url_meta = DataMeta {
            format: Some("url".to_string()),
            max_length: Some(32),
            ..Default::default()
        };
        validate_value_against(
            DataType::String,
            Some(&url_meta),
            &json!("https://example.com/a"),
            0,
            "$",
        )
        .unwrap();
        assert_eq!(
            validate_value_against(
                DataType::String,
                Some(&url_meta),
                &json!("javascript:alert(1)"),
                0,
                "$",
            )
            .unwrap_err()
            .code,
            "INVALID_ARGUMENT"
        );

        // 4. Enum strict check
        let enum_meta = DataMeta {
            options: Some(vec![
                EnumOption::Plain("light".to_string()),
                EnumOption::Labeled {
                    value: "dark".to_string(),
                    label: Some("深色".to_string()),
                },
            ]),
            ..Default::default()
        };
        validate_schema_definition(DataType::Enum, Some(&enum_meta), 0).unwrap();
        validate_value_against(DataType::Enum, Some(&enum_meta), &json!("dark"), 0, "$").unwrap();
        assert_eq!(
            validate_value_against(DataType::Enum, Some(&enum_meta), &json!("neon"), 0, "$")
                .unwrap_err()
                .code,
            "INVALID_ENUM_VALUE"
        );

        // 5. Typed list with nullable elements
        let list_meta = DataMeta {
            max_items: Some(2),
            item: Some(Box::new(ItemSchema {
                data_type: DataType::Number,
                meta: None,
                nullable: true,
            })),
            ..Default::default()
        };
        validate_schema_definition(DataType::List, Some(&list_meta), 0).unwrap();
        validate_value_against(DataType::List, Some(&list_meta), &json!([1, null]), 0, "$")
            .unwrap();
        assert_eq!(
            validate_value_against(DataType::List, Some(&list_meta), &json!([1, 2, 3]), 0, "$")
                .unwrap_err()
                .code,
            "OUT_OF_RANGE"
        );

        // 6. Struct: free object vs strict fields + optional/nullable
        validate_value_against(
            DataType::Struct,
            None,
            &json!({"any": null, "nested": [1, 2]}),
            0,
            "$",
        )
        .unwrap();

        let mut fields = BTreeMap::new();
        fields.insert(
            "columns".to_string(),
            FieldSchema {
                data_type: DataType::Number,
                meta: Some(Box::new(DataMeta {
                    min: Some(1.0),
                    max: Some(4.0),
                    ..Default::default()
                })),
                optional: false,
                nullable: false,
            },
        );
        fields.insert(
            "subtitle".to_string(),
            FieldSchema {
                data_type: DataType::String,
                meta: None,
                optional: true,
                nullable: true,
            },
        );
        let struct_meta = DataMeta {
            fields: Some(fields),
            ..Default::default()
        };
        validate_schema_definition(DataType::Struct, Some(&struct_meta), 0).unwrap();
        validate_value_against(
            DataType::Struct,
            Some(&struct_meta),
            &json!({"columns": 2, "subtitle": null}),
            0,
            "$",
        )
        .unwrap();
        assert_eq!(
            validate_value_against(DataType::Struct, Some(&struct_meta), &json!({}), 0, "$")
                .unwrap_err()
                .code,
            "STRUCT_FIELD_INVALID"
        );
        assert_eq!(
            validate_value_against(
                DataType::Struct,
                Some(&struct_meta),
                &json!({"columns": 9}),
                0,
                "$",
            )
            .unwrap_err()
            .code,
            "STRUCT_FIELD_INVALID"
        );
    }

    #[test]
    fn test_parse_default_entry_full_and_shorthand() {
        let full = json!({
            "type": "enum",
            "value": "dark",
            "meta": { "options": ["light", "dark"], "label": "主题" }
        });
        let parsed_full = parse_default_entry(&full).unwrap();
        assert_eq!(parsed_full.data_type, DataType::Enum);
        assert_eq!(parsed_full.value, json!("dark"));

        let shorthand_num = parse_default_entry(&json!(300)).unwrap();
        assert_eq!(shorthand_num.data_type, DataType::Number);
        assert_eq!(shorthand_num.value, json!(300));

        // Plain struct shorthand that happens to contain extra keys must stay Struct
        let shorthand_struct =
            parse_default_entry(&json!({ "columns": 2, "compact": true })).unwrap();
        assert_eq!(shorthand_struct.data_type, DataType::Struct);
    }
}
