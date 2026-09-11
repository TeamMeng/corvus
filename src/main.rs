use anyhow::Result;
use corvus::{
    engine::Engine,
    message::{Context, Message},
    observability::init_tracing,
    operations::{inference::InferenceOperation, tool_exec::ToolExecutionOperation},
    pipeline::Operation,
    sandbox::SandboxedBashTool,
    tool::Tool,
};
use microsandbox::Sandbox;
use std::{
    io::{self, Write},
    sync::Arc,
};
use tracing::{Instrument, debug, info, info_span, trace};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    println!("==================================================");
    println!("        🦅 Corvus Autonomous Agent 启动中        ");
    println!("==================================================");

    info!("\n[1/4] 启动硬件隔离微虚拟机 (microsandbox)...");

    let sandbox = Sandbox::builder("corvus-workspace")
        .image("python:3.11-slim")
        .memory(512)
        .replace()
        .create()
        .await?;
    let sb = Arc::new(sandbox);

    debug!(sandbox = %sb.name(), sandbox_id = %sb.id(), "微虚拟机已就绪");

    let bash_tool = Arc::new(SandboxedBashTool::new(sb.clone()));
    let tool_list: Vec<Arc<dyn Tool>> = vec![bash_tool];

    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string());
    let api_key =
        std::env::var("DEEPSEEK_API_KEY").unwrap_or_else(|_| "sk-your-key-here".to_string());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());

    info!("[2/4] 按优先级组装状态机工序流水线...");
    let pipeline: Vec<Box<dyn Operation>> = vec![
        Box::new(ToolExecutionOperation::new(tool_list.clone())),
        Box::new(InferenceOperation::new(
            &base_url,
            &api_key,
            model,
            Some(
                "你是一个顶尖的自主软件工程师。遇到任何任务，必须通过 bash在沙箱中编写代码运行并验证结果。"
                    .to_string(),
            ),
            &tool_list,
        )),
    ];

    let engine = Engine::new(pipeline);

    let mut ctx = Context::new();
    let mut turn = 0;
    println!("\n🐦 Corvus 已就绪！输入你的需求（输入 exit或 quit 退出）：");

    loop {
        print!("\n>>>");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        // 分级记录用户输入：debug 只记长度，trace 才记全文（隐私/长文本友好）
        debug!(len = input.len(), "读到用户输入");
        trace!(input = %input, "用户输入全文");

        if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
            print!("再见！正在关闭沙箱...");
            break;
        }

        if input.is_empty() {
            continue;
        }

        ctx.push(Message::user(input));

        turn += 1;
        let turn_span = info_span!("turn", turn);
        engine.run(&mut ctx).instrument(turn_span).await?;
    }

    sb.stop().await?;
    info!("沙箱已回收，进程正常退出");
    println!("沙箱已回收。全流程圆满结束！");

    Ok(())
}
