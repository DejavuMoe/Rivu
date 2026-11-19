use std::{process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("初始化 Tokio 运行时");
    let outcome = runtime.block_on(rivu::cli::main());
    // Blocking I/O may outlive a cancelled task. Bound the final runtime shutdown too.
    runtime.shutdown_timeout(Duration::from_secs(1));
    outcome
}
