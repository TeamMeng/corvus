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
                    Effect::AppendMessage(msg) => {
                        ctx.push(msg);
                    }
                    Effect::ReplaceConversation(new_msgs) => ctx.messages = new_msgs,
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
