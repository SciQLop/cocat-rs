use std::process::ExitCode;

/// A room's `Doc` is ~540 MB of small allocations; the system allocator keeps them in
/// its heap after the last client leaves, jemalloc hands them back.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> ExitCode {
    log_to_stderr();
    match cocat_server::cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Info and above by default; `RUST_LOG` (e.g. `RUST_LOG=debug,sqlx=warn`) overrides it.
/// Stderr, because stdout carries `listening on <address>` for whoever started us.
fn log_to_stderr() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
