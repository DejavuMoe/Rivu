use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    rivu::cli::main().await
}
