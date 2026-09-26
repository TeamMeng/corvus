use tokio::sync::mpsc;

use crate::message::{Message, ToolCall};

/// Agent 生命周期事件 —— **唯一的前端契约**。
///
/// 工序不再自己 `println!`，只负责把「发生了什么」描述成事件；
/// 怎么显示、显示给谁、要不要广播，全都由订阅端决定。
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// 一轮推演开始
    RunStarted,
    /// 某道工序命中并被应用
    OperationApplied { operation: &'static str },
    /// 转录新增了一条消息（前端据此渲染对话）
    MessageAppended { message: Message },
    /// 工具开始执行
    ToolStarted {
        tool: String,
        call_id: String,
        arguments: String,
    },
    /// 工具执行结束
    ToolFinished {
        tool: String,
        call_id: String,
        output: String,
    },
    /// 需要人类裁决（结构化数据，不含任何渲染文案）
    ApprovalRequested { calls: Vec<ToolCall> },
    /// 转录被整体替换（压缩的产物）。
    ///
    /// 没有它，「历史被静默重写」就完全不可观测 ——
    /// 人类只会觉得 Agent 突然不记得事了。
    HistoryReplaced { before: usize, after: usize },
    /// 本轮结束，控制权交还人类
    RunFinished,
}

/// 事件发射器：`Clone` 后可下发给任意工序。
#[derive(Clone)]
pub struct Emitter {
    tx: mpsc::Sender<AgentEvent>,
}

impl Emitter {
    pub fn new(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self { tx }
    }

    /// 供单元测试与离线场景使用：事件直接丢弃，且永不阻塞。
    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self { tx }
    }

    /// 发送失败只说明订阅端已关闭，不影响 Agent 继续工作。
    pub async fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event).await;
    }
}
