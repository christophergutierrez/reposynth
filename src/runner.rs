use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Embedded Python scripts (compiled into the binary)
// ---------------------------------------------------------------------------

const LLM_CLIENT_PY: &str = include_str!("../python/llm_client.py");
const GENERATE_PY: &str = include_str!("../python/generate.py");
const BOOSTER_PY: &str = include_str!("../python/booster.py");
const CONTRAST_PY: &str = include_str!("../python/contrast.py");
const HOLDOUT_PY: &str = include_str!("../python/holdout.py");

// ---------------------------------------------------------------------------
// Script extraction — writes embedded scripts to ~/.reposynth/python/
// ---------------------------------------------------------------------------

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("USERPROFILE").map(PathBuf::from))
        .context("Cannot determine home directory ($HOME not set)")
}

pub fn scripts_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".reposynth").join("python"))
}

/// Extract embedded Python scripts to ~/.reposynth/python/.
/// Only writes a file if its content has changed (idempotent).
pub fn ensure_scripts_extracted() -> Result<PathBuf> {
    let dir = scripts_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Cannot create scripts dir: {}", dir.display()))?;

    let scripts = [
        ("llm_client.py", LLM_CLIENT_PY),
        ("generate.py", GENERATE_PY),
        ("booster.py", BOOSTER_PY),
        ("contrast.py", CONTRAST_PY),
        ("holdout.py", HOLDOUT_PY),
    ];

    for (name, content) in &scripts {
        let path = dir.join(name);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != *content {
            std::fs::write(&path, content)
                .with_context(|| format!("Cannot write {}", path.display()))?;
        }
    }

    Ok(dir)
}

// ---------------------------------------------------------------------------
// Python invocation
// ---------------------------------------------------------------------------

/// Run a Python script with config JSON passed on stdin.
/// The script is located in the extracted scripts directory.
pub fn run_script(script_name: &str, config_json: &Value) -> Result<()> {
    let scripts_dir = ensure_scripts_extracted()?;
    let script_path = scripts_dir.join(script_name);

    // Find python3 or python
    let python = find_python()?;

    let mut child = Command::new(&python)
        .arg(&script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to spawn {python} {}", script_path.display()))?;

    // Write config JSON to stdin, then close it
    {
        let stdin = child.stdin.take().expect("stdin is piped");
        let mut stdin = std::io::BufWriter::new(stdin);
        serde_json::to_writer(&mut stdin, config_json)
            .context("Failed to write config JSON to script stdin")?;
        stdin.flush()?;
        // stdin is dropped here, closing the pipe
    }

    let status = child.wait().context("Failed to wait for script")?;
    if !status.success() {
        bail!(
            "Script {} exited with {}",
            script_name,
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

fn find_python() -> Result<String> {
    for candidate in &["python3", "python"] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Ok(candidate.to_string());
        }
    }
    bail!("Python not found. Install Python 3 and ensure it's on your PATH.")
}

// ---------------------------------------------------------------------------
// High-level run functions
// ---------------------------------------------------------------------------

/// Build the config JSON blob passed to Python scripts.
pub fn build_script_config(
    config: &Config,
    repo_root: &Path,
    output_file: &Path,
    resume: bool,
    verbose: bool,
    extra: Option<serde_json::Map<String, Value>>,
) -> Value {
    let mut obj = serde_json::Map::new();

    obj.insert("repo_root".into(), repo_root.to_string_lossy().into());
    obj.insert("output_file".into(), output_file.to_string_lossy().into());
    obj.insert("resume".into(), resume.into());
    obj.insert("verbose".into(), verbose.into());

    // Provider
    obj.insert("provider".into(), serde_json::to_value(&config.provider).unwrap());

    // Paths
    obj.insert("conventions_dir".into(), config.conventions_dir().into());
    obj.insert(
        "patterns_file".into(),
        config.patterns_file().to_string_lossy().into(),
    );
    obj.insert(
        "contrast_file".into(),
        config.contrast_file().to_string_lossy().into(),
    );

    // Generate config
    obj.insert(
        "generate".into(),
        serde_json::json!({
            "rules_per_file": config.rules_per_file(),
            "booster_n": config.booster_n(),
            "concurrency": config.concurrency(),
        }),
    );

    // Optional strings
    if let Some(ctx) = &config.codebase_context {
        obj.insert("codebase_context".into(), ctx.clone().into());
    }
    if let Some(sp) = &config.system_prompt {
        obj.insert("system_prompt".into(), sp.clone().into());
    }

    obj.insert("languages".into(), serde_json::to_value(&config.languages).unwrap());

    // Merge any extra fields
    if let Some(extra) = extra {
        obj.extend(extra);
    }

    Value::Object(obj)
}

/// Check that required Python packages are installed.
pub fn check_python_deps() -> Result<()> {
    let scripts_dir = ensure_scripts_extracted()?;
    let req_src = include_str!("../python/requirements.txt");

    // Write requirements.txt temporarily to check
    let req_path = scripts_dir.join("requirements.txt");
    std::fs::write(&req_path, req_src)?;

    let python = find_python()?;
    let output = Command::new(&python)
        .args(["-c", "import anthropic, yaml; print('ok')"])
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(()),
        _ => {
            eprintln!("Missing Python dependencies. Install them with:");
            eprintln!("  pip install -r {}", req_path.display());
            bail!("Python dependencies not installed");
        }
    }
}
