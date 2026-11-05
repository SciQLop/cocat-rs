use std::process::ExitCode;

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
