mod deploy;
mod local;

use tauri::Manager;

/// 桌面端只暴露执行器地址与本地令牌，不接触任何交易所密钥。
#[tauri::command]
fn executor_base_url(runtime: tauri::State<'_, local::ExecutorRuntime>) -> String {
    runtime
        .local
        .as_ref()
        .map(|local| local.config().base_url.clone())
        .or_else(|| std::env::var("ARBPILOT_EXECUTOR_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8787".to_string())
}

/// 内置执行器的连接参数；未启动内置执行器时返回空，前端回退到手动配置。
#[tauri::command]
fn executor_local_config(
    runtime: tauri::State<'_, local::ExecutorRuntime>,
) -> Option<local::LocalExecutorConfig> {
    runtime.local.as_ref().map(|local| local.config().clone())
}

/// 客户端更新安装完成后重启自身。
#[tauri::command]
fn app_restart(app: tauri::AppHandle) {
    app.restart();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            executor_base_url,
            executor_local_config,
            app_restart,
            deploy::server_credentials_save,
            deploy::server_credentials_load,
            deploy::server_credentials_delete,
            deploy::server_probe,
            deploy::server_deploy,
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("ArbPilot");
            }
            // 随包启动本地执行器：本地 SQLite、随机令牌、仅监听本机回环地址。
            app.manage(local::start(app.handle()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("启动 ArbPilot 桌面端失败");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(runtime) = app_handle.try_state::<local::ExecutorRuntime>() {
                if let Some(local) = &runtime.local {
                    local.stop();
                }
            }
        }
    });
}
