// Default trait method impls may not be used in the production wiring.
#![allow(dead_code)]
//! Data access layer.
//!
//! The repository is the only place the rest of the app touches Ollama or
//! the local process table. The Model talks to a `dyn Repository`; the
//! concrete `OllamaRepository` delegates to the `crate::ollama` module.
//! Tests can substitute an in-memory fake to drive the Model deterministically.

use crate::ollama::{
    installed_states, launch_agent, list_agents, list_cloud_models, list_local_models,
    restore_agent, restore_available, running_states, test_connection, Agent, Model,
};
use anyhow::Result;

/// Snapshot of "what does the world look like right now?" — the canonical
/// input the Model turns into a `StateSnapshot` for the View.
#[derive(Clone, Debug)]
pub struct WorldSnapshot {
    pub agents: Vec<Agent>,
    pub running: Vec<bool>,
    pub installed: Vec<bool>,
    pub cloud_models: Vec<String>,
}

/// What the test-connection flow returns. Kept separate from `WorldSnapshot`
/// because the result of a Test is what updates local models, not the agents.
#[derive(Clone, Debug)]
pub struct TestResult {
    pub info: String,
    pub local_models: Vec<String>,
}

/// Repository abstracts every I/O the app does against Ollama and the
/// local process table. All methods are `async` so the Model can await
/// them uniformly; sync operations are wrapped in `spawn_blocking`.
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn list_agents(&self) -> Result<Vec<Agent>>;
    async fn list_cloud_models(&self) -> Result<Vec<Model>>;
    async fn list_local_models(&self, url: &str) -> Result<Vec<Model>>;
    async fn test_connection(&self, url: &str) -> Result<String>;
    fn running_states(&self, agents: &[Agent]) -> Vec<bool>;
    fn installed_states(&self, agents: &[Agent]) -> Vec<bool>;
    fn restore_available(&self, agent_token: &str) -> bool;
    async fn restore_agent(&self, agent_token: &str) -> Result<()>;
    async fn launch_agent(
        &self,
        agent: &Agent,
        model: &str,
        ollama_host: Option<&str>,
    ) -> Result<()>;

    /// Convenience: full refresh. Heavy work is parallelised where safe
    /// (cloud + agents can run together; running/installed must share a
    /// process-table scan so they go through `spawn_blocking` together).
    async fn fetch_world(&self) -> Result<WorldSnapshot> {
        let agents = self.list_agents().await?;
        let models = self.list_cloud_models().await?;
        let agents_for_scan = agents.clone();
        let (running, installed) = tokio::task::spawn_blocking(move || {
            // owned repo captured via &self would be cleaner, but Repository is dyn
            // and the scan needs a handle. We use static helpers via the
            // OllamaRepository concrete impl in practice — but trait callers go
            // through the default here using direct ollama helpers.
            (running_states(&agents_for_scan), installed_states(&agents_for_scan))
        })
        .await?;
        Ok(WorldSnapshot {
            agents,
            running,
            installed,
            cloud_models: models.into_iter().map(|m| m.name).collect(),
        })
    }

    async fn test(&self, url: &str) -> Result<TestResult> {
        let info = self.test_connection(url).await?;
        let local = self.list_local_models(url).await?;
        Ok(TestResult {
            info,
            local_models: local.into_iter().map(|m| m.name).collect(),
        })
    }
}

/// Production repository — thin shim over `crate::ollama`.
pub struct OllamaRepository;

#[async_trait::async_trait]
impl Repository for OllamaRepository {
    async fn list_agents(&self) -> Result<Vec<Agent>> {
        Ok(list_agents().await?)
    }
    async fn list_cloud_models(&self) -> Result<Vec<Model>> {
        Ok(list_cloud_models().await?)
    }
    async fn list_local_models(&self, url: &str) -> Result<Vec<Model>> {
        Ok(list_local_models(url).await?)
    }
    async fn test_connection(&self, url: &str) -> Result<String> {
        Ok(test_connection(url).await?)
    }
    fn running_states(&self, agents: &[Agent]) -> Vec<bool> {
        running_states(agents)
    }
    fn installed_states(&self, agents: &[Agent]) -> Vec<bool> {
        installed_states(agents)
    }
    fn restore_available(&self, agent_token: &str) -> bool {
        restore_available(agent_token)
    }
    async fn restore_agent(&self, agent_token: &str) -> Result<()> {
        let token = agent_token.to_string();
        tokio::task::spawn_blocking(move || restore_agent(&token)).await?
    }
    async fn launch_agent(
        &self,
        agent: &Agent,
        model: &str,
        ollama_host: Option<&str>,
    ) -> Result<()> {
        let agent = agent.clone();
        let model = model.to_string();
        let host = ollama_host.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || {
            launch_agent(&agent, &model, host.as_deref())
        })
        .await?
    }

    /// Override: reuse `&self` for the synchronous scan so we can call the
    /// trait methods (not the bare ollama helpers) and keep the rest of the
    /// app decoupled from the concrete type.
    async fn fetch_world(&self) -> Result<WorldSnapshot> {
        let agents = self.list_agents().await?;
        let models = self.list_cloud_models().await?;
        let agents_for_scan = agents.clone();
        let (running, installed) = tokio::task::spawn_blocking(move || {
            (running_states(&agents_for_scan), installed_states(&agents_for_scan))
        })
        .await?;
        Ok(WorldSnapshot {
            agents,
            running,
            installed,
            cloud_models: models.into_iter().map(|m| m.name).collect(),
        })
    }
}
