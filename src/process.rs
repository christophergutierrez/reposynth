use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

/// Remove the `_source` field from every record in a JSONL file.
/// Returns the number of records processed.
pub fn strip_meta(input: &Path, output: &Path) -> Result<usize> {
    let reader = open_reader(input)?;
    let mut writer = open_writer(output)?;
    let mut count = 0;

    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut record: Value =
            serde_json::from_str(line).with_context(|| format!("JSON parse error in {}", input.display()))?;
        if let Value::Object(ref mut map) = record {
            map.remove("_source");
        }
        writer.write_all(serde_json::to_string(&record)?.as_bytes())?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

/// Convert messages format (role/content) to ShareGPT format (from/value).
/// Input records must have a "messages" array.
/// Output records have a "conversations" array.
pub fn convert_sharegpt(input: &Path, output: &Path) -> Result<usize> {
    let role_map = |role: &str| -> &'static str {
        match role {
            "system" => "system",
            "user" => "human",
            "assistant" => "gpt",
            _ => "human",
        }
    };

    let reader = open_reader(input)?;
    let mut writer = open_writer(output)?;
    let mut count = 0;

    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line)
            .with_context(|| format!("JSON parse error in {}", input.display()))?;

        // If already in ShareGPT format, pass through
        if record.get("conversations").is_some() {
            writer.write_all(serde_json::to_string(&record)?.as_bytes())?;
            writer.write_all(b"\n")?;
            count += 1;
            continue;
        }

        let messages = match record.get("messages").and_then(|v| v.as_array()) {
            Some(m) => m.clone(),
            None => continue,
        };

        let conversations: Vec<Value> = messages
            .iter()
            .filter_map(|m| {
                let role = m.get("role")?.as_str()?;
                let content = m.get("content")?.clone();
                Some(serde_json::json!({
                    "from": role_map(role),
                    "value": content,
                }))
            })
            .collect();

        let out = serde_json::json!({"conversations": conversations});
        writer.write_all(serde_json::to_string(&out)?.as_bytes())?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

/// Normalize assistant responses: extract code fences, strip language-specific
/// boilerplate (package declarations, imports) before the first function definition.
/// Returns the number of records processed.
pub fn normalize(input: &Path, output: &Path, languages: &[String]) -> Result<usize> {
    let reader = open_reader(input)?;
    let mut writer = open_writer(output)?;
    let fence_re = Regex::new(r"```(\w*)\n([\s\S]*?)```")?;
    let mut count = 0;

    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut record: Value = serde_json::from_str(line)
            .with_context(|| format!("JSON parse error in {}", input.display()))?;

        // Handle both messages and conversations format
        normalize_record(&mut record, &fence_re, languages);

        writer.write_all(serde_json::to_string(&record)?.as_bytes())?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

fn normalize_record(record: &mut Value, fence_re: &Regex, languages: &[String]) {
    // Find the assistant turn in either format
    if let Some(messages) = record.get_mut("messages").and_then(|v| v.as_array_mut()) {
        for msg in messages.iter_mut() {
            let is_assistant = msg.get("role").and_then(|r| r.as_str()) == Some("assistant");
            if is_assistant {
                if let Some(content) = msg.get_mut("content").and_then(|v| v.as_str()) {
                    let normalized = normalize_content(content, fence_re, languages);
                    msg["content"] = Value::String(normalized);
                }
            }
        }
    } else if let Some(convos) = record.get_mut("conversations").and_then(|v| v.as_array_mut()) {
        for turn in convos.iter_mut() {
            let is_gpt = turn.get("from").and_then(|r| r.as_str()) == Some("gpt");
            if is_gpt {
                if let Some(value) = turn.get_mut("value").and_then(|v| v.as_str()) {
                    let normalized = normalize_content(value, fence_re, languages);
                    turn["value"] = Value::String(normalized);
                }
            }
        }
    }
}

fn normalize_content(content: &str, fence_re: &Regex, languages: &[String]) -> String {
    // Find all code fences in the content
    let fences: Vec<_> = fence_re.captures_iter(content).collect();
    if fences.is_empty() {
        return content.to_string();
    }

    // Find the first and last fence, keep everything between them (inclusive)
    let first_match = fence_re.find(content);
    let last_match = {
        let mut last = None;
        for m in fence_re.find_iter(content) {
            last = Some(m);
        }
        last
    };

    if let (Some(first), Some(last)) = (first_match, last_match) {
        let body = &content[first.start()..last.end()];
        // Apply language-specific normalization to the body
        apply_language_normalization(body, fence_re, languages)
    } else {
        content.to_string()
    }
}

fn apply_language_normalization(body: &str, fence_re: &Regex, languages: &[String]) -> String {
    let mut result = body.to_string();

    for cap in fence_re.captures_iter(body) {
        let full_match = cap.get(0).unwrap().as_str();
        let lang_tag = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let fence_body = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        let normalized_body = if is_go_fence(lang_tag, languages) {
            normalize_go_body(fence_body)
        } else {
            fence_body.to_string()
        };

        let replacement = format!("```{}\n{}\n```", lang_tag, normalized_body.trim_end());
        result = result.replacen(full_match, &replacement, 1);
    }

    result
}

fn is_go_fence(lang_tag: &str, languages: &[String]) -> bool {
    lang_tag == "go"
        || (lang_tag.is_empty() && languages.iter().any(|l| l == "go"))
}

/// Strip package declaration, import block, and leading comments before the
/// first top-level `func` (or `type`/`var`) definition.
fn normalize_go_body(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();

    // Find the first line that starts a top-level declaration
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("func ")
            || trimmed.starts_with("func(")
            || trimmed.starts_with("type ")
            || trimmed.starts_with("var ")
            || trimmed.starts_with("const ")
        {
            return lines[i..].join("\n");
        }
    }

    // No top-level declaration found — return as-is
    body.to_string()
}

/// Concatenate multiple JSONL files into one, returning total record count.
pub fn combine_files(inputs: &[&Path], output: &Path) -> Result<usize> {
    let mut writer = open_writer(output)?;
    let mut total = 0;

    for input in inputs {
        if !input.exists() {
            continue;
        }
        let reader = open_reader(input)?;
        for line in reader.lines() {
            let line = line.context("Failed to read line")?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Validate it's valid JSON
            let _: Value = serde_json::from_str(line)
                .with_context(|| format!("JSON parse error in {}", input.display()))?;
            writer.write_all(line.as_bytes())?;
            writer.write_all(b"\n")?;
            total += 1;
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_reader(path: &Path) -> Result<std::io::BufReader<std::fs::File>> {
    let f = std::fs::File::open(path)
        .with_context(|| format!("Cannot open {}", path.display()))?;
    Ok(std::io::BufReader::new(f))
}

fn open_writer(path: &Path) -> Result<std::io::BufWriter<std::fs::File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = std::fs::File::create(path)
        .with_context(|| format!("Cannot create {}", path.display()))?;
    Ok(std::io::BufWriter::new(f))
}
