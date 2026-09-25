use anyhow::Result;
use corvus::{
    engine::Engine,
    events::{AgentEvent, Emitter},
    message::{Context, Message},
    observability::init_tracing,
    operations::{
        inference::InferenceOperation, tool_approval::ToolApprovalOperation,
        tool_exec::ToolExecutionOperation,
    },
    pipeline::Operation,
    render::render_text,
    sandbox::SandboxedBashTool,
    session::SessionStore,
    tool::Tool,
};
use microsandbox::Sandbox;
use std::{
    io::{self, Write},
    sync::Arc,
};
use tokio::sync::mpsc;
use tracing::{Instrument, debug, info, info_span, trace, warn};

/// ★ 事件 → 终端的唯一出口：
/// 需要显示的部分交给库里的纯函数 `render_text`，只进日志的部分就地打点。
fn render(event: AgentEvent) {
    match &event {
        AgentEvent::OperationApplied { operation } => {
            debug!(operation, "工序命中");
        }
        AgentEvent::ToolFinished { tool, output, .. } => {
            debug!(tool, output_len = output.len(), "工具返回");
        }
        _ => {
            if let Some(text) = render_text(&event) {
                println!("{text}");
            }
        }
    }
}

#[derive(Debug, Default)]
struct Args {
    resume: Option<String>,
    continue_latest: bool,
    list: bool,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--resume" | "-r" => args.resume = it.next(),
            "--continue" | "-c" => args.continue_latest = true,
            "--list" | "-l" => args.list = true,
            other => eprintln!("忽略未知参数: {other}"),
        }
    }
    args
}

/// 会话 id：unix 秒 + 随机短后缀 —— 既可按时间排序，又不会撞名
fn new_session_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let suffix = uuid::Uuid::new_v4().to_string();
    format!("{secs}-{}", &suffix[..8])
}

/// 沙箱空闲回收保险丝（秒）。
///
/// 进程被 SIGKILL 时无法执行显式 `stop()`，只能由 runtime 在心跳超时后兜底回收。
/// 默认 1800 秒（30 分钟无活动）；`CORVUS_SANDBOX_IDLE_SECS=0` 可关闭。
fn sandbox_idle_timeout_secs() -> Option<u64> {
    let secs = std::env::var("CORVUS_SANDBOX_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1800);
    (secs > 0).then_some(secs)
}

/// REPL 主循环：读输入 → 驱动引擎 → 落盘。
///
/// 单独成函数是为了让 `main` 能在**任何退出路径**上做收尾
/// （停沙箱、保存会话），而不是被 `?` 提前 return 跳过。
async fn run_repl(
    engine: &Engine,
    store: &SessionStore,
    session_id: &str,
    ctx: &mut Context,
    emitter: &Emitter,
    rx: &mut mpsc::Receiver<AgentEvent>,
) -> Result<()> {
    let mut turn = 0;
    println!("\n🐦 Corvus 已就绪！输入你的需求（exit/quit 退出）");

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
            println!("再见！正在关闭沙箱...");
            break;
        }

        if input.is_empty() {
            continue;
        }

        if ctx.awaiting_human_input() {
            ctx.push(Message::approval_answer(input));
        } else {
            ctx.push(Message::user(input));
        }

        turn += 1;
        let turn_span = info_span!("turn", turn);

        // ★ 引擎与渲染在同一任务内用 select! 交错推进：
        //   事件一产生就立刻画出来（真正的流式），且不需要跨任务同步
        //
        //   用一层显式作用域把 run future 圈起来：它借用了 &mut ctx，
        //   作用域结束时借用随之释放，下面才能再读 ctx 落盘。
        {
            let run = engine.run(ctx, emitter).instrument(turn_span);
            tokio::pin!(run);
            loop {
                tokio::select! {
                    Some(event) = rx.recv() => render(event),
                    result = &mut run => {
                        result?;
                        // 排空队列里剩余事件，保证「审批提问」先于下一个提示符出现
                        while let Ok(event) = rx.try_recv() {
                            render(event);
                        }
                        break;
                    }
                }
            }
        }

        // ★ 每轮结束即落盘：哪怕下一步按下 Ctrl+C，本轮工作也不丢
        store.save(session_id, ctx)?;
        debug!(messages = ctx.messages.len(), "会话已保存");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let args = parse_args();
    let store = SessionStore::new(SessionStore::default_root());

    if args.list {
        let sessions = store.list();
        if sessions.is_empty() {
            println!("暂无历史会话。");
        } else {
            println!("历史会话（最近的在前）：");
            for id in sessions {
                println!("  {id}");
            }
        }
        return Ok(());
    }

    println!("==================================================");
    println!("        🦅 Corvus Autonomous Agent 启动中        ");
    println!("==================================================");

    // ---------- 会话装配：恢复 or 新建 ----------
    let (session_id, mut ctx) = if let Some(id) = args.resume {
        let ctx = store.load(&id)?;
        println!("\n[会话] 已恢复 {id}（{} 条消息）", ctx.messages.len());
        (id, ctx)
    } else if args.continue_latest {
        let id = store
            .latest()
            .ok_or_else(|| anyhow::anyhow!("没有可恢复的会话，请先跑一次对话"))?;
        let ctx = store.load(&id)?;
        println!(
            "\n[会话] 已恢复最近会话 {id}（{} 条消息）",
            ctx.messages.len()
        );
        (id, ctx)
    } else {
        let id = new_session_id();
        println!("\n[会话] 新建 {id}");
        (id, Context::new())
    };

    store.prepare(&session_id)?;

    // ---------- 沙箱：工作目录绑定到宿主机磁盘 ----------
    info!("[1/4] 启动硬件隔离微虚拟机 (microsandbox)...");
    let workspace = store.workspace_dir(&session_id);
    println!("[工作区] {} → 沙箱内 /workspace", workspace.display());

    let builder = Sandbox::builder(format!("corvus-{session_id}")) // ★ 沙箱名跟会话绑定
        .image("python:3.11-slim")
        .memory(512)
        .workdir("/workspace") // 所有命令默认在 /workspace 下执行
        .volume("/workspace", |m| m.bind(&workspace)) // 绑定宿主机目录 → 文件持久化
        .replace(); // 仅当同名残留实例存在时清理（同名 = 同一会话重入）

    // 保险丝：进程被强杀时交给 runtime 兜底回收
    let builder = match sandbox_idle_timeout_secs() {
        Some(secs) => builder.idle_timeout(secs),
        None => builder,
    };

    let sandbox = builder.create().await?;
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
        Box::new(ToolApprovalOperation::new()),
        Box::new(ToolExecutionOperation::new(tool_list.clone())),
        Box::new(InferenceOperation::new(
            &base_url,
            &api_key,
            model,
            Some(
                "你是一个顶尖的自主软件工程师。你的工作目录是 /workspace（与宿主机磁盘实时同步）。遇到任何任务，必须通过 bash 在沙箱中编写代码运行并验证结果。"
                    .to_string(),
            ),
            &tool_list,
        )),
    ];

    let engine = Engine::new(pipeline);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
    let emitter = Emitter::new(tx);

    let outcome = run_repl(&engine, &store, &session_id, &mut ctx, &emitter, &mut rx).await;

    // ★ 无论 REPL 因何退出（用户 exit / LLM 报错 / 落盘失败），都必须收尸：
    //   `Sandbox` 没有实现 Drop，丢弃句柄不会停掉 VM，会留下孤儿进程。
    if let Err(error) = sb.stop().await {
        warn!(%error, "沙箱停止失败，可能残留后台实例（靠 idle_timeout 兜底回收）");
    }

    // 尽力保留最后状态：即使上面是错误退出，也把已完成的工作存下来
    if let Err(error) = store.save(&session_id, &ctx) {
        warn!(%error, "会话保存失败");
    }

    let total = ctx.total_usage();
    println!(
        "\n本次会话累计用量：prompt {} / completion {} / 合计 {} tokens",
        total.prompt, total.completion, total.total
    );
    outcome?;

    info!(session = %session_id, "沙箱已回收，会话已保存");
    println!("\n沙箱已回收，会话已保存。");
    println!("下次继续：cargo run -- --continue");

    Ok(())
}
