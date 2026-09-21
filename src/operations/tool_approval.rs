use crate::{
    message::{Context, Message, Role, ToolCall},
    pipeline::{
        Effect, Operation,
        OperationResult::{self},
    },
};
use anyhow::Result;
use tracing::warn;

/// 需要人类二次确认的高危命令特征。
const DEFAULT_RISKY_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "mkfs",
    "dd if=",
    "shutdown",
    "reboot",
    "poweroff",
    ":(){",
    "chmod 777 /",
    "> /dev/sd",
    "sudo ",
    "curl | sh",
    "wget | sh",
];

/// 人工审批门禁：高危工具调用在真正执行前，必须先取得人类的明确许可。
///
/// 1. 它排在执行工序**之前**，是执行路径上唯一的闸门；
/// 2. 需要裁决时产出 `yield_turn: true`，把控制权交还 REPL 等人类输入；
/// 3. 提问与答复都写进转录，但用 2×2 视角矩阵标记为**模型不可见**——
///    模型永远不知道人类被拦过一次，也不会被 "y" 这种噪音污染上下文；
/// 4. 拒绝时必须为被拒调用回填一条 Tool 结果，否则该调用永远 pending，
///    流水线会无限重问；同时让模型立刻得知「此路不通」并改道。
pub struct ToolApprovalOperation {
    risky_patterns: Vec<String>,
}

impl ToolApprovalOperation {
    pub fn new() -> Self {
        Self {
            risky_patterns: DEFAULT_RISKY_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    pub fn with_patterns(patterns: Vec<String>) -> Self {
        Self {
            risky_patterns: patterns,
        }
    }

    fn is_risky(&self, call: &ToolCall) -> bool {
        self.risky_patterns
            .iter()
            .any(|pattern| call.arguments.contains(pattern.as_str()))
    }
}

impl Default for ToolApprovalOperation {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Operation for ToolApprovalOperation {
    fn name(&self) -> &'static str {
        "tool_approval"
    }

    async fn evaluate(&self, ctx: &Context) -> Result<OperationResult> {
        // 1. 有未应答的工具调用吗？没有就放行
        let calls = ctx.pending_tool_calls();

        if calls.is_empty() {
            return Ok(OperationResult::NotApplicable);
        }

        // 2. 这一批里有高危的吗？全安全就直接放行给执行工序
        let risky: Vec<&ToolCall> = calls.iter().filter(|call| self.is_risky(call)).collect();
        if risky.is_empty() {
            return Ok(OperationResult::NotApplicable);
        }

        // 3. 人类裁决过了吗？
        match latest_answer(ctx) {
            // 已批准 -> 放行，交给 tool_execution 真正执行
            Some(true) => return Ok(OperationResult::NotApplicable),
            // 已拒绝 -> 回填拒绝结果，让模型立刻改道（yield_turn = false）
            Some(false) => return Ok(OperationResult::applied(denied_effects(&risky))),
            None => {}
        }

        // 4. 还没裁决。若上一步已经问过，就继续挂起、不重复刷屏。
        //    这里绝不能放行——否则高危命令会被静默执行（fail-safe 原则）。
        if ctx.awaiting_human_input() {
            return Ok(OperationResult::yielded_with(vec![]));
        }

        warn!(
            risky_calls = risky.len(),
            "高危工具调用已暂停，等待人工审批"
        );

        // 5. 第一次遇到 -> 提问并挂起，等人类回话
        Ok(OperationResult::yielded_with(vec![Effect::AppendMessage(
            Message::user_only_notification(prompt_text(&risky)),
        )]))
    }
}

fn latest_answer(ctx: &Context) -> Option<bool> {
    let request_idx = ctx
        .messages
        .iter()
        .rposition(|message| message.tool_calls.is_some())?;

    ctx.messages[request_idx + 1..]
        .iter()
        .rev()
        .find(|message| message.role == Role::User && !message.agent_visible)
        .map(|message| parse_answer(&message.content))
}

fn parse_answer(content: &str) -> bool {
    matches!(
        content.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "是" | "允许" | "ok"
    )
}

fn prompt_text(risky: &[&ToolCall]) -> String {
    let mut text = String::from("⚠️  检测到高危操作，已暂停执行，等待你裁决：\n");
    for call in risky {
        text.push_str(&format!("   • [{}] {}\n", call.name, call.arguments));
    }
    text.push_str("\n输入 y 允许执行；其它任意输入均视为拒绝");
    text
}

fn denied_effects(risky: &[&ToolCall]) -> Vec<Effect> {
    risky
        .iter()
        .map(|call| {
            Effect::AppendMessage(Message::tool_response(
                call.id.clone(),
                "用户拒绝执行该命令。请勿重复尝试同一条命令，改为说明理由或提出更安全的替代方案。",
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risky_call() -> ToolCall {
        ToolCall {
            id: "call_rm".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"command":"rm -rf /tmp/data"}"#.to_string(),
        }
    }

    fn safe_call() -> ToolCall {
        ToolCall {
            id: "call_ls".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"command":"ls -la"}"#.to_string(),
        }
    }

    #[tokio::test]
    async fn safe_calls_pass_through() -> Result<()> {
        let op = ToolApprovalOperation::new();
        let mut ctx = Context::new();
        ctx.push(Message::assistant_tool_call(vec![safe_call()]));

        assert!(matches!(
            op.evaluate(&ctx).await?,
            OperationResult::NotApplicable
        ));
        Ok(())
    }

    #[tokio::test]
    async fn risky_call_pauses_and_asks_human_only() -> Result<()> {
        let op = ToolApprovalOperation::new();
        let mut ctx = Context::new();
        ctx.push(Message::assistant_tool_call(vec![risky_call()]));

        let OperationResult::Applied(step) = op.evaluate(&ctx).await? else {
            panic!("高危调用必须触发审批");
        };

        assert!(step.yield_turn, "必须挂起，等待人类输入");
        let Effect::AppendMessage(notice) = &step.effects[0] else {
            panic!("应产出一条提问消息");
        };
        assert!(notice.user_visible, "提问必须让人看见");
        assert!(!notice.agent_visible, "提问绝不能进入模型上下文");

        Ok(())
    }

    #[tokio::test]
    async fn approved_call_passes_through() -> Result<()> {
        let op = ToolApprovalOperation::new();
        let mut ctx = Context::new();
        ctx.push(Message::assistant_tool_call(vec![risky_call()]));
        ctx.push(Message::user_only_notification("⚠️ 等待裁决"));
        ctx.push(Message::approval_answer("y"));

        assert!(
            matches!(op.evaluate(&ctx).await?, OperationResult::NotApplicable),
            "批准后必须放行给执行工序"
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_call_gets_a_tool_response() -> Result<()> {
        let op = ToolApprovalOperation::new();
        let mut ctx = Context::new();
        ctx.push(Message::assistant_tool_call(vec![risky_call()]));
        ctx.push(Message::user_only_notification("⚠️ 等待裁决"));
        ctx.push(Message::approval_answer("这是手滑乱敲的内容"));

        let OperationResult::Applied(step) = op.evaluate(&ctx).await? else {
            panic!("拒绝后必须回填结果");
        };
        assert!(!step.yield_turn, "拒绝后要让模型立刻改道");

        let Effect::AppendMessage(response) = &step.effects[0] else {
            panic!("应产生一条 Tool 结果");
        };
        assert_eq!(response.tool_call_id.as_deref(), Some("call_rm"));
        assert!(response.agent_visible, "模型必须知道命令被拒");

        // 关键不变量：回填后该调用不再 pending，流水线不会无限重问
        ctx.push(response.clone());
        assert!(ctx.pending_tool_calls().is_empty());
        Ok(())
    }
}
