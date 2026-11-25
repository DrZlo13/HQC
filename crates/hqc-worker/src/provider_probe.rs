//! `provider.probe` (SPEC 0.236): what an Anthropic- or OpenAI-compatible endpoint says of its
//! models, read by the worker, which reaches the owner's network where the hub does not.
//! Settings fills a provider from its URL with it: the models for a list, the context window
//! where the endpoint gives one.
//!
//! What it asks: `GET <base>/v1/models` (llama.cpp, vLLM, LiteLLM, LM Studio, Anthropic, our
//! gpu-arbiter), then for a context window still missing llama.cpp's `GET <base>/props` and LM
//! Studio's `GET <base>/api/v0/models`, and Ollama's `GET <base>/api/tags` when there is no
//! `/v1/models`. A key the worker keeps for that `base_url` (`[providers.*] api_key`) goes with
//! it. Only http(s), no redirects, ten seconds a request, a megabyte an answer; and only names
//! and numbers go back to the hub, never the endpoint's text.

use std::time::Duration;

use hqc_proto::frame::{ProbedModel, ProviderProbe};
use serde_json::Value;

use crate::config::Config;

const MAX_MODELS: usize = 200;
const MAX_BODY: usize = 1024 * 1024;
const MAX_ID: usize = 200;

/// The models of the endpoint at `base_url`.
pub async fn probe(cfg: &Config, base_url: &str) -> ProviderProbe {
    match run(cfg, base_url).await {
        Ok(models) => ProviderProbe {
            models,
            error: None,
        },
        Err(e) => ProviderProbe {
            models: vec![],
            error: Some(e),
        },
    }
}

async fn run(cfg: &Config, base_url: &str) -> Result<Vec<ProbedModel>, String> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    let u = reqwest::Url::parse(&base).map_err(|e| format!("not a URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("the URL must be http(s) with a host".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("the URL must not carry a user or password".into());
    }
    let key = cfg
        .providers
        .iter()
        .find(|p| p.base_url == base)
        .and_then(|p| p.api_key.clone());
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let get = |path: &str| {
        let mut r = http
            .get(format!("{base}{path}"))
            .header("accept", "application/json");
        if let Some(k) = &key {
            r = r
                .header("x-api-key", k.expose())
                .bearer_auth(k.expose())
                .header("anthropic-version", "2023-06-01");
        }
        r
    };
    // `/v1/models`, or `/models` when the base already ends in `/v1`.
    let models_path = if base.ends_with("/v1") {
        "/models"
    } else {
        "/v1/models"
    };
    let mut models = match json(get(models_path)).await {
        Ok(v) => listed(&v),
        Err(first) => match json(get("/api/tags")).await {
            Ok(v) => ollama(&v),
            Err(_) => return Err(first),
        },
    };
    if models.is_empty() {
        return Err("the endpoint lists no models".into());
    }
    if models.iter().any(|m| m.context_tokens.is_none()) {
        // llama.cpp serves one model: its `n_ctx` is that model's window.
        if let Ok(p) = json(get("/props")).await {
            let n = p
                .pointer("/default_generation_settings/n_ctx")
                .or_else(|| p.get("n_ctx"))
                .and_then(tokens);
            if let (Some(n), 1) = (n, models.len()) {
                models[0].context_tokens.get_or_insert(n);
            }
        }
    }
    if models.iter().any(|m| m.context_tokens.is_none()) {
        if let Ok(v) = json(get("/api/v0/models")).await {
            for m in v
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let (Some(id), Some(n)) = (
                    m.get("id").and_then(Value::as_str),
                    m.get("loaded_context_length")
                        .or_else(|| m.get("max_context_length"))
                        .and_then(tokens),
                ) else {
                    continue;
                };
                if let Some(x) = models.iter_mut().find(|x| x.id == id) {
                    x.context_tokens.get_or_insert(n);
                }
            }
        }
    }
    Ok(models)
}

/// The JSON of a `200`, at most [`MAX_BODY`]; the reason otherwise, in a few words.
async fn json(req: reqwest::RequestBuilder) -> Result<Value, String> {
    let mut r = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "the endpoint did not answer in 10 s".to_string()
        } else if e.is_connect() {
            "the endpoint is not reachable from this worker".to_string()
        } else {
            "the request failed".to_string()
        }
    })?;
    if !r.status().is_success() {
        return Err(format!("the endpoint answered {}", r.status().as_u16()));
    }
    let mut buf = Vec::new();
    while let Some(c) = r
        .chunk()
        .await
        .map_err(|_| "the answer broke off".to_string())?
    {
        if buf.len() + c.len() > MAX_BODY {
            return Err("the answer is too large".into());
        }
        buf.extend_from_slice(&c);
    }
    serde_json::from_slice(&buf).map_err(|_| "the answer is not JSON".into())
}

/// A context window as a number of tokens, 1000 to 10 000 000, as the hub takes it.
fn tokens(v: &Value) -> Option<u32> {
    v.as_u64()
        .filter(|n| (1000..=10_000_000).contains(n))
        .map(|n| n as u32)
}

fn clean_id(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= MAX_ID && s.chars().all(|c| c.is_ascii_graphic()))
        .then(|| s.to_string())
}

/// `{data: [{id, max_model_len? (vLLM), context_length?, meta: {n_ctx_train?} (llama.cpp)}]}`.
fn listed(v: &Value) -> Vec<ProbedModel> {
    let mut out: Vec<ProbedModel> = v
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = clean_id(m.get("id").and_then(Value::as_str)?)?;
            let context_tokens = m
                .get("max_model_len")
                .or_else(|| m.get("context_length"))
                .or_else(|| m.get("context_window"))
                .or_else(|| m.pointer("/meta/n_ctx_train"))
                .and_then(tokens);
            Some(ProbedModel { id, context_tokens })
        })
        .take(MAX_MODELS)
        .collect();
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Ollama's `{models: [{name}]}`.
fn ollama(v: &Value) -> Vec<ProbedModel> {
    v.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(ProbedModel {
                id: clean_id(m.get("name").and_then(Value::as_str)?)?,
                context_tokens: None,
            })
        })
        .take(MAX_MODELS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lists_of_the_usual_servers() {
        // vLLM.
        let m = listed(&json!({"data": [{"id": "qwen3.8-27b", "max_model_len": 131072}]}));
        assert_eq!(
            m,
            [ProbedModel {
                id: "qwen3.8-27b".into(),
                context_tokens: Some(131072)
            }]
        );
        // llama.cpp, and a gpu-arbiter without numbers.
        let m = listed(
            &json!({"data": [{"id": "a", "meta": {"n_ctx_train": 262144}}, {"id": "b"}, {"id": "bad id"}, {"id": "x", "max_model_len": 12}]}),
        );
        assert_eq!(m.len(), 3, "a bad id is left out: {m:?}");
        assert_eq!(m[0].context_tokens, Some(262144));
        assert_eq!(m[1].context_tokens, None);
        assert_eq!(m[2].context_tokens, None, "an absurd window is dropped");
        // Ollama.
        assert_eq!(
            ollama(&json!({"models": [{"name": "llama3:8b"}]}))[0].id,
            "llama3:8b"
        );
    }
}
