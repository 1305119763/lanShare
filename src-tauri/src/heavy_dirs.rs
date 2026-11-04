use std::path::Path;
use walkdir::WalkDir;

pub const HEAVY_DIR_NAMES: &[&str] = &[
  "node_modules",
  "bower_components",
  "vendor",
  ".git",
  ".svn",
  ".hg",
  "target",
  "dist",
  "build",
  ".next",
  ".nuxt",
  ".output",
  ".vercel",
  ".cache",
  ".parcel-cache",
  "coverage",
  "__pycache__",
  ".venv",
  "venv",
  "Pods",
  "DerivedData",
  ".gradle",
  ".idea",
  ".turbo",
  "jspm_packages",
];

pub fn is_heavy_dir_name(name: &str) -> bool {
  HEAVY_DIR_NAMES.iter().any(|n| *n == name)
}

pub fn rel_has_heavy_segment(rel: &str) -> bool {
  rel.split(['/', '\\'])
    .filter(|s| !s.is_empty() && *s != ".")
    .any(is_heavy_dir_name)
}

/// 扫描依赖/缓存类目录，返回相对路径（如 src/node_modules），不深入这些目录内部
pub fn scan_heavy_dirs(root: &Path) -> Vec<String> {
  let mut found = Vec::new();
  let mut it = WalkDir::new(root).into_iter();
  loop {
    let entry = match it.next() {
      Some(Ok(e)) => e,
      Some(Err(_)) => continue,
      None => break,
    };
    if entry.depth() == 0 {
      continue;
    }
    if !entry.file_type().is_dir() {
      continue;
    }
    let name = entry.file_name().to_string_lossy();
    if name.starts_with('.') && !is_heavy_dir_name(&name) {
      it.skip_current_dir();
      continue;
    }
    if is_heavy_dir_name(&name) {
      if let Ok(rel) = entry.path().strip_prefix(root) {
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !rel.is_empty() {
          found.push(rel);
        }
      }
      it.skip_current_dir();
    }
  }
  found.sort();
  found.dedup();
  found
}

pub fn heavy_dir_json_items(paths: Vec<String>) -> Vec<serde_json::Value> {
  paths
    .into_iter()
    .map(|path| {
      serde_json::json!({
        "path": path,
        "display": format!("/{}/", path.trim_matches('/')),
      })
    })
    .collect()
}
