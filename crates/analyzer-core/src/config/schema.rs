//! Declarative configuration schema contracts.
use super::Config;
use serde_yaml::Value;
use std::sync::Arc;
pub type Validator = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;
pub type Handler = Arc<dyn Fn(&mut Config, &Value) + Send + Sync>;
#[derive(Clone)]
pub struct KeySpec {
    pub name: String,
    pub description: String,
    pub default: Option<Value>,
    pub validator: Validator,
    pub handler: Option<Handler>,
}
impl KeySpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        validator: Validator,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            default: None,
            validator,
            handler: None,
        }
    }
}
