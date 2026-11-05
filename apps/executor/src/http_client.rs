use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Context};
use rustls::{pki_types::ServerName, ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde_json::Value;

/// 极简 HTTP/1.1 客户端，仅支持 `GET` + `Connection: close`。
///
/// 执行器只读取交易所公开行情，用 rustls 直连可以避免为一次 GET 引入完整的
/// HTTP 客户端依赖；请求与响应都是小体积 JSON，一次读到底足够。
/// 配置代理时，https 请求先向代理发 `CONNECT` 建立隧道再握手。
pub struct HttpClient {
    tls: Arc<ClientConfig>,
    timeout: Duration,
    proxy: Option<Proxy>,
}

impl HttpClient {
    pub fn new(timeout: Duration, proxy: Option<String>) -> anyhow::Result<Self> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("初始化 TLS 协议版本失败")?
            .with_root_certificates(roots)
            .with_no_client_auth();

        let proxy = match proxy
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            Some(url) => Some(Proxy::parse(url)?),
            None => None,
        };

        Ok(Self {
            tls: Arc::new(tls),
            timeout,
            proxy,
        })
    }

    /// 同步 GET 并解析 JSON。调用方需放在 `spawn_blocking` 中执行。
    pub fn get_json(&self, url: &str) -> anyhow::Result<Value> {
        let body = self.get(url)?;
        serde_json::from_slice(&body).with_context(|| format!("解析 JSON 失败: {url}"))
    }

    /// 带自定义请求头的 GET（私有接口签名需要 API Key 等头部）。
    pub fn get_json_with_headers(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<Value> {
        let body = self.request("GET", url, headers, None)?;
        serde_json::from_slice(&body).with_context(|| format!("解析 JSON 失败: {url}"))
    }

    /// 同步 POST JSON 并解析 JSON 响应（Hyperliquid 的 info 接口只接受 POST）。
    pub fn post_json(&self, url: &str, payload: &Value) -> anyhow::Result<Value> {
        let body = payload.to_string();
        let raw = self.request("POST", url, &[], Some(&body))?;
        serde_json::from_slice(&raw).with_context(|| format!("解析 JSON 失败: {url}"))
    }

    /// 无请求体的 POST：签名参数放在 URL 上，交易所私有下单接口常用这种形式。
    pub fn post_with_headers(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<Value> {
        let raw = self.request("POST", url, headers, None)?;
        serde_json::from_slice(&raw).with_context(|| format!("解析 JSON 失败: {url}"))
    }

    /// 带自定义请求头与 JSON 请求体的 POST，签名写在头部或参与请求体哈希。
    pub fn post_body_with_headers(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> anyhow::Result<Value> {
        let raw = self.request("POST", url, headers, Some(body))?;
        serde_json::from_slice(&raw).with_context(|| format!("解析 JSON 失败: {url}"))
    }

    /// 建立到 `url` 主机的原始连接（`https://` / `wss://` 走 TLS，可经代理隧道），
    /// 供 WebSocket 握手复用同一套证书与代理配置。
    pub(crate) fn open(&self, url: &str) -> anyhow::Result<Connection> {
        let target = Target::parse(url)?;
        target.connect(&self.tls, self.timeout, self.proxy.as_ref())
    }

    pub fn get(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        self.request("GET", url, &[], None)
    }

    /// 通用请求：自定义方法与请求头，可选 JSON 请求体。
    pub fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> anyhow::Result<Vec<u8>> {
        let target = Target::parse(url)?;
        let mut stream = target.connect(&self.tls, self.timeout, self.proxy.as_ref())?;

        stream
            .set_read_timeout(Some(self.timeout))
            .context("设置读超时失败")?;
        stream
            .set_write_timeout(Some(self.timeout))
            .context("设置写超时失败")?;

        // 走代理访问明文 http 时，请求行必须是绝对地址。
        let path = encode_non_ascii(&target.path);
        let request_target = match (&self.proxy, target.tls) {
            (Some(_), false) => format!("http://{}{path}", target.host_header()),
            _ => path,
        };
        let mut request = format!(
            "{method} {request_target} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ArbPilot/0.1\r\nAccept: application/json\r\n",
            target.host_header()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if let Some(body) = body {
            if !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            {
                request.push_str("Content-Type: application/json\r\n");
            }
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("Connection: close\r\n\r\n");
        if let Some(body) = body {
            request.push_str(body);
        }
        stream
            .write_all(request.as_bytes())
            .context("发送请求失败")?;

        let mut raw = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => raw.extend_from_slice(&buffer[..read]),
                // 部分服务端直接断开而不发 close_notify，已收到的数据仍然完整。
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error).context("读取响应失败"),
            }
        }
        parse_response(&raw)
    }
}

pub(crate) enum Connection {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl Connection {
    pub(crate) fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_read_timeout(timeout),
            Self::Tls(stream) => stream.sock.set_read_timeout(timeout),
        }
    }

    pub(crate) fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_write_timeout(timeout),
            Self::Tls(stream) => stream.sock.set_write_timeout(timeout),
        }
    }
}

struct Target {
    tls: bool,
    host: String,
    port: u16,
    /// 含查询字符串的请求路径。
    path: String,
}

/// 对请求路径中的非 ASCII 字节做百分号编码。
///
/// 部分交易所的合约名带中文（如 Bitget 的 `哈基米USDT`），原始 UTF-8 字节写进
/// 请求行属于非法字符，会被网关判为 `HTTP 400`。
fn encode_non_ascii(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if (0x21..=0x7e).contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

impl Target {
    fn parse(url: &str) -> anyhow::Result<Self> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("wss://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("ws://") {
            (false, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            bail!("仅支持 http/https/ws/wss 地址: {url}");
        };

        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            bail!("地址缺少主机名: {url}");
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host.to_string(),
                port.parse().with_context(|| format!("端口不合法: {url}"))?,
            ),
            None => (authority.to_string(), if tls { 443 } else { 80 }),
        };

        Ok(Self {
            tls,
            host,
            port,
            path: path.to_string(),
        })
    }

    fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// `Host` 头：默认端口不写端口号，部分交易所的 WAF 会拒绝 `host:443`。
    fn host_header(&self) -> String {
        if self.port == if self.tls { 443 } else { 80 } {
            self.host.clone()
        } else {
            self.authority()
        }
    }

    fn connect(
        &self,
        tls: &Arc<ClientConfig>,
        timeout: Duration,
        proxy: Option<&Proxy>,
    ) -> anyhow::Result<Connection> {
        let socket = match proxy {
            Some(proxy) => proxy.connect(self, timeout)?,
            None => self.connect_direct(timeout)?,
        };

        if !self.tls {
            return Ok(Connection::Plain(socket));
        }

        let server_name =
            ServerName::try_from(self.host.clone()).context("主机名不适用于 TLS 握手")?;
        let session =
            ClientConnection::new(tls.clone(), server_name).context("创建 TLS 会话失败")?;
        Ok(Connection::Tls(Box::new(StreamOwned::new(session, socket))))
    }

    fn connect_direct(&self, timeout: Duration) -> anyhow::Result<TcpStream> {
        let address = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .with_context(|| format!("解析域名失败: {}", self.host))?
            .next()
            .ok_or_else(|| anyhow!("域名无可用地址: {}", self.host))?;

        TcpStream::connect_timeout(&address, timeout)
            .with_context(|| format!("连接 {}:{} 失败", self.host, self.port))
    }
}

/// 本地/远端 HTTP 代理（仅支持明文 `http://` 代理，https 目标通过 `CONNECT` 隧道）。
struct Proxy {
    host: String,
    port: u16,
}

impl Proxy {
    fn parse(url: &str) -> anyhow::Result<Self> {
        let rest = match url.strip_prefix("http://") {
            Some(rest) => rest,
            None => url,
        };
        if rest.starts_with("https://") {
            bail!("暂不支持 https 代理，请使用 http:// 形式的代理地址: {url}");
        }

        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty() {
            bail!("代理地址缺少主机名: {url}");
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host.to_string(),
                port.parse()
                    .with_context(|| format!("代理端口不合法: {url}"))?,
            ),
            None => (authority.to_string(), 8080),
        };

        Ok(Self { host, port })
    }

    /// 连接代理；对 https 目标先建立 `CONNECT` 隧道。
    fn connect(&self, target: &Target, timeout: Duration) -> anyhow::Result<TcpStream> {
        let address = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .with_context(|| format!("解析代理域名失败: {}", self.host))?
            .next()
            .ok_or_else(|| anyhow!("代理域名无可用地址: {}", self.host))?;

        let mut socket = TcpStream::connect_timeout(&address, timeout)
            .with_context(|| format!("连接代理 {}:{} 失败", self.host, self.port))?;
        socket
            .set_read_timeout(Some(timeout))
            .context("设置代理读超时失败")?;
        socket
            .set_write_timeout(Some(timeout))
            .context("设置代理写超时失败")?;

        if !target.tls {
            return Ok(socket);
        }

        let authority = target.authority();
        let request =
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: keep-alive\r\n\r\n");
        socket
            .write_all(request.as_bytes())
            .context("发送代理隧道请求失败")?;

        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match socket.read(&mut byte) {
                Ok(0) => bail!("代理关闭连接，未返回隧道响应"),
                Ok(_) => head.push(byte[0]),
                Err(error) => return Err(error).context("读取代理隧道响应失败"),
            }
            if head.len() > 8 * 1024 {
                bail!("代理隧道响应过长");
            }
        }

        let status = String::from_utf8_lossy(&head);
        let code = status
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        if code != 200 {
            let first = status.lines().next().unwrap_or_default().trim().to_string();
            bail!("代理隧道建立失败: {first}");
        }

        Ok(socket)
    }
}

fn parse_response(raw: &[u8]) -> anyhow::Result<Vec<u8>> {
    let split = find(raw, b"\r\n\r\n").ok_or_else(|| anyhow!("响应缺少头部结束标记"))?;
    let (head, body) = raw.split_at(split);
    let body = &body[4..];

    let head = std::str::from_utf8(head).context("响应头部不是合法 UTF-8")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow!("无法解析状态行: {status_line}"))?;

    let mut chunked = false;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if name == "content-length" {
            content_length = value.parse::<usize>().ok();
        }
    }

    if code != 200 {
        let snippet = String::from_utf8_lossy(&body[..body.len().min(200)]).replace('\n', " ");
        bail!("HTTP {code}: {snippet}");
    }

    if chunked {
        return decode_chunked(body);
    }
    if let Some(length) = content_length {
        if body.len() < length {
            bail!("响应体不完整：期望 {length} 字节，实际 {}", body.len());
        }
        return Ok(body[..length].to_vec());
    }
    Ok(body.to_vec())
}

fn decode_chunked(mut data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = find(data, b"\r\n").ok_or_else(|| anyhow!("分块响应缺少长度行"))?;
        let line = std::str::from_utf8(&data[..line_end]).context("分块长度不是合法 UTF-8")?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
            .with_context(|| format!("分块长度不合法: {line}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            bail!("分块长度超出响应体");
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        if data.starts_with(b"\r\n") {
            data = &data[2..];
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target_with_port() {
        let target = Target::parse("https://fapi.binance.com/fapi/v1/fundingRate?symbol=BTCUSDT")
            .expect("地址应可解析");
        assert!(target.tls);
        assert_eq!(target.host, "fapi.binance.com");
        assert_eq!(target.port, 443);
        assert_eq!(target.path, "/fapi/v1/fundingRate?symbol=BTCUSDT");
    }

    #[test]
    fn parses_plain_http_with_explicit_port() {
        let target = Target::parse("http://127.0.0.1:9911/fapi/v1/ping").expect("地址应可解析");
        assert!(!target.tls);
        assert_eq!(target.host, "127.0.0.1");
        assert_eq!(target.port, 9911);
        assert_eq!(target.path, "/fapi/v1/ping");
    }

    #[test]
    fn parses_http_proxy() {
        let proxy = Proxy::parse("http://127.0.0.1:7897").expect("代理应可解析");
        assert_eq!(proxy.host, "127.0.0.1");
        assert_eq!(proxy.port, 7897);
    }

    #[test]
    fn percent_encodes_non_ascii_path() {
        assert_eq!(
            encode_non_ascii("/api/v2/mix/market/history-fund-rate?symbol=哈基米USDT&pageSize=1"),
            "/api/v2/mix/market/history-fund-rate?symbol=%E5%93%88%E5%9F%BA%E7%B1%B3USDT&pageSize=1"
        );
    }

    #[test]
    fn rejects_https_proxy() {
        assert!(Proxy::parse("https://127.0.0.1:7897").is_err());
    }

    #[test]
    fn decodes_content_length_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]";
        assert_eq!(parse_response(raw).expect("应可解析"), b"[]".to_vec());
    }

    #[test]
    fn decodes_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[]\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).expect("应可解析"), b"[]".to_vec());
    }

    #[test]
    fn surfaces_non_200_status() {
        let raw = b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 4\r\n\r\nbusy";
        let error = parse_response(raw).expect_err("非 200 应当报错");
        assert!(error.to_string().contains("429"));
    }
}
