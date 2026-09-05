use anyhow::Result;
use microsandbox::Sandbox;
use serde_json::Value;
use std::sync::Arc;

use crate::tool::Tool;

pub struct SandboxedBashTool {
    sandbox: Arc<Sandbox>,
}

impl SandboxedBashTool {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait::async_trait]
impl Tool for SandboxedBashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "在隔离的 Linux 沙箱微虚拟机中执行 shell 命令或 python 脚本，并获取终端输出"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "需要执行的具体 shell 命令，例如 'python3 -c ...' 或 'ls -la'"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args_json: &str) -> Result<String> {
        let args: Value = serde_json::from_str(args_json)?;
        let cmd = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("缺少必填字段: command"))?;

        let output = self.sandbox.shell(cmd).await?;
        let code = output.status().code;
        let stdout = output.stdout()?;
        let stderr = output.stderr()?;

        let raw_result = if code == 0 { stdout } else { stderr };

        let safe_result = if raw_result.len() > 2000 {
            format!(
                "{}...\n\n[警告：终端输出过长已自动截断，仅保留前 2000 字符]",
                &raw_result[..2000]
            )
        } else {
            raw_result
        };

        Ok(format!("exit_code: {}\noutput:\n{}", code, safe_result))
    }
}
