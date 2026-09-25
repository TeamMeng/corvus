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

    #[serde(default)]
    pub usage: Option<TokenUsage>,

    pub user_visible: bool,
    pub agent_visible: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Context {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt: u32,
    pub completion: u32,
    pub total: u32,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            usage: None,
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
            usage: None,
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
            usage: None,
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
            usage: None,
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
            usage: None,
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
            usage: None,
            user_visible: false,
            agent_visible: true,
        }
    }

    /// 人类对「内部审批提问」的答复：只在转录里留痕，既不打印也不喂给模型。
    ///
    /// `content` 保存人类敲入的原文，由 `ToolApprovalOperation` 解析成裁决结果。
    pub fn approval_answer(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            usage: None,
            user_visible: false,
            agent_visible: false,
        }
    }

    /// 内部协调记录：某批高危调用正在等待人类裁决。
    ///
    /// 落在 2×2 矩阵的「右下象限」——人类看不见、模型也看不见，
    /// 但它仍是转录里的一等状态：进程重启后依然能还原「在等谁回答」。
    /// 渲染文案不放在这里，前端从 `AgentEvent::ApprovalRequested` 自行渲染。
    pub fn approval_pending() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: "awaiting human approval".to_string(),
            tool_calls: None,
            tool_call_id: None,
            usage: None,
            user_visible: false,
            agent_visible: false,
        }
    }

    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = Some(usage);
        self
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

    pub fn total_usage(&self) -> TokenUsage {
        self.messages
            .iter()
            .filter_map(|m| m.usage)
            .fold(TokenUsage::default(), |mut acc, u| {
                acc.accumulate(u);
                acc
            })
    }

    pub fn last_prompt_tokens(&self) -> Option<u32> {
        self.messages
            .iter()
            .rev()
            .find_map(|m| m.usage.map(|u| u.prompt))
    }
}

impl TokenUsage {
    pub fn accumulate(&mut self, other: TokenUsage) {
        self.prompt += other.prompt;
        self.completion += other.completion;
        self.total += other.total
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

    #[test]
    fn test_usage_accounting() {
        let mut ctx = Context::new();
        assert_eq!(ctx.last_prompt_tokens(), None, "没有任何用量时应为 None");

        ctx.push(Message::user("读一下 big.py"));
        ctx.push(Message::assistant_tool_call(vec![]).with_usage(TokenUsage {
            prompt: 1000,
            completion: 50,
            total: 1050,
        }));
        ctx.push(Message::assistant("读完了").with_usage(TokenUsage {
            prompt: 3000,
            completion: 20,
            total: 3020,
        }));
        ctx.push(Message::user("谢谢"));

        assert_eq!(ctx.total_usage().total, 4070, "总量 = 各轮之和");
        assert_eq!(
            ctx.last_prompt_tokens(),
            Some(3000),
            "取最近一条带用量的，而不是最后一条"
        )
    }
}
