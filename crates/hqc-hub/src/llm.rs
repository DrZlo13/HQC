//! A language model the hub calls itself (T21): session titles now, the bare model of
//! iteration 4 later (SPEC §8.3). One method, [`Llm::complete`]: a system prompt and one user
//! message in, text out — no streaming, no tools.
//!
//! Providers: `anthropic` (the Messages API, `POST {base}/v1/messages`) and `openai` (any
//! OpenAI-compatible Chat Completions endpoint, `POST {base}/chat/completions`: OpenAI,
//! OpenRouter, a local model). Configured by `HQC_TITLE_MODEL=<provider>:<model>`,
//! `HQC_TITLE_BASE_URL` and `HQC_TITLE_API_KEY` (absent for a local model).

use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
}

#[derive(Clone)]
pub struct LlmConfig {
    pub provider: Provider,
    pub model: String,
    /// Without a trailing `/`: `https://api.anthropic.com`, `https://openrouter.ai/api/v1`,
    /// `http://llm-server:8082/v1`.
    pub base_url: String,
    pub api_key: Option<String>,
    /// The whole request, connection included.
    pub timeout: Duration,
}

impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

impl LlmConfig {
    /// `HQC_TITLE_MODEL` (`anthropic:claude-haiku-5-5`, `openai:qwen3.8-27b`), with the base
    /// URL (default per provider) and the key.
    pub fn parse(
        model: &str,
        base_url: Option<&str>,
        api_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let Some((provider, name)) = model.trim().split_once(':') else {
            bail!("HQC_TITLE_MODEL={model}: expected <provider>:<model>, provider anthropic or openai");
        };
        let provider = match provider {
            "anthropic" => Provider::Anthropic,
            "openai" => Provider::OpenAi,
            p => bail!("HQC_TITLE_MODEL: unknown provider {p:?} (anthropic or openai)"),
        };
        if name.trim().is_empty() {
            bail!("HQC_TITLE_MODEL={model}: the model name is empty");
        }
        let base_url = match base_url.map(str::trim).filter(|b| !b.is_empty()) {
            Some(b) => b.trim_end_matches('/').to_string(),
            None => match provider {
                Provider::Anthropic => "https://api.anthropic.com".into(),
                Provider::OpenAi => "https://api.openai.com/v1".into(),
            },
        };
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            bail!("HQC_TITLE_BASE_URL={base_url}: expected an http(s) URL");
        }
        Ok(Self {
            provider,
            model: name.trim().to_string(),
            base_url,
            api_key: api_key
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(String::from),
            timeout: DEFAULT_TIMEOUT,
        })
    }
}

#[derive(Clone)]
pub struct Llm {
    cfg: LlmConfig,
    http: reqwest::Client,
}

impl Llm {
    pub fn new(cfg: LlmConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            // The key must not follow a redirect to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        Self { cfg, http }
    }

    pub fn config(&self) -> &LlmConfig {
        &self.cfg
    }

    /// One completion: `system`, one user message, at most `max_tokens` tokens of answer.
    pub async fn complete(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> anyhow::Result<String> {
        let c = &self.cfg;
        let req = match c.provider {
            Provider::Anthropic => {
                let mut r = self
                    .http
                    .post(format!("{}/v1/messages", c.base_url))
                    .header("anthropic-version", "2023-06-01")
                    .json(&json!({
                        "model": c.model,
                        "max_tokens": max_tokens,
                        "system": system,
                        "messages": [{"role": "user", "content": user}],
                    }));
                if let Some(k) = &c.api_key {
                    r = r.header("x-api-key", k);
                }
                r
            }
            Provider::OpenAi => {
                let mut r = self
                    .http
                    .post(format!("{}/chat/completions", c.base_url))
                    .json(&json!({
                        "model": c.model,
                        "max_tokens": max_tokens,
                        "messages": [
                            {"role": "system", "content": system},
                            {"role": "user", "content": user},
                        ],
                    }));
                if let Some(k) = &c.api_key {
                    r = r.bearer_auth(k);
                }
                r
            }
        };
        let resp = req.send().await.context("model request")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("model response")?;
        if !status.is_success() {
            let msg = body["error"]["message"]
                .as_str()
                .or(body["error"].as_str())
                .unwrap_or("");
            bail!("model answered HTTP {status}: {msg}");
        }
        let text = match c.provider {
            Provider::Anthropic => body["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<String>()
                })
                .unwrap_or_default(),
            Provider::OpenAi => body["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        };
        if text.trim().is_empty() {
            bail!("model returned no text");
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model() {
        let c = LlmConfig::parse("anthropic:claude-haiku-5-5", None, Some("k")).unwrap();
        assert_eq!(c.provider, Provider::Anthropic);
        assert_eq!(c.model, "claude-haiku-5-5");
        assert_eq!(c.base_url, "https://api.anthropic.com");
        let c = LlmConfig::parse(
            "openai:qwen/qwen3:free",
            Some("https://openrouter.ai/api/v1/"),
            Some(" "),
        )
        .unwrap();
        assert_eq!(
            (c.provider, c.model.as_str()),
            (Provider::OpenAi, "qwen/qwen3:free")
        );
        assert_eq!(c.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(c.api_key, None);
        assert!(!format!(
            "{:?}",
            LlmConfig::parse("openai:m", None, Some("secret")).unwrap()
        )
        .contains("secret"));
        for bad in ["haiku", "google:gemini", "openai:", ""] {
            assert!(LlmConfig::parse(bad, None, None).is_err(), "{bad}");
        }
        assert!(LlmConfig::parse("openai:m", Some("ftp://x"), None).is_err());
    }
}
