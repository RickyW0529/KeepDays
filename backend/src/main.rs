use keepdays_server::{build_app_state, config::AppConfig, create_router};
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Binary ABI / glibc / architecture smoke test flag used by Debian 13 Docker verifier & SSH deploy pre-swap check
    if std::env::args().any(|a| a == "--self-check" || a == "--version") {
        let sf = keepdays_server::snowflake::SnowflakeGenerator::new(1);
        let sample_id = sf.next_id();
        let bj = keepdays_server::time_bj::current_ambient_status();
        println!(
            "keepdays-server v{} OK | arch={} os={} | sample_id={} | bj_time={} ({:?})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH,
            std::env::consts::OS,
            sample_id,
            bj.beijing_iso,
            bj.phase
        );
        return Ok(());
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "keepdays_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = AppConfig::from_env();

    // Tailored Tokio runtime for 1vCPU / 512MB RAM servers:
    // Caps worker & blocking thread stacks so OS virtual/resident memory stays ultra-lean!
    let mut rt_builder = tokio::runtime::Builder::new_multi_thread();
    if cfg.low_mem_mode {
        rt_builder
            .worker_threads(2)
            .max_blocking_threads(4)
            .thread_stack_size(1024 * 1024); // 1MB stack instead of default 2MB
    }
    let runtime = rt_builder.enable_all().build()?;

    runtime.block_on(async move {
        let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
        let low_mem = cfg.low_mem_mode;

        let state = build_app_state(cfg).await?;
        let app = create_router(state);

        tracing::info!(
            "✨ KeepDays Server listening on http://{} (low_mem_mode={})",
            addr,
            low_mem
        );
        let listener = tokio::net::TcpListener::bind(addr).await?;

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await?;

        Ok(())
    })
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Shutting down KeepDays server gracefully...");
}
