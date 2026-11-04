use crate::heavy_dirs::{heavy_dir_json_items, is_heavy_dir_name, rel_has_heavy_segment, scan_heavy_dirs};
use crate::models::ShareFolder;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::io::{Read, Seek, Write};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[derive(Clone)]
struct HttpState {
  app: AppState,
}

#[derive(Deserialize)]
struct ListQuery {
  #[serde(rename = "folderId")]
  folder_id: String,
  path: Option<String>,
}

#[derive(Deserialize)]
struct FileQuery {
  #[serde(rename = "folderId")]
  folder_id: String,
  #[serde(default)]
  path: String,
}

#[derive(Deserialize)]
struct UploadQuery {
  #[serde(rename = "folderId")]
  folder_id: String,
  path: Option<String>,
}

#[derive(Deserialize)]
struct ZipQuery {
  #[serde(rename = "folderId")]
  folder_id: String,
  path: Option<String>,
  /// 逗号分隔的相对路径，默认排除；勾选的不会出现在此列表
  exclude: Option<String>,
}

fn parse_exclude_list(raw: Option<&str>) -> Vec<String> {
  raw
    .unwrap_or("")
    .split(',')
    .map(|s| s.trim().trim_matches('/').replace('\\', "/"))
    .filter(|s| !s.is_empty())
    .collect()
}

fn is_excluded(rel: &str, excludes: &[String]) -> bool {
  let rel = rel.trim_matches('/');
  excludes.iter().any(|ex| {
    let ex = ex.trim_matches('/');
    rel == ex || rel.starts_with(&format!("{ex}/"))
  })
}

fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
  let rel = if relative.is_empty() || relative == "." {
    PathBuf::from("")
  } else {
    PathBuf::from(relative.replace('\\', "/"))
  };
  for c in rel.components() {
    match c {
      Component::Normal(_) | Component::CurDir => {}
      _ => return None,
    }
  }
  let joined = root.join(&rel);
  let root_c = root.canonicalize().ok()?;
  let joined_c = if joined.exists() {
    joined.canonicalize().ok()?
  } else {
    let parent = joined.parent()?.canonicalize().ok()?;
    if !parent.starts_with(&root_c) {
      return None;
    }
    return Some(joined);
  };
  if joined_c.starts_with(&root_c) {
    Some(joined_c)
  } else {
    None
  }
}

fn find_share(folders: &[ShareFolder], id: &str) -> Option<ShareFolder> {
  folders.iter().find(|f| f.id == id && f.enabled).cloned()
}

fn is_file_share(share: &ShareFolder) -> bool {
  share.kind == "file"
}

/// Resolve a path inside a share (directory root or single file).
fn resolve_target(share: &ShareFolder, rel: &str) -> Option<PathBuf> {
  if is_file_share(share) {
    let p = PathBuf::from(&share.path);
    if !p.is_file() {
      return None;
    }
    let rel = rel.trim().trim_matches(|c| c == '/' || c == '\\');
    if rel.is_empty() || rel == "." {
      return Some(p);
    }
    let name = p.file_name()?.to_string_lossy();
    if rel == name.as_ref() {
      Some(p)
    } else {
      None
    }
  } else {
    safe_join(Path::new(&share.path), rel)
  }
}

fn format_size(bytes: u64) -> String {
  const KB: f64 = 1024.0;
  const MB: f64 = KB * 1024.0;
  const GB: f64 = MB * 1024.0;
  let b = bytes as f64;
  if b < KB {
    format!("{bytes} B")
  } else if b < MB {
    format!("{:.1} KB", b / KB)
  } else if b < GB {
    format!("{:.1} MB", b / MB)
  } else {
    format!("{:.2} GB", b / GB)
  }
}

fn escape_html(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
}

fn file_response(target: &Path) -> Response {
  let data = match std::fs::read(target) {
    Ok(d) => d,
    Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "读取失败").into_response(),
  };
  let mime = mime_guess::from_path(target)
    .first_or_octet_stream()
    .to_string();
  let name = target
    .file_name()
    .map(|s| s.to_string_lossy().to_string())
    .unwrap_or_else(|| "file".into());
  let disposition = format!(
    "attachment; filename*=UTF-8''{}",
    urlencoding::encode(&name)
  );
  Response::builder()
    .status(StatusCode::OK)
    .header(header::CONTENT_TYPE, mime)
    .header(header::CONTENT_DISPOSITION, disposition)
    .body(Body::from(data))
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn zip_path_to_bytes(
  target: &Path,
  archive_name: &str,
  excludes: &[String],
) -> Result<(Vec<u8>, String), String> {
  let mut tmp = tempfile::tempfile().map_err(|e| e.to_string())?;
  {
    let mut zip = ZipWriter::new(&mut tmp);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    if target.is_file() {
      let name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
      zip
        .start_file(name, opts)
        .map_err(|e| e.to_string())?;
      let mut f = std::fs::File::open(target).map_err(|e| e.to_string())?;
      let mut buf = Vec::new();
      f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
      zip.write_all(&buf).map_err(|e| e.to_string())?;
    } else if target.is_dir() {
      let root = target;
      let mut it = WalkDir::new(root).into_iter();
      loop {
        let entry = match it.next() {
          Some(Ok(e)) => e,
          Some(Err(_)) => continue,
          None => break,
        };
        let path = entry.path();
        if path == root {
          continue;
        }
        let rel = match path.strip_prefix(root) {
          Ok(r) => r.to_string_lossy().replace('\\', "/"),
          Err(_) => continue,
        };
        if rel.is_empty() {
          continue;
        }
        if is_excluded(&rel, excludes) {
          if entry.file_type().is_dir() {
            it.skip_current_dir();
          }
          continue;
        }
        // 仍跳过未排除的隐藏目录内容（.git 若未排除仍会进；名单内靠 exclude）
        if rel.split('/').any(|p| p.starts_with('.') && !is_heavy_dir_name(p)) {
          if entry.file_type().is_dir() {
            it.skip_current_dir();
          }
          continue;
        }
        if entry.file_type().is_dir() {
          let dir_name = if rel.ends_with('/') {
            rel.clone()
          } else {
            format!("{rel}/")
          };
          let _ = zip.add_directory(dir_name, opts);
        } else if entry.file_type().is_file() {
          if zip.start_file(&rel, opts).is_err() {
            continue;
          }
          let mut f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => continue,
          };
          let mut buf = Vec::new();
          if f.read_to_end(&mut buf).is_err() {
            continue;
          }
          let _ = zip.write_all(&buf);
        }
      }
    } else {
      return Err("路径无效".into());
    }
    zip.finish().map_err(|e| e.to_string())?;
  }
  tmp.rewind().map_err(|e| e.to_string())?;
  let mut out = Vec::new();
  tmp.read_to_end(&mut out).map_err(|e| e.to_string())?;
  let zip_name = if archive_name.ends_with(".zip") {
    archive_name.to_string()
  } else {
    format!("{archive_name}.zip")
  };
  Ok((out, zip_name))
}

async fn meta(State(st): State<HttpState>) -> impl IntoResponse {
  let g = st.app.inner.lock();
  Json(json!({
    "name": "瞬传 LanShare",
    "hostName": g.hostname,
    "allowUpload": g.settings.allow_upload,
    "folders": g.settings.folders.iter().filter(|f| f.enabled).map(|f| json!({
      "id": f.id,
      "name": f.name,
      "kind": f.kind,
    })).collect::<Vec<_>>(),
  }))
}

async fn list(State(st): State<HttpState>, Query(q): Query<ListQuery>) -> impl IntoResponse {
  let folders = st.app.enabled_folders();
  let Some(share) = find_share(&folders, &q.folder_id) else {
    return (StatusCode::NOT_FOUND, Json(json!({"error":"共享不存在"}))).into_response();
  };

  if is_file_share(&share) {
    let p = PathBuf::from(&share.path);
    if !p.is_file() {
      return (StatusCode::NOT_FOUND, Json(json!({"error":"文件不存在"}))).into_response();
    }
    let size = p.metadata().map(|m| m.len()).unwrap_or(0);
    let name = p
      .file_name()
      .map(|s| s.to_string_lossy().to_string())
      .unwrap_or_else(|| share.name.clone());
    return Json(json!({
      "folderId": share.id,
      "folderName": share.name,
      "path": "",
      "kind": "file",
      "entries": [{
        "name": name,
        "type": "file",
        "size": size,
        "sizeText": format_size(size),
        "mtime": 0,
      }],
    }))
    .into_response();
  }

  let rel = q.path.unwrap_or_else(|| ".".into());
  if rel_has_heavy_segment(&rel) {
    return (
      StatusCode::FORBIDDEN,
      Json(json!({"error":"该目录已被自动过滤"})),
    )
      .into_response();
  }
  let Some(target) = resolve_target(&share, &rel) else {
    return (StatusCode::BAD_REQUEST, Json(json!({"error":"非法路径"}))).into_response();
  };
  if !target.is_dir() {
    return (StatusCode::BAD_REQUEST, Json(json!({"error":"不是目录"}))).into_response();
  }

  let at_share_root = rel == "." || rel.is_empty();
  let share_root = PathBuf::from(&share.path);
  let filtered_dirs = if at_share_root {
    tokio::task::spawn_blocking(move || heavy_dir_json_items(scan_heavy_dirs(&share_root)))
      .await
      .unwrap_or_default()
  } else {
    Vec::new()
  };

  let mut entries = Vec::new();
  if let Ok(rd) = std::fs::read_dir(&target) {
    for ent in rd.flatten() {
      let name = ent.file_name().to_string_lossy().to_string();
      if name.starts_with('.') {
        continue;
      }
      if is_heavy_dir_name(&name) {
        continue;
      }
      let meta = ent.metadata().ok();
      let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
      let size = meta
        .as_ref()
        .map(|m| if m.is_file() { m.len() } else { 0 })
        .unwrap_or(0);
      let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
      entries.push(json!({
        "name": name,
        "type": if is_dir { "dir" } else { "file" },
        "size": size,
        "sizeText": if is_dir { "—".to_string() } else { format_size(size) },
        "mtime": mtime,
      }));
    }
  }
  entries.sort_by(|a, b| {
    let ta = a.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let tb = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match (ta, tb) {
      ("dir", "file") => std::cmp::Ordering::Less,
      ("file", "dir") => std::cmp::Ordering::Greater,
      _ => {
        let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        na.cmp(nb)
      }
    }
  });
  let path_out = if rel == "." || rel.is_empty() {
    String::new()
  } else {
    rel.replace('\\', "/")
  };
  Json(json!({
    "folderId": share.id,
    "folderName": share.name,
    "path": path_out,
    "kind": "dir",
    "entries": entries,
    "filteredDirs": filtered_dirs,
  }))
  .into_response()
}

async fn download(State(st): State<HttpState>, Query(q): Query<FileQuery>) -> Response {
  let folders = st.app.enabled_folders();
  let Some(share) = find_share(&folders, &q.folder_id) else {
    return (StatusCode::NOT_FOUND, "共享不存在").into_response();
  };
  let Some(target) = resolve_target(&share, &q.path) else {
    return (StatusCode::BAD_REQUEST, "非法路径").into_response();
  };
  if !target.is_file() {
    return (StatusCode::NOT_FOUND, "文件不存在").into_response();
  }
  st.app
    .push_log("info", format!("下载 {}", target.display()));
  file_response(&target)
}

async fn preview(State(st): State<HttpState>, Query(q): Query<FileQuery>) -> Response {
  let folders = st.app.enabled_folders();
  let Some(share) = find_share(&folders, &q.folder_id) else {
    return (StatusCode::NOT_FOUND, "共享不存在").into_response();
  };
  let Some(target) = resolve_target(&share, &q.path) else {
    return (StatusCode::BAD_REQUEST, "非法路径").into_response();
  };
  if !target.is_file() {
    return (StatusCode::NOT_FOUND, "文件不存在").into_response();
  }
  let data = match std::fs::read(&target) {
    Ok(d) => d,
    Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "读取失败").into_response(),
  };
  let mime = mime_guess::from_path(&target)
    .first_or_octet_stream()
    .to_string();
  Response::builder()
    .status(StatusCode::OK)
    .header(header::CONTENT_TYPE, mime)
    .body(Body::from(data))
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn zip_scan(State(st): State<HttpState>, Query(q): Query<ZipQuery>) -> impl IntoResponse {
  let folders = st.app.enabled_folders();
  let Some(share) = find_share(&folders, &q.folder_id) else {
    return (StatusCode::NOT_FOUND, Json(json!({"error":"共享不存在"}))).into_response();
  };
  if is_file_share(&share) {
    return Json(json!({"items": []})).into_response();
  }
  let rel = q.path.unwrap_or_else(|| ".".into());
  let Some(target) = resolve_target(&share, &rel) else {
    return (StatusCode::BAD_REQUEST, Json(json!({"error":"非法路径"}))).into_response();
  };
  if !target.is_dir() {
    return Json(json!({"items": []})).into_response();
  }

  let items = tokio::task::spawn_blocking(move || heavy_dir_json_items(scan_heavy_dirs(&target)))
    .await
    .unwrap_or_default();

  Json(json!({ "items": items })).into_response()
}

async fn zip_download(State(st): State<HttpState>, Query(q): Query<ZipQuery>) -> Response {
  let folders = st.app.enabled_folders();
  let Some(share) = find_share(&folders, &q.folder_id) else {
    return (StatusCode::NOT_FOUND, "共享不存在").into_response();
  };
  let rel = q.path.clone().unwrap_or_else(|| ".".into());
  let Some(target) = resolve_target(&share, &rel) else {
    return (StatusCode::BAD_REQUEST, "非法路径").into_response();
  };

  let archive_stem = if target.is_file() {
    target
      .file_stem()
      .map(|s| s.to_string_lossy().to_string())
      .unwrap_or_else(|| share.name.clone())
  } else if rel == "." || rel.is_empty() {
    share.name.clone()
  } else {
    Path::new(&rel)
      .file_name()
      .map(|s| s.to_string_lossy().to_string())
      .unwrap_or_else(|| share.name.clone())
  };

  let excludes = parse_exclude_list(q.exclude.as_deref());
  st.app.push_log(
    "info",
    format!(
      "打包下载 {}（排除 {} 项）",
      target.display(),
      excludes.len()
    ),
  );

  let result =
    tokio::task::spawn_blocking(move || zip_path_to_bytes(&target, &archive_stem, &excludes)).await;
  match result {
    Ok(Ok((bytes, name))) => {
      let disposition = format!(
        "attachment; filename*=UTF-8''{}",
        urlencoding::encode(&name)
      );
      Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
    Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
  }
}

async fn upload(
  State(st): State<HttpState>,
  Query(q): Query<UploadQuery>,
  mut multipart: Multipart,
) -> impl IntoResponse {
  let allow = st.app.inner.lock().settings.allow_upload;
  if !allow {
    return (StatusCode::FORBIDDEN, Json(json!({"error":"已禁止上传"}))).into_response();
  }
  let folders = st.app.enabled_folders();
  let Some(share) = find_share(&folders, &q.folder_id) else {
    return (StatusCode::NOT_FOUND, Json(json!({"error":"共享不存在"}))).into_response();
  };
  if is_file_share(&share) {
    return (
      StatusCode::BAD_REQUEST,
      Json(json!({"error":"单文件共享不支持上传"})),
    )
      .into_response();
  }
  let rel = q.path.unwrap_or_else(|| ".".into());
  let Some(dest_dir) = resolve_target(&share, &rel) else {
    return (StatusCode::BAD_REQUEST, Json(json!({"error":"非法路径"}))).into_response();
  };
  if !dest_dir.is_dir() {
    return (StatusCode::BAD_REQUEST, Json(json!({"error":"无效上传路径"}))).into_response();
  }

  let mut count = 0u32;
  while let Ok(Some(field)) = multipart.next_field().await {
    let name = field
      .file_name()
      .map(|s| s.to_string())
      .unwrap_or_else(|| format!("upload-{count}"));
    let safe_name = Path::new(&name)
      .file_name()
      .map(|s| s.to_string_lossy().to_string())
      .unwrap_or_else(|| format!("upload-{count}"));
    let bytes = match field.bytes().await {
      Ok(b) => b,
      Err(e) => {
        st.app.push_log("error", format!("上传失败: {e}"));
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response();
      }
    };
    let target = dest_dir.join(&safe_name);
    if let Err(e) = std::fs::write(&target, &bytes) {
      st.app.push_log("error", format!("写入失败: {e}"));
      return (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
      )
        .into_response();
    }
    count += 1;
  }
  st.app.push_log("info", format!("上传成功 {count} 个文件"));
  Json(json!({"ok": true, "count": count})).into_response()
}

async fn portal(State(st): State<HttpState>) -> Html<String> {
  let g = st.app.inner.lock();
  let folders: Vec<_> = g
    .settings
    .folders
    .iter()
    .filter(|f| f.enabled)
    .map(|f| json!({"id": f.id, "name": f.name, "kind": f.kind}))
    .collect();
  let folders_json = serde_json::to_string(&folders).unwrap_or_else(|_| "[]".into());
  let allow = if g.settings.allow_upload {
    "true"
  } else {
    "false"
  };
  let host = escape_html(&g.hostname);
  Html(render_portal(&host, &folders_json, allow))
}

fn render_portal(host: &str, folders_json: &str, allow_upload: &str) -> String {
  format!(
    r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover" />
<meta name="theme-color" content="#0b1220" />
<title>瞬传 · {host}</title>
<link href="https://fonts.googleapis.com/css2?family=Outfit:wght@500;600;700&family=IBM+Plex+Sans:wght@400;500;600&display=swap" rel="stylesheet" />
<style>
:root {{
  --bg:#0b1220;--panel:rgba(18,28,48,.82);--line:rgba(125,211,252,.18);--text:#e8eef8;
  --muted:#8fa3bf;--accent:#2dd4bf;--accent-2:#38bdf8;--radius:16px;--shadow:0 20px 50px rgba(0,0,0,.35);
}}
*{{box-sizing:border-box}} html,body{{margin:0;min-height:100%}}
body{{font-family:"IBM Plex Sans",system-ui,sans-serif;color:var(--text);
background:radial-gradient(1200px 600px at 10% -10%,rgba(45,212,191,.22),transparent 55%),
radial-gradient(900px 500px at 100% 0%,rgba(56,189,248,.18),transparent 50%),
linear-gradient(165deg,#0b1220 0%,#111827 50%,#0a1628 100%);background-attachment:fixed}}
.app{{max-width:860px;margin:0 auto;padding:20px 16px 48px}}
.brand{{font-family:Outfit,sans-serif;font-size:28px;font-weight:700;
background:linear-gradient(120deg,#fff,#99f6e4 45%,#7dd3fc);-webkit-background-clip:text;background-clip:text;color:transparent}}
.sub{{color:var(--muted);font-size:14px;margin:6px 0 16px}}
.panel{{background:var(--panel);border:1px solid var(--line);border-radius:var(--radius);backdrop-filter:blur(12px);box-shadow:var(--shadow);overflow:hidden}}
.toolbar{{display:none;flex-wrap:wrap;gap:10px;align-items:center;padding:14px;border-bottom:1px solid var(--line)}}
.crumbs{{flex:1;min-width:140px;display:flex;flex-wrap:wrap;gap:6px;align-items:center}}
.crumb{{color:var(--muted);font-size:13px;cursor:pointer;border:0;background:transparent;padding:0}}
.crumb:hover,.crumb.active{{color:var(--accent-2)}}
.crumb+.crumb::before{{content:"/";margin-right:6px;color:#4b5d78}}
.btn{{border:0;border-radius:999px;padding:10px 16px;font-weight:600;font-size:13px;cursor:pointer;font-family:inherit}}
.btn-primary{{background:linear-gradient(135deg,var(--accent),var(--accent-2));color:#06201c}}
.btn-ghost{{background:rgba(255,255,255,.06);color:var(--text);border:1px solid var(--line)}}
.btn-zip{{background:rgba(56,189,248,.18);color:#bae6fd;border:1px solid rgba(56,189,248,.35)}}
.list{{padding:8px}}.folders{{display:grid;gap:10px;padding:12px}}
.item{{display:grid;grid-template-columns:40px 1fr auto;gap:10px;align-items:center;padding:12px;border-radius:12px;cursor:pointer}}
.item:hover{{background:rgba(255,255,255,.04)}}
.icon{{width:40px;height:40px;border-radius:12px;display:grid;place-items:center;background:rgba(45,212,191,.12);font-size:18px}}
.icon.file{{background:rgba(56,189,248,.12)}}
.meta .name{{font-weight:600;word-break:break-all}}.meta .desc{{color:var(--muted);font-size:12px;margin-top:2px}}
.pill{{border:0;border-radius:999px;padding:8px 12px;font-size:12px;font-weight:600;background:rgba(45,212,191,.18);color:#99f6e4;cursor:pointer}}
.empty{{padding:48px 20px;text-align:center;color:var(--muted)}}
.folder-card{{display:flex;align-items:center;gap:12px;padding:16px;border-radius:14px;background:rgba(255,255,255,.04);border:1px solid var(--line);cursor:pointer}}
.upload-bar{{display:none;padding:12px 14px;border-top:1px solid var(--line);gap:10px;align-items:center;flex-wrap:wrap}}
.upload-bar.show{{display:flex}}
.toast{{position:fixed;left:50%;bottom:24px;transform:translateX(-50%);background:#122033;border:1px solid var(--line);color:var(--text);padding:10px 16px;border-radius:999px;font-size:13px;opacity:0;transition:.2s;pointer-events:none;z-index:9}}
.toast.show{{opacity:1}} input[type=file]{{display:none}}
.badge{{font-size:11px;padding:2px 8px;border-radius:999px;background:rgba(255,255,255,.08);color:var(--muted);margin-left:6px}}
.modal-mask{{position:fixed;inset:0;z-index:20;background:rgba(4,10,20,.62);display:none;place-items:center;padding:16px;backdrop-filter:blur(6px)}}
.modal-mask.show{{display:grid}}
.modal{{width:min(440px,100%);background:#122033;border:1px solid var(--line);border-radius:18px;padding:18px 16px 14px;box-shadow:var(--shadow);max-height:min(80vh,560px);display:flex;flex-direction:column}}
.modal h3{{margin:0 0 8px;font-family:Outfit,sans-serif;font-size:18px}}
.modal .tip{{color:var(--muted);font-size:13px;line-height:1.5;margin:0 0 12px}}
.heavy-list{{overflow:auto;flex:1;display:flex;flex-direction:column;gap:6px;margin:0 0 14px;padding:4px}}
.heavy-item{{display:flex;align-items:flex-start;gap:10px;padding:10px 12px;border-radius:12px;background:rgba(255,255,255,.04);border:1px solid var(--line);cursor:pointer}}
.heavy-item input{{margin-top:3px;accent-color:#2dd4bf;width:16px;height:16px;flex-shrink:0}}
.heavy-item .path{{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:13px;word-break:break-all;color:#e8eef8}}
.modal-actions{{display:flex;gap:8px;justify-content:flex-end;flex-wrap:wrap}}
.filter-notice{{display:none;margin:0 12px 10px;padding:12px 14px;border-radius:12px;background:rgba(45,212,191,.08);border:1px solid rgba(45,212,191,.22);font-size:13px;line-height:1.5}}
.filter-notice.show{{display:block}}
.filter-notice .title{{font-weight:600;color:#99f6e4;margin:0 0 6px}}
.filter-notice .paths{{margin:0;padding-left:18px;color:var(--muted)}}
.filter-notice .paths li{{margin:2px 0;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px;color:#cbd5e1}}
</style>
</head>
<body>
<div class="app">
  <div class="brand">瞬传</div>
  <div class="sub">来自 {host} · 局域网文件快传</div>
  <section class="panel">
    <div class="toolbar" id="toolbar">
      <div class="crumbs" id="crumbs"></div>
      <button class="btn btn-zip" id="zipBtn" style="display:none">打包全部下载</button>
      <button class="btn btn-primary" id="uploadBtn" style="display:none">上传文件</button>
      <button class="btn btn-ghost" id="homeBtn">全部共享</button>
    </div>
    <div class="filter-notice" id="filterNotice">
      <p class="title">依赖/缓存目录已自动过滤</p>
      <p class="sub" style="margin:0 0 8px">如 node_modules、dist、target 等不会出现在列表中，也无法进入浏览。</p>
      <ul class="paths" id="filterNoticeList"></ul>
    </div>
    <div id="view" class="folders"></div>
    <div class="upload-bar" id="uploadBar"><span class="sub">选择文件上传到当前目录</span><button class="btn btn-ghost" id="pickFiles">选择文件</button></div>
  </section>
</div>
<input type="file" id="fileInput" multiple />
<div class="toast" id="toast"></div>
<div class="modal-mask" id="zipModal">
  <div class="modal" role="dialog" aria-modal="true">
    <h3>检测到依赖目录</h3>
    <p class="tip">以下目录体积通常很大，打包会明显变慢。默认不勾选（不打包）；若需要可主动勾选纳入。</p>
    <div class="heavy-list" id="heavyList"></div>
    <div class="modal-actions">
      <button class="btn btn-ghost" id="zipCancel">取消</button>
      <button class="btn btn-primary" id="zipConfirm">开始打包</button>
    </div>
  </div>
</div>
<script>
const FOLDERS = {folders_json};
const ALLOW_UPLOAD = {allow_upload};
let state = {{ folderId: null, path: '', kind: 'dir' }};
let shareFilteredDirs = [];
let pendingHeavy = [];
const filterNotice = document.getElementById('filterNotice');
const filterNoticeList = document.getElementById('filterNoticeList');
const view = document.getElementById('view');
const toolbar = document.getElementById('toolbar');
const crumbs = document.getElementById('crumbs');
const toastEl = document.getElementById('toast');
const fileInput = document.getElementById('fileInput');
const zipBtn = document.getElementById('zipBtn');
const uploadBtn = document.getElementById('uploadBtn');
const zipModal = document.getElementById('zipModal');
const heavyList = document.getElementById('heavyList');
function toast(msg){{toastEl.textContent=msg;toastEl.classList.add('show');setTimeout(()=>toastEl.classList.remove('show'),2200)}}
function renderFilterNotice(dirs){{
  if(!dirs || !dirs.length){{
    filterNotice.classList.remove('show');
    filterNoticeList.innerHTML='';
    return;
  }}
  filterNotice.classList.add('show');
  filterNoticeList.innerHTML = dirs.map(d=>`<li>${{d.display || d.path}}</li>`).join('');
}}
function iconFor(type,name){{
  if(type==='dir')return '📁';
  const ext=(name.split('.').pop()||'').toLowerCase();
  if(['png','jpg','jpeg','gif','webp','svg'].includes(ext))return '🖼️';
  if(['mp4','mov','mkv','webm'].includes(ext))return '🎬';
  if(['mp3','wav','flac'].includes(ext))return '🎵';
  if(['zip','rar','7z'].includes(ext))return '🗜️';
  return '📎';
}}
function downloadFile(folderId, path){{
  location.href='/api/download?'+new URLSearchParams({{folderId, path: path||''}});
}}
function startZip(excludePaths){{
  const q = new URLSearchParams({{folderId:state.folderId, path: state.path||'.'}});
  if(excludePaths && excludePaths.length){{
    q.set('exclude', excludePaths.join(','));
  }}
  toast('正在打包…');
  location.href='/api/zip?'+q;
}}
function closeZipModal(){{
  zipModal.classList.remove('show');
  pendingHeavy = [];
  heavyList.innerHTML = '';
}}
function openZipModal(items){{
  pendingHeavy = items;
  heavyList.innerHTML = items.map((it,i)=>`
    <label class="heavy-item">
      <input type="checkbox" data-idx="${{i}}" />
      <span class="path">${{it.display}}</span>
    </label>`).join('');
  zipModal.classList.add('show');
}}
async function zipAll(){{
  if(!state.folderId)return;
  toast('正在检测依赖目录…');
  try {{
    const q = new URLSearchParams({{folderId:state.folderId, path: state.path||'.'}});
    const res = await fetch('/api/zip-scan?'+q);
    const data = await res.json();
    if(!res.ok){{ toast(data.error||'检测失败'); return; }}
    const items = data.items || [];
    if(!items.length){{
      startZip([]);
      return;
    }}
    openZipModal(items);
  }} catch(e) {{
    toast('检测失败，请重试');
  }}
}}
document.getElementById('zipCancel').onclick = closeZipModal;
zipModal.addEventListener('click', (e)=>{{ if(e.target===zipModal) closeZipModal(); }});
document.getElementById('zipConfirm').onclick = ()=>{{
  const exclude = [];
  heavyList.querySelectorAll('input[type=checkbox]').forEach((box,i)=>{{
    if(!box.checked && pendingHeavy[i]) exclude.push(pendingHeavy[i].path);
  }});
  closeZipModal();
  startZip(exclude);
}};
function renderHome(){{
  state={{folderId:null,path:'',kind:'dir'}};
  shareFilteredDirs = [];
  renderFilterNotice([]);
  toolbar.style.display='none';
  document.getElementById('uploadBar').classList.remove('show');
  if(!FOLDERS.length){{view.className='empty';view.textContent='暂无共享内容';return}}
  view.className='folders';
  view.innerHTML=FOLDERS.map(f=>`<div class="folder-card" data-id="${{f.id}}" data-kind="${{f.kind||'dir'}}">
    <div class="icon ${{f.kind==='file'?'file':''}}">${{f.kind==='file'?'📎':'📂'}}</div>
    <div class="meta"><div class="name">${{f.name}}<span class="badge">${{f.kind==='file'?'文件':'文件夹'}}</span></div>
    <div class="desc">${{f.kind==='file'?'点击下载':'点击浏览'}}</div></div>
  </div>`).join('');
  view.querySelectorAll('.folder-card').forEach(el=>{{
    el.onclick=()=>{{
      if(el.dataset.kind==='file') downloadFile(el.dataset.id,'');
      else openFolder(el.dataset.id,'');
    }};
  }});
}}
function renderCrumbs(folderName,relPath){{
  const parts=relPath?relPath.split('/').filter(Boolean):[];
  let html=`<button class="crumb active" data-p="">${{folderName}}</button>`;
  let acc=[];
  parts.forEach(p=>{{acc.push(p);html+=`<button class="crumb" data-p="${{acc.join('/')}}">${{p}}</button>`}});
  crumbs.innerHTML=html;
  crumbs.querySelectorAll('.crumb').forEach(btn=>btn.onclick=()=>openFolder(state.folderId,btn.dataset.p||''));
}}
async function openFolder(folderId,relPath){{
  state={{folderId,path:relPath||'',kind:'dir'}};
  toolbar.style.display='flex';
  zipBtn.style.display='inline-block';
  const canUpload = ALLOW_UPLOAD;
  uploadBtn.style.display=canUpload?'inline-block':'none';
  if(canUpload) document.getElementById('uploadBar').classList.add('show');
  else document.getElementById('uploadBar').classList.remove('show');
  view.className='list';view.innerHTML='<div class="empty">加载中…</div>';
  const q=new URLSearchParams({{folderId,path:relPath||'.'}});
  const res=await fetch('/api/list?'+q); const data=await res.json();
  if(!res.ok){{view.innerHTML=`<div class="empty">${{data.error||'加载失败'}}</div>`;return}}
  if(!relPath && Array.isArray(data.filteredDirs)){{
    shareFilteredDirs = data.filteredDirs;
  }}
  renderFilterNotice(shareFilteredDirs);
  state.kind=data.kind||'dir';
  if(state.kind==='file'){{
    zipBtn.style.display='none';
    uploadBtn.style.display='none';
    document.getElementById('uploadBar').classList.remove('show');
  }}
  renderCrumbs(data.folderName,data.path);
  if(!data.entries.length){{view.innerHTML='<div class="empty">空文件夹</div>';return}}
  view.innerHTML=data.entries.map(e=>`<div class="item" data-type="${{e.type}}" data-name="${{encodeURIComponent(e.name)}}">
    <div class="icon ${{e.type}}">${{iconFor(e.type,e.name)}}</div>
    <div class="meta"><div class="name">${{e.name}}</div><div class="desc">${{e.sizeText}}</div></div>
    <div>${{e.type==='file'?`<button class="pill" data-dl="${{encodeURIComponent(e.name)}}">下载</button>`:''}}</div>
  </div>`).join('');
  view.querySelectorAll('.item').forEach(el=>{{
    const name=decodeURIComponent(el.dataset.name);
    const next=state.path?state.path+'/'+name:name;
    if(el.dataset.type==='dir') el.onclick=()=>openFolder(state.folderId,next);
  }});
  view.querySelectorAll('[data-dl]').forEach(btn=>btn.onclick=ev=>{{
    ev.stopPropagation();
    const name=decodeURIComponent(btn.dataset.dl);
    const next=state.path?state.path+'/'+name:name;
    downloadFile(state.folderId, next);
  }});
}}
document.getElementById('homeBtn').onclick=renderHome;
zipBtn.onclick=zipAll;
if(ALLOW_UPLOAD){{
  uploadBtn.onclick=()=>fileInput.click();
  document.getElementById('pickFiles').onclick=()=>fileInput.click();
  fileInput.onchange=async()=>{{
    if(!fileInput.files?.length||!state.folderId)return;
    const fd=new FormData();[...fileInput.files].forEach(f=>fd.append('files',f));
    const q=new URLSearchParams({{folderId:state.folderId,path:state.path||'.'}});
    toast('上传中…');
    const res=await fetch('/api/upload?'+q,{{method:'POST',body:fd}});
    const data=await res.json(); fileInput.value='';
    if(res.ok){{toast('上传成功');openFolder(state.folderId,state.path)}} else toast(data.error||'上传失败');
  }};
}}
renderHome();
</script>
</body>
</html>"##,
    host = host,
    folders_json = folders_json,
    allow_upload = allow_upload
  )
}

fn build_router(app: AppState) -> Router {
  let st = HttpState { app };
  Router::new()
    .route("/", get(portal))
    .route("/api/meta", get(meta))
    .route("/api/list", get(list))
    .route("/api/download", get(download))
    .route("/api/preview", get(preview))
    .route("/api/zip", get(zip_download))
    .route("/api/zip-scan", get(zip_scan))
    .route("/api/upload", post(upload))
    .layer(CorsLayer::permissive())
    .with_state(st)
}

pub async fn start_server(
  app: AppState,
  port: u16,
  mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
  let router = build_router(app.clone());
  let addr = SocketAddr::from(([0, 0, 0, 0], port));
  let listener = TcpListener::bind(addr)
    .await
    .map_err(|e| format!("绑定端口失败 {port}: {e}"))?;

  app.push_log("info", format!("服务已启动，端口 {port}"));

  axum::serve(listener, router)
    .with_graceful_shutdown(async move {
      loop {
        if *shutdown_rx.borrow() {
          break;
        }
        if shutdown_rx.changed().await.is_err() {
          break;
        }
      }
    })
    .await
    .map_err(|e| format!("服务异常: {e}"))?;

  app.push_log("info", "服务已停止");
  Ok(())
}

pub fn spawn_server(app: AppState, port: u16) -> watch::Sender<bool> {
  let (tx, rx) = watch::channel(false);
  let app2 = app.clone();
  tauri::async_runtime::spawn(async move {
    if let Err(e) = start_server(app2.clone(), port, rx).await {
      app2.push_log("error", e);
    }
    app2.inner.lock().running = false;
  });
  tx
}
