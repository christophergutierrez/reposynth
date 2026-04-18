use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use anyhow::Result;
use regex::Regex;

use crate::config::Config;

pub struct HealthReport {
    pub total_records: usize,
    pub assistant_turns: usize,
    pub language_results: HashMap<String, LangHealth>,
    pub passed: bool,
}

pub struct LangHealth {
    pub code_fences: usize,
    pub func_start_count: usize,
    pub func_start_pct: f64,
    pub func_start_required: Option<f64>,  // None = not configured (auto-pass)
    pub pattern_coverage: HashMap<String, usize>,
    pub min_coverage_required: usize,
    pub passed: bool,
}

/// Run a data health check on a JSONL file and return a report.
pub fn check(data_file: &Path, config: &Config) -> Result<HealthReport> {
    let fence_re = Regex::new(r"```(\w*)\n([\s\S]*?)```")?;

    let mut total_records = 0usize;
    let mut assistant_turns = 0usize;

    // Per-language accumulators
    struct LangAccum {
        fences: usize,
        func_starts: usize,
        pattern_hits: HashMap<String, usize>,
    }

    let mut lang_accums: HashMap<String, LangAccum> = HashMap::new();

    // Tracked Go patterns
    let go_patterns = [
        "NamedQueryContext",
        "StructScan",
        "NamedExecContext",
        "sqlx.In",
        "NotFoundError",
        "valog.Logger",
        "WithTimeout",
    ];

    let reader = {
        let f = std::fs::File::open(data_file)
            .map_err(|e| anyhow::anyhow!("Cannot open {}: {}", data_file.display(), e))?;
        std::io::BufReader::new(f)
    };

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        total_records += 1;

        // Extract assistant content from either format
        let assistant_content = extract_assistant_content(&record);
        if assistant_content.is_empty() {
            continue;
        }
        assistant_turns += 1;

        // Process each code fence
        for cap in fence_re.captures_iter(&assistant_content) {
            let lang_tag = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_lowercase();
            let body = cap.get(2).map(|m| m.as_str()).unwrap_or("");

            // Determine language
            let lang = if lang_tag == "go" || (lang_tag.is_empty() && config.languages.contains(&"go".to_string())) {
                "go"
            } else if lang_tag == "python" || lang_tag == "py" {
                "python"
            } else if lang_tag.is_empty() {
                continue;
            } else {
                &lang_tag
            };

            let accum = lang_accums.entry(lang.to_string()).or_insert(LangAccum {
                fences: 0,
                func_starts: 0,
                pattern_hits: HashMap::new(),
            });

            accum.fences += 1;

            // Check start marker
            let first_line = body.lines().next().unwrap_or("").trim_start();
            let is_func_start = match lang {
                "go" => first_line.starts_with("func ") || first_line.starts_with("func("),
                "python" | "py" => first_line.starts_with("def ") || first_line.starts_with("async def ") || first_line.starts_with("class "),
                "rust" => first_line.starts_with("pub fn ") || first_line.starts_with("fn ") || first_line.starts_with("pub struct "),
                _ => true,
            };
            if is_func_start {
                accum.func_starts += 1;
            }

            // Count pattern hits (Go-specific for now)
            if lang == "go" {
                for pattern in &go_patterns {
                    if body.contains(pattern) {
                        *accum.pattern_hits.entry(pattern.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Build per-language results
    let mut language_results: HashMap<String, LangHealth> = HashMap::new();
    let mut all_passed = true;

    for (lang, accum) in &lang_accums {
        // Only check languages that have an explicit health config entry.
        // Languages without a config entry are not checked (auto-pass).
        let lang_health_cfg = config
            .health
            .as_ref()
            .and_then(|h| h.get(lang.as_str()));

        let threshold = lang_health_cfg.and_then(|c| c.func_start_pct);
        let min_coverage = lang_health_cfg.and_then(|c| c.min_pattern_coverage).unwrap_or(20);

        let pct = if accum.fences > 0 {
            (accum.func_starts as f64 / accum.fences as f64) * 100.0
        } else {
            0.0
        };

        let patterns_ok = accum
            .pattern_hits
            .values()
            .all(|&v| v >= min_coverage);

        // Pass if: no threshold configured, OR pct meets threshold AND patterns ok
        let lang_passed = match threshold {
            None => true,  // no config for this language — skip check
            Some(t) => pct >= t && (accum.pattern_hits.is_empty() || patterns_ok),
        };
        if !lang_passed {
            all_passed = false;
        }

        language_results.insert(
            lang.clone(),
            LangHealth {
                code_fences: accum.fences,
                func_start_count: accum.func_starts,
                func_start_pct: pct,
                func_start_required: threshold,
                pattern_coverage: accum.pattern_hits.clone(),
                min_coverage_required: min_coverage,
                passed: lang_passed,
            },
        );
    }

    Ok(HealthReport {
        total_records,
        assistant_turns,
        language_results,
        passed: all_passed,
    })
}

pub fn print_report(report: &HealthReport) {
    println!("\n=== reposynth data health check ===");
    println!("Total records:    {}", report.total_records);
    println!("Assistant turns:  {}", report.assistant_turns);

    if report.language_results.is_empty() {
        println!("\nNo code fences detected. Is this the right file?");
        return;
    }

    for (lang, result) in &report.language_results {
        // Skip unconfigured languages unless they have pattern coverage data to show
        if result.func_start_required.is_none() && result.pattern_coverage.is_empty() {
            continue;
        }

        println!("\n--- {} ---", lang.to_uppercase());

        if let Some(required) = result.func_start_required {
            println!(
                "Code fences:      {} ({} start with func/def/fn — {:.1}%{})",
                result.code_fences,
                result.func_start_count,
                result.func_start_pct,
                if result.func_start_pct >= required { " ✓" } else { " ✗" }
            );
            println!("Required:         {:.0}%", required);
        } else {
            println!(
                "Code fences:      {} ({} start with func/def/fn — {:.1}%) [not configured]",
                result.code_fences,
                result.func_start_count,
                result.func_start_pct,
            );
        }

        if !result.pattern_coverage.is_empty() {
            println!("Pattern coverage (min {}):", result.min_coverage_required);
            let mut patterns: Vec<_> = result.pattern_coverage.iter().collect();
            patterns.sort_by_key(|(k, _)| k.as_str());
            for (pattern, count) in patterns {
                let marker = if *count >= result.min_coverage_required { "✓" } else { "✗" };
                println!("  {marker} {pattern}: {count}");
            }
        }

        if result.func_start_required.is_some() {
            let verdict = if result.passed { "PASS" } else { "FAIL" };
            println!("Result: {verdict}");
        }
    }

    println!();
    if report.passed {
        println!("READY TO TRAIN ✓");
    } else {
        println!("NOT READY — fix failing checks above, then re-run: reposynth check");
    }
    println!();
}

fn extract_assistant_content(record: &serde_json::Value) -> String {
    // Messages format
    if let Some(messages) = record.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                    return content.to_string();
                }
            }
        }
    }

    // ShareGPT format
    if let Some(convos) = record.get("conversations").and_then(|v| v.as_array()) {
        for turn in convos {
            if turn.get("from").and_then(|r| r.as_str()) == Some("gpt") {
                if let Some(value) = turn.get("value").and_then(|c| c.as_str()) {
                    return value.to_string();
                }
            }
        }
    }

    String::new()
}
