use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::api_client::{ApiClient, AuthMethod};
use super::base::{ConfigKey, MessageStream, Provider, ProviderDef, ProviderMetadata};
use super::errors::{OpenAIError, ProviderError};
use super::http_status::map_http_error_to_provider_error;
use super::openai_compatible::stream_openai_compat;
use super::retry::ProviderRetry;
use super::utils::{ImageFormat, RequestLog};
use crate::conversation::message::Message;
use crate::model::ModelConfig;
use crate::providers::formats::openai::create_request;
use rmcp::model::Tool;

const ROUTSTR_PROVIDER_NAME: &str = "routstr";
pub const ROUTSTR_HOST: &str = "https://api.routstr.com";
pub const ROUTSTR_DEFAULT_MODEL: &str = "anthropic/claude-sonnet-4";
pub const ROUTSTR_MODEL_PREFIX_ANTHROPIC: &str = "anthropic";

pub const ROUTSTR_KNOWN_MODELS: &[&str] = &[
    "meta-llama/llama-3.2-1b-instruct",
    "deepseek/deepseek-r1-0528-qwen3-8b",
    "anthropic/claude-3.5-sonnet",
    "anthropic/claude-sonnet-4",
];
pub const ROUTSTR_DOC_URL: &str = "https://routstr.com/docs";
pub const ROUTSTR_DEFAULT_MINT_URL: &str = "https://mint.minibits.cash/Bitcoin";
pub const ROUTSTR_DEFAULT_CURRENCY_UNIT: &str = "sat";

/// Pricing information for a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub prompt: f64,
    pub completion: f64,
}

/// Individual model information returned by `/v1/models`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    pub permission: Vec<Value>,
    pub pricing: ModelPricing,
    pub context_length: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

#[derive(serde::Serialize)]
pub struct RoutstrProvider {
    #[serde(skip)]
    api_client: ApiClient,
    model: ModelConfig,
    #[serde(skip)]
    name: String,
}

impl RoutstrProvider {
    pub async fn from_env(model: ModelConfig) -> Result<Self> {
        let config = crate::config::Config::global();

        let host: String = config
            .get_param("ROUTSTR_HOST")
            .unwrap_or_else(|_| ROUTSTR_HOST.to_string());

        // ROUTSTR_API_KEY is a Cashu token managed by the wallet CLI
        // (`goose wallet topup/withdraw/balance`), so it lives in the regular
        // params store rather than the secrets keychain — the wallet has to
        // rewrite it on every consolidate.
        let api_key: String = config.get_param("ROUTSTR_API_KEY")?;

        let api_client = ApiClient::new(host, AuthMethod::BearerToken(api_key))?;

        Ok(Self {
            api_client,
            model,
            name: ROUTSTR_PROVIDER_NAME.to_string(),
        })
    }

    async fn fetch_models_info(&self) -> Result<ModelsResponse, ProviderError> {
        let response = self
            .api_client
            .response_get(None, "v1/models")
            .await
            .map_err(|e| ProviderError::RequestFailed(format!("Failed to fetch models: {e}")))?;
        let response = handle_routstr_status(response).await?;

        response.json::<ModelsResponse>().await.map_err(|e| {
            ProviderError::RequestFailed(format!("Failed to parse models response: {e}"))
        })
    }
}

/// Inspect the response and convert non-2xx replies into the right
/// `ProviderError`. Routstr piggy-backs on the OpenAI error envelope and
/// signals an out-of-balance wallet via `code = "insufficient_balance"`
/// in the body of a 400 — we surface that as `InsufficientBalance(sats)` so
/// the CLI can prompt the user to top up.
async fn handle_routstr_status(response: Response) -> Result<Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let payload: Option<Value> = serde_json::from_str(&body).ok();

    if status == StatusCode::BAD_REQUEST {
        if let Some(p) = &payload {
            if let Some(error_obj) = p.get("error") {
                if let Ok(err) = serde_json::from_value::<OpenAIError>(error_obj.clone()) {
                    if let Some(sats) = err.get_insufficient_balance() {
                        return Err(ProviderError::InsufficientBalance(sats));
                    }
                }
            }
        }
    }

    Err(map_http_error_to_provider_error(status, payload))
}

/// Apply Anthropic prompt-caching markers to an OpenAI-shaped payload.
/// Mirrors the OpenRouter helper: cache-control on the system message,
/// the last two user messages, and the final tool spec.
fn update_request_for_anthropic(original_payload: &Value) -> Value {
    let mut payload = original_payload.clone();

    if let Some(messages_spec) = payload
        .as_object_mut()
        .and_then(|obj| obj.get_mut("messages"))
        .and_then(|messages| messages.as_array_mut())
    {
        let mut user_count = 0;
        for message in messages_spec.iter_mut().rev() {
            if message.get("role") == Some(&json!("user")) {
                if let Some(content) = message.get_mut("content") {
                    if let Some(content_str) = content.as_str() {
                        *content = json!([{
                            "type": "text",
                            "text": content_str,
                            "cache_control": { "type": "ephemeral" }
                        }]);
                    }
                }
                user_count += 1;
                if user_count >= 2 {
                    break;
                }
            }
        }

        if let Some(system_message) = messages_spec
            .iter_mut()
            .find(|msg| msg.get("role") == Some(&json!("system")))
        {
            if let Some(content) = system_message.get_mut("content") {
                if let Some(content_str) = content.as_str() {
                    *system_message = json!({
                        "role": "system",
                        "content": [{
                            "type": "text",
                            "text": content_str,
                            "cache_control": { "type": "ephemeral" }
                        }]
                    });
                }
            }
        }
    }

    if let Some(tools_spec) = payload
        .as_object_mut()
        .and_then(|obj| obj.get_mut("tools"))
        .and_then(|tools| tools.as_array_mut())
    {
        if let Some(last_tool) = tools_spec.last_mut() {
            if let Some(function) = last_tool.get_mut("function") {
                if let Some(obj) = function.as_object_mut() {
                    obj.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
                }
            }
        }
    }

    payload
}

impl ProviderDef for RoutstrProvider {
    type Provider = Self;

    fn metadata() -> ProviderMetadata {
        ProviderMetadata::new(
            ROUTSTR_PROVIDER_NAME,
            "Routstr",
            "LLM provider with Cashu (CDK) wallet payment integration",
            ROUTSTR_DEFAULT_MODEL,
            ROUTSTR_KNOWN_MODELS.to_vec(),
            ROUTSTR_DOC_URL,
            vec![
                // Cashu token managed by `goose wallet`; not a keychain secret
                // because the wallet has to rewrite it on every consolidate.
                ConfigKey::new("ROUTSTR_API_KEY", true, false, None, true),
                ConfigKey::new("ROUTSTR_HOST", false, false, Some(ROUTSTR_HOST), false),
            ],
        )
        .with_setup_steps(vec![
            "Run `goose wallet topup <cashu-token>` to seed the routstr balance",
            "`goose wallet balance` writes a fresh Cashu token to ROUTSTR_API_KEY",
            "Optionally override ROUTSTR_HOST to point at a self-hosted Routstr",
        ])
    }

    fn from_env(
        model: ModelConfig,
        _extensions: Vec<crate::config::ExtensionConfig>,
    ) -> BoxFuture<'static, Result<Self::Provider>> {
        Box::pin(Self::from_env(model))
    }
}

#[async_trait]
impl Provider for RoutstrProvider {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn get_model_config(&self) -> ModelConfig {
        self.model.clone()
    }

    async fn fetch_supported_models(&self) -> Result<Vec<String>, ProviderError> {
        let models = self.fetch_models_info().await?;
        let mut ids: Vec<String> = models.data.into_iter().map(|m| m.id).collect();
        ids.sort();
        Ok(ids)
    }

    async fn supports_cache_control(&self) -> bool {
        self.model
            .model_name
            .starts_with(ROUTSTR_MODEL_PREFIX_ANTHROPIC)
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        session_id: &str,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let mut payload = create_request(
            model_config,
            system,
            messages,
            tools,
            &ImageFormat::OpenAi,
            true,
        )?;

        if self.supports_cache_control().await {
            payload = update_request_for_anthropic(&payload);
        }

        let mut log = RequestLog::start(model_config, &payload)?;

        let response = self
            .with_retry(|| async {
                let resp = self
                    .api_client
                    .response_post(Some(session_id), "v1/chat/completions", &payload)
                    .await?;
                handle_routstr_status(resp).await
            })
            .await
            .inspect_err(|e| {
                let _ = log.error(e);
            })?;

        stream_openai_compat(response, log)
    }
}
