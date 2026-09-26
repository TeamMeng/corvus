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
use tracing::warn;

use crate::{
    events::Emitter,
    message::{Context, Message, Role, TokenUsage, ToolCall},
    pipeline::{Effect, Operation, OperationResult},
    provider_error::{self, ErrorKind},
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

    async fn evaluate(&self, ctx: &Context, emit: &Emitter) -> Result<OperationResult> {
        let _ = emit;

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

        // ★ 上游失败不再让 anyhow 穿透：那会顺着 `?` 一路冒到 main，
        //   把整个 REPL 打死 —— 一个 402 就够了。
        //   改成「分类 → 写进转录 → 本轮优雅结束」。
        //
        //   唯一例外是「上下文超长」的首次出现：此时不结束本轮，
        //   让排在前面的压缩工序在同一个回合里自愈（见 CompactionOperation）。
        let response = match self.client.chat().create(request).await {
            Ok(response) => response,
            Err(error) => {
                let (kind, detail) = provider_error::classify(&error);

                // 自愈只在同时满足两条时才有意义：
                //   一、转录末尾不是同一个病因 —— 否则说明刚刚那次重试又白跑了
                //       （典型情形：根本没东西可压，压缩工序拒绝了，一转就回原点）；
                //   二、手上有新证据 —— 压缩之后既没跑通过、人类也没再说话，
                //       再压一次用的还是同一份滞后的读数。
                // 两条都不满足就认输交还人类，不烧一堆注定失败的请求。
                let self_healing = kind == ErrorKind::ContextLengthExceeded
                    && ctx.last_error_kind() != Some(ErrorKind::ContextLengthExceeded)
                    && ctx.has_new_evidence_since_compaction();

                warn!(?kind, self_healing, %detail, "上游调用失败，病因已记入转录");

                let message = Message::error(kind, detail);
                return Ok(if self_healing {
                    OperationResult::applied(vec![Effect::AppendMessage(message)])
                } else {
                    OperationResult::yielded_with(vec![Effect::AppendMessage(message)])
                });
            }
        };

        let usage = response.usage.as_ref().map(|u| TokenUsage {
            prompt: u.prompt_tokens,
            completion: u.completion_tokens,
            total: u.total_tokens,
        });

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

            let mut assistant_msg = Message::assistant_tool_call(calls);
            if let Some(usage) = usage {
                assistant_msg = assistant_msg.with_usage(usage);
            }

            return Ok(OperationResult::applied(vec![Effect::AppendMessage(
                assistant_msg,
            )]));
        }

        let content = msg.content.unwrap_or_default();

        let mut assistant_msg = Message::assistant(content);
        if let Some(usage) = usage {
            assistant_msg = assistant_msg.with_usage(usage);
        }

        Ok(OperationResult::yielded_with(vec![Effect::AppendMessage(
            assistant_msg,
        )]))
    }
}
