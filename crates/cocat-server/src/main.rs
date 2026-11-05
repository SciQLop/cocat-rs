use std::process::ExitCode;

/// A room's `Doc` is ~540 MB of small allocations that glibc keeps in its heap after the
/// last client leaves; mimalloc hands them back, so an unloaded room costs nothing.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
