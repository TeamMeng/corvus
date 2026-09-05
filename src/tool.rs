use anyhow::Result;
use serde_json::Value;

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn parameters_schema(&self) -> Value;

    async fn execute(&self, args_json: &str) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AddTool;

    #[async_trait::async_trait]
    impl Tool for AddTool {
        fn name(&self) -> &'static str {
            "add"
        }

        fn description(&self) -> &'static str {
            "计算两个数相加"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "a": {"type": "number"},
                    "b": {"type": "number"},
                },
                "required": ["a", "b"]
            })
        }

        async fn execute(&self, args_json: &str) -> Result<String> {
            let v: Value = serde_json::from_str(args_json)?;
            let a = v["a"].as_f64().unwrap_or(0.0);
            let b = v["b"].as_f64().unwrap_or(0.0);
            Ok((a + b).to_string())
        }
    }

    #[tokio::test]
    async fn test_tool_execution() -> Result<()> {
        let tool = AddTool;
        assert_eq!(tool.name(), "add");

        let result = tool.execute(r#"{"a": 10, "b": 20}"#).await?;
        assert_eq!(result, "30");
        Ok(())
    }
}
