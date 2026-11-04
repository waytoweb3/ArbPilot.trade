use tauri::Manager;

/// 桌面端只暴露执行器地址，不接触任何交易所密钥。
#[tauri::command]
fn executor_base_url() -> String {
    std::env::var("ARBPILOT_EXECUTOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8787".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![executor_base_url])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("ArbPilot");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 ArbPilot 桌面端失败");
}
