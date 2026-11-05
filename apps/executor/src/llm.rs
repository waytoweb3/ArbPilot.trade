//! 用自然语言描述生成策略代码（WebAssembly Text）。
//!
//! - 密钥只保存在执行器本机，不通过 API 回传，也不写日志；
//! - 生成结果只是源码，仍需用户查看、修改、编译与显式部署；
//! - 生成的模块与手写模块走同一套沙箱检查，没有密钥、网络或下单能力。

use std::time::Duration;

use serde_json::{json, Value};

use crate::{config::Config, http_client::HttpClient};

/// AI 生成结果。`source` 已去除 Markdown 围栏，可直接编译。
#[derive(Debug, Clone)]
pub struct GeneratedCode {
    pub source: String,
    pub notes: Option<String>,
}

const SYSTEM_PROMPT: &str = r#"你是 ArbPilot 的策略代码生成器。只输出一个 WebAssembly Text (WAT) 代码块，不要输出解释性文字。

沙箱 ABI 版本 1，模块必须导出：
- memory
- alloc(len: i32) -> i32：返回可写入 len 字节的线性内存指针，必要时用 memory.grow 扩容
- abi_version() -> i32：必须返回 1
- evaluate(input_ptr: i32, input_len: i32) -> i32：返回 0 表示成功

模块只能导入命名空间 "arbpilot" 的以下函数：
- markets() -> i32：可用市场数量
- market(index: i32, field: i32) -> f64：读取市场字段
- position_present() -> i32：当前是否有持仓
- position_notional() -> f64：持仓名义价值（USD）
- position_held_secs() -> i64：持仓已持有秒数
- open(symbol_ptr: i32, symbol_len: i32, notional_usd: f64) -> i32：提交开仓意图
- close(symbol_ptr: i32, symbol_len: i32) -> i32：提交平仓意图
- hold() -> i32：本次不操作
- emit(ptr: i32, len: i32) -> i32：提交原始 JSON 决策（可不用）
- log(ptr: i32, len: i32) -> i32：写一行日志

market 字段编号：
0 spot_bid, 1 spot_ask, 2 perp_bid, 3 perp_ask, 4 funding_rate,
5 gross_spread_bps, 6 net_edge_bps, 7 max_notional_usd。

安全边界（不可绕过）：
- 没有 WASI、文件、网络、环境变量、账户或交易所密钥；
- 不能撤单、转账或直接下单；输出意图仍会经过执行器统一风控网关；
- 只能输出 open / close / hold 三种意图。

代码要求：
- 交易对字符串写在模块自己的 data 段，通过指针和长度传给 open/close；
- 用 ;; 注释说明阈值含义；
- 默认分支必须调用 hold()；
- 不要使用浮点 NaN 或无限循环。"#;

/// 调用 OpenAI 兼容的 chat completions 接口生成 WAT 源码。
pub async fn generate(
    config: &Config,
    description: &str,
    context: &str,
) -> Result<GeneratedCode, String> {
    let key = config
        .llm_api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            "未配置 ARBPILOT_LLM_API_KEY，无法调用 AI 生成策略代码".to_string()
        })?
        .to_string();

    let url = format!(
        "{}/chat/completions",
        config.llm_base_url.trim_end_matches('/')
    );
    let body = json!({
        "model": config.llm_model,
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            {
                "role": "user",
                "content": format!(
                    "当前策略定义：\n{context}\n\n用户描述：\n{description}\n\n请只输出一个 ```wat 代码块。"
                )
            }
        ],
    });
    let body = serde_json::to_string(&body).map_err(|error| format!("构造 AI 请求失败: {error}"))?;
    let timeout = Duration::from_secs(config.llm_timeout_secs.max(5));
    let proxy = config.market_proxy.clone();

    tokio::task::spawn_blocking(move || {
        let http = HttpClient::new(timeout, proxy)
            .map_err(|error| format!("初始化 AI 请求客户端失败: {error}"))?;
        let authorization = format!("Bearer {key}");
        let response = http
            .post_body_with_headers(
                &url,
                &[
                    ("Authorization", authorization.as_str()),
                    ("Content-Type", "application/json"),
                ],
                &body,
            )
            .map_err(|error| format!("调用 AI 接口失败: {error}"))?;
        parse_response(&response)
    })
    .await
    .map_err(|error| format!("AI 请求线程异常退出: {error}"))?
}

/// 解析 chat completions 响应并提取 WAT 源码。
pub fn parse_response(response: &Value) -> Result<GeneratedCode, String> {
    let content = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| "AI 响应缺少 choices[0].message.content".to_string())?;

    let source = extract_wat(content);
    if !source.contains("(module") {
        return Err("AI 返回的内容不是 WebAssembly Text 模块".to_string());
    }
    Ok(GeneratedCode {
        source,
        notes: None,
    })
}

/// 去掉 Markdown 代码围栏；没有围栏时返回原文。
pub fn extract_wat(content: &str) -> String {
    let trimmed = content.trim();
    for tag in ["```wat", "```webassembly", "```"] {
        let Some(start) = trimmed.find(tag) else {
            continue;
        };
        let rest = &trimmed[start + tag.len()..];
        let rest = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix('\n'))
            .unwrap_or(rest);
        if let Some(end) = rest.find("```") {
            return rest[..end].trim().to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_wat() {
        let content = "说明\n```wat\n(module (func))\n```\n结束";
        assert_eq!(extract_wat(content), "(module (func))");
    }

    #[test]
    fn extracts_plain_wat() {
        assert_eq!(extract_wat(" (module) "), "(module)");
    }

    #[test]
    fn parses_chat_completion() {
        let response = json!({
            "choices": [
                { "message": { "content": "```wat\n(module)\n```" } }
            ]
        });
        let generated = parse_response(&response).expect("应能解析");
        assert_eq!(generated.source, "(module)");
    }

    #[test]
    fn rejects_non_wat_content() {
        let response = json!({ "choices": [ { "message": { "content": "hello" } } ] });
        assert!(parse_response(&response).is_err());
    }
}
