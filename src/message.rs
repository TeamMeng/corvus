use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,

    pub user_visible: bool,
    pub agent_visible: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Context {
    pub messages: Vec<Message>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            user_visible: true,
            agent_visible: true,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            user_visible: true,
            agent_visible: true,
        }
    }

    pub fn assistant_tool_call(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            user_visible: true,
            agent_visible: true,
        }
    }

    pub fn tool_response(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            user_visible: true,
            agent_visible: true,
        }
    }

    pub fn user_only_notification(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            user_visible: true,
            agent_visible: false,
        }
    }

    pub fn agent_only_instruction(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            user_visible: false,
            agent_visible: true,
        }
    }

    /// 人类对「内部审批提问」的答复：只在转录里留痕，既不打印也不喂给模型
    pub fn approval_answer(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            user_visible: false,
            agent_visible: false,
        }
    }
}

impl Context {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn for_agent(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().filter(|m| m.agent_visible)
    }

    pub fn for_user(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().filter(|m| m.user_visible)
    }

    /// 扫描转录，找出「已声明、但尚未拿到结果」的工具调用。
    pub fn pending_tool_calls(&self) -> Vec<ToolCall> {
        let answered: HashSet<&str> = self
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();

        self.messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .filter(|call| !answered.contains(call.id.as_str()))
            .cloned()
            .collect()
    }

    /// 最后一条消息是否是「等人类回话」的内部提示（例如审批提问）。
    ///
    /// 判定依据：该消息只给人类看（`agent_visible == false`）且是系统侧发言。
    /// 用途有两处：
    /// 1. `ToolApprovalOperation` —— 已经问过就别重复刷屏，继续挂起；
    /// 2. REPL —— 下一条输入是「裁决」而非「新需求」，不能当成普通用户消息。
    pub fn awaiting_human_input(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|m| m.role == Role::Assistant && !m.agent_visible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visibility_matrix() {
        let mut ctx = Context::new();

        ctx.push(Message::user("你好"));
        ctx.push(Message::user_only_notification("本地命令 /help 已执行"));
        ctx.push(Message::agent_only_instruction("<hint>注意回答简短</hint>"));

        let agent_view: Vec<_> = ctx.for_agent().collect();
        assert_eq!(agent_view.len(), 2);
        assert_eq!(agent_view[0].content, "你好");
        assert_eq!(agent_view[1].content, "<hint>注意回答简短</hint>");

        let user_view: Vec<_> = ctx.for_user().collect();
        assert_eq!(user_view.len(), 2);
        assert_eq!(user_view[0].content, "你好");
        assert_eq!(user_view[1].content, "本地命令 /help 已执行");
    }

    #[test]
    fn test_awaiting_human_input() {
        let mut ctx = Context::new();
        assert!(!ctx.awaiting_human_input(), "空转录不算等待");

        ctx.push(Message::user("你好"));
        assert!(!ctx.awaiting_human_input());

        ctx.push(Message::user_only_notification("⚠️ 高危操作，等待裁决"));
        assert!(ctx.awaiting_human_input());

        ctx.push(Message::approval_answer("y"));
        assert!(!ctx.awaiting_human_input());
    }
}
