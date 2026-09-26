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

/// 连续多少步「只有坏消息」就判定为空转。
///
/// 故障是合法的产出（人类需要看到它），但**连续**只产出故障说明状态机在原地打转：
/// 既没有新信息，也没有新动作，只是在烧钱。心跳循环必须能识别这一点。
const MAX_BARREN_STEPS: usize = 2;

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
        let mut barren_streak = 0;

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

            let progressed = step_result.effects.iter().any(Effect::is_progress);
            let yield_turn = step_result.yield_turn;

            for effect in step_result.effects {
                match effect {
                    Effect::AppendMessage(message) => {
                        ctx.push(message.clone());
                        // ★ 必须广播：否则前端完全看不到新消息，
                        //   表现为"模型明明答了、磁盘上也有，终端却一片安静"。
                        emit.emit(AgentEvent::MessageAppended { message }).await;
                    }
                    Effect::ReplaceConversation(messages) => {
                        // ★ 顺序：先数出 before，再替换 —— 换完就再也数不出旧的了。
                        // 只统计**对话消息**：压缩标记本身不是对话，
                        // 把它算进去会渲染出「10 条 → 10 条」这种莫名其妙的提示。
                        let before = ctx
                            .for_agent()
                            .filter(|m| !m.is_compaction_marker())
                            .count();
                        let after = messages
                            .iter()
                            .filter(|m| m.agent_visible && !m.is_compaction_marker())
                            .count();

                        ctx.messages = messages;
                        emit.emit(AgentEvent::HistoryReplaced { before, after })
                            .await;
                    }
                }
            }

            barren_streak = if progressed { 0 } else { barren_streak + 1 };
            if barren_streak >= MAX_BARREN_STEPS {
                warn!(
                    steps = barren_streak,
                    "连续多步没有任何进展，熔断并把控制权交还人类！"
                );
                break;
            }

            if yield_turn {
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

    /// 空转熔断：连续只产出坏消息时，必须停下来，而不是跑满 15 步。
    ///
    /// 这个场景真的会发生：上游带着「压缩之前那个已经过期的用量数字」
    /// 反复失败，每一步都在花钱，一步都没往前。
    #[tokio::test]
    async fn breaks_a_barren_loop_instead_of_burning_max_steps() -> Result<()> {
        struct AlwaysFails;

        #[async_trait::async_trait]
        impl Operation for AlwaysFails {
            fn name(&self) -> &'static str {
                "always_fails"
            }

            async fn evaluate(&self, _ctx: &Context, _emit: &Emitter) -> Result<OperationResult> {
                Ok(OperationResult::applied(vec![Effect::AppendMessage(
                    Message::error(crate::provider_error::ErrorKind::Unknown, "boom"),
                )]))
            }
        }

        let engine = Engine::new(vec![Box::new(AlwaysFails)]);
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let mut ctx = Context::new();

        engine.run(&mut ctx, &Emitter::new(tx)).await?;

        assert_eq!(
            ctx.messages.len(),
            MAX_BARREN_STEPS,
            "空转必须在 {MAX_BARREN_STEPS} 步内被识别并熔断"
        );
        Ok(())
    }

    /// 压缩工序落地前的契约测试：整体替换转录时，人类必须看得到「几条变成几条」。
    #[tokio::test]
    async fn replacing_the_history_broadcasts_before_and_after() -> Result<()> {
        struct CompactOnce;

        #[async_trait::async_trait]
        impl Operation for CompactOnce {
            fn name(&self) -> &'static str {
                "compact_once"
            }

            async fn evaluate(&self, ctx: &Context, _emit: &Emitter) -> Result<OperationResult> {
                // 只有一次可压缩空间：替换后自认不适用，避免主循环空转
                if ctx.messages.len() <= 2 {
                    return Ok(OperationResult::NotApplicable);
                }
                Ok(OperationResult::applied(vec![Effect::ReplaceConversation(
                    vec![Message::user("（摘要）"), Message::assistant("好")],
                )]))
            }
        }

        let engine = Engine::new(vec![Box::new(CompactOnce)]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let emit = Emitter::new(tx);

        let mut ctx = Context::new();
        for i in 0..5 {
            ctx.push(Message::user(format!("msg {i}")));
        }

        engine.run(&mut ctx, &emit).await?;

        assert_eq!(ctx.messages.len(), 2, "转录必须真的被替换");

        let seen: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            seen.iter().any(|event| matches!(
                event,
                AgentEvent::HistoryReplaced {
                    before: 5,
                    after: 2
                }
            )),
            "替换转录必须广播 HistoryReplaced（含前后条数），实际事件: {seen:?}"
        );
        Ok(())
    }
}
