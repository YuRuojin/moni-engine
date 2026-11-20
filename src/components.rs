use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Sprite, Transform};
use crate::scene3d::Model3d;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptComponent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ComponentValue {
    Transform(Transform),
    Sprite(Sprite),
    Model(Model3d),
    Script(ScriptComponent),
}

#[derive(Debug, Clone)]
pub enum PropertyType {
    Number,
    Integer,
    String,
    Object,
    Color,
    Path,
    Enum(Vec<&'static str>),
}

#[derive(Debug, Clone)]
pub struct PropertyField {
    pub name: &'static str,
    pub kind: PropertyType,
    pub required: bool,
    pub default: Option<Value>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub max_length: Option<usize>,
    pub label: &'static str,
    pub description: &'static str,
}

impl PropertyField {
    fn new(name: &'static str, kind: PropertyType, required: bool) -> Self {
        Self {
            name,
            kind,
            required,
            default: None,
            minimum: None,
            maximum: None,
            max_length: None,
            label: name,
            description: name,
        }
    }

    fn default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }

    fn range(mut self, minimum: f64, maximum: f64) -> Self {
        self.minimum = Some(minimum);
        self.maximum = Some(maximum);
        self
    }

    fn max_length(mut self, maximum: usize) -> Self {
        self.max_length = Some(maximum);
        self
    }

    fn validate(&self, value: &Value) -> Result<(), String> {
        match &self.kind {
            PropertyType::Number if !value.is_number() => return Err("must be a number".into()),
            PropertyType::Integer if !value.is_i64() && !value.is_u64() => {
                return Err("must be an integer".into());
            }
            PropertyType::String if !value.is_string() => return Err("must be a string".into()),
            PropertyType::Object if !value.is_object() => return Err("must be an object".into()),
            PropertyType::Color => {
                let length = value.as_str().map(str::len).unwrap_or(0);
                if length == 0 || length > 64 {
                    return Err("must be a color string with 1 to 64 characters".into());
                }
            }
            PropertyType::Path => {
                let path = value.as_str().ok_or("must be a path string")?;
                if path.is_empty() || path.len() > 256 {
                    return Err("path must contain 1 to 256 characters".into());
                }
                let path = Path::new(path);
                if path.is_absolute()
                    || path.components().any(|component| {
                        matches!(
                            component,
                            Component::Prefix(_) | Component::RootDir | Component::ParentDir
                        )
                    })
                {
                    return Err("path must stay inside the project".into());
                }
            }
            PropertyType::Enum(variants) => {
                let value = value.as_str().ok_or("must be a string")?;
                if !variants.contains(&value) {
                    return Err(format!("must be one of {}", variants.join(", ")));
                }
            }
            _ => {}
        }
        if let Some(number) = value.as_f64()
            && (self.minimum.is_some_and(|minimum| number < minimum)
                || self.maximum.is_some_and(|maximum| number > maximum))
        {
            return Err(format!(
                "must be between {:?} and {:?}",
                self.minimum, self.maximum
            ));
        }
        if let Some(maximum) = self.max_length
            && value.as_str().is_some_and(|value| value.len() > maximum)
        {
            return Err(format!("must contain at most {maximum} bytes"));
        }
        Ok(())
    }

    fn agent_schema(&self) -> Value {
        let mut schema = match &self.kind {
            PropertyType::Number | PropertyType::Integer => {
                json!({ "type": if matches!(self.kind, PropertyType::Integer) { "integer" } else { "number" } })
            }
            PropertyType::String | PropertyType::Color | PropertyType::Path => {
                json!({ "type": "string" })
            }
            PropertyType::Object => json!({ "type": "object" }),
            PropertyType::Enum(variants) => json!({ "enum": variants }),
        };
        if let Some(default) = &self.default {
            schema["default"] = default.clone();
        }
        if let Some(minimum) = self.minimum {
            schema["minimum"] = json!(minimum);
        }
        if let Some(maximum) = self.maximum {
            schema["maximum"] = json!(maximum);
        }
        if let Some(max_length) = self.max_length {
            schema["maxLength"] = json!(max_length);
        }
        schema["x-editor"] = json!({
            "label": self.label,
            "description": self.description,
        });
        schema
    }
}

#[derive(Debug, Clone)]
pub struct PropertySchema {
    pub label: &'static str,
    pub description: &'static str,
    pub fields: Vec<PropertyField>,
}

impl PropertySchema {
    pub fn apply_defaults(&self, value: &mut Value) -> Result<(), String> {
        let object = value
            .as_object_mut()
            .ok_or_else(|| "component must be an object".to_string())?;
        for field in &self.fields {
            if let Some(default) = &field.default {
                object
                    .entry(field.name.to_string())
                    .or_insert_with(|| default.clone());
            }
        }
        Ok(())
    }

    pub fn validate(&self, value: &Value) -> Result<(), String> {
        let object = value
            .as_object()
            .ok_or_else(|| "component must be an object".to_string())?;
        for field in &self.fields {
            if field.required && !object.contains_key(field.name) {
                return Err(format!("missing required field '{}'", field.name));
            }
            if let Some(value) = object.get(field.name) {
                if value.is_null() && !field.required {
                    continue;
                }
                field
                    .validate(value)
                    .map_err(|error| format!("field '{}': {error}", field.name))?;
            }
        }
        if let Some(unknown) = object
            .keys()
            .find(|name| !self.fields.iter().any(|field| field.name == name.as_str()))
        {
            return Err(format!("unknown field '{unknown}'"));
        }
        Ok(())
    }

    pub fn agent_schema(&self) -> Value {
        let properties: serde_json::Map<String, Value> = self
            .fields
            .iter()
            .map(|field| (field.name.to_string(), field.agent_schema()))
            .collect();
        let required: Vec<&str> = self
            .fields
            .iter()
            .filter(|field| field.required)
            .map(|field| field.name)
            .collect();
        json!({
            "type": "object",
            "title": self.label,
            "description": self.description,
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }
}

pub struct ComponentDescriptor {
    pub id: &'static str,
    pub legacy_id: Option<&'static str>,
    pub owner: &'static str,
    pub version: u32,
    pub schema: PropertySchema,
    deserialize: fn(&Value) -> Result<ComponentValue, String>,
    serialize: fn(&ComponentValue) -> Result<Value, String>,
}

impl ComponentDescriptor {
    pub fn deserialize(&self, value: &Value) -> Result<ComponentValue, String> {
        let mut value = value.clone();
        self.schema.apply_defaults(&mut value)?;
        self.schema.validate(&value)?;
        (self.deserialize)(&value)
    }

    pub fn serialize(&self, value: &ComponentValue) -> Result<Value, String> {
        let value = (self.serialize)(value)?;
        self.schema.validate(&value)?;
        Ok(value)
    }

    pub fn agent_schema(&self) -> Value {
        let mut schema = self.schema.agent_schema();
        schema["x-component"] = json!({
            "id": self.id,
            "owner": self.owner,
            "version": self.version,
        });
        schema
    }
}

#[derive(Default)]
pub struct ComponentRegistry {
    components: BTreeMap<&'static str, ComponentDescriptor>,
    legacy_ids: BTreeMap<&'static str, &'static str>,
}

impl ComponentRegistry {
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry.register_core_components();
        registry
    }

    pub fn register(&mut self, descriptor: ComponentDescriptor) -> Result<(), String> {
        if self.components.contains_key(descriptor.id) {
            return Err(format!("duplicate component ID '{}'", descriptor.id));
        }
        if let Some(legacy) = descriptor.legacy_id
            && self.legacy_ids.insert(legacy, descriptor.id).is_some()
        {
            return Err(format!("duplicate legacy component ID '{legacy}'"));
        }
        self.components.insert(descriptor.id, descriptor);
        Ok(())
    }

    pub fn resolve(&self, id: &str) -> Option<&ComponentDescriptor> {
        self.components.get(id).or_else(|| {
            self.legacy_ids
                .get(id)
                .and_then(|id| self.components.get(id))
        })
    }

    pub fn agent_schemas(&self) -> Value {
        self.components
            .values()
            .map(|descriptor| (descriptor.id.to_string(), descriptor.agent_schema()))
            .collect::<serde_json::Map<String, Value>>()
            .into()
    }

    fn register_core_components(&mut self) {
        self.register(ComponentDescriptor {
            id: "core.transform",
            legacy_id: Some("transform"),
            owner: "moni.core",
            version: 1,
            schema: schema(
                "Transform",
                "2D entity position and rotation",
                &[number("x", 0.0), number("y", 0.0), number("rotation", 0.0)],
            ),
            deserialize: |value| {
                serde_json::from_value(value.clone())
                    .map(ComponentValue::Transform)
                    .map_err(|error| error.to_string())
            },
            serialize: |value| match value {
                ComponentValue::Transform(value) => {
                    serde_json::to_value(value).map_err(|error| error.to_string())
                }
                _ => Err("expected core.transform value".into()),
            },
        })
        .expect("register core.transform");
        self.register(ComponentDescriptor {
            id: "core.sprite",
            legacy_id: Some("sprite"),
            owner: "moni.core",
            version: 1,
            schema: schema(
                "Sprite",
                "2D rectangle or circle",
                &[
                    PropertyField::new(
                        "type",
                        PropertyType::Enum(vec!["rectangle", "circle"]),
                        true,
                    ),
                    number("width", 32.0).range(0.000001, f64::MAX),
                    number("height", 32.0).range(0.000001, f64::MAX),
                    PropertyField::new("color", PropertyType::Color, false)
                        .default(json!("#ffffff")),
                    PropertyField::new("layer", PropertyType::Integer, false).default(json!(0)),
                ],
            ),
            deserialize: |value| {
                serde_json::from_value(value.clone())
                    .map(ComponentValue::Sprite)
                    .map_err(|error| error.to_string())
            },
            serialize: |value| match value {
                ComponentValue::Sprite(value) => {
                    serde_json::to_value(value).map_err(|error| error.to_string())
                }
                _ => Err("expected core.sprite value".into()),
            },
        })
        .expect("register core.sprite");
        self.register(ComponentDescriptor {
            id: "core.model",
            legacy_id: Some("model"),
            owner: "moni.core",
            version: 1,
            schema: schema(
                "Model",
                "Built-in or FBX model with material",
                &[
                    PropertyField::new(
                        "type",
                        PropertyType::Enum(vec!["cube", "plane", "fbx"]),
                        true,
                    ),
                    PropertyField::new("source", PropertyType::Path, false),
                    PropertyField::new("transform", PropertyType::Object, false).default(json!({})),
                    PropertyField::new("color", PropertyType::Color, false)
                        .default(json!("#ffffff")),
                    number("metallic", 0.0).range(0.0, 1.0),
                    number("roughness", 0.5).range(0.0, 1.0),
                ],
            ),
            deserialize: |value| {
                serde_json::from_value(value.clone())
                    .map(ComponentValue::Model)
                    .map_err(|error| error.to_string())
            },
            serialize: |value| match value {
                ComponentValue::Model(value) => {
                    serde_json::to_value(value).map_err(|error| error.to_string())
                }
                _ => Err("expected core.model value".into()),
            },
        })
        .expect("register core.model");
        self.register(ComponentDescriptor {
            id: "core.script",
            legacy_id: Some("script"),
            owner: "moni.core",
            version: 1,
            schema: schema(
                "Script",
                "External JavaScript path or inline source",
                &[
                    PropertyField::new("source", PropertyType::Path, false),
                    PropertyField::new("inline", PropertyType::String, false)
                        .max_length(256 * 1024),
                ],
            ),
            deserialize: |value| {
                let script: ScriptComponent =
                    serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
                if script.source.is_some() == script.inline.is_some() {
                    return Err("script requires exactly one of source or inline".into());
                }
                Ok(ComponentValue::Script(script))
            },
            serialize: |value| match value {
                ComponentValue::Script(value) => {
                    serde_json::to_value(value).map_err(|error| error.to_string())
                }
                _ => Err("expected core.script value".into()),
            },
        })
        .expect("register core.script");
    }
}

fn schema(
    label: &'static str,
    description: &'static str,
    fields: &[PropertyField],
) -> PropertySchema {
    PropertySchema {
        label,
        description,
        fields: fields.to_vec(),
    }
}

fn number(name: &'static str, default: f64) -> PropertyField {
    PropertyField::new(name, PropertyType::Number, false).default(json!(default))
}
