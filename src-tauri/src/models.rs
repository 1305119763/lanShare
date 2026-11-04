use serde::{Deserialize, Serialize};

fn default_kind() -> String {
  "dir".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareFolder {
  pub id: String,
  pub name: String,
  pub path: String,
  pub enabled: bool,
  /// "dir" | "file"
  #[serde(default = "default_kind")]
  pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLog {
  pub id: String,
  pub time: String,
  pub level: String,
  pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
  pub port: u16,
  pub folders: Vec<ShareFolder>,
  pub allow_upload: bool,
}

impl Default for AppSettings {
  fn default() -> Self {
    Self {
      port: 9726,
      folders: Vec::new(),
      allow_upload: true,
    }
  }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
  pub running: bool,
  pub port: u16,
  pub ips: Vec<String>,
  pub urls: Vec<String>,
  pub logs: Vec<ServerLog>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
  pub version: String,
  pub platform: String,
  pub hostname: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
  pub port: Option<u16>,
  pub allow_upload: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPatch {
  pub name: Option<String>,
  pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddShareResult {
  #[serde(flatten)]
  pub folder: ShareFolder,
  pub filtered_dirs: Vec<String>,
}
