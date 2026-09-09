use anyhow::Result;
use corvus::{
    engine::Engine,
    message::{Context, Message},
    operations::{inference::InferenceOperation, tool_exec::ToolExecutionOperation},
    pipeline::Operation,
    sandbox::SandboxedBashTool,
    tool::Tool,
};
use microsandbox::Sandbox;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("==================================================");
    println!("        🦅 Corvus Autonomous Agent 启动中        ");
    println!("==================================================");

    println!("\n[1/4] 启动硬件隔离微虚拟机 (microsandbox)...");

    let sandbox = Sandbox::builder("corvus-workspace")
        .image("python:3.11-slim")
        .memory(512)
        .replace()
        .create()
        .await?;
    let sb = Arc::new(sandbox);

    let bash_tool = Arc::new(SandboxedBashTool::new(sb.clone()));
    let tool_list: Vec<Arc<dyn Tool>> = vec![bash_tool];

    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string());
    let api_key =
        std::env::var("DEEPSEEK_API_KEY").unwrap_or_else(|_| "sk-your-key-here".to_string());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());

    println!("[2/4] 按优先级组装状态机工序流水线...");
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
    let prompt = "请用 Python 编写一段脚本，计算 1 到 100 之间所有能被 3 整除但不能被 5
整除的数字之和，执行它并将结果告诉我。";
    println!("\n[3/4] 用户下达目标:\n\"{}\"", prompt);
    ctx.push(Message::user(prompt));

    println!("\n[4/4] 状态机开始点火运转...");
    engine.run(&mut ctx).await?;

    println!("\n正在安全关闭 MicroVM 沙箱...");
    sb.stop().await?;
    println!("沙箱已回收。全流程圆满结束！");

    Ok(())
}
