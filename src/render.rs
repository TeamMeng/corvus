//! 事件 → 人类可读文本的**纯函数**映射。
//!
//! 放在库里而不是 `main.rs`，有两个理由：
//! 1. 它属于「表现层逻辑」，未来 TUI / Web / Iggy 订阅端会复用同一套显示语义；
//! 2. 纯函数（不打印、不触 IO）才能被单测直接断言 —— 而"到底有没有渲染出来"
//!    恰恰是最容易悄悄坏掉、又最难发现的一环。

use crate::events::AgentEvent;
use crate::message::Role;
use crate::provider_error::ErrorKind;

/// 上游故障 → 人类可读文案。
///
/// 同一个病因，Web 端可以渲染成红色气泡、TUI 端可以渲染成一行告警，
/// 但**事实只有一份**，存在转录里 —— 所以措辞必须在表现层，不能存进消息。
fn render_upstream_error(kind: ErrorKind, detail: &str) -> String {
    match kind {
        ErrorKind::AuthOrQuota => format!(
            "⚠️ 上游拒绝了本次请求（凭据无效或余额耗尽，本进程内重试无用）：{detail}\n   修好凭据后 exit，再用 cargo run -- --continue 接着聊，历史不会丢。"
        ),
        ErrorKind::RateLimited => {
            format!("⚠️ 上游限流：{detail}\n   稍等片刻再发一次即可，历史与用量都已保留。")
        }
        ErrorKind::ContextLengthExceeded => format!(
            "⚠️ 上下文超出模型上限：{detail}\n   已尝试压缩历史自愈；若连续失败，请 exit 后 --continue，或换个上下文更大的模型。"
        ),
        ErrorKind::Transport => format!("⚠️ 与上游的连接异常：{detail}"),
        ErrorKind::Unknown => format!("⚠️ 上游调用失败（病因未识别）：{detail}"),
    }
}

/// 返回该事件应当显示给人类的文本；`None` 表示无需显示。
pub fn render_text(event: &AgentEvent) -> Option<String> {
    match event {
        // 只渲染「助手说的话」：
        // 用户输入终端已回显，工具结果无需刷屏，内部协调记录本就双不可见。
        AgentEvent::MessageAppended { message } => {
            if !message.user_visible || message.content.is_empty() {
                return None;
            }

            // 上游故障不是「模型说的话」：同一条消息，措辞由病因决定。
            if let Some(kind) = message.error_kind {
                return Some(render_upstream_error(kind, &message.content));
            }

            (message.role == Role::Assistant).then(|| message.content.clone())
        }

        AgentEvent::ToolStarted {
            tool, arguments, ..
        } => Some(format!("  ⚡ [{tool}] {arguments}")),

        AgentEvent::ApprovalRequested { calls } => {
            let mut text = String::from("\n⚠️  检测到高危操作，已暂停执行，等待你裁决：\n");
            for call in calls {
                text.push_str(&format!("   • [{}] {}\n", call.name, call.arguments));
            }
            text.push_str("\n输入 y 允许执行；其它任意输入均视为拒绝");
            Some(text)
        }

        // 历史被改写是个**大事件**：没有这条提示，人类只会觉得 Agent 突然不记得事了
        AgentEvent::HistoryReplaced { before, after } => Some(format!(
            "\n〔历史已压缩：模型可见消息 {before} 条 → {after} 条〕\n"
        )),

        // 纯生命周期事件：只进日志，不进终端对话流
        AgentEvent::RunStarted
        | AgentEvent::RunFinished
        | AgentEvent::OperationApplied { .. }
        | AgentEvent::ToolFinished { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, ToolCall};

    #[test]
    fn renders_assistant_reply() {
        let event = AgentEvent::MessageAppended {
            message: Message::assistant("你好"),
        };
        assert_eq!(render_text(&event).as_deref(), Some("你好"));
    }

    /// 回归测试：曾经因为引擎漏发 `MessageAppended`，
    /// 模型答了、转录存了，终端却一片安静。
    #[test]
    fn skips_everything_that_must_not_be_printed() {
        let cases = [
            // 用户发言：终端已回显，重复打印是噪音
            AgentEvent::MessageAppended {
                message: Message::user("你好"),
            },
            // 工具结果：可能上千行，不该刷屏
            AgentEvent::MessageAppended {
                message: Message::tool_response("c1", "output"),
            },
            // 内部协调记录：双方都不可见
            AgentEvent::MessageAppended {
                message: Message::approval_pending(),
            },
            // 空回复：模型既没说话也没调工具
            AgentEvent::MessageAppended {
                message: Message::assistant(""),
            },
        ];

        for event in cases {
            assert!(render_text(&event).is_none(), "不该渲染: {event:?}");
        }
    }

    #[test]
    fn renders_provider_error_as_warning_not_dialogue() {
        let event = AgentEvent::MessageAppended {
            message: Message::error(ErrorKind::AuthOrQuota, "402 Insufficient Balance"),
        };

        let text = render_text(&event).expect("上游故障必须让人看见");
        assert!(
            text.contains("402 Insufficient Balance"),
            "必须保留上游原文: {text}"
        );
        assert!(text.contains("凭据"), "必须说清病因: {text}");
        assert!(
            text.contains("--continue"),
            "必须告诉人类下一步怎么办: {text}"
        );
    }

    #[test]
    fn renders_history_replacement_with_both_counts() {
        let text = render_text(&AgentEvent::HistoryReplaced {
            before: 42,
            after: 9,
        })
        .expect("历史被改写必须可见");

        assert!(text.contains("42"), "缺压缩前的条数: {text}");
        assert!(text.contains('9'), "缺压缩后的条数: {text}");
    }

    #[test]
    fn renders_approval_request_with_every_call() {
        let event = AgentEvent::ApprovalRequested {
            calls: vec![ToolCall {
                id: "c1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"rm -rf /tmp/x"}"#.to_string(),
            }],
        };

        let text = render_text(&event).expect("审批请求必须可渲染");
        assert!(text.contains("bash"), "缺少工具名: {text}");
        assert!(text.contains("rm -rf /tmp/x"), "缺少命令原文: {text}");
        assert!(text.contains('y'), "缺少操作提示: {text}");
    }
}
