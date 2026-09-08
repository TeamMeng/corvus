use serde::{Deserialize, Serialize};

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
            id: uuid::Uuid::new_v4().to_string(),
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
            id: uuid::Uuid::new_v4().to_string(),
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
            id: uuid::Uuid::new_v4().to_string(),
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
            id: uuid::Uuid::new_v4().to_string(),
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
            id: uuid::Uuid::new_v4().to_string(),
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
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            user_visible: false,
            agent_visible: true,
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
}
