use crate::models::{AppSettings, ServerLog, ShareFolder};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone)]
pub struct AppState {
  pub inner: Arc<Mutex<StateInner>>,
  pub shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
}

pub struct StateInner {
  pub settings: AppSettings,
  pub running: bool,
  pub logs: Vec<ServerLog>,
  pub settings_path: PathBuf,
  pub hostname: String,
}

impl AppState {
  pub fn new(settings_path: PathBuf, hostname: String) -> Self {
    let settings = load_settings(&settings_path);
    Self {
      inner: Arc::new(Mutex::new(StateInner {
        settings,
        running: false,
        logs: Vec::new(),
        settings_path,
        hostname,
      })),
      shutdown_tx: Arc::new(Mutex::new(None)),
    }
  }

  pub fn push_log(&self, level: &str, message: impl Into<String>) -> ServerLog {
    let log = ServerLog {
      id: uuid::Uuid::new_v4().to_string(),
      time: chrono::Local::now().format("%H:%M:%S").to_string(),
      level: level.to_string(),
      message: message.into(),
    };
    let mut g = self.inner.lock();
    g.logs.insert(0, log.clone());
    if g.logs.len() > 200 {
      g.logs.truncate(200);
    }
    log
  }

  pub fn save_settings(&self) {
    let g = self.inner.lock();
    if let Some(parent) = g.settings_path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&g.settings) {
      let _ = std::fs::write(&g.settings_path, json);
    }
  }

  pub fn enabled_folders(&self) -> Vec<ShareFolder> {
    self
      .inner
      .lock()
      .settings
      .folders
      .iter()
      .filter(|f| f.enabled)
      .cloned()
      .collect()
  }
}

fn load_settings(path: &PathBuf) -> AppSettings {
  if let Ok(raw) = std::fs::read_to_string(path) {
    if let Ok(s) = serde_json::from_str::<AppSettings>(&raw) {
      return s;
    }
  }
  AppSettings::default()
}

pub fn lan_ips() -> Vec<String> {
  let mut ips = Vec::new();
  if let Ok(ip) = local_ip_address::local_ip() {
    ips.push(ip.to_string());
  }
  if let Ok(list) = local_ip_address::list_afinet_netifas() {
    for (_, ip) in list {
      if ip.is_ipv4() && !ip.is_loopback() {
        let s = ip.to_string();
        if !ips.contains(&s) {
          ips.push(s);
        }
      }
    }
  }
  if ips.is_empty() {
    ips.push("127.0.0.1".into());
  }
  ips
}
