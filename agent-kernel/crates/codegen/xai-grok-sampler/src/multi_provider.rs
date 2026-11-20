//! Multi-provider role-based router and failover engine built on top of [`crate::SamplerActor`].

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use xai_grok_sampling_types::{
    ApiBackend, ConversationRequest, ConversationResponse, Result, SamplingError,
};

use crate::actor::SamplerActor;
use crate::config::{AuthScheme, RequestCompression, RetryPolicy, SamplerConfig};
use crate::events::SamplingEvent;
use crate::metrics::InferenceLatencyStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Chat,
    Memory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    pub id: String,
    pub label: String,
    pub api_backend: ApiBackend,
    pub base_url: String,
    pub api_key: String,
    pub chat_model: String,
    #[serde(default)]
    pub memory_model: Option<String>,
    #[serde(default)]
    pub extra_headers: IndexMap<String, String>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl ProviderEndpoint {
    pub fn model_for_role(&self, role: ModelRole) -> &str {
        match role {
            ModelRole::Chat => &self.chat_model,
            ModelRole::Memory => self
                .memory_model
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&self.chat_model),
        }
    }

    pub fn to_sampler_config(&self, role: ModelRole) -> SamplerConfig {
        let auth_scheme = match self.api_backend {
            ApiBackend::Messages => AuthScheme::XApiKey,
            _ => AuthScheme::Bearer,
        };
        let mut query_params = IndexMap::new();
        let mut extra_headers = self.extra_headers.clone();
        if self.api_backend == ApiBackend::Gemini && !self.api_key.trim().is_empty() {
            extra_headers
                .entry("x-goog-api-key".to_string())
                .or_insert_with(|| self.api_key.clone());
            query_params.insert("alt".to_string(), "sse".to_string());
        }
        if self.api_backend == ApiBackend::Messages {
            extra_headers
                .entry("anthropic-version".to_string())
                .or_insert_with(|| "2023-06-01".to_string());
        }

        SamplerConfig {
            base_url: self.base_url.clone(),
            api_key: (!self.api_key.trim().is_empty() && self.api_backend != ApiBackend::Gemini)
                .then(|| self.api_key.clone()),
            model: self.model_for_role(role).to_string(),
            max_completion_tokens: self.max_completion_tokens,
            temperature: self.temperature,
            max_retries: Some(1),
            rate_limit_retry_threshold: Some(1),
            idle_timeout_secs: Some(90),
            api_backend: self.api_backend.clone(),
            auth_scheme,
            request_compression: RequestCompression::None,
            extra_headers,
            query_params,
            context_window: 128_000,
            stream_tool_calls: true,
            ..SamplerConfig::default()
        }
    }
}

/// Failover-capable multi-provider router backed by [`SamplerActor`].
#[derive(Clone, Debug, Default)]
pub struct MultiProviderRouter {
    endpoints: Vec<ProviderEndpoint>,
}

impl MultiProviderRouter {
    pub fn new(endpoints: Vec<ProviderEndpoint>) -> Self {
        Self { endpoints }
    }

    pub fn endpoints(&self) -> &[ProviderEndpoint] {
        &self.endpoints
    }

    /// Stream a [`ConversationRequest`] for `role`, forwarding real token-by-token [`SamplingEvent`]s
    /// to `on_event` and automatically failing over to the next configured provider if an upstream
    /// fails before emitting tokens.
    pub async fn sample_streaming<F>(
        &self,
        role: ModelRole,
        request: ConversationRequest,
        mut on_event: F,
    ) -> Result<(ConversationResponse, InferenceLatencyStats, String)>
    where
        F: FnMut(&SamplingEvent) + Send,
    {
        if self.endpoints.is_empty() {
            return Err(SamplingError::InvalidConfiguration(
                "No LLM provider endpoints configured",
            ));
        }

        let mut last_err: Option<SamplingError> = None;

        for ep in &self.endpoints {
            let mut req_for_ep = request.clone();
            req_for_ep.model = Some(ep.model_for_role(role).to_string());
            let cfg = ep.to_sampler_config(role);

            let (event_tx, mut event_rx) = mpsc::unbounded_channel::<SamplingEvent>();
            let handle = SamplerActor::spawn(
                cfg,
                RetryPolicy {
                    max_retries: 1,
                    rate_limit_retry_threshold: 1,
                    retry_only_before_output: true,
                },
                event_tx,
            );

            let submit_fut =
                handle.submit_and_collect(crate::types::RequestId::random(), req_for_ep);
            tokio::pin!(submit_fut);

            let mut tokens_emitted = false;
            let outcome = loop {
                tokio::select! {
                    biased;
                    maybe_ev = event_rx.recv() => {
                        if let Some(ev) = maybe_ev {
                            if matches!(ev, SamplingEvent::ChannelToken { .. }) {
                                tokens_emitted = true;
                            }
                            on_event(&ev);
                        }
                    }
                    res = &mut submit_fut => {
                        while let Ok(ev) = event_rx.try_recv() {
                            if matches!(ev, SamplingEvent::ChannelToken { .. }) {
                                tokens_emitted = true;
                            }
                            on_event(&ev);
                        }
                        break res;
                    }
                }
            };

            match outcome {
                Ok((resp, stats)) => return Ok((resp, stats, ep.id.clone())),
                Err(err) => {
                    tracing::warn!(
                        provider_id = %ep.id,
                        model = %ep.model_for_role(role),
                        tokens_emitted,
                        error = %err,
                        "provider attempt failed in MultiProviderRouter"
                    );
                    last_err = Some(err);
                    if tokens_emitted {
                        break;
                    }
                }
            }
        }

        Err(last_err.unwrap_or(SamplingError::InvalidConfiguration(
            "All configured LLM providers failed",
        )))
    }
}
