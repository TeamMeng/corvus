use crate::{
    events::{AgentEvent, Emitter},
    message::Context,
    pipeline::{
        Effect, Operation,
        OperationResult::{self},
        StepResult,
    },
};
use anyhow::Result;
use tracing::{info, warn};

pub struct Engine {
    pipeline: Vec<Box<dyn Operation>>,
    max_steps: usize,
}

impl Engine {
    pub fn new(pipeline: Vec<Box<dyn Operation>>) -> Self {
        Self {
            pipeline,
            max_steps: 15,
        }
    }

    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub async fn step(
        &self,
        ctx: &Context,
        emit: &Emitter,
    ) -> Result<Option<(&'static str, StepResult)>> {
        for op in &self.pipeline {
            match op.evaluate(ctx, emit).await? {
                OperationResult::NotApplicable => continue,
                OperationResult::Applied(result) => return Ok(Some((op.name(), result))),
            }
        }
        Ok(None)
    }

    /// 心跳主循环：驱动 Agent 自主思考与执行，直到交出控制权或熔断。
    pub async fn run(&self, ctx: &mut Context, emit: &Emitter) -> Result<()> {
        emit.emit(AgentEvent::RunStarted).await;

        let mut applied = 0;

        loop {
            if applied > self.max_steps {
                warn!(
                    max_steps = self.max_steps,
                    "触发最大步数熔断，强制终止循环！"
                );
                break;
            }

            let Some((operation, step_result)) = self.step(ctx, emit).await? else {
                info!("状态机无可用工序命中，平稳退出。");
                break;
            };
            applied += 1;

            emit.emit(AgentEvent::OperationApplied { operation }).await;

            for effect in step_result.effects {
                match effect {
                    Effect::AppendMessage(message) => {
                        ctx.push(message.clone());
                        // ★ 必须广播：否则前端完全看不到新消息，
                        //   表现为"模型明明答了、磁盘上也有，终端却一片安静"。
                        emit.emit(AgentEvent::MessageAppended { message }).await;
                    }
                    // 目前无人产生 ReplaceConversation（压缩工序尚未实现）。
                    // 等 CompactionOperation 落地时，这里要补一个 HistoryReplaced 事件，
                    Effect::ReplaceConversation(messages) => ctx.messages = messages,
                }
            }

            if step_result.yield_turn {
                break;
            }
        }

        emit.emit(AgentEvent::RunFinished).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::pipeline::Operation;

    #[tokio::test]
    async fn appending_a_message_broadcasts_it() -> Result<()> {
        struct ReplyOnce;

        #[async_trait::async_trait]
        impl Operation for ReplyOnce {
            fn name(&self) -> &'static str {
                "reply_once"
            }

            async fn evaluate(&self, _ctx: &Context, _emit: &Emitter) -> Result<OperationResult> {
                Ok(OperationResult::yielded_with(vec![Effect::AppendMessage(
                    Message::assistant("hi"),
                )]))
            }
        }

        let engine = Engine::new(vec![Box::new(ReplyOnce)]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let emit = Emitter::new(tx);
        let mut ctx = Context::new();

        engine.run(&mut ctx, &emit).await?;

        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }

        assert_eq!(ctx.messages.len(), 1, "消息必须进入转录");
        assert!(
            seen.iter().any(|event| matches!(
                event,
                AgentEvent::MessageAppended { message } if message.content == "hi"
            )),
            "追加消息必须同时广播 MessageAppended，实际事件: {seen:?}"
        );
        Ok(())
    }
}
