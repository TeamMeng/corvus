use crate::message::{Context, Message};
use anyhow::Result;

#[derive(Debug, Clone)]
pub enum Effect {
    AppendMessage(Message),
    ReplaceConversation(Vec<Message>),
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

    async fn evaluate(&self, ctx: &Context) -> Result<OperationResult>;
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

        async fn evaluate(&self, ctx: &Context) -> Result<OperationResult> {
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

    #[tokio::test]
    async fn test_operation_evaluation() -> Result<()> {
        let op = PingOperation;
        assert_eq!(op.name(), "ping_op");

        let mut ctx = Context::new();
        ctx.push(Message::user("Hello"));
        let res = op.evaluate(&ctx).await?;
        assert!(matches!(res, OperationResult::NotApplicable));

        ctx.push(Message::user("ping"));
        let res = op.evaluate(&ctx).await?;
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
