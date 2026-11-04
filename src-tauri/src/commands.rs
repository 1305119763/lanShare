use crate::heavy_dirs::scan_heavy_dirs;
use crate::models::{
  AddShareResult, AppInfo, AppSettings, FolderPatch, ServerStatus, SettingsPatch, ShareFolder,
};
use crate::server::spawn_server;
use crate::state::{lan_ips, AppState};
use tauri::{AppHandle, Emitter, State};

fn make_share(path: String) -> Result<ShareFolder, String> {
  let path = normalize_drop_path(&path);
  let p = std::path::PathBuf::from(&path);
  let meta = std::fs::metadata(&p).map_err(|_| format!("无法访问路径: {path}"))?;
  let kind = if meta.is_dir() {
    "dir"
  } else if meta.is_file() {
    "file"
  } else {
    return Err("不支持的路径类型".into());
  };
  let name = p
    .file_name()
    .map(|s| s.to_string_lossy().to_string())
    .unwrap_or_else(|| if kind == "dir" { "共享文件夹" } else { "共享文件" }.into());
  Ok(ShareFolder {
    id: uuid::Uuid::new_v4().to_string(),
    name,
    path: p.to_string_lossy().to_string(),
    enabled: true,
    kind: kind.into(),
  })
}

fn normalize_drop_path(raw: &str) -> String {
  let s = raw.trim().trim_matches('"');
  if let Some(rest) = s.strip_prefix("file:///") {
    let decoded = urlencoding::decode(rest)
      .map(|c| c.into_owned())
      .unwrap_or_else(|_| rest.to_string());
    #[cfg(target_os = "windows")]
    {
      return decoded.replace('/', "\\");
    }
    #[cfg(not(target_os = "windows"))]
    {
      return format!("/{decoded}");
    }
  }
  if let Some(rest) = s.strip_prefix("file://") {
    return urlencoding::decode(rest)
      .map(|c| c.into_owned())
      .unwrap_or_else(|_| rest.to_string());
  }
  s.to_string()
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> AppSettings {
  state.inner.lock().settings.clone()
}

#[tauri::command]
pub fn set_settings(
  state: State<'_, AppState>,
  patch: SettingsPatch,
) -> Result<AppSettings, String> {
  {
    let mut g = state.inner.lock();
    if g.running && patch.port.is_some() {
      return Err("请先停止服务再修改端口".into());
    }
    if let Some(port) = patch.port {
      if !(1024..=65535).contains(&port) {
        return Err("端口需在 1024–65535".into());
      }
      g.settings.port = port;
    }
    if let Some(allow) = patch.allow_upload {
      g.settings.allow_upload = allow;
    }
  }
  state.save_settings();
  Ok(state.inner.lock().settings.clone())
}

#[tauri::command]
pub fn add_folder(state: State<'_, AppState>, path: String) -> Result<AddShareResult, String> {
  add_share(state, path)
}

fn filtered_dirs_for_share(folder: &ShareFolder) -> Vec<String> {
  if folder.kind != "dir" {
    return Vec::new();
  }
  scan_heavy_dirs(std::path::Path::new(&folder.path))
}

#[tauri::command]
pub fn add_share(state: State<'_, AppState>, path: String) -> Result<AddShareResult, String> {
  let folder = make_share(path)?;
  // skip duplicates by absolute path
  {
    let mut g = state.inner.lock();
    if g.settings.folders.iter().any(|f| f.path == folder.path) {
      return Err("该路径已在共享列表中".into());
    }
    g.settings.folders.push(folder.clone());
  }
  state.save_settings();
  let filtered_dirs = filtered_dirs_for_share(&folder);
  Ok(AddShareResult {
    folder,
    filtered_dirs,
  })
}

#[tauri::command]
pub fn add_shares(
  state: State<'_, AppState>,
  paths: Vec<String>,
) -> Result<Vec<AddShareResult>, String> {
  let mut added = Vec::new();
  for path in paths {
    match make_share(path) {
      Ok(folder) => {
        let mut g = state.inner.lock();
        if g.settings.folders.iter().any(|f| f.path == folder.path) {
          continue;
        }
        g.settings.folders.push(folder.clone());
        let filtered_dirs = filtered_dirs_for_share(&folder);
        added.push(AddShareResult {
          folder,
          filtered_dirs,
        });
      }
      Err(_) => continue,
    }
  }
  if added.is_empty() {
    return Err("没有可添加的文件或文件夹".into());
  }
  state.save_settings();
  Ok(added)
}

#[tauri::command]
pub fn update_folder(
  state: State<'_, AppState>,
  id: String,
  patch: FolderPatch,
) -> Result<Vec<ShareFolder>, String> {
  {
    let mut g = state.inner.lock();
    let Some(folder) = g.settings.folders.iter_mut().find(|f| f.id == id) else {
      return Err("共享项不存在".into());
    };
    if let Some(name) = patch.name {
      folder.name = name;
    }
    if let Some(enabled) = patch.enabled {
      folder.enabled = enabled;
    }
  }
  state.save_settings();
  Ok(state.inner.lock().settings.folders.clone())
}

#[tauri::command]
pub fn remove_folder(state: State<'_, AppState>, id: String) -> Result<Vec<ShareFolder>, String> {
  state.inner.lock().settings.folders.retain(|f| f.id != id);
  state.save_settings();
  Ok(state.inner.lock().settings.folders.clone())
}

#[tauri::command]
pub fn start_server(app: AppHandle, state: State<'_, AppState>) -> Result<ServerStatus, String> {
  let (port, empty, already) = {
    let g = state.inner.lock();
    (
      g.settings.port,
      g.settings.folders.is_empty(),
      g.running,
    )
  };
  if empty {
    return Err("请先添加要共享的文件或文件夹".into());
  }
  if already {
    return Ok(status_from_state(&state));
  }

  if let Some(tx) = state.shutdown_tx.lock().take() {
    let _ = tx.send(true);
  }

  let app_state = (*state).clone();
  let tx = spawn_server(app_state, port);
  *state.shutdown_tx.lock() = Some(tx);
  state.inner.lock().running = true;

  let status = status_from_state(&state);
  let _ = app.emit("server-status", &status);
  Ok(status)
}

#[tauri::command]
pub fn stop_server(app: AppHandle, state: State<'_, AppState>) -> Result<ServerStatus, String> {
  if let Some(tx) = state.shutdown_tx.lock().take() {
    let _ = tx.send(true);
  }
  state.inner.lock().running = false;
  let log = state.push_log("info", "服务已停止");
  let _ = app.emit("server-log", &log);
  let status = status_from_state(&state);
  let _ = app.emit("server-status", &status);
  Ok(status)
}

#[tauri::command]
pub fn get_server_status(state: State<'_, AppState>) -> ServerStatus {
  status_from_state(&state)
}

#[tauri::command]
pub fn get_app_info(state: State<'_, AppState>) -> AppInfo {
  let hostname = state.inner.lock().hostname.clone();
  AppInfo {
    version: env!("CARGO_PKG_VERSION").into(),
    platform: std::env::consts::OS.into(),
    hostname,
  }
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
  #[cfg(target_os = "windows")]
  {
    let p = std::path::Path::new(&path);
    if p.is_file() {
      // reveal file in explorer
      std::process::Command::new("explorer")
        .arg(format!("/select,{}", path))
        .spawn()
        .map_err(|e| e.to_string())?;
    } else {
      std::process::Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    }
  }
  #[cfg(target_os = "macos")]
  {
    let p = std::path::Path::new(&path);
    if p.is_file() {
      std::process::Command::new("open")
        .args(["-R", &path])
        .spawn()
        .map_err(|e| e.to_string())?;
    } else {
      std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    }
  }
  #[cfg(not(any(target_os = "windows", target_os = "macos")))]
  {
    std::process::Command::new("xdg-open")
      .arg(&path)
      .spawn()
      .map_err(|e| e.to_string())?;
  }
  Ok(())
}

fn status_from_state(state: &AppState) -> ServerStatus {
  let g = state.inner.lock();
  let ips = lan_ips();
  let urls = ips
    .iter()
    .map(|ip| format!("http://{ip}:{}", g.settings.port))
    .collect();
  ServerStatus {
    running: g.running,
    port: g.settings.port,
    ips,
    urls,
    logs: g.logs.clone(),
  }
}
