use std::io::{IsTerminal, stderr};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("corvus=debug,info"));

    let fmt_layer = fmt::layer()
        .with_writer(stderr)
        .with_ansi(stderr().is_terminal())
        .with_target(true)
        .with_line_number(false)
        .with_span_events(fmt::format::FmtSpan::CLOSE);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
}
