use serde::Serialize;
use serde_json::{Map, Value};
use std::io::{self, Write};

use crate::{QuestionAction, QuestionAnswer, SessionError};

pub(crate) const MAX_FORM_BYTES: usize = 65536;

pub fn validate_question_schema(schema: &Value) -> Result<(), SessionError> {
    compile(schema).map(|_| ())
}

pub(crate) fn validate_answer(schema: &Value, answer: &QuestionAnswer) -> Result<(), SessionError> {
    bounded_json(answer, "answer")?;
    match answer.action {
        QuestionAction::Decline | QuestionAction::Cancel => {
            if answer.content.is_some() {
                return Err(invalid("decline and cancel cannot include content"));
            }
        }
        QuestionAction::Accept => {
            let content = answer
                .content
                .as_ref()
                .filter(|content| content.is_object())
                .ok_or_else(|| invalid("accept requires object content"))?;
            let validator = compile(schema)?;
            if let Err(error) = validator.validate(content) {
                return Err(invalid(&format!("answer does not satisfy form: {error}")));
            }
        }
    }
    Ok(())
}

pub(crate) fn bounded_json(value: &impl Serialize, label: &str) -> Result<String, SessionError> {
    let mut buffer = BoundedBuffer(Vec::new());
    if let Err(error) = serde_json::to_writer(&mut buffer, value) {
        return if error.is_io() {
            Err(invalid(&format!("{label} exceeds 64 KiB")))
        } else {
            Err(error.into())
        };
    }
    String::from_utf8(buffer.0).map_err(|error| SessionError::Storage(error.to_string()))
}

fn compile(schema: &Value) -> Result<jsonschema::Validator, SessionError> {
    bounded_json(schema, "requested_schema")?;
    let root = schema
        .as_object()
        .ok_or_else(|| invalid("form schema must be an object"))?;
    check_keys(
        root,
        &[
            "type",
            "properties",
            "required",
            "title",
            "description",
            "additionalProperties",
            "minProperties",
            "maxProperties",
        ],
    )?;
    if root.get("type").and_then(Value::as_str) != Some("object") {
        return Err(invalid("form schema type must be object"));
    }
    if root
        .get("additionalProperties")
        .is_some_and(|value| value != &Value::Bool(false))
    {
        return Err(invalid("form schemas cannot allow additional properties"));
    }
    let properties = root
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("form schema requires properties"))?;
    for property in properties.values() {
        check_property(property)?;
    }
    if let Some(required) = root.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| invalid("required must be an array"))?;
        for name in required {
            if !name
                .as_str()
                .is_some_and(|name| properties.contains_key(name))
            {
                return Err(invalid("required must name a declared property"));
            }
        }
    }
    let mut strict = schema.clone();
    strict["additionalProperties"] = Value::Bool(false);
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_retriever(NoExternalResources)
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(10_000)
                .size_limit(1024 * 1024)
                .dfa_size_limit(1024 * 1024),
        )
        .should_validate_formats(true)
        .build(&strict)
        .map_err(|error| invalid(&format!("invalid form schema: {error}")))
}

fn check_property(property: &Value) -> Result<(), SessionError> {
    let property = property
        .as_object()
        .ok_or_else(|| invalid("form properties must be primitive schemas"))?;
    let kind = property
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("form property requires a primitive type"))?;
    let constraints: &[&str] = match kind {
        "string" => &["minLength", "maxLength", "pattern", "format", "enumNames"],
        "integer" | "number" => &[
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
        ],
        "boolean" => &[],
        _ => return Err(invalid("nested or unsupported form property type")),
    };
    let allowed: Vec<_> = ["type", "title", "description", "default", "enum"]
        .into_iter()
        .chain(constraints.iter().copied())
        .collect();
    check_keys(property, &allowed)?;
    if let Some(values) = property.get("enum") {
        let values = values
            .as_array()
            .ok_or_else(|| invalid("enum must be an array"))?;
        if values.is_empty() || values.iter().any(|value| !has_type(value, kind)) {
            return Err(invalid("enum values must match the primitive type"));
        }
    }
    if property
        .get("default")
        .is_some_and(|value| !has_type(value, kind))
    {
        return Err(invalid("default must match the primitive type"));
    }
    if let Some(names) = property.get("enumNames") {
        let names = names
            .as_array()
            .ok_or_else(|| invalid("enumNames must be an array"))?;
        let values = property
            .get("enum")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("enumNames requires enum"))?;
        if names.len() != values.len() || names.iter().any(|name| !name.is_string()) {
            return Err(invalid("enumNames must label every enum value"));
        }
    }
    if let Some(format) = property.get("format")
        && !format
            .as_str()
            .is_some_and(|format| matches!(format, "email" | "uri" | "date" | "date-time"))
    {
        return Err(invalid("unsupported form string format"));
    }
    Ok(())
}

fn check_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), SessionError> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(&format!("unsupported form keyword: {key}")));
    }
    for key in ["title", "description"] {
        if object.get(key).is_some_and(|value| !value.is_string()) {
            return Err(invalid(&format!("{key} must be a string")));
        }
    }
    Ok(())
}

fn has_type(value: &Value, kind: &str) -> bool {
    match kind {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => false,
    }
}

fn invalid(message: &str) -> SessionError {
    SessionError::Invalid(message.into())
}

struct NoExternalResources;

impl jsonschema::Retrieve for NoExternalResources {
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external form schema resources are forbidden".into())
    }
}

struct BoundedBuffer(Vec<u8>);

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FORM_BYTES - self.0.len() {
            return Err(io::Error::other("form JSON exceeds 64 KiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
