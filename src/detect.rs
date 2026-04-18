use std::collections::HashMap;
use std::path::Path;

/// Walk a repository and return a sorted list of detected language names.
/// Languages are identified by file extension counts (top extensions win).
pub fn detect_languages(repo: &Path) -> Vec<String> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();

    let walker = walkdir::WalkDir::new(repo)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // Skip hidden dirs and common non-source dirs
            !(name.starts_with('.') && e.file_type().is_dir())
                && name != "node_modules"
                && name != "vendor"
                && name != "target"
                && name != "__pycache__"
        });

    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let lang = match ext {
            "go" => Some("go"),
            "py" => Some("python"),
            "rs" => Some("rust"),
            "ts" | "tsx" => Some("typescript"),
            "js" | "jsx" => Some("javascript"),
            "java" => Some("java"),
            "kt" | "kts" => Some("kotlin"),
            "cs" => Some("csharp"),
            "rb" => Some("ruby"),
            "php" => Some("php"),
            "swift" => Some("swift"),
            "cpp" | "cc" | "cxx" => Some("cpp"),
            "c" => Some("c"),
            _ => None,
        };

        if let Some(l) = lang {
            *counts.entry(l).or_insert(0) += 1;
        }
    }

    // Sort by file count descending, return language names
    let mut sorted: Vec<(&str, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    sorted.into_iter().map(|(lang, _)| lang.to_string()).collect()
}
