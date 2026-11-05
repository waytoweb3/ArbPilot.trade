use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use ssh2::Session;
use tauri::{AppHandle, Emitter};

const KEYRING_SERVICE: &str = "arbpilot.desktop";
const LOG_EVENT: &str = "deploy-log";
const DEFAULT_INSTALL_DIR: &str = "/opt/arbpilot";

/// 服务器连接参数：密码与私钥只会写入系统钥匙串，不落盘到前端。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub install_dir: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredCredentials {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub version: Option<String>,
    pub is_root: bool,
    pub install_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployPlan {
    pub version: String,
    pub bundle_url: String,
    pub sha256: String,
}

fn entry(server: &ServerConfig) -> Result<keyring::Entry, String> {
    keyring::Entry::new(
        KEYRING_SERVICE,
        &format!("{}@{}:{}", server.username, server.host, server.port),
    )
    .map_err(|error| format!("打开系统钥匙串失败：{error}"))
}

fn install_dir(server: &ServerConfig) -> Result<String, String> {
    let dir = server
        .install_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_INSTALL_DIR)
        .trim_end_matches('/')
        .to_string();
    let valid = dir.starts_with('/')
        && !dir.contains("..")
        && dir
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'));
    if !valid {
        return Err("安装目录只能使用绝对路径，且仅包含字母、数字、. _ - /".to_string());
    }
    Ok(dir)
}

fn validate_host(host: &str) -> Result<(), String> {
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':'));
    if valid {
        Ok(())
    } else {
        Err("服务器地址格式不正确".to_string())
    }
}

fn validate_plan(plan: &DeployPlan) -> Result<(), String> {
    if !plan.bundle_url.starts_with("https://")
        || plan
            .bundle_url
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '\'' || ch == '"' || ch == '`')
    {
        return Err("部署包地址必须是 https 链接".to_string());
    }
    if plan.sha256.len() != 64 || !plan.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("部署包校验值格式不正确".to_string());
    }
    let version_valid = !plan.version.is_empty()
        && plan.version.len() <= 32
        && plan
            .version
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if !version_valid {
        return Err("版本号格式不正确".to_string());
    }
    Ok(())
}

fn connect(server: &ServerConfig) -> Result<Session, String> {
    validate_host(&server.host)?;
    if server.port == 0 {
        return Err("服务器端口无效".to_string());
    }
    let tcp = TcpStream::connect((server.host.as_str(), server.port))
        .map_err(|error| format!("无法连接 {}:{}（{error}）", server.host, server.port))?;
    // 构建镜像时可能长时间没有输出，因此只限制写入超时，读保持阻塞。
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));

    let mut session = Session::new().map_err(|error| format!("初始化 SSH 失败：{error}"))?;
    session.set_tcp_stream(tcp);
    session
        .handshake()
        .map_err(|error| format!("SSH 握手失败：{error}"))?;

    if server.auth == "key" {
        let key = server
            .private_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "请填写 SSH 私钥".to_string())?;
        session
            .userauth_pubkey_memory(
                &server.username,
                None,
                key,
                server.passphrase.as_deref().filter(|value| !value.is_empty()),
            )
            .map_err(|error| format!("私钥认证失败：{error}"))?;
    } else {
        let password = server
            .password
            .as_deref()
            .ok_or_else(|| "请填写服务器密码".to_string())?;
        session
            .userauth_password(&server.username, password)
            .map_err(|error| format!("密码认证失败：{error}"))?;
    }

    if !session.authenticated() {
        return Err("SSH 认证未通过，请检查账号或凭据".to_string());
    }
    Ok(session)
}

fn run_capture(session: &Session, command: &str) -> Result<String, String> {
    let mut channel = session
        .channel_session()
        .map_err(|error| format!("打开 SSH 通道失败：{error}"))?;
    channel
        .exec(command)
        .map_err(|error| format!("执行远程命令失败：{error}"))?;
    let mut output = String::new();
    channel
        .read_to_string(&mut output)
        .map_err(|error| format!("读取远程输出失败：{error}"))?;
    let _ = channel.wait_close();
    Ok(output)
}

#[tauri::command]
pub fn server_credentials_save(server: ServerConfig) -> Result<(), String> {
    let keyring_entry = entry(&server)?;
    let payload = serde_json::to_string(&StoredCredentials {
        password: server.password,
        private_key: server.private_key,
        passphrase: server.passphrase,
    })
    .map_err(|error| format!("序列化凭据失败：{error}"))?;
    keyring_entry
        .set_password(&payload)
        .map_err(|error| format!("写入系统钥匙串失败：{error}"))
}

#[tauri::command]
pub fn server_credentials_load(server: ServerConfig) -> Result<Option<StoredCredentials>, String> {
    match entry(&server)?.get_password() {
        Ok(value) => serde_json::from_str(&value)
            .map(Some)
            .map_err(|error| format!("解析钥匙串凭据失败：{error}")),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("读取系统钥匙串失败：{error}")),
    }
}

#[tauri::command]
pub fn server_credentials_delete(server: ServerConfig) -> Result<(), String> {
    match entry(&server)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("删除系统钥匙串凭据失败：{error}")),
    }
}

#[tauri::command]
pub async fn server_probe(server: ServerConfig) -> Result<ProbeResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = install_dir(&server)?;
        let session = connect(&server)?;
        let raw = run_capture(
            &session,
            &format!("id -u; cat '{dir}/.arbpilot-version' 2>/dev/null || true"),
        )?;
        let mut lines = raw.lines();
        let uid = lines.next().unwrap_or("").trim().to_string();
        let version = lines
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string);
        Ok(ProbeResult {
            version,
            is_root: uid == "0",
            install_dir: dir,
        })
    })
    .await
    .map_err(|error| format!("探测任务失败：{error}"))?
}

fn deploy_script(
    server: &ServerConfig,
    plan: &DeployPlan,
) -> Result<String, String> {
    validate_plan(plan)?;
    let dir = install_dir(server)?;
    let pass_b64 = BASE64.encode(server.password.clone().unwrap_or_default());
    Ok(format!(
        r#"set -e
PASS_B64='{pass_b64}'
INSTALL_DIR='{dir}'
mkdir -p "$INSTALL_DIR"
cd "$INSTALL_DIR"
echo "==> 下载部署包 {version}"
curl -fsSL --retry 3 --connect-timeout 20 -o .arbpilot-bundle.tgz '{url}'
echo "{sha}  .arbpilot-bundle.tgz" | sha256sum -c -
tar xzf .arbpilot-bundle.tgz
rm -f .arbpilot-bundle.tgz
echo '{version}' > .arbpilot-version
echo "==> 构建并启动执行器"
if [ "$(id -u)" -eq 0 ]; then
  bash deploy/install.sh
else
  PASS=$(printf '%s' "$PASS_B64" | base64 -d 2>/dev/null || true)
  printf '%s\n' "$PASS" | sudo -S -p '' bash deploy/install.sh
fi
echo "==> 部署完成：{version}"
"#,
        pass_b64 = pass_b64,
        dir = dir,
        version = plan.version,
        url = plan.bundle_url,
        sha = plan.sha256,
    ))
}

fn run_stream(app: &AppHandle, session: &Session, script: &str) -> Result<i32, String> {
    let mut channel = session
        .channel_session()
        .map_err(|error| format!("打开 SSH 通道失败：{error}"))?;
    channel
        .exec("bash -s 2>&1")
        .map_err(|error| format!("启动远程脚本失败：{error}"))?;
    channel
        .write_all(script.as_bytes())
        .map_err(|error| format!("发送部署脚本失败：{error}"))?;
    channel
        .send_eof()
        .map_err(|error| format!("结束脚本输入失败：{error}"))?;

    let mut stream = channel.stream(0);
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let chunk = String::from_utf8_lossy(&buffer[..read]).to_string();
                let _ = app.emit(LOG_EVENT, chunk);
            }
            Err(error) => return Err(format!("读取部署日志失败：{error}")),
        }
    }
    let _ = channel.wait_close();
    Ok(channel.exit_status().unwrap_or(-1))
}

#[tauri::command]
pub async fn server_deploy(
    app: AppHandle,
    server: ServerConfig,
    plan: DeployPlan,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let script = deploy_script(&server, &plan)?;
        let session = connect(&server)?;
        let status = run_stream(&app, &session, &script)?;
        if status != 0 {
            return Err(format!("远程部署失败（退出码 {status}），请查看上方日志"));
        }
        Ok(plan.version)
    })
    .await
    .map_err(|error| format!("部署任务失败：{error}"))?
}
