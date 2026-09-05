use anyhow::Result;
use corvus::{sandbox::SandboxedBashTool, tool::Tool};
use microsandbox::Sandbox;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== 1. 正在启动 MicroVM 沙箱 ===");

    let sandbox = Sandbox::builder("corvus-test-box")
        .image("alpine:latest")
        .memory(512)
        .replace()
        .create()
        .await?;
    let sb = Arc::new(sandbox);

    println!("=== 2. 将微虚拟机挂载到 Tool 接口 ===");
    let bash_tool = SandboxedBashTool::new(sb.clone());

    println!("=== 3. 测试工具执行：让微虚拟机打印系统内核 ===");
    let call_args = r#"{"command": "uname -a && echo 'Hello from isolated MicroVM!'"}"#;
    let result = bash_tool.execute(call_args).await?;

    println!("\n【沙箱工具返回结果】:\n{}", result);

    println!("\n=== 4. 安全关闭沙箱 ===");
    sb.stop().await?;
    println!("沙箱已安全回收。");
    println!("Hello, world!");

    Ok(())
}
