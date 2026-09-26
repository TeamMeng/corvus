//! 上游（OpenAI 兼容协议）故障的**分类**：把任意传输层错误归约成
//! 可穷举、可判定「能不能自愈」的少数几种病因。
//!
//! 为什么值得单独一个模块、单独一套测试？因为「上游报错」这条路径天生难以复现：
//! 它依赖网络、余额、限流窗口，以及各家网关千奇百怪的字段习惯。
//! 抽成**纯函数**之后，就能用构造出来的 (状态码, code, message) 三件套
//! 打满全部分支，不必真的把钱花光或把上下文塞爆。
//!
//! 放在 crate 根而不是 `operations/` 下：它被 `message.rs`（数据层）与
//! `render.rs`（表现层）共用，是跨层词汇，不是一道工序。

use async_openai::error::OpenAIError;
use serde::{Deserialize, Serialize};

/// 上游故障的病因。
///
/// ★ 它会被写进会话文件（`Message::error_kind`），因此一诞生就是
///   **持久化契约**的一部分：以后只能增变体，不能改已有变体的语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// 凭据无效 / 余额耗尽。重试**永远**没用，必须人类介入。
    AuthOrQuota,
    /// 限流。退避后重试是有意义的。
    RateLimited,
    /// 上下文超过模型上限。压缩历史后重试有意义 —— ④ 自愈的入口。
    ContextLengthExceeded,
    /// 网络、连接重置、流中断：请求可能根本没被处理。
    Transport,
    /// 其余一切（含 5xx、响应解析失败）。保守地当作「病因不明」。
    Unknown,
}

impl ErrorKind {
    /// 「原样重发一次」是否可能成功？
    ///
    /// 语义边界：它只回答「这个病因本身可不可逆」，
    /// **不**回答「该怎么处理」—— 退避多久、要不要压缩，属于调用方的策略。
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::ContextLengthExceeded | ErrorKind::RateLimited | ErrorKind::Transport
        )
    }
}

/// SDK 错误 → (病因, 上游原文)。
///
/// 只做分类，**不做措辞**：给人看的话术在 `render.rs`。
/// 两者不能互相替代 —— 原文里可能藏着官方错误码、请求 id（排查全靠它），
/// 病因则是「要不要压缩、要不要重试」的判断依据。
pub fn classify(error: &OpenAIError) -> (ErrorKind, String) {
    match error {
        OpenAIError::ApiError(resp) => (
            classify_parts(
                resp.status_code.as_u16(),
                resp.api_error
                    .code
                    .as_deref()
                    .or(resp.api_error.r#type.as_deref()),
                &resp.api_error.message,
            ),
            resp.to_string(),
        ),
        OpenAIError::Reqwest(e) => (ErrorKind::Transport, e.to_string()),
        OpenAIError::StreamError(e) => (ErrorKind::Transport, e.to_string()),
        other => (ErrorKind::Unknown, other.to_string()),
    }
}

/// 判定顺序本身就是设计决策：**message 优先于 status_code**。
fn classify_parts(status: u16, code: Option<&str>, message: &str) -> ErrorKind {
    let code = code.unwrap_or_default().to_ascii_lowercase();
    let message = message.to_ascii_lowercase();

    // 1) 上下文超长：各家写法差异最大的一种，先按关键词兜住。
    if code.contains("context_length_exceeded")
        || message.contains("context length")
        || message.contains("maximum context")
        || message.contains("too many tokens")
        || message.contains("reduce the length")
    {
        return ErrorKind::ContextLengthExceeded;
    }

    // 2) 余额耗尽：常和 429/402 混在一起。
    if code.contains("insufficient_quota")
        || message.contains("insufficient balance")
        || message.contains("insufficient quota")
        || message.contains("quota")
    {
        return ErrorKind::AuthOrQuota;
    }

    // 3) 状态码兜底
    match status {
        401..=403 => ErrorKind::AuthOrQuota,
        429 => ErrorKind::RateLimited,
        // ★ 5xx 故意归入 Unknown：现在没有任何人为它做重试，
        //   贸然标成「可重试」只会给压缩工序留一条没人走的分支。
        //   真见到 5xx 满天飞时，再拆出 UpstreamUnavailable。
        _ => ErrorKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_context_length_by_code() {
        assert_eq!(
            classify_parts(400, Some("context_length_exceeded"), "bad request"),
            ErrorKind::ContextLengthExceeded
        );
    }

    #[test]
    fn detects_context_length_by_message_when_code_is_generic() {
        // 真实见闻：很多网关把 code 统一成 invalid_request_error，
        // 只有 message 里透露了真正原因 —— 只按 code 判定会漏。
        assert_eq!(
            classify_parts(
                400,
                Some("invalid_request_error"),
                "This model's maximum context length is 65536 tokens."
            ),
            ErrorKind::ContextLengthExceeded
        );
    }

    #[test]
    fn quota_wins_over_rate_limit_when_both_look_true() {
        // 真实见闻：OpenAI 余额耗尽返回的是 **429** + insufficient_quota ——
        // 两个信号都像「限流」。只按状态码分类 → 判成限流 →
        // 退避重试到天荒地老也不会成功。
        assert_eq!(
            classify_parts(
                429,
                Some("insufficient_quota"),
                "You exceeded your current quota"
            ),
            ErrorKind::AuthOrQuota
        );
    }

    #[test]
    fn classifies_by_status_when_message_says_nothing() {
        assert_eq!(
            classify_parts(402, None, "Insufficient Balance"),
            ErrorKind::AuthOrQuota
        );
        assert_eq!(
            classify_parts(401, None, "invalid api key"),
            ErrorKind::AuthOrQuota
        );
        assert_eq!(
            classify_parts(429, None, "slow down"),
            ErrorKind::RateLimited
        );
        assert_eq!(classify_parts(503, None, "bad gateway"), ErrorKind::Unknown);
    }

    #[test]
    fn retryable_is_only_the_recoverable_kinds() {
        assert!(ErrorKind::ContextLengthExceeded.is_retryable());
        assert!(ErrorKind::RateLimited.is_retryable());
        assert!(ErrorKind::Transport.is_retryable());
        assert!(!ErrorKind::AuthOrQuota.is_retryable());
        assert!(!ErrorKind::Unknown.is_retryable());
    }
}
