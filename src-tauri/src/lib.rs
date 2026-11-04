mod commands;
mod heavy_dirs;
mod models;
mod server;
mod state;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_opener::init())
    .setup(|app| {
      let data_dir = app
        .path()
        .app_data_dir()
        .expect("app data dir");
      std::fs::create_dir_all(&data_dir).ok();
      let settings_path = data_dir.join("settings.json");
      let hostname = hostname::get()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|_| "LanShare".into());
      let state = AppState::new(settings_path, hostname);
      app.manage(state);
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      commands::get_settings,
      commands::set_settings,
      commands::add_folder,
      commands::add_share,
      commands::add_shares,
      commands::update_folder,
      commands::remove_folder,
      commands::start_server,
      commands::stop_server,
      commands::get_server_status,
      commands::get_app_info,
      commands::open_path,
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
