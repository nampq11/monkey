use clap::Parser;
use monkey_app::cli::{Cli, run};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    if let Err(error) = run(cli).await {
        // Print Display, not Debug, so typed errors read as plain messages.
        eprintln!("Error: {}", error);
        std::process::exit(1);
    }
}
