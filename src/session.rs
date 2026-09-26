use crate::message::Context;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::{cmp, fs, path::PathBuf, time};

const SCHEMA_VERSION: u32 = 1;

/// 会话文件的外层信封：带 schema 版本，方便未来无损升级格式。
#[derive(Debug, Serialize, Deserialize)]
struct SessionFile {
    schema_version: u32,
    id: String,
    context: Context,
}

/// 会话存储：负责转录的读写与磁盘布局。
///
/// .corvus/
/// ├── sessions/<id>.json    # 转录（对话、审批、视角标记）
/// └── workspaces/<id>/      # 沙箱工作目录（bind mount 进 MicroVM）
///
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn default_root() -> PathBuf {
        PathBuf::from(".corvus")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// 沙箱工作目录：与转录文件**完全分离**的一棵子树。
    pub fn workspace_dir(&self, id: &str) -> PathBuf {
        self.root.join("workspaces").join(id)
    }

    pub fn session_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.json"))
    }

    /// 为新会话准备磁盘上的目录（转录目录 + 工作目录）
    pub fn prepare(&self, id: &str) -> Result<()> {
        fs::create_dir_all(self.sessions_dir())?;
        fs::create_dir_all(self.workspace_dir(id))?;
        Ok(())
    }

    /// 原子保存：先写临时文件再 rename。
    /// 直接覆写原文件时若中途崩溃，会留下半截 JSON 导致会话永久损坏；
    /// rename 在同一文件系统内是原子的，读者要么看到旧版本、要么看到新版本。
    pub fn save(&self, id: &str, context: &Context) -> Result<()> {
        self.prepare(id)?;
        let file = SessionFile {
            schema_version: SCHEMA_VERSION,
            id: id.to_string(),
            context: context.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)?;

        let path = self.session_path(id);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;

        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Context> {
        let path = self.session_path(id);
        let raw = fs::read(&path).with_context(|| format!("读取会话失败: {}", path.display()))?;
        let file: SessionFile = serde_json::from_slice(&raw)
            .with_context(|| format!("解析会话失败: {}", path.display()))?;

        if file.schema_version != SCHEMA_VERSION {
            anyhow::bail!(
                "会话 schema 版本不匹配：文件为 {}，当前程序支持 {}",
                file.schema_version,
                SCHEMA_VERSION
            )
        }

        Ok(file.context)
    }

    pub fn latest(&self) -> Option<String> {
        let mut entries: Vec<(time::SystemTime, String)> = fs::read_dir(self.sessions_dir())
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension()?.to_str()? != "json" {
                    return None;
                }
                let id = path.file_stem()?.to_str()?.to_string();
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, id))
            })
            .collect();

        entries.sort_by_key(|a| cmp::Reverse(a.0));
        entries.into_iter().next().map(|(_, id)| id)
    }

    pub fn list(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.sessions_dir()) else {
            return Vec::new();
        };

        let mut rows: Vec<(time::SystemTime, String)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension()?.to_str()? != "json" {
                    return None;
                }
                let id = path.file_stem()?.to_str()?.to_string();
                Some((entry.metadata().ok()?.modified().ok()?, id))
            })
            .collect();

        rows.sort_by_key(|a| cmp::Reverse(a.0));
        rows.into_iter().map(|(_, id)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::message::{Message, ToolCall};

    use super::*;
    use std::{
        env,
        time::{Duration, SystemTime},
    };
    use uuid::Uuid;

    fn temp_store(tag: &str) -> SessionStore {
        SessionStore::new(env::temp_dir().join(format!("corvus-{tag}-{}", Uuid::new_v4())))
    }

    /// 核心不变量：转录（含视角标记与工具状态）必须无损往返。
    #[test]
    fn round_trips_transcript_with_visibility() -> Result<()> {
        let store = temp_store("roundtrip");

        let mut ctx = Context::new();
        ctx.push(Message::user("你好"));
        ctx.push(Message::assistant_tool_call(vec![ToolCall {
            id: "c1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
        }]));
        ctx.push(Message::approval_pending());
        ctx.push(Message::approval_answer("y"));

        store.save("s1", &ctx)?;
        let restored = store.load("s1")?;

        assert_eq!(restored.messages.len(), ctx.messages.len());
        assert!(restored.messages[1].user_visible && restored.messages[1].agent_visible);
        assert!(!restored.messages[2].user_visible && !restored.messages[2].agent_visible);
        assert!(restored.messages[3].content == "y", "裁决原文必须保留");
        assert_eq!(
            restored.pending_tool_calls().len(),
            1,
            "待办工具也要原样恢复"
        );

        Ok(())
    }

    #[test]
    fn latest_prefers_most_recently_modified() -> Result<()> {
        let store = temp_store("latest");
        store.save("older", &Context::new())?;
        store.save("newer", &Context::new())?;

        let f = fs::File::options()
            .write(true)
            .open(store.session_path("older"))?;
        f.set_modified(SystemTime::now() - Duration::from_secs(60))?;

        assert_eq!(store.latest().as_deref(), Some("newer"));

        Ok(())
    }

    #[test]
    fn rejects_unknown_schema_version() -> Result<()> {
        let store = temp_store("schema");
        store.prepare("s1")?;
        fs::write(
            store.session_path("s1"),
            br#"{"schema_version": 999, "id": "s1", "context": {"messages": []}}"#,
        )?;

        let err = store.load("s1").unwrap_err().to_string();
        assert!(err.contains("schema 版本不匹配"), "实际错误: {err}");
        Ok(())
    }

    /// 兼容性回归：老会话文件里没有 `usage` / `error_kind` 字段，必须仍然能读。
    ///
    /// 这也是「什么时候不用升 `SCHEMA_VERSION`」的判据：纯增字段、且旧文件
    /// 依然可读，就不算破坏性变更（靠 `#[serde(default)]` 兜住）。
    #[test]
    fn loads_legacy_session_without_the_new_fields() -> Result<()> {
        let store = temp_store("legacy");
        store.prepare("s1")?;
        fs::write(
            store.session_path("s1"),
            br#"{"schema_version":1,"id":"s1","context":{"messages":[
                {"id":"m1","role":"User","content":"hi",
                 "tool_calls":null,"tool_call_id":null,
                 "user_visible":true,"agent_visible":true}]}}"#,
        )?;

        let ctx = store.load("s1")?;
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].usage, None);
        assert_eq!(ctx.messages[0].error_kind, None);
        assert!(ctx.has_new_evidence_since_compaction(), "老会话不可能压过");
        Ok(())
    }
}
