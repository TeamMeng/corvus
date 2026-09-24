//! 事件 → 人类可读文本的**纯函数**映射。
//!
//! 放在库里而不是 `main.rs`，有两个理由：
//! 1. 它属于「表现层逻辑」，未来 TUI / Web / Iggy 订阅端会复用同一套显示语义；
//! 2. 纯函数（不打印、不触 IO）才能被单测直接断言 —— 而"到底有没有渲染出来"
//!    恰恰是最容易悄悄坏掉、又最难发现的一环。

use crate::events::AgentEvent;
use crate::message::Role;

/// 返回该事件应当显示给人类的文本；`None` 表示无需显示。
pub fn render_text(event: &AgentEvent) -> Option<String> {
    match event {
        // 只渲染「助手说的话」：
        // 用户输入终端已回显，工具结果无需刷屏，内部协调记录本就双不可见。
        AgentEvent::MessageAppended { message } => {
            (message.role == Role::Assistant && message.user_visible && !message.content.is_empty())
                .then(|| message.content.clone())
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
