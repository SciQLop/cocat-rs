use std::process::ExitCode;

/// A room's `Doc` is ~540 MB of small allocations; the system allocator keeps them in
/// its heap after the last client leaves, jemalloc hands them back.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> ExitCode {
    match cocat_server::cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
