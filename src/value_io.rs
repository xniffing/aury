//! Type-directed CLI value parsing and interpreter-compatible display.
//!
//! Scalar command-line arguments keep their original syntax. Composite values
//! use JSON: arrays for vectors, objects for structs, and `{ "ok": ... }` or
//! `{ "err": ... }` for results.

use crate::ast::{Module, ModuleItem};
use crate::interp::Value;
use crate::types::Type;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value as Json;
use std::collections::HashSet;
use std::fmt;

/// A JSON sink used for a validation pass before parsing into `serde_json::Value`.
/// `Value` itself cannot represent duplicate object keys and would silently keep
/// the last value, so duplicates must be rejected while deserializing.
struct DuplicateChecked;

impl<'de> Deserialize<'de> for DuplicateChecked {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DuplicateCheckedVisitor;

        impl<'de> Visitor<'de> for DuplicateCheckedVisitor {
            type Value = DuplicateChecked;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_string<E>(self, _: String) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                DuplicateChecked::deserialize(deserializer)
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<DuplicateChecked>()?.is_some() {}
                Ok(DuplicateChecked)
            }
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom(format!(
                            "duplicate JSON object key `{}`",
                            key
                        )));
                    }
                    map.next_value::<DuplicateChecked>()?;
                }
                Ok(DuplicateChecked)
            }
        }

        deserializer.deserialize_any(DuplicateCheckedVisitor)
    }
}

fn parse_unique_json(text: &str) -> Result<Json, String> {
    serde_json::from_str::<DuplicateChecked>(text)
        .map_err(|error| format!("invalid JSON value `{}`: {}", text, error))?;
    serde_json::from_str(text).map_err(|error| format!("invalid JSON value `{}`: {}", text, error))
}

pub fn parse_cli_value(module: &Module, ty: &Type, text: &str) -> Result<Value, String> {
    match ty {
        Type::I64 => text
            .parse::<i64>()
            .map(Value::I64)
            .map_err(|_| format!("`{}` is not an i64", text)),
        Type::F64 => text
            .parse::<f64>()
            .map(Value::F64)
            .map_err(|_| format!("`{}` is not an f64", text)),
        Type::Bool => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("`{}` is not a bool", text)),
        },
        Type::Str => Ok(Value::Str(text.to_string())),
        Type::Unit if text == "unit" => Ok(Value::Unit),
        Type::Vec(_) | Type::Struct(_) | Type::Result(_, _) | Type::Unit => {
            let json = parse_unique_json(text)?;
            parse_json_value(module, ty, &json)
        }
        Type::Ref { .. } | Type::Region => {
            Err(format!("CLI arguments of type {:?} are not supported", ty))
        }
    }
}

fn parse_json_value(module: &Module, ty: &Type, json: &Json) -> Result<Value, String> {
    match ty {
        Type::I64 => json
            .as_i64()
            .map(Value::I64)
            .ok_or_else(|| format!("expected JSON i64, got {}", json)),
        Type::F64 => json
            .as_f64()
            .map(Value::F64)
            .ok_or_else(|| format!("expected JSON f64, got {}", json)),
        Type::Bool => json
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| format!("expected JSON bool, got {}", json)),
        Type::Str => json
            .as_str()
            .map(|value| Value::Str(value.to_string()))
            .ok_or_else(|| format!("expected JSON string, got {}", json)),
        Type::Unit => {
            if json.is_null() {
                Ok(Value::Unit)
            } else {
                Err(format!("expected JSON null for unit, got {}", json))
            }
        }
        Type::Vec(inner) => {
            let values = json
                .as_array()
                .ok_or_else(|| format!("expected JSON array for {:?}, got {}", ty, json))?;
            values
                .iter()
                .map(|value| parse_json_value(module, inner, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Vec)
        }
        Type::Struct(name) => {
            let object = json
                .as_object()
                .ok_or_else(|| format!("expected JSON object for {:?}, got {}", ty, json))?;
            let definition = module
                .items
                .iter()
                .find_map(|item| match item {
                    ModuleItem::Struct(definition) if definition.name == *name => Some(definition),
                    _ => None,
                })
                .ok_or_else(|| format!("unknown struct `{}`", name))?;
            for key in object.keys() {
                if !definition.fields.iter().any(|(field, _)| field == key) {
                    return Err(format!("unknown field `{}` for struct `{}`", key, name));
                }
            }
            let mut fields = Vec::with_capacity(definition.fields.len());
            for (field, field_ty) in &definition.fields {
                let value = object
                    .get(field)
                    .ok_or_else(|| format!("missing field `{}` for struct `{}`", field, name))?;
                fields.push((field.clone(), parse_json_value(module, field_ty, value)?));
            }
            Ok(Value::Struct(name.clone(), fields))
        }
        Type::Result(ok, err) => {
            let object = json
                .as_object()
                .ok_or_else(|| format!("expected result object, got {}", json))?;
            if object.len() != 1 {
                return Err("result JSON must contain exactly one of `ok` or `err`".into());
            }
            if let Some(value) = object.get("ok") {
                Ok(Value::ResultOk(Box::new(parse_json_value(
                    module, ok, value,
                )?)))
            } else if let Some(value) = object.get("err") {
                Ok(Value::ResultErr(Box::new(parse_json_value(
                    module, err, value,
                )?)))
            } else {
                Err("result JSON must contain `ok` or `err`".into())
            }
        }
        Type::Ref { .. } | Type::Region => {
            Err(format!("JSON construction for {:?} is unsupported", ty))
        }
    }
}

pub fn show_value(value: &Value) -> String {
    match value {
        Value::I64(number) => number.to_string(),
        Value::F64(number) => crate::interp::format_f64(*number),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Str(string) => format!("{:?}", string),
        Value::Unit => "unit".into(),
        Value::Vec(values) => format!(
            "[{}]",
            values.iter().map(show_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Struct(name, fields) => format!(
            "{}{{{}}}",
            name,
            fields
                .iter()
                .map(|(field, value)| format!("{}: {}", field, show_value(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Region(_) => "region".into(),
        Value::ResultOk(value) => format!("ok({})", show_value(value)),
        Value::ResultErr(value) => format!("err({})", show_value(value)),
    }
}
