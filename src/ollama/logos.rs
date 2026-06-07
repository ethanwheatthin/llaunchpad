//! Mapping from model name → provider slug.
//!
//! The Slint UI (`ProviderLogo` in `ui/app.slint`) shows a logo next to
//! each model in the dropdown based on the cloud provider that ships
//! it. The mapping is intentionally lossy: model families that come
//! from the same lab (e.g. `gpt-*`, `o1-*`, `o3-*` are all OpenAI) are
//! folded into one provider. Add new branches here when ollama adds
//! models from a new family.

/// Map a launchable model name to its provider slug. The result is
/// stable (one of the keys in `ProviderLogo` in `app.slint`) and is
/// used both to look up the right PNG and to flag "this is an ollama
/// local model" vs "this is a cloud partner model".
pub fn provider_for_model(name: &str) -> &'static str {
    let n = name.split(':').next().unwrap_or(name);
    if n.starts_with("gpt-")
        || n.starts_with("o1")
        || n.starts_with("o2")
        || n.starts_with("o3")
        || n.starts_with("o4")
    {
        "openai"
    } else if n.starts_with("gemini") {
        "gemini"
    } else if n.starts_with("gemma") {
        "gemma"
    } else if n.starts_with("mistral") || n.starts_with("ministral") || n.starts_with("devstral") {
        "mistral"
    } else if n.starts_with("deepseek") {
        "deepseek"
    } else if n.starts_with("qwen") {
        "qwen"
    } else if n.starts_with("glm") {
        "zhipu"
    } else if n.starts_with("kimi") || n.starts_with("moonshot") {
        "moonshot"
    } else if n.starts_with("nemotron") {
        "nvidia"
    } else if n.starts_with("minimax") {
        "minimax"
    } else {
        "ollama"
    }
}
