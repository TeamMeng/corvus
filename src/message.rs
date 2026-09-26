use crate::provider_error::ErrorKind;
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

    /// 上游故障的病因。`Some` 表示「这条消息记录的是一次失败」。
    ///
    /// 带 `#[serde(default)]`：老会话文件没有这个字段也能读回。
    #[serde(default)]
    pub error_kind: Option<ErrorKind>,

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
            error_kind: None,
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
            error_kind: None,
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
            error_kind: None,
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
            error_kind: None,
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
            error_kind: None,
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
            error_kind: None,
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
            error_kind: None,
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
            error_kind: None,
            user_visible: false,
            agent_visible: false,
        }
    }

    /// 上游故障：把「这次请求失败了」写进转录，作为一等事实。
    ///
    /// `content` 只存放**上游原文**；怎么说给人类听由 `render.rs` 依据
    /// `error_kind` 决定 —— 渲染文案不进转录（和 `approval_pending` 同一条规矩）。
    ///
    /// 为什么必须落进转录，而不是只 `warn!` 一条日志？因为分类结果要**跨进程存活**：
    /// 压缩工序要在重启后的会话里读到「上一轮是因为上下文超长才失败的」。
    pub fn error(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: detail.into(),
            tool_calls: None,
            tool_call_id: None,
            usage: None,
            error_kind: Some(kind),
            user_visible: true,
            // ★ 故障是基础设施的事，不是模型的发言：不进模型上下文。
            //   副作用是它落在「助手发言 + 模型看不见」这一格 ——
            //   正是 `awaiting_human_input` 曾经会误判的那一格，见那里的注释。
            agent_visible: false,
        }
    }

    /// 是不是「人类真的敲进来的」那一轮发言。
    ///
    /// `user_visible` 是关键词：`agent_only_instruction`（提示词）与 `approval_answer`
    /// 的 role 也是 User，但它们不是人类的发言，不能当压缩切点。
    pub fn is_human_turn(&self) -> bool {
        self.role == Role::User && self.user_visible
    }

    /// 压缩标记：告诉模型「更早的对话已被移除」的那条提示词。
    ///
    /// 注意它与「渲染文案不进转录」那条规矩不矛盾 —— 它是**提示词**，
    /// 本身就要进模型上下文，所以文案必须存在消息里；
    /// 人类看到的是 `AgentEvent::HistoryReplaced` 事件。
    pub fn compaction_marker(dropped: usize) -> Self {
        Self::agent_only_instruction(format!(
            "（系统提示：为腾出上下文，更早的 {dropped} 条对话已从上下文中移除，你看不到其内容了。\
             如需细节，请重新读取相关文件，或向人类确认。）"
        ))
    }

    /// 是不是压缩标记？（模型看得见、人类看不见的那一格）
    ///
    /// ⚠️ 今天这一格只住着压缩标记（`agent_only_instruction` 也只被它用）。
    ///    但判据是从可见性位反推语义 —— 一旦真的接入「只在幕后注入提示词」，
    ///    两边就会住在同一格里，压缩闸门会把提示词误认成压缩痕迹。
    ///    到那一天，正确做法是给 `Message` 加一个显式的 `intent` 字段
    ///    （照 `#[serde(default)]` 的老规矩加），而不是继续叠可见性位。
    pub fn is_compaction_marker(&self) -> bool {
        self.role == Role::User && self.agent_visible && !self.user_visible
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
    /// 判定依据：该消息**人类与模型都看不见** —— 即 2×2 视角矩阵的右下象限，
    /// 那是内部协调记录的专属格子。用途有两处：
    /// 1. `ToolApprovalOperation` —— 已经问过就别重复刷屏，继续挂起；
    /// 2. REPL —— 下一条输入是「裁决」而非「新需求」，不能当成普通用户消息。
    ///
    /// ★ 旧写法是 `role == Assistant && !agent_visible`，看上去更宽更保险，
    ///   实则把「只给人看的助手发言」也算了进来 —— 而 `Message::error` 正好是后者。
    ///   一旦误判，REPL 会把人类的下一条输入当成**裁决答复**吞掉
    ///   （`approval_answer` 双方都不可见），模型永远收不到那句话。
    pub fn awaiting_human_input(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|m| m.role == Role::Assistant && !m.user_visible && !m.agent_visible)
    }

    /// 转录末尾那条消息记录的故障病因（若有）。
    ///
    /// 用途有两处：压缩工序用它判断「上一轮是不是撞了上下文上限」，
    /// 推理工序用它避免「同一个病因连续重试」。
    /// 注意它只看**最后一条**：任何后续追加（工具结果、人类发言、压缩标记）
    /// 都会把故障遮蔽掉 —— 这是故意的，故障只对紧接着的那一步有意义。
    pub fn last_error_kind(&self) -> Option<ErrorKind> {
        self.messages.last().and_then(|m| m.error_kind)
    }

    /// 模型视图里的工具往返是否成对、且不被别的消息打断？
    pub fn pairing_intact(&self) -> bool {
        tool_pairing_intact(&self.messages)
    }

    /// 允许再次压缩吗？—— 亦即「上次压缩用的那份证据过期了没有」。
    ///
    /// 压缩的触发依据（用量、故障）都是**回头看**的信息，而同一份信息只该用一次：
    /// 压完还拿它当理由，就会一路压到地板，把本该保留的上下文也压掉。
    /// 所以要求自上一步压缩以来至少出现一件**新事实**：
    ///
    /// - 一次成功的推理上报（新的真实用量 —— 说明上下文确实又长回来了）；
    /// - 或者新的人类发言（新的一轮请求，值得重新试一次）。
    ///
    /// 反过来，压缩之后紧接着的**失败**不算新事实 —— 它恰恰说明压得还不够，
    /// 而它自己也是同一份滞后证据的重复使用。
    pub fn has_new_evidence_since_compaction(&self) -> bool {
        let Some(marker) = self
            .messages
            .iter()
            .rposition(Message::is_compaction_marker)
        else {
            return true; // 从没压过
        };

        self.messages[marker + 1..]
            .iter()
            .any(|m| m.usage.is_some() || m.is_human_turn())
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

/// 模型视图里的工具往返是否成对、且不被别的消息打断？
///
/// OpenAI 协议的硬要求：助手声明的每个 `tool_calls`，都必须在**下一条**
/// 助手/用户发言之前拿到它全部的 `tool` 结果。两种违反形态：
/// 「孤儿结果」（结果在、声明没了）与「未应答声明」（反之）。
///
/// 为什么不能只用 `Context::pending_tool_calls()`？它只回答「声明有没有结果」，
/// 对**孤儿结果完全无感** —— 而压缩最危险的产物恰恰可能是孤儿结果。
pub fn tool_pairing_intact(messages: &[Message]) -> bool {
    let mut pending: HashSet<&str> = HashSet::new();

    for message in messages.iter().filter(|m| m.agent_visible) {
        if message.role == Role::Tool {
            let Some(id) = message.tool_call_id.as_deref() else {
                return false;
            };
            if !pending.remove(id) {
                return false; // 孤儿结果：没有声明认领它
            }
            continue;
        }

        // 任何非工具消息出现时，上一批调用必须已经结清
        if !pending.is_empty() {
            return false;
        }
        if let Some(calls) = &message.tool_calls {
            pending.extend(calls.iter().map(|call| call.id.as_str()));
        }
    }

    pending.is_empty()
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

        // ★ 语义收紧：只有「双方都不可见」的协调记录才算在等裁决。
        //   「只给人看」的助手发言（上游故障提示正是这种）不该被当成提问 ——
        //   否则报错之后人类说的下一句话会被当成裁决吞掉。
        ctx.push(Message::user_only_notification("本地命令 /help 已执行"));
        assert!(!ctx.awaiting_human_input());

        ctx.push(Message::approval_pending());
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

    /// 回归测试：上游报错之后，人类的下一条输入必须还是**正常用户消息**。
    ///
    /// 报错消息是「助手发言 + 模型看不见」，正好落在审批哨兵的误判区里；
    /// 一旦判据放宽回 `!agent_visible`，REPL 就会把下一句话当裁决吞掉。
    #[test]
    fn a_provider_error_does_not_swallow_the_next_human_message() {
        let mut ctx = Context::new();
        ctx.push(Message::user("把这个脚本跑一遍"));
        ctx.push(Message::error(
            ErrorKind::AuthOrQuota,
            "402 Payment Required Insufficient Balance",
        ));

        assert!(
            !ctx.awaiting_human_input(),
            "上游故障不是审批提问，REPL 不能再把下一句人类输入当裁决吞掉"
        );
        assert_eq!(ctx.last_error_kind(), Some(ErrorKind::AuthOrQuota));

        // 人类接着说了一句：必须是正常用户消息，且模型看得见
        ctx.push(Message::user("换成新钥匙了，继续"));
        assert_eq!(ctx.last_error_kind(), None, "新消息之后不再是故障态");
        assert!(ctx.messages.last().is_some_and(|m| m.agent_visible));
    }

    /// 压缩闸门：同一份「滞后证据」只能用一次。
    #[test]
    fn compaction_marker_opens_a_fresh_evidence_window() {
        let mut ctx = Context::new();
        assert!(
            ctx.has_new_evidence_since_compaction(),
            "从没压过：闸门开着"
        );

        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant("回复").with_usage(TokenUsage {
            prompt: 99_000,
            completion: 20,
            total: 99_020,
        }));
        let marker = Message::compaction_marker(7);
        assert!(marker.is_compaction_marker());
        assert!(!marker.is_human_turn(), "标记不是人类发言，不能当压缩切点");
        ctx.push(marker);

        assert!(
            !ctx.has_new_evidence_since_compaction(),
            "刚压完：那个滞后数字不许再用"
        );
        assert!(!ctx.awaiting_human_input(), "标记也不是审批提问");

        ctx.push(Message::error(ErrorKind::ContextLengthExceeded, "too long"));
        assert!(
            !ctx.has_new_evidence_since_compaction(),
            "压缩之后的失败不算新事实，否则会连环重写历史"
        );

        ctx.push(Message::assistant("新回复").with_usage(TokenUsage {
            prompt: 5_000,
            completion: 10,
            total: 5_010,
        }));
        assert!(
            ctx.has_new_evidence_since_compaction(),
            "真的跑通一次之后，闸门重新打开"
        );

        // 另一条重开路径：新的人类发言
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::compaction_marker(3));
        assert!(!ctx.has_new_evidence_since_compaction());
        ctx.push(Message::user("新的一轮"));
        assert!(ctx.has_new_evidence_since_compaction());
    }
}
