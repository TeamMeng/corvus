use crate::{
    events::Emitter,
    message::{Context, Message},
};
use anyhow::Result;

#[derive(Debug, Clone)]
pub enum Effect {
    AppendMessage(Message),
    ReplaceConversation(Vec<Message>),
}

impl Effect {
    /// 这一步算「有进展」吗？
    ///
    /// 只有坏消息（故障记录）不算进展：Agent 反复报错而没有任何产出，
    /// 就是在空转，越转越贵。判定放在 `Effect` 自己身上，
    /// 因为“什么算推进一步”是效果的属性，而不是引擎的策略。
    pub fn is_progress(&self) -> bool {
        match self {
            // 整体替换是明确的推进动作（压缩）
            Effect::ReplaceConversation(_) => true,
            Effect::AppendMessage(message) => message.error_kind.is_none(),
        }
    }
}

#[derive(Debug)]
pub struct StepResult {
    pub effects: Vec<Effect>,
    pub yield_turn: bool,
}

pub enum OperationResult {
    NotApplicable,
    Applied(StepResult),
}

#[async_trait::async_trait]
pub trait Operation: Send + Sync {
    fn name(&self) -> &'static str;

    async fn evaluate(&self, ctx: &Context, emit: &Emitter) -> Result<OperationResult>;
}

impl OperationResult {
    pub fn applied(effects: impl IntoIterator<Item = Effect>) -> Self {
        Self::Applied(StepResult {
            effects: effects.into_iter().collect(),
            yield_turn: false,
        })
    }

    pub fn yielded_with(effects: impl IntoIterator<Item = Effect>) -> Self {
        Self::Applied(StepResult {
            effects: effects.into_iter().collect(),
            yield_turn: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PingOperation;

    #[async_trait::async_trait]
    impl Operation for PingOperation {
        fn name(&self) -> &'static str {
            "ping_op"
        }

        async fn evaluate(&self, ctx: &Context, emit: &Emitter) -> Result<OperationResult> {
            let _ = emit;
            if let Some(last) = ctx.messages.last()
                && last.content == "ping"
            {
                let pong_msg = Message::assistant("pong");
                return Ok(OperationResult::yielded_with(vec![Effect::AppendMessage(
                    pong_msg,
                )]));
            }

            Ok(OperationResult::NotApplicable)
        }
    }

    #[test]
    fn only_non_fault_messages_count_as_progress() {
        use crate::provider_error::ErrorKind;

        assert!(Effect::AppendMessage(Message::assistant("hi")).is_progress());
        assert!(
            !Effect::AppendMessage(Message::error(ErrorKind::Unknown, "boom")).is_progress(),
            "反复报错而没有产出 = 空转"
        );
        assert!(Effect::ReplaceConversation(vec![]).is_progress());
    }

    #[tokio::test]
    async fn test_operation_evaluation() -> Result<()> {
        let op = PingOperation;
        assert_eq!(op.name(), "ping_op");

        let mut ctx = Context::new();
        ctx.push(Message::user("Hello"));
        let res = op.evaluate(&ctx, &Emitter::noop()).await?;
        assert!(matches!(res, OperationResult::NotApplicable));

        ctx.push(Message::user("ping"));
        let res = op.evaluate(&ctx, &Emitter::noop()).await?;
        match res {
            OperationResult::Applied(step_result) => {
                assert!(step_result.yield_turn);
                assert_eq!(step_result.effects.len(), 1);
            }
            _ => panic!("Expected operation to be applied"),
        }

        Ok(())
    }
}
