use serde::{Deserialize, Serialize};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub sites: Vec<Site>,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub subagent_models: Vec<SubagentModel>,
}
impl AppConfig {
    pub fn sanitized(&self) -> Self {
        let mut c = self.clone();
        for site in &mut c.sites {
            for key in &mut site.keys {
                key.has_secret = key.secret.as_ref().is_some_and(|s| !s.is_empty());
                key.secret = None;
            }
        }
        c.search.has_api_key = c.search.api_key.as_ref().is_some_and(|s| !s.is_empty());
        c.search.api_key = None;
        c
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    pub provider: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub has_api_key: bool,
}
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            provider: "duckduckgo".into(),
            base_url: String::new(),
            api_key: None,
            has_api_key: false,
        }
    }
}
impl std::fmt::Debug for SearchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field(
                "has_api_key",
                &self.api_key.as_ref().is_some_and(|s| !s.is_empty()),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub keys: Vec<ApiKey>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("id", &self.id)
            .field("name", &self.name)
            .field(
                "has_secret",
                &self.secret.as_ref().is_some_and(|s| !s.is_empty()),
            )
            .field("models", &self.models)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub id: String,
    pub name: String,
    pub model: String,
    pub supports_images: bool,
    pub max_output_tokens: u32,
    pub context_window: u32,
    pub reasoning_efforts: Vec<String>,
    pub reasoning_effort: String,
    pub strict_tools: bool,
    pub omit_tool_strict: bool,
    pub reasoning_replay: bool,
    pub send_store: bool,
    pub cache_routing: bool,
}
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            model: String::new(),
            supports_images: false,
            max_output_tokens: 16384,
            context_window: 128000,
            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
            reasoning_effort: "medium".into(),
            strict_tools: true,
            omit_tool_strict: true,
            reasoning_replay: false,
            send_store: true,
            cache_routing: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub proxy_mode: String,
    pub proxy_url: String,
    pub no_proxy: String,
    pub connect_timeout_secs: u64,
    pub headers_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub max_attempts: u32,
}
impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            proxy_mode: "environment".into(),
            proxy_url: String::new(),
            no_proxy: String::new(),
            connect_timeout_secs: 10,
            headers_timeout_secs: 120,
            idle_timeout_secs: 300,
            request_timeout_secs: 1800,
            max_attempts: 3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub site_id: String,
    pub key_id: String,
    pub model_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentModel {
    pub id: String,
    pub name: String,
    pub selection: Selection,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

fn default_encryption() -> String {
    "none".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub selection: Selection,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub depth: u32,
    pub context_epoch: u32,
    #[serde(default = "default_encryption")]
    pub encryption: String,
    #[serde(default)]
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub session_id: String,
    pub call_id: String,
    pub question: String,
    pub blocking: bool,
    pub options: Vec<String>,
    pub status: String,
    pub answer: Option<String>,
    pub delivered: bool,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub objective: String,
    pub status: String,
    pub progress: String,
    pub rounds: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub session_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: serde_json::Value,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInput {
    pub mime: String,
    pub data: String,
}

fn after_call() -> String {
    "after_call".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageInput {
    #[serde(default = "after_call")]
    pub delivery_mode: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub images: Vec<ImageInput>,
    #[serde(default)]
    pub client_message_id: Option<String>,
}
impl Default for MessageInput {
    fn default() -> Self {
        Self {
            text: String::new(),
            images: Vec::new(),
            client_message_id: None,
            delivery_mode: after_call(),
        }
    }
}
