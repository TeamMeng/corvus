use crate::{
    message::Context,
    pipeline::{
        Effect, Operation,
        OperationResult::{self},
        StepResult,
    },
};
use anyhow::Result;

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

    pub async fn step(&self, ctx: &Context) -> Result<Option<StepResult>> {
        for op in &self.pipeline {
            match op.evaluate(ctx).await? {
                OperationResult::NotApplicable => continue,
                OperationResult::Applied(res) => {
                    return Ok(Some(res));
                }
            }
        }
        Ok(None)
    }

    pub async fn run(&self, ctx: &mut Context) -> Result<()> {
        let mut step_count = 0;

        loop {
            step_count += 1;
            if step_count > self.max_steps {
                println!("🛑 [Safety] 触发最大步数熔断，强制终止循环！");
                break;
            }

            println!("\n━━━━━━━━━ ▶ 第 {} 轮状态机心跳 ━━━━━━━━━", step_count);

            let Some(step_result) = self.step(ctx).await? else {
                println!("状态机无可用工序命中，平稳退出。");
                break;
            };

            for effect in step_result.effects {
                match effect {
                    Effect::AppendMessage(msg) => {
                        ctx.push(msg);
                    }
                    Effect::ReplaceConversation(new_msgs) => ctx.messages = new_msgs,
                }
            }

            if step_result.yield_turn {
                println!("━━━━━━━━━ 🏁 任务达成，控制权交还人类 ━━━━━━━━━\n");
                break;
            }
        }

        Ok(())
    }
}
