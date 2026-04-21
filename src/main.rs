mod config;
mod detect;
mod health;
mod process;
mod runner;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};

// ---------------------------------------------------------------------------
// Embedded template files
// ---------------------------------------------------------------------------

const SYNTH_YAML_TPL: &str = include_str!("../templates/synth.yaml.tpl");
const GO_PATTERNS_YAML: &str = include_str!("../templates/patterns/go.yaml");
const PYTHON_PATTERNS_YAML: &str = include_str!("../templates/patterns/python.yaml");

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "reposynth",
    about = "Generate synthetic LLM fine-tuning data from any repo's conventions",
    version
)]
struct Cli {
    /// Path to synth.yaml (defaults to ./synth.yaml)
    #[arg(short, long, default_value = "synth.yaml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Detect languages, write synth.yaml, and copy default pattern templates
    Init {
        /// Force overwrite of existing synth.yaml
        #[arg(short, long)]
        force: bool,
    },

    /// Run the full data generation pipeline
    Generate {
        /// Run only 'rules', 'booster', or 'contrast' (default: all)
        #[arg(long, value_parser = ["rules", "booster", "contrast"])]
        only: Option<String>,

        /// Resume an interrupted rules generation run
        #[arg(long)]
        resume: bool,

        /// Skip the health check after generation
        #[arg(long)]
        skip_check: bool,

        /// Enable verbose LLM logging
        #[arg(long)]
        verbose: bool,

        /// Run the booster N times and deduplicate across passes (default: 1)
        #[arg(long, default_value_t = 1)]
        passes: u32,
    },

    /// Build a holdout eval set from real repo functions
    Holdout {
        /// Path to candidates YAML (default: .reposynth/holdout_candidates.yaml)
        #[arg(long)]
        candidates: Option<PathBuf>,

        /// Output file (default: .reposynth/data/holdout_<date>.jsonl)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Enable verbose logging
        #[arg(long)]
        verbose: bool,
    },

    /// Run a data quality health check on a JSONL file
    Check {
        /// JSONL file to check (default: latest combined_*.jsonl in output_dir)
        file: Option<PathBuf>,
    },

    /// Normalize assistant responses in a JSONL file (strip preambles, extract fences)
    Clean {
        /// Input JSONL file
        input: PathBuf,

        /// Output JSONL file (default: <input>_clean.jsonl)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Show where reposynth stores its Python scripts
    ScriptsDir,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_config(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    bail!(
        "Config not found: {}\nRun `reposynth init` to create it.",
        path.display()
    )
}

fn load_config(config_path: &Path) -> Result<config::Config> {
    config::load_config(config_path)
}

fn repo_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn latest_combined(output_dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(output_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("combined_") && name.ends_with(".jsonl")
        })
        .collect();
    files.sort_by_key(|e| e.file_name());
    files.last().map(|e| e.path())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_init(config_path: &Path, force: bool) -> Result<()> {
    if config_path.exists() && !force {
        bail!(
            "synth.yaml already exists. Use --force to overwrite, or edit it directly."
        );
    }

    let repo = repo_root();
    println!("Detecting languages in {}...", repo.display());
    let languages = detect::detect_languages(&repo);

    if languages.is_empty() {
        println!("No recognized source files found. Edit synth.yaml to set languages manually.");
    } else {
        println!("Detected: {}", languages.join(", "));
    }

    // Build language list yaml snippet
    let lang_yaml = if languages.is_empty() {
        "  - go  # add your languages here".to_string()
    } else {
        languages.iter().map(|l| format!("  - {l}")).collect::<Vec<_>>().join("\n")
    };

    let synth_yaml = SYNTH_YAML_TPL.replace("{{languages}}", &lang_yaml);
    std::fs::write(config_path, &synth_yaml)
        .with_context(|| format!("Cannot write {}", config_path.display()))?;
    println!("Wrote {}", config_path.display());

    // Copy pattern templates for detected languages
    let patterns_base = repo.join(".reposynth").join("patterns");
    std::fs::create_dir_all(&patterns_base)?;

    let templates = [
        ("go", GO_PATTERNS_YAML, "go.yaml"),
        ("python", PYTHON_PATTERNS_YAML, "python.yaml"),
    ];

    for (lang, content, filename) in &templates {
        if languages.iter().any(|l| l == lang) || languages.is_empty() {
            let dest = patterns_base.join(filename);
            if !dest.exists() {
                std::fs::write(&dest, content)?;
                println!("Wrote {}", dest.display());
            }
        }
    }

    // Scaffold holdout candidates file
    let candidates_path = repo.join(".reposynth").join("holdout_candidates.yaml");
    if !candidates_path.exists() {
        std::fs::write(
            &candidates_path,
            "# Holdout eval candidates — real functions from your repo.\n\
             # Each entry: file path, function name, and optional convention tags.\n\
             # Run: reposynth holdout  after filling this in.\n\
             #\n\
             # Example:\n\
             # - file: myservice/repository/get.go\n\
             #   func: GetByID\n\
             #   tags: [sqlx, error_wrapping]\n",
        )?;
        println!("Wrote {}", candidates_path.display());
    }

    println!("\nNext steps:");
    println!("  1. Edit synth.yaml — set codebase_context with your import paths and conventions");
    println!("  2. Edit .reposynth/patterns/*.yaml — add/remove patterns for your codebase");
    println!("  3. reposynth generate");
    Ok(())
}

fn cmd_generate(
    cfg: &config::Config,
    only: Option<&str>,
    resume: bool,
    skip_check: bool,
    verbose: bool,
    passes: u32,
) -> Result<()> {
    let repo = repo_root();
    let output_dir = repo.join(cfg.output_dir());
    std::fs::create_dir_all(&output_dir)?;

    let date = Local::now().format("%Y%m%d").to_string();
    let run_rules = only.map_or(true, |o| o == "rules");
    let run_booster = only.map_or(true, |o| o == "booster");
    let run_contrast = only.map_or(true, |o| o == "contrast");

    // Temp files for intermediate steps
    let rules_raw = output_dir.join("rules_raw.jsonl");
    let booster_raw = output_dir.join("booster_raw.jsonl");
    let contrast_raw = output_dir.join("contrast_raw.jsonl");

    // Step 1: Generate from rules
    if run_rules {
        println!("Generating rule-based examples...");
        let script_cfg = runner::build_script_config(
            cfg,
            &repo,
            &rules_raw,
            resume,
            verbose,
            None,
        );
        runner::run_script("generate.py", &script_cfg)?;
    }

    // Step 2: Generate booster examples (one or more passes)
    if run_booster {
        let total_passes = passes.max(1);
        let mut pass_files: Vec<PathBuf> = Vec::new();

        for pass in 1..=total_passes {
            let pass_file = if total_passes == 1 {
                booster_raw.clone()
            } else {
                output_dir.join(format!("booster_raw_pass{pass}.jsonl"))
            };
            if total_passes == 1 {
                println!("Generating booster examples...");
            } else {
                println!("Generating booster examples (pass {pass}/{total_passes})...");
            }
            let script_cfg = runner::build_script_config(
                cfg,
                &repo,
                &pass_file,
                false,
                verbose,
                None,
            );
            runner::run_script("booster.py", &script_cfg)?;
            if pass_file.exists() {
                pass_files.push(pass_file);
            }
        }

        // Concatenate pass files into booster_raw.jsonl when running multiple passes
        if total_passes > 1 {
            let pass_refs: Vec<&Path> = pass_files.iter().map(|p| p.as_path()).collect();
            let combined_count = process::combine_files(&pass_refs, &booster_raw)?;
            println!("Combined {total_passes} passes: {combined_count} raw examples");
            for f in &pass_files {
                let _ = std::fs::remove_file(f);
            }
        }
    }

    // Step 2b: Generate contrast examples (wrong → correct pairs)
    if run_contrast {
        println!("Generating contrast examples...");
        let script_cfg = runner::build_script_config(
            cfg,
            &repo,
            &contrast_raw,
            false,
            verbose,
            None,
        );
        runner::run_script("contrast.py", &script_cfg)?;
    }

    // Step 3: Process each file — strip meta, convert to ShareGPT, normalize
    let mut inputs_to_combine: Vec<PathBuf> = Vec::new();

    if run_rules && rules_raw.exists() {
        println!("Processing rules output...");
        let processed = process_pipeline(&rules_raw, &output_dir, "rules", &cfg.languages)?;
        inputs_to_combine.push(processed);
    }

    if run_booster && booster_raw.exists() {
        println!("Processing booster output...");
        let processed = process_pipeline(&booster_raw, &output_dir, "booster", &cfg.languages)?;

        // Deduplicate across passes when more than one pass was run
        let final_processed = if passes > 1 {
            let deduped = output_dir.join("booster_raw_deduped_clean.jsonl");
            let (total, kept) = process::dedup(&processed, &deduped)?;
            let removed = total - kept;
            println!("  [booster] dedup: {kept}/{total} kept ({removed} duplicates removed)");
            let _ = std::fs::remove_file(&processed);
            deduped
        } else {
            processed
        };

        inputs_to_combine.push(final_processed);
    }

    if run_contrast && contrast_raw.exists() {
        println!("Processing contrast output...");
        let processed = process_pipeline(&contrast_raw, &output_dir, "contrast", &cfg.languages)?;
        inputs_to_combine.push(processed);
    }

    if inputs_to_combine.is_empty() {
        bail!("No data generated — check errors above.");
    }

    // Step 4: Combine into versioned dataset
    let combined_path = output_dir.join(format!("combined_{date}_clean.jsonl"));
    let refs: Vec<&Path> = inputs_to_combine.iter().map(|p| p.as_path()).collect();
    let total = process::combine_files(&refs, &combined_path)?;
    println!("Combined {} records → {}", total, combined_path.display());

    // Clean up temp files
    for raw in &[&rules_raw, &booster_raw, &contrast_raw] {
        if raw.exists() {
            let _ = std::fs::remove_file(raw);
        }
    }
    for input in &inputs_to_combine {
        if input != &combined_path {
            let _ = std::fs::remove_file(input);
        }
    }

    // Step 5: Health check
    if !skip_check {
        println!("\nRunning health check...");
        let report = health::check(&combined_path, cfg)?;
        health::print_report(&report);
    }

    println!("\nOutput: {}", combined_path.display());
    println!("Next: scp to training machine and run cycle.py --version {date}");
    Ok(())
}

/// strip_meta → convert_sharegpt → normalize, returns path to final file
fn process_pipeline(
    raw: &Path,
    output_dir: &Path,
    label: &str,
    languages: &[String],
) -> Result<PathBuf> {
    let stem = raw.file_stem().unwrap_or_default().to_string_lossy();
    let nometa = output_dir.join(format!("{stem}_nometa.jsonl"));
    let sharegpt = output_dir.join(format!("{stem}_sharegpt.jsonl"));
    let clean = output_dir.join(format!("{stem}_clean.jsonl"));

    let n1 = process::strip_meta(raw, &nometa)?;
    println!("  [{label}] strip_meta: {n1} records");

    let n2 = process::convert_sharegpt(&nometa, &sharegpt)?;
    println!("  [{label}] convert_sharegpt: {n2} records");

    let n3 = process::normalize(&sharegpt, &clean, languages)?;
    println!("  [{label}] normalize: {n3} records");

    // Clean up intermediate files
    let _ = std::fs::remove_file(&nometa);
    let _ = std::fs::remove_file(&sharegpt);

    Ok(clean)
}

fn cmd_holdout(
    cfg: &config::Config,
    candidates: Option<&Path>,
    output: Option<&Path>,
    verbose: bool,
) -> Result<()> {
    let repo = repo_root();
    let output_dir = repo.join(cfg.output_dir());
    std::fs::create_dir_all(&output_dir)?;

    let date = Local::now().format("%Y%m%d").to_string();
    let candidates_path = candidates
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo.join(".reposynth").join("holdout_candidates.yaml"));
    let output_path = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| output_dir.join(format!("holdout_{date}.jsonl")));

    let mut extra = serde_json::Map::new();
    extra.insert(
        "candidates_file".into(),
        candidates_path.to_string_lossy().into(),
    );

    let script_cfg = runner::build_script_config(cfg, &repo, &output_path, false, verbose, Some(extra));
    runner::run_script("holdout.py", &script_cfg)?;
    println!("Holdout written to {}", output_path.display());
    Ok(())
}

fn cmd_check(cfg: &config::Config, file: Option<&Path>) -> Result<()> {
    let repo = repo_root();
    let output_dir = repo.join(cfg.output_dir());

    let data_file = match file {
        Some(f) => f.to_path_buf(),
        None => {
            latest_combined(&output_dir)
                .with_context(|| format!("No combined_*.jsonl found in {}", output_dir.display()))?
        }
    };

    println!("Checking {}", data_file.display());
    let report = health::check(&data_file, cfg)?;
    health::print_report(&report);
    Ok(())
}

fn cmd_clean(input: &Path, output: Option<&Path>, cfg: &config::Config) -> Result<()> {
    let default_out = {
        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        input
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{stem}_clean.jsonl"))
    };
    let output = output.unwrap_or(&default_out);
    let n = process::normalize(input, output, &cfg.languages)?;
    println!("Normalized {n} records → {}", output.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { force } => {
            cmd_init(&cli.config, force)?;
        }

        Commands::Generate { only, resume, skip_check, verbose, passes } => {
            let config_path = find_config(&cli.config)?;
            let cfg = load_config(&config_path)?;
            runner::check_python_deps()?;
            cmd_generate(&cfg, only.as_deref(), resume, skip_check, verbose, passes)?;
        }

        Commands::Holdout { candidates, output, verbose } => {
            let config_path = find_config(&cli.config)?;
            let cfg = load_config(&config_path)?;
            runner::check_python_deps()?;
            cmd_holdout(&cfg, candidates.as_deref(), output.as_deref(), verbose)?;
        }

        Commands::Check { file } => {
            let config_path = find_config(&cli.config)?;
            let cfg = load_config(&config_path)?;
            cmd_check(&cfg, file.as_deref())?;
        }

        Commands::Clean { input, output } => {
            let config_path = find_config(&cli.config)?;
            let cfg = load_config(&config_path)?;
            let default_out = {
                let stem = input.file_stem().unwrap_or_default().to_string_lossy();
                input
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(format!("{stem}_clean.jsonl"))
            };
            let output_path = output.unwrap_or(default_out);
            cmd_clean(&input, Some(&output_path), &cfg)?;
        }

        Commands::ScriptsDir => {
            let dir = runner::scripts_dir()?;
            println!("{}", dir.display());
        }
    }

    Ok(())
}
