use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub languages: Vec<String>,
    pub provider: ProviderConfig,
    pub conventions_dir: Option<String>,
    pub patterns_file: Option<String>,
    pub contrast_file: Option<String>,
    pub output_dir: Option<String>,
    pub generate: Option<GenerateConfig>,
    pub health: Option<HashMap<String, HealthConfig>>,
    pub codebase_context: Option<String>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GenerateConfig {
    pub rules_per_file: Option<usize>,
    pub booster_n: Option<usize>,
    pub concurrency: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthConfig {
    pub func_start_pct: Option<f64>,
    pub min_pattern_coverage: Option<usize>,
}

impl Config {
    pub fn conventions_dir(&self) -> &str {
        self.conventions_dir.as_deref().unwrap_or(".claude/rules")
    }

    pub fn output_dir(&self) -> PathBuf {
        PathBuf::from(self.output_dir.as_deref().unwrap_or(".reposynth/data"))
    }

    pub fn patterns_file(&self) -> PathBuf {
        PathBuf::from(
            self.patterns_file
                .as_deref()
                .unwrap_or(".reposynth/patterns/go.yaml"),
        )
    }

    pub fn contrast_file(&self) -> PathBuf {
        PathBuf::from(
            self.contrast_file
                .as_deref()
                .unwrap_or(".reposynth/patterns/contrast.yaml"),
        )
    }

    pub fn rules_per_file(&self) -> usize {
        self.generate.as_ref().and_then(|g| g.rules_per_file).unwrap_or(5)
    }

    pub fn booster_n(&self) -> usize {
        self.generate.as_ref().and_then(|g| g.booster_n).unwrap_or(8)
    }

    pub fn concurrency(&self) -> usize {
        self.generate.as_ref().and_then(|g| g.concurrency).unwrap_or(3)
    }
}

pub fn load_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;
    let config: Config = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse config: {}", path.display()))?;
    Ok(config)
}

#[allow(dead_code)]
pub fn save_config(config: &Config, path: &Path) -> Result<()> {
    let content = serde_yaml::to_string(config)
        .context("Failed to serialize config")?;
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write config: {}", path.display()))?;
    Ok(())
}

#[allow(dead_code)]
pub fn default_config(languages: Vec<String>) -> Config {
    Config {
        languages,
        provider: ProviderConfig {
            provider_type: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            base_url: None,
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
        },
        conventions_dir: Some(".claude/rules".to_string()),
        patterns_file: Some(".reposynth/patterns/go.yaml".to_string()),
        contrast_file: None,
        output_dir: Some(".reposynth/data".to_string()),
        generate: Some(GenerateConfig {
            rules_per_file: Some(5),
            booster_n: Some(8),
            concurrency: Some(3),
        }),
        health: Some({
            let mut m = HashMap::new();
            m.insert(
                "go".to_string(),
                HealthConfig {
                    func_start_pct: Some(85.0),
                    min_pattern_coverage: Some(20),
                },
            );
            m
        }),
        codebase_context: None,
        system_prompt: None,
    }
}
