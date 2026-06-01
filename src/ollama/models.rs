use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
}

// ---- Ollama Cloud catalog (OpenAI-compatible /v1/models) ----
#[derive(Deserialize)]
struct OpenAiModels {
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
}

const CLOUD_MODELS_URL: &str = "https://ollama.com/v1/models";

/// Convert a cloud catalog id into the name ollama uses to run it.
/// Tagged names append `-cloud` (gpt-oss:120b -> gpt-oss:120b-cloud);
/// untagged names append `:cloud` (glm-4.6 -> glm-4.6:cloud).
fn to_cloud_ref(id: &str) -> String {
    // already a cloud ref? leave as-is
    if id.ends_with("-cloud") || id.ends_with(":cloud") {
        return id.to_string();
    }
    if id.contains(':') {
        format!("{id}-cloud")
    } else {
        format!("{id}:cloud")
    }
}

/// Full cloud model catalog from the Ollama subscription, as launchable names.
/// Listing does not require an API key.
pub async fn list_cloud_models() -> Result<Vec<Model>> {
    let resp = reqwest::Client::new()
        .get(CLOUD_MODELS_URL)
        .send()
        .await
        .context("failed to reach ollama.com cloud catalog")?;
    let parsed: OpenAiModels = resp.json().await.context("invalid /v1/models response")?;
    let mut models: Vec<Model> = parsed
        .data
        .into_iter()
        .map(|m| Model { name: to_cloud_ref(&m.id) })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

// ---- Ollama local server catalog (/api/tags) ----
#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<LocalModel>,
}

#[derive(Deserialize)]
struct LocalModel {
    name: String,
}

/// Build the `/api/tags` URL for a given base, tolerating a trailing slash.
fn tags_url(base_url: &str) -> String {
    format!("{}/api/tags", base_url.trim_end_matches('/'))
}

/// Parse an `/api/tags` response body into a sorted list of model names.
fn parse_tags(body: &str) -> Result<Vec<Model>> {
    let parsed: TagsResponse = serde_json::from_str(body).context("invalid /api/tags response")?;
    let mut models: Vec<Model> = parsed.models.into_iter().map(|m| Model { name: m.name }).collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

/// Fetch the list of models available on a local/remote Ollama server.
pub async fn list_local_models(base_url: &str) -> Result<Vec<Model>> {
    let url = tags_url(base_url);
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("server returned {}", resp.status());
    }
    let body = resp.text().await.context("could not read /api/tags response")?;
    parse_tags(&body)
}

/// Test connectivity to a local/remote Ollama server by hitting its `/api/version` endpoint.
/// Returns a short description string on success (e.g. "Ollama 0.6.5").
pub async fn test_connection(base_url: &str) -> Result<String> {
    let url = format!("{}/api/version", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("server returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await.context("unexpected response body")?;
    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    Ok(format!("Connected — Ollama {version}"))
}

#[cfg(test)]
mod tests {
    use super::{parse_tags, tags_url, to_cloud_ref};

    #[test]
    fn cloud_ref_rule() {
        assert_eq!(to_cloud_ref("gpt-oss:120b"), "gpt-oss:120b-cloud");
        assert_eq!(to_cloud_ref("glm-4.6"), "glm-4.6:cloud");
        assert_eq!(to_cloud_ref("deepseek-v4-pro"), "deepseek-v4-pro:cloud");
        assert_eq!(to_cloud_ref("gemma4:31b-cloud"), "gemma4:31b-cloud");
        assert_eq!(to_cloud_ref("deepseek-v4-pro:cloud"), "deepseek-v4-pro:cloud");
    }

    #[test]
    fn tags_url_handles_trailing_slash() {
        assert_eq!(tags_url("http://localhost:11434"), "http://localhost:11434/api/tags");
        assert_eq!(tags_url("http://localhost:11434/"), "http://localhost:11434/api/tags");
        assert_eq!(tags_url("http://host:11434///"), "http://host:11434/api/tags");
    }

    #[test]
    fn parse_tags_sorts_and_extracts_names() {
        let body = r#"{"models":[{"name":"llama3:8b","size":1},{"name":"gemma:2b"}]}"#;
        let names: Vec<String> = parse_tags(body).unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["gemma:2b", "llama3:8b"]);
    }

    #[test]
    fn parse_tags_empty_list() {
        let names = parse_tags(r#"{"models":[]}"#).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn parse_tags_rejects_malformed_body() {
        assert!(parse_tags("not json").is_err());
        assert!(parse_tags(r#"{"unexpected":true}"#).is_err());
    }
}
