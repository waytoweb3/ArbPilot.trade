use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// 内置执行器的运行参数；前端启动时据此连接本机执行器。
#[derive(Debug, Clone, Serialize)]
pub struct LocalExecutorConfig {
    pub base_url: String,
    pub token: String,
    /// 本地 SQLite 文件位置，便于设置页展示与备份。
    pub database: String,
}

/// 随桌面端启动的本机执行器。密钥与业务数据都留在用户电脑上。
pub struct LocalExecutor {
    config: LocalExecutorConfig,
    child: Mutex<Option<Child>>,
}

impl LocalExecutor {
    pub fn config(&self) -> &LocalExecutorConfig {
        &self.config
    }

    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().expect("执行器进程锁不可用").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for LocalExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 桌面端运行时状态：内置执行器可用时前端走本机，否则回退到外部地址。
pub struct ExecutorRuntime {
    pub local: Option<LocalExecutor>,
}

/// 启动随包分发的执行器：本地 SQLite、随机令牌、只监听 127.0.0.1。
pub fn start(app: &AppHandle) -> ExecutorRuntime {
    // 显式指定外部执行器地址时不再启动内置进程（便于开发调试）。
    if let Ok(url) = std::env::var("ARBPILOT_EXECUTOR_URL") {
        let url = url.trim();
        if !url.is_empty() {
            eprintln!("已配置 ARBPILOT_EXECUTOR_URL，使用外部执行器 {url}");
            return ExecutorRuntime { local: None };
        }
    }

    let Some(binary) = resolve_binary() else {
        eprintln!("未找到内置执行器二进制，改用外部执行器");
        return ExecutorRuntime { local: None };
    };

    let data_dir = match app.path().app_data_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("获取应用数据目录失败：{error}");
            return ExecutorRuntime { local: None };
        }
    };
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("创建应用数据目录失败：{error}（{}）", data_dir.display());
        return ExecutorRuntime { local: None };
    }

    let port = match free_port() {
        Some(port) => port,
        None => {
            eprintln!("未找到可用的本地端口");
            return ExecutorRuntime { local: None };
        }
    };
    let token = random_token();
    let database = data_dir.join("executor.db");

    let mut command = Command::new(&binary);
    command
        .env("ARBPILOT_BIND_ADDR", format!("127.0.0.1:{port}"))
        .env("ARBPILOT_API_TOKEN", &token)
        .env("ARBPILOT_LOCAL_DB", &database)
        .env("ARBPILOT_ACCOUNTS_FILE", data_dir.join("accounts.json"))
        .env("ARBPILOT_RISK_FILE", data_dir.join("risk_limits.json"))
        .env("ARBPILOT_LICENSE_FILE", data_dir.join("license.json"))
        .env(
            "ARBPILOT_TELEGRAM_BINDINGS_FILE",
            data_dir.join("telegram_bindings.json"),
        )
        .env("ARBPILOT_PUBLIC_MODE", "false")
        // 内置执行器永远用本地 SQLite：置空而不是删除，防止 dotenvy 从上级目录的 .env 里补回 PostgreSQL 配置。
        .env("DATABASE_URL", "")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = binary.parent() {
        command.current_dir(dir);
    }

    match command.spawn() {
        Ok(child) => {
            // 等执行器开始监听再交给前端，避免首屏请求打到尚未就绪的端口。
            wait_until_ready(port, std::time::Duration::from_secs(3));
            eprintln!("内置执行器已启动：{}（端口 {port}）", binary.display());
            ExecutorRuntime {
                local: Some(LocalExecutor {
                    config: LocalExecutorConfig {
                        base_url: format!("http://127.0.0.1:{port}"),
                        token,
                        database: database.display().to_string(),
                    },
                    child: Mutex::new(Some(child)),
                }),
            }
        }
        Err(error) => {
            eprintln!("启动内置执行器失败：{error}（{}）", binary.display());
            ExecutorRuntime { local: None }
        }
    }
}

/// 依次尝试安装包内路径与开发态构建产物。
fn resolve_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "arbpilot-executor.exe"
    } else {
        "arbpilot-executor"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join(name);
            if bundled.is_file() {
                return Some(bundled);
            }
        }
    }

    if cfg!(debug_assertions) {
        // 开发态：优先使用 workspace 的调试/发布产物。
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for profile in ["debug", "release"] {
            let candidate = repo.join("target").join(profile).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// 绑定 0 端口拿到空闲端口后立即释放，交给执行器监听。
fn free_port() -> Option<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    listener.local_addr().ok().map(|addr| addr.port())
}

/// 轮询确认执行器已开始监听；超时不影响启动，前端请求会自行重试。
fn wait_until_ready(port: u16, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200))
            .is_ok()
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn random_token() -> String {
    let mut token = String::with_capacity(64);
    for _ in 0..2 {
        token.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    token
}
