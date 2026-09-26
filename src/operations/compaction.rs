//! 上下文压缩：掐头去尾之后，把**模型可见消息**的中间一段丢掉。
//!
//! 它解决的是**物理约束**：模型能收的 token 有上限，而 Agent 会一直往里塞
//! （工具输出最占地方）。撞到上限时上游直接 400，Agent 就彻底停摆 ——
//! 压缩是它能在长任务里活下来的唯一办法。
//!
//! 三条设计约束，没有一条是可选的：
//!
//! 1. **只动模型视图**：人类可见的消息、内部协调记录、故障记录一律原位保留。
//!    压缩的目的是省 token，不是删人类记忆 —— 转录始终是完整的事实账本。
//! 2. **切点只认安全边界**：工具往返（助手声明调用 → 紧随其后的结果）是上游
//!    协议的硬耦合，从中间切开会切出「孤儿结果」，上游直接 400 拒绝。
//! 3. **同一份证据只用一次**：用量与故障都是**滞后指标**，压缩之前它报的还是
//!    旧的大数字；不设闸门就会一路压到地板，把本该保留的上下文也压没了。

use crate::{
    events::Emitter,
    message::{Context, Message, Role, tool_pairing_intact},
    pipeline::{Effect, Operation, OperationResult},
    provider_error::ErrorKind,
};
use anyhow::Result;
use tracing::{debug, info, warn};

/// token 配额兜底。真实上限因模型而异，部署时用 `CORVUS_COMPACT_AT_TOKENS` 覆盖。
const DEFAULT_TOKEN_BUDGET: u32 = 60_000;
/// 尾部保留多少条模型可见消息（近因效应的保护带）。
const DEFAULT_KEEP_TAIL: usize = 8;
/// 头部保留几条：最初那句任务陈述往往是整段对话的锚点，值得一直留着。
const DEFAULT_KEEP_HEAD: usize = 1;

/// 压缩的触发原因。只影响日志，不影响压缩动作本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// 主动：上一轮真实上报的 prompt token 已逼近配额，趁着还能发出去先瘦身
    Proactive { prompt_tokens: u32 },
    /// 被动：上一轮已经因为上下文超长失败，这一轮属于自救
    Recover,
}

pub struct CompactionOperation {
    budget: u32,
    keep_head: usize,
    keep_tail: usize,
}

impl CompactionOperation {
    pub fn new() -> Self {
        let budget = std::env::var("CORVUS_COMPACT_AT_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TOKEN_BUDGET);
        let keep_tail = std::env::var("CORVUS_COMPACT_KEEP_TAIL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_KEEP_TAIL);

        Self::with_limits(budget, DEFAULT_KEEP_HEAD, keep_tail)
    }

    /// 显式给参数（测试与未来配置系统用）。
    ///
    /// `keep_tail` 至少为 1：尾部窗口为 0 会让「至少保留多少条」的窗口计算
    /// 正好落在数组末尾之外（下标越界 panic）。与其在使用处到处防御，
    /// 不如把这个不合法参数在入口处就归一化掉。
    pub fn with_limits(budget: u32, keep_head: usize, keep_tail: usize) -> Self {
        Self {
            budget,
            keep_head,
            keep_tail: keep_tail.max(1),
        }
    }

    /// 该压了吗？
    ///
    /// 顺序有讲究：**故障优先于配额**。已经失败过，就不该再去看那个
    /// （已经被证明不够小的）滞后的配额比较。
    fn trigger(&self, ctx: &Context) -> Option<Trigger> {
        if ctx.last_error_kind() == Some(ErrorKind::ContextLengthExceeded) {
            return Some(Trigger::Recover);
        }

        match ctx.last_prompt_tokens() {
            Some(tokens) if tokens >= self.budget => Some(Trigger::Proactive {
                prompt_tokens: tokens,
            }),
            _ => None,
        }
    }
}

impl Default for CompactionOperation {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Operation for CompactionOperation {
    fn name(&self) -> &'static str {
        "compaction"
    }

    async fn evaluate(&self, ctx: &Context, emit: &Emitter) -> Result<OperationResult> {
        let _ = emit;

        // 闸门一：手上这份证据还新鲜吗？—— 「同一份滞后证据只用一次」。
        if !ctx.has_new_evidence_since_compaction() {
            debug!("压缩依据已被用过且尚无新事实，本轮不重复压缩");
            return Ok(OperationResult::NotApplicable);
        }

        let Some(trigger) = self.trigger(ctx) else {
            return Ok(OperationResult::NotApplicable);
        };

        // 闸门二：有工具往返在飞时一律不压。
        // `pending_tool_calls()` 原本是执行器用的查询，在这里变成压缩的安全前提 ——
        // 切点安全性依赖「工具往返不会跨越人类发言」这个不变量，
        // 而「还有调用没拿到结果」恰恰说明这个不变量此刻不成立。
        if !ctx.pending_tool_calls().is_empty() {
            debug!(?trigger, "有待工具结果的调用在飞，本轮不压缩");
            return Ok(OperationResult::NotApplicable);
        }

        let (compacted, dropped) = compact(ctx, self.keep_head, self.keep_tail);

        // 改写历史是有代价的（模型会失去连续性），所以要求「值得」：
        // 主动瘦身至少要腾出一个尾部窗口那么多；被动自愈则饥不择食 —— 能省一点是一点。
        let min_drop = match trigger {
            Trigger::Recover => 1,
            Trigger::Proactive { .. } => self.keep_tail,
        };

        if dropped < min_drop {
            if matches!(trigger, Trigger::Recover) {
                // 自愈失败必须让人知道：否则表现为「每轮多烧两个请求、却什么都没变」。
                warn!(
                    "自愈失败：已经无处可压（往往是因为尾部窗口里就装着一个巨大的工具结果）—— \
                     建议换上下文更大的模型，或开新会话"
                );
            }
            debug!(?trigger, dropped, min_drop, "可压缩量不足，本轮不压缩");
            return Ok(OperationResult::NotApplicable);
        }

        // 条数由引擎在广播 HistoryReplaced 时统计（那里也是人类看到的数字），
        // 这里不再重复算一遍 —— 两处计算迟早会漂移。
        info!(?trigger, dropped, "上下文已压缩");

        Ok(OperationResult::applied(vec![Effect::ReplaceConversation(
            compacted,
        )]))
    }
}

/// 真正的压缩：返回（新转录, 丢掉的条数）。丢 0 条表示「无处可压」。
///
/// 它是**纯函数**，不碰 IO、不打印 —— 压缩这种「改写历史」的动作必须能被
/// 单测逐字断言，否则没人敢信它没吃掉东西。
pub fn compact(ctx: &Context, keep_head: usize, keep_tail: usize) -> (Vec<Message>, usize) {
    let untouched = || (ctx.messages.clone(), 0);

    // 防御性归一化：`compact` 是公开的纯函数，不能假定调用方已经夹紧过参数
    let keep_tail = keep_tail.max(1);

    // 模型视图的消息下标（只有这些参与压缩决策）
    let view: Vec<usize> = ctx
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.agent_visible)
        .map(|(i, _)| i)
        .collect();

    // `saturating_add`：两个旋钮都来自环境变量，不能靠“不会那么大”来保证不溢出
    if view.len() <= keep_head.saturating_add(keep_tail) {
        return untouched();
    }

    // 头部边界：只落在「人类发言」处，且不超过 keep_head 条。
    let head_end = (0..keep_head)
        .rev()
        .find(|&i| ctx.messages[view[i]].is_human_turn())
        .map_or(0, |i| i + 1);

    // 尾部边界：从「至少留 keep_tail 条」的位置起**往前**找切点。
    // 优先找人类发言（语义干净的切点）；只有整段工具链里都没有新的人类发言时，
    // 才退而求其次找一条非工具消息 —— 那也安全：孤儿结果只可能由「切在工具结果上」产生。
    let limit = view.len() - keep_tail;
    let candidates = (head_end + 1)..=limit;
    let tail_start = candidates
        .clone()
        .rev()
        .find(|&i| ctx.messages[view[i]].is_human_turn())
        .or_else(|| {
            candidates
                .rev()
                .find(|&i| ctx.messages[view[i]].role != Role::Tool)
        });

    let Some(tail_start) = tail_start else {
        return untouched();
    };

    // 系统消息不参与压缩：它通常是一条不可重建的指令。
    let doomed: Vec<usize> = view[head_end..tail_start]
        .iter()
        .copied()
        .filter(|&i| ctx.messages[i].role != Role::System)
        .collect();

    if doomed.is_empty() {
        return untouched();
    }

    // 重建：按原顺序走一遍，跳过被丢掉的模型可见消息，其余（人类侧消息、
    // 内部协调记录、故障记录）原位保留 —— 所以是 filter 语义，不是切片拼接。
    let mut out = Vec::with_capacity(ctx.messages.len() - doomed.len() + 1);
    for (i, message) in ctx.messages.iter().enumerate() {
        if doomed.binary_search(&i).is_ok() {
            continue;
        }
        out.push(message.clone());
    }

    // ★ 标记追加在**末尾**：位置本身就是状态。
    //   它落在末尾 ≡「刚压过一轮，而且此后还没有产生任何新事实」，
    //   于是「能不能再压」不需要任何额外字段或内部可变量。
    out.push(Message::compaction_marker(doomed.len()));

    // ★ 最后一道闸门：亲手校验产物。
    //   前面的切点规则只保证了「相邻往返不被劈开」，但那依赖于「工具往返不会
    //   被别的消息打断」这个前提 —— 而转录**本身**可能已经违反过协议（例：
    //   max_steps 熔断留下未应答声明，人类又说了一句，下一轮才补上结果），
    //   这种情况下照常搬运会把「孤儿结果」放大成上游 400。
    //   宁可这一轮不压，也不能把坏转录变得更坏。
    if !tool_pairing_intact(&out) {
        debug!("压缩会产出不合法的转录（工具往返被破坏），放弃本轮压缩");
        return untouched();
    }

    (out, doomed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{TokenUsage, ToolCall};

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: "bash".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
        }
    }

    fn noop() -> Emitter {
        Emitter::noop()
    }

    /// 切点只能落在安全边界上：工具往返绝不能被劈开。
    #[test]
    fn cuts_only_at_safe_boundaries() {
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant_tool_call(vec![call("c1")]));
        ctx.push(Message::tool_response("c1", "输出"));
        ctx.push(Message::user("继续"));
        ctx.push(Message::assistant_tool_call(vec![call("c2")]));
        ctx.push(Message::tool_response("c2", "输出"));
        ctx.push(Message::assistant("阶段结论"));
        ctx.push(Message::user("再来"));
        ctx.push(Message::assistant("做完了"));

        let (out, dropped) = compact(&ctx, 1, 2);

        assert_eq!(dropped, 6, "头部 1 条 + 尾部 2 条之外全丢");
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].content, "任务", "头部锚点必须留着");
        assert_eq!(out[1].content, "再来");
        assert_eq!(out[2].content, "做完了");
        assert!(out[3].is_compaction_marker(), "末尾要补一条提示");

        // 关键不变量：压缩后的转录仍然合法 —— 没有孤儿结果、没有未应答声明
        assert!(tool_pairing_intact(&out), "工具往返必须成对");
        let mut compacted = Context::new();
        compacted.messages = out;
        assert!(compacted.pending_tool_calls().is_empty());
        assert!(compacted.for_agent().all(|m| m.role != Role::Tool));
    }

    /// 输入**本身**已在协议上不合法时，宁可不压，也不能把坏转录变得更坏。
    ///
    /// 这个反例是真存在的：`max_steps` 熔断会把一次「已声明、未应答」的调用
    /// 留在转录里，人类接着又发了一句，下一轮工具工序才把结果补上 ——
    /// 于是声明与结果之间夹了一条非工具消息。此时若照常搬运，
    /// 会把「孤儿结果」送到上游，换来一个 400。
    #[test]
    fn refuses_to_produce_an_orphan_tool_result() {
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant_tool_call(vec![call("c1")]));
        ctx.push(Message::assistant("顺口解释一句"));
        ctx.push(Message::tool_response("c1", "输出"));

        let (out, dropped) = compact(&ctx, 1, 2);

        assert_eq!(dropped, 0, "会切出孤儿结果，所以放弃压缩");
        assert_eq!(out.len(), ctx.messages.len(), "转录必须原样不动");
        assert!(
            !tool_pairing_intact(&ctx.messages),
            "前提：这份转录本身已经违反协议（说明夹在声明与结果之间）"
        );
    }

    /// 退化参数不得 panic。
    ///
    /// `keep_tail = 0` 曾经让「至少留多少条」的窗口计算正好落在数组末尾之外 ——
    /// 而 panic 会从工序一路冒到 `main`，跳过沙箱回收与会话落盘。
    #[test]
    fn survives_degenerate_limits() {
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant("回复"));
        ctx.push(Message::user("继续"));
        ctx.push(Message::assistant("收尾"));

        for keep_tail in [0, 1, 99] {
            let (out, _) = compact(&ctx, 1, keep_tail);
            assert!(
                tool_pairing_intact(&out),
                "keep_tail={keep_tail} 时也不得破坏配对"
            );
        }

        // keep_head = 0：不保护头部，但其余照常
        let (out, _) = compact(&ctx, 0, 1);
        assert!(out.is_empty() || tool_pairing_intact(&out));
    }

    /// 尾部窗口正好落在一个工具结果上时，必须继续往前挪到安全边界。
    #[test]
    fn tail_never_starts_on_a_tool_result() {
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant("中间闲聊"));
        ctx.push(Message::user("第二轮"));
        ctx.push(Message::assistant_tool_call(vec![call("c1")]));
        ctx.push(Message::tool_response("c1", "输出"));

        let (out, dropped) = compact(&ctx, 1, 2);

        assert_eq!(dropped, 1, "只丢得掉中间那句闲聊");
        let roles: Vec<_> = out.iter().map(|m| m.role.clone()).collect();
        assert_eq!(
            roles,
            vec![
                Role::User,      // 头部
                Role::User,      // 人类发言：尾部起点
                Role::Assistant, // 工具调用
                Role::Tool,      // 结果仍然紧跟着它的调用
                Role::User,      // 压缩标记（role=User，但人类看不见）
            ]
        );

        let mut compacted = Context::new();
        compacted.messages = out;
        assert!(compacted.pending_tool_calls().is_empty());
        assert!(tool_pairing_intact(&compacted.messages));
    }

    /// 整段尾巴全是工具往返、没有新的人类发言时，退到「非工具消息」切点。
    #[test]
    fn falls_back_to_a_non_tool_cut_in_a_long_tool_chain() {
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        for (i, id) in ["c1", "c2", "c3", "c4"].iter().enumerate() {
            ctx.push(Message::assistant_tool_call(vec![call(id)]));
            ctx.push(Message::tool_response(*id, format!("输出 {i}")));
        }

        let (out, dropped) = compact(&ctx, 1, 2);

        assert_eq!(dropped, 6, "中途几轮工具往返全丢，只留头部与尾部窗口");
        assert_eq!(out.last().map(|m| m.is_compaction_marker()), Some(true));
        assert_eq!(out[0].content, "任务", "头部锚点必须留着");

        let mut compacted = Context::new();
        compacted.messages = out;
        assert!(
            compacted.pending_tool_calls().is_empty(),
            "切点必须保证调用与结果成对"
        );
        assert!(tool_pairing_intact(&compacted.messages));
    }

    /// 压缩只动模型视图：人类侧的记录与故障诊断原位保留。
    #[test]
    fn keeps_human_only_records_and_fault_diagnostics() {
        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::user_only_notification("本地命令已执行"));
        ctx.push(Message::error(ErrorKind::RateLimited, "429 slow down"));
        ctx.push(Message::assistant("中间"));
        ctx.push(Message::user("新的一轮"));
        ctx.push(Message::assistant("收尾"));

        let (out, dropped) = compact(&ctx, 1, 1);

        assert_eq!(dropped, 1, "丢掉的只有一条模型可见消息");
        assert!(
            out.iter()
                .any(|m| m.content == "本地命令已执行" && !m.agent_visible),
            "人类侧记录不该被压缩吃掉"
        );
        assert!(
            out.iter()
                .any(|m| m.error_kind == Some(ErrorKind::RateLimited)),
            "故障诊断必须留在转录里（它是运维证据）"
        );

        // 但它们都不在模型视图里
        let mut compacted = Context::new();
        compacted.messages = out;
        assert_eq!(compacted.for_agent().count(), 4);
    }

    /// 有工具往返在飞时拒绝压缩：不变量此刻不成立，宁可不压。
    #[tokio::test]
    async fn refuses_while_a_tool_round_trip_is_in_flight() -> Result<()> {
        let op = CompactionOperation::with_limits(1_000, 1, 1);

        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant_tool_call(vec![call("c1")]));
        ctx.push(Message::error(ErrorKind::ContextLengthExceeded, "too long"));

        assert!(matches!(
            op.evaluate(&ctx, &noop()).await?,
            OperationResult::NotApplicable
        ));
        Ok(())
    }

    /// 没超配额、也没有故障 → 不适用。
    #[tokio::test]
    async fn stays_out_of_the_way_below_budget() -> Result<()> {
        let op = CompactionOperation::with_limits(60_000, 1, 1);

        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        ctx.push(Message::assistant("回复").with_usage(TokenUsage {
            prompt: 1_200,
            completion: 20,
            total: 1_220,
        }));

        assert!(matches!(
            op.evaluate(&ctx, &noop()).await?,
            OperationResult::NotApplicable
        ));
        Ok(())
    }

    /// 主动压缩：超配额就压，但同一份滞后的用量数字只用一次。
    #[tokio::test]
    async fn compacts_once_per_stale_usage_reading() -> Result<()> {
        let op = CompactionOperation::with_limits(60_000, 1, 2);

        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        for i in 0..4 {
            ctx.push(Message::assistant(format!("第 {i} 步")));
            ctx.push(Message::user(format!("继续 {i}")));
        }
        // 滞后指标：这是「上一次请求」的真实用量
        ctx.push(Message::assistant("阶段结论").with_usage(TokenUsage {
            prompt: 99_000,
            completion: 30,
            total: 99_030,
        }));

        let OperationResult::Applied(step) = op.evaluate(&ctx, &noop()).await? else {
            panic!("超配额必须触发压缩");
        };
        assert!(!step.yield_turn, "压缩是推进动作，本轮不能结束");

        let Effect::ReplaceConversation(messages) = &step.effects[0] else {
            panic!("压缩必须整体替换转录");
        };
        assert!(messages.len() < ctx.messages.len());
        ctx.messages = messages.clone();

        // 同一个数字再问一次：必须拒绝，否则会一路压到地板
        assert!(matches!(
            op.evaluate(&ctx, &noop()).await?,
            OperationResult::NotApplicable
        ));

        // 一旦真的跑通一次推理（拿到新的真实用量），闸门重新打开
        ctx.push(Message::assistant("新回复").with_usage(TokenUsage {
            prompt: 99_000,
            completion: 10,
            total: 99_010,
        }));
        assert!(matches!(
            op.evaluate(&ctx, &noop()).await?,
            OperationResult::Applied(_)
        ));
        Ok(())
    }

    /// 自愈路径：超长故障 → 压一次；压完再失败就不许再压（否则会连环重写历史）。
    #[tokio::test]
    async fn recovers_from_a_context_length_failure_exactly_once() -> Result<()> {
        let op = CompactionOperation::with_limits(60_000, 1, 2);

        let mut ctx = Context::new();
        ctx.push(Message::user("任务"));
        for i in 0..4 {
            ctx.push(Message::assistant(format!("第 {i} 步")));
            ctx.push(Message::user(format!("继续 {i}")));
        }
        ctx.push(Message::error(
            ErrorKind::ContextLengthExceeded,
            "maximum context length is 65536 tokens",
        ));

        let OperationResult::Applied(step) = op.evaluate(&ctx, &noop()).await? else {
            panic!("故障必须触发自愈压缩");
        };
        let Effect::ReplaceConversation(messages) = &step.effects[0] else {
            panic!("压缩必须整体替换转录");
        };
        ctx.messages = messages.clone();

        // 复用「之前的失败」当理由 —— 不算新事实，必须拒绝
        ctx.push(Message::error(
            ErrorKind::ContextLengthExceeded,
            "maximum context length is 65536 tokens",
        ));
        assert!(matches!(
            op.evaluate(&ctx, &noop()).await?,
            OperationResult::NotApplicable
        ));
        Ok(())
    }
}
