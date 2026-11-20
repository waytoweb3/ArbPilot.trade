//! 极简 WebSocket 客户端（RFC 6455 文本帧）。
//!
//! 执行器只需要接收交易所的公开行情推送，因此没有引入完整 WebSocket 依赖：
//! 握手与帧编解码都在这里，复用 [`HttpClient`] 的 TLS 证书与代理隧道配置。

use std::{
    io::{Read, Write},
    time::Duration,
};

use anyhow::{anyhow, bail, Context};
use uuid::Uuid;

use crate::http_client::{Connection, HttpClient};

/// 单帧上限，防止异常数据把内存吃满。
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// 握手响应头上限。
const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;

/// 已建立的 WebSocket 连接。
pub struct WebSocket {
    conn: Connection,
    parser: FrameParser,
    closed: bool,
}

impl WebSocket {
    /// 建立连接并完成 `Upgrade` 握手。
    ///
    /// `poll_timeout` 决定 [`WebSocket::poll`] 的最长阻塞时间，调用方借此定期发送心跳。
    pub fn connect(
        http: &HttpClient,
        url: &str,
        handshake_timeout: Duration,
        poll_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let (host, path) = split_url(url)?;
        let mut conn = http.open(url)?;
        conn.set_read_timeout(Some(handshake_timeout))
            .context("设置 WebSocket 读超时失败")?;
        conn.set_write_timeout(Some(handshake_timeout))
            .context("设置 WebSocket 写超时失败")?;

        let key = base64_encode(Uuid::new_v4().as_bytes());
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: ArbPilot/0.1\r\n\r\n"
        );
        conn.write_all(request.as_bytes())
            .context("发送 WebSocket 握手失败")?;

        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match conn.read(&mut byte) {
                Ok(0) => bail!("WebSocket 握手时连接被关闭"),
                Ok(_) => head.push(byte[0]),
                Err(error) => return Err(error).context("读取 WebSocket 握手响应失败"),
            }
            if head.len() > MAX_HANDSHAKE_BYTES {
                bail!("WebSocket 握手响应过长");
            }
        }

        let status = String::from_utf8_lossy(&head);
        let status_line = status.lines().next().unwrap_or_default().trim().to_string();
        let code = status_line.split_whitespace().nth(1).unwrap_or_default();
        if code != "101" {
            bail!("WebSocket 握手被拒绝: {status_line}");
        }

        conn.set_read_timeout(Some(poll_timeout))
            .context("设置 WebSocket 轮询超时失败")?;
        Ok(Self {
            conn,
            parser: FrameParser::new(),
            closed: false,
        })
    }

    pub fn send_text(&mut self, payload: &str) -> anyhow::Result<()> {
        self.send_frame(0x1, payload.as_bytes())
    }

    /// 读取一条消息。
    ///
    /// - 有完整消息时返回 `Ok(Some(event))`；
    /// - 轮询超时内没有新数据时返回 `Ok(None)`，调用方可继续做心跳；
    /// - 连接关闭或协议错误时返回 `Err`，调用方负责重连。
    ///
    /// 先消费解析器里已缓冲的帧，缓冲不足时才读 socket；否则高频率行情会让
    /// 缓冲无限增长，ping 得不到及时回复而被交易所断开。
    pub fn poll(&mut self) -> anyhow::Result<Option<WsEvent>> {
        if self.closed {
            bail!("WebSocket 已关闭");
        }

        loop {
            match self.parser.next_event()? {
                Some(WsEvent::Text(text)) => return Ok(Some(WsEvent::Text(text))),
                Some(WsEvent::Binary(bytes)) => return Ok(Some(WsEvent::Binary(bytes))),
                Some(WsEvent::Ping(payload)) => self.send_frame(0xA, &payload)?,
                Some(WsEvent::Close) => {
                    self.closed = true;
                    bail!("对端发送 WebSocket 关闭帧");
                }
                None => {
                    if !self.read_chunk()? {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// 从 socket 读取一块数据；读超时返回 `false`。
    fn read_chunk(&mut self) -> anyhow::Result<bool> {
        let mut chunk = [0u8; 8192];
        match self.conn.read(&mut chunk) {
            Ok(0) => {
                self.closed = true;
                bail!("WebSocket 连接已被对端关闭");
            }
            Ok(read) => {
                self.parser.push(&chunk[..read]);
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error).context("读取 WebSocket 失败"),
        }
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> anyhow::Result<()> {
        let mask = Uuid::new_v4().into_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        match payload.len() {
            len if len < 126 => frame.push(0x80 | len as u8),
            len if len <= usize::from(u16::MAX) => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(len as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask[..4]);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );

        self.conn
            .write_all(&frame)
            .context("发送 WebSocket 帧失败")?;
        self.conn.flush().context("刷新 WebSocket 缓冲失败")
    }
}

/// 从字节流中拆出的 WebSocket 事件。
#[derive(Debug, PartialEq, Eq)]
pub enum WsEvent {
    Text(String),
    /// 二进制消息（部分交易所用 gzip 压缩后再以二进制帧下发）。
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close,
}

/// 增量帧解析器：读超时只影响何时解析，不丢已收到的半帧。
pub struct FrameParser {
    buffer: Vec<u8>,
    fragments: Vec<u8>,
    /// 分片消息的首帧类型：0x1 文本、0x2 二进制。
    fragment_opcode: Option<u8>,
}

impl FrameParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            fragments: Vec::new(),
            fragment_opcode: None,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_event(&mut self) -> anyhow::Result<Option<WsEvent>> {
        loop {
            let Some((fin, opcode, payload)) = self.take_frame()? else {
                return Ok(None);
            };
            match opcode {
                // 续帧：拼接到上一次未完成的消息。
                0x0 => {
                    self.fragments.extend_from_slice(&payload);
                    if fin {
                        let opcode = self.fragment_opcode.take().unwrap_or(0x1);
                        let bytes = std::mem::take(&mut self.fragments);
                        return match opcode {
                            0x2 => Ok(Some(WsEvent::Binary(bytes))),
                            _ => String::from_utf8(bytes)
                                .map(|text| Some(WsEvent::Text(text)))
                                .context("WebSocket 文本帧不是合法 UTF-8"),
                        };
                    }
                }
                0x1 => {
                    if fin {
                        return String::from_utf8(payload)
                            .map(|text| Some(WsEvent::Text(text)))
                            .context("WebSocket 文本帧不是合法 UTF-8");
                    }
                    self.fragments = payload;
                    self.fragment_opcode = Some(0x1);
                }
                0x2 => {
                    if fin {
                        return Ok(Some(WsEvent::Binary(payload)));
                    }
                    self.fragments = payload;
                    self.fragment_opcode = Some(0x2);
                }
                0x8 => return Ok(Some(WsEvent::Close)),
                0x9 => return Ok(Some(WsEvent::Ping(payload))),
                // pong 与未知控制帧直接忽略。
                _ => {}
            }
        }
    }

    fn take_frame(&mut self) -> anyhow::Result<Option<(bool, u8, Vec<u8>)>> {
        let data = &self.buffer;
        if data.len() < 2 {
            return Ok(None);
        }

        let fin = data[0] & 0x80 != 0;
        let opcode = data[0] & 0x0f;
        let masked = data[1] & 0x80 != 0;
        let mut len = usize::from(data[1] & 0x7f);
        let mut offset = 2;
        if len == 126 {
            if data.len() < 4 {
                return Ok(None);
            }
            len = usize::from(u16::from_be_bytes([data[2], data[3]]));
            offset = 4;
        } else if len == 127 {
            if data.len() < 10 {
                return Ok(None);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&data[2..10]);
            len = usize::try_from(u64::from_be_bytes(bytes))
                .map_err(|_| anyhow!("WebSocket 帧长度溢出"))?;
            offset = 10;
        }
        if len > MAX_FRAME_BYTES {
            bail!("WebSocket 帧过大: {len} 字节");
        }

        let mask_offset = offset;
        if masked {
            offset += 4;
        }
        if data.len() < offset + len {
            return Ok(None);
        }

        let mut payload = data[offset..offset + len].to_vec();
        if masked {
            let mask = &data[mask_offset..mask_offset + 4];
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        self.buffer.drain(..offset + len);
        Ok(Some((fin, opcode, payload)))
    }
}

fn split_url(url: &str) -> anyhow::Result<(String, String)> {
    let rest = url
        .strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .ok_or_else(|| anyhow!("仅支持 ws/wss 地址: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        bail!("WebSocket 地址缺少主机名: {url}");
    }
    Ok((authority.to_string(), path.to_string()))
}

/// 标准字母表 + `=` 填充的 Base64，仅用于握手随机数。
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map(|value| u32::from(*value)).unwrap_or(0);
        let b2 = chunk.get(2).map(|value| u32::from(*value)).unwrap_or(0);
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(combined >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(combined >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(combined >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[combined as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工拼一个服务端（未加掩码）帧。
    fn server_frame(opcode: u8, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.push(if fin { 0x80 } else { 0 } | opcode);
        if payload.len() < 126 {
            frame.push(payload.len() as u8);
        } else {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn decodes_text_and_fragments() {
        let mut parser = FrameParser::new();
        parser.push(&server_frame(0x1, false, b"hel"));
        parser.push(&server_frame(0x0, true, b"lo"));

        assert_eq!(
            parser.next_event().expect("应解析成功"),
            Some(WsEvent::Text("hello".into()))
        );
        assert_eq!(parser.next_event().expect("应解析成功"), None);
    }

    #[test]
    fn waits_for_incomplete_frame() {
        let mut parser = FrameParser::new();
        parser.push(&server_frame(0x1, true, b"abc")[..4]);
        assert_eq!(parser.next_event().expect("半帧应等待"), None);

        parser.push(&server_frame(0x1, true, b"abc")[4..]);
        assert_eq!(
            parser.next_event().expect("补全后应解析"),
            Some(WsEvent::Text("abc".into()))
        );
    }

    #[test]
    fn surfaces_ping_and_close() {
        let mut parser = FrameParser::new();
        parser.push(&server_frame(0x9, true, b"hb"));
        assert_eq!(
            parser.next_event().expect("应解析成功"),
            Some(WsEvent::Ping(b"hb".to_vec()))
        );

        parser.push(&server_frame(0x8, true, b""));
        assert_eq!(
            parser.next_event().expect("应解析成功"),
            Some(WsEvent::Close)
        );
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut frame = vec![0x81, 127];
        frame.extend_from_slice(&(MAX_FRAME_BYTES as u64 + 1).to_be_bytes());
        let mut parser = FrameParser::new();
        parser.push(&frame);
        assert!(parser.next_event().is_err());
    }
}
