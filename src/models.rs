use serde::{Deserialize, Serialize};

const DEFAULT_CREATED_AT: i64 = 1_687_882_411;

#[derive(Clone, Debug)]
pub struct ModelRegistry {
    default_client_model: String,
    models: Vec<ModelDefinition>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::from_config(None, Vec::new())
    }
}

impl ModelRegistry {
    pub fn default_client_model(&self) -> &str {
        &self.default_client_model
    }

    pub fn from_config(
        default_client_model: Option<String>,
        models: Vec<ModelRegistryEntry>,
    ) -> Self {
        let entries = if models.is_empty() {
            vec![
                ModelRegistryEntry::virtual_alias("auto", "proxy", None),
                ModelRegistryEntry::virtual_alias("balanced", "proxy", None),
                ModelRegistryEntry::virtual_alias("small", "proxy", Some("gpt-5.1-codex-mini")),
                ModelRegistryEntry::virtual_alias("medium", "proxy", Some("gpt-5.3-codex")),
                ModelRegistryEntry::virtual_alias("large", "proxy", Some("gpt-5.4")),
                ModelRegistryEntry::backend("gpt-5.1-codex-mini", "openai"),
                ModelRegistryEntry::backend("gpt-5.3-codex", "openai"),
                ModelRegistryEntry::backend("gpt-5.4", "openai"),
            ]
        } else {
            models
        };

        Self {
            default_client_model: default_client_model.unwrap_or_else(|| "auto".to_string()),
            models: entries
                .into_iter()
                .map(ModelDefinition::from_entry)
                .collect(),
        }
    }

    pub fn list_response(&self) -> ModelsListResponse {
        let data = self.models.iter().map(ModelDefinition::to_card).collect();

        ModelsListResponse {
            object: "list",
            data,
        }
    }

    pub fn backend_for_alias(&self, alias: &str) -> Option<&str> {
        self.models
            .iter()
            .find(|model| model.id == alias)
            .and_then(|model| model.backend_target.as_deref())
    }

    pub fn is_virtual_alias(&self, model_id: &str) -> bool {
        self.models
            .iter()
            .any(|model| model.id == model_id && model.backend_target.is_some())
            || matches!(model_id, "auto" | "balanced")
    }

    pub fn knows_model(&self, model_id: &str) -> bool {
        self.models.iter().any(|model| model.id == model_id)
    }
}

#[derive(Clone, Debug)]
struct ModelDefinition {
    id: String,
    owned_by: String,
    backend_target: Option<String>,
}

impl ModelDefinition {
    fn from_entry(entry: ModelRegistryEntry) -> Self {
        Self {
            id: entry.id,
            owned_by: entry.owned_by,
            backend_target: entry.backend_target,
        }
    }

    fn to_card(&self) -> ModelCard {
        ModelCard {
            id: self.id.clone(),
            object: "model",
            created: DEFAULT_CREATED_AT,
            owned_by: self.owned_by.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelRegistryEntry {
    pub id: String,
    #[serde(default = "default_owned_by")]
    pub owned_by: String,
    #[serde(default)]
    pub backend_target: Option<String>,
}

impl ModelRegistryEntry {
    fn virtual_alias(id: &str, owned_by: &str, backend_target: Option<&str>) -> Self {
        Self {
            id: id.to_string(),
            owned_by: owned_by.to_string(),
            backend_target: backend_target.map(ToOwned::to_owned),
        }
    }

    fn backend(id: &str, owned_by: &str) -> Self {
        Self {
            id: id.to_string(),
            owned_by: owned_by.to_string(),
            backend_target: None,
        }
    }
}

fn default_owned_by() -> String {
    "proxy".to_string()
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelsListResponse {
    pub object: &'static str,
    pub data: Vec<ModelCard>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelCard {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: String,
}
