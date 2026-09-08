use anyhow::Result;
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
        ChatCompletionRequestUserMessageArgs, ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequestArgs, FunctionCall, FunctionObjectArgs,
    },
};
use std::sync::Arc;

use crate::{
    message::{Context, Message, Role, ToolCall},
    pipeline::{Effect, Operation, OperationResult},
    tool::Tool,
};

pub struct InferenceOperation {
    client: Client<OpenAIConfig>,
    model: String,
    system_prompt: Option<String>,
    tools: Vec<ChatCompletionTools>,
}

impl InferenceOperation {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: impl Into<String>,
        system_prompt: Option<String>,
        tool_list: &[Arc<dyn Tool>],
    ) -> Self {
        let config = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);
        let client = Client::with_config(config);

        let tools: Vec<ChatCompletionTools> = tool_list
            .iter()
            .map(|t| {
                let function = FunctionObjectArgs::default()
                    .name(t.name())
                    .description(t.description())
                    .parameters(t.parameters_schema())
                    .build()
                    .expect("Failed to build function schema");
                ChatCompletionTools::Function(ChatCompletionTool { function })
            })
            .collect();

        Self {
            client,
            model: model.into(),
            system_prompt,
            tools,
        }
    }
}

#[async_trait::async_trait]
impl Operation for InferenceOperation {
    fn name(&self) -> &'static str {
        "inference"
    }

    async fn evaluate(&self, ctx: &Context) -> Result<OperationResult> {
        println!(
            "🧠 [InferenceOp] 准备向大模型发起推理请求 (模型: {})...",
            self.model
        );

        let mut api_messages: Vec<ChatCompletionRequestMessage> = Vec::new();

        if let Some(sys) = &self.system_prompt {
            api_messages.push(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(sys.as_str())
                    .build()?
                    .into(),
            );
        }

        for msg in ctx.for_agent() {
            match msg.role {
                Role::User => {
                    api_messages.push(
                        ChatCompletionRequestUserMessageArgs::default()
                            .content(msg.content.clone())
                            .build()?
                            .into(),
                    );
                }
                Role::Assistant => {
                    let mut b = ChatCompletionRequestAssistantMessageArgs::default();
                    if !msg.content.is_empty() {
                        b.content(msg.content.clone());
                    }
                    if let Some(ref calls) = msg.tool_calls {
                        let openai_calls: Vec<ChatCompletionMessageToolCalls> = calls
                            .iter()
                            .map(|c| {
                                ChatCompletionMessageToolCalls::Function(
                                    ChatCompletionMessageToolCall {
                                        id: c.id.clone(),
                                        function: FunctionCall {
                                            name: c.name.clone(),
                                            arguments: c.arguments.clone(),
                                        },
                                    },
                                )
                            })
                            .collect();
                        b.tool_calls(openai_calls);
                    }
                    api_messages.push(b.build()?.into());
                }
                Role::Tool => {
                    api_messages.push(
                        ChatCompletionRequestToolMessageArgs::default()
                            .tool_call_id(msg.tool_call_id.clone().unwrap_or_default())
                            .content(msg.content.clone())
                            .build()?
                            .into(),
                    );
                }
                Role::System => {
                    api_messages.push(
                        ChatCompletionRequestSystemMessageArgs::default()
                            .content(msg.content.clone())
                            .build()?
                            .into(),
                    );
                }
            }
        }

        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(api_messages)
            .tools(self.tools.clone())
            .build()?;

        let response = self.client.chat().create(request).await?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("模型没有返回任何候选结果"))?;

        let msg = choice.message;

        if let Some(openai_calls) = msg.tool_calls
            && !openai_calls.is_empty()
        {
            let calls: Vec<ToolCall> = openai_calls
                .into_iter()
                .filter_map(|c| match c {
                    ChatCompletionMessageToolCalls::Function(f) => Some(ToolCall {
                        id: f.id,
                        name: f.function.name,
                        arguments: f.function.arguments,
                    }),
                    _ => None,
                })
                .collect();

            println!("💡 [Agent 思考]: 决定调用工具 (共 {} 个)", calls.len());

            let assistant_msg = Message::assistant_tool_call(calls);

            return Ok(OperationResult::applied(vec![Effect::AppendMessage(
                assistant_msg,
            )]));
        }

        let content = msg.content.unwrap_or_default();
        println!("💬 [Agent 最终回复]:\n{}", content);

        let assistant_msg = Message::assistant(content);
        Ok(OperationResult::yielded_with(vec![Effect::AppendMessage(
            assistant_msg,
        )]))
    }
}
