use anyhow::Result;
use std::{collections::HashMap, sync::Arc};

use crate::{
    message::{Context, Message, Role},
    pipeline::{
        Effect, Operation,
        OperationResult::{self},
    },
    tool::Tool,
};

pub struct ToolExecutionOperation {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolExecutionOperation {
    pub fn new(tool_list: Vec<Arc<dyn Tool>>) -> Self {
        let mut tools = HashMap::new();
        for tool in tool_list {
            tools.insert(tool.name(), tool);
        }
        Self { tools }
    }
}

#[async_trait::async_trait]
impl Operation for ToolExecutionOperation {
    fn name(&self) -> &'static str {
        "tool_execution"
    }

    async fn evaluate(&self, ctx: &Context) -> Result<OperationResult> {
        let Some(last_msg) = ctx.messages.last() else {
            return Ok(OperationResult::NotApplicable);
        };

        let (Role::Assistant, Some(calls)) = (&last_msg.role, &last_msg.tool_calls) else {
            return Ok(OperationResult::NotApplicable);
        };

        if calls.is_empty() {
            return Ok(OperationResult::NotApplicable);
        }

        println!(
            "⚡ [ToolOp] 命中！捕获到大模型发出的 {} 个工具调用请求",
            calls.len()
        );

        let mut execution_tasks = Vec::new();

        for call in calls {
            let call_id = call.id.clone();
            let call_name = call.name.clone();
            let call_args = call.arguments.clone();

            let tool = self.tools.get(call_name.as_str()).cloned();

            execution_tasks.push(async move {
                let output = match tool {
                    Some(t) => {
                        println!("  -> 启动工具 [{}] (id: {})", call_name, call_id);
                        match t.execute(&call_args).await {
                            Ok(res) => res,
                            Err(e) => format!("工具执行出错: {}", e),
                        }
                    }
                    None => format!("错误：未找到名为 '{}' 的工具", call_name),
                };

                Message::tool_response(call_id, output)
            });
        }

        let tool_response_messages = futures::future::join_all(execution_tasks).await;

        let effects: Vec<Effect> = tool_response_messages
            .into_iter()
            .map(Effect::AppendMessage)
            .collect();

        Ok(OperationResult::applied(effects))
    }
}

#[cfg(test)]
mod tests {
    use crate::message::ToolCall;

    use super::*;

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }

        fn description(&self) -> &'static str {
            "回显内容"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn execute(&self, args_json: &str) -> Result<String> {
            Ok(format!("Echoed: {}", args_json))
        }
    }

    #[tokio::test]
    async fn test_tool_exec_operation() -> Result<()> {
        let tool = Arc::new(EchoTool);
        let op = ToolExecutionOperation::new(vec![tool]);

        let mut ctx = Context::new();

        let tool_call = ToolCall {
            id: "call_123".to_string(),
            name: "echo".to_string(),
            arguments: "hello world".to_string(),
        };
        ctx.push(Message::assistant_tool_call(vec![tool_call]));

        let result = op.evaluate(&ctx).await?;

        match result {
            OperationResult::Applied(step_res) => {
                assert!(!step_res.yield_turn, "工具跑完绝不能停下来等人类");
                assert_eq!(step_res.effects.len(), 1);
                if let Effect::AppendMessage(msg) = &step_res.effects[0] {
                    assert_eq!(msg.role, Role::Tool);
                    assert_eq!(msg.tool_call_id.as_deref(), Some("call_123"));
                    assert_eq!(msg.content, "Echoed: hello world");
                } else {
                    panic!("Expected AppendMessage effect");
                }
            }
            _ => panic!("Expected operation to be applied"),
        }

        Ok(())
    }
}
