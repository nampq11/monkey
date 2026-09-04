use clap::{Parser, Subcommand};
use std::path::Path;
use std::sync::Arc;

use crate::webhook::{WebhookState, app as webhook_app};
use crate::worker::worker_loop;
use monkey_core::config::Settings;
use monkey_core::db::Store;
use monkey_core::sandbox::cleanup_workspace;
use monkey_engine::adapters::pi::PiAdapter;
use monkey_github::gh_proxy::{GhProxyState, app as gh_proxy_app};

#[derive(Parser)]
#[command(name = "monkey", about = "Self-hosted GitHub triage bot")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run orchestrator (webhook + worker)
    Serve,
    /// Run token-holding proxy service
    GhProxy,
    /// Manually triage owner/repo#N
    Triage { target: String },
    /// Show queue state
    Status,
    /// Remove a worktree
    Cleanup { target: String },
}

pub async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Serve => run_serve().await,
        Commands::GhProxy => run_gh_proxy().await,
        Commands::Triage { target } => run_triage(&target),
        Commands::Status => run_status(),
        Commands::Cleanup { target } => run_cleanup(&target),
    }
}

async fn run_serve() -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::load_from_env().map_err(|e| format!("config error: {}", e))?;
    let db_path = std::env::var("MONKEY_DB_PATH").unwrap_or_else(|_| "/data/monkey.db".to_string());
    let store = Store::new(&db_path)?;

    let adapter = Arc::new(PiAdapter::default());

    tokio::spawn(worker_loop(
        store.clone(),
        adapter.clone(),
        settings.clone(),
    ));

    let webhook_state = WebhookState { settings, store };
    let app = webhook_app(webhook_state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8000".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("monkey orchestrator listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn run_gh_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let github_token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    let hmac_key = std::env::var("MONKEY_GH_PROXY_HMAC_KEY").unwrap_or_default();

    if github_token.is_empty() {
        return Err("gh-proxy refuses to start without GITHUB_TOKEN".into());
    }
    if hmac_key.is_empty() {
        return Err("gh-proxy refuses to start without MONKEY_GH_PROXY_HMAC_KEY".into());
    }

    let state = GhProxyState::new(github_token, hmac_key);
    let app = gh_proxy_app(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("gh-proxy listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

fn run_triage(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (owner, (repo, number)) = split_target(target)?;
    let db_path = std::env::var("MONKEY_DB_PATH").unwrap_or_else(|_| "/data/monkey.db".to_string());
    let store = Store::new(&db_path)?;

    match store.get_latest_event_for_issue(&owner, &repo, number)? {
        Some(ev) => {
            println!("{}", serde_json::to_string_pretty(&ev)?);
            Ok(())
        }
        None => {
            eprintln!("no event found for {}", target);
            std::process::exit(1);
        }
    }
}

fn run_status() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::var("MONKEY_DB_PATH").unwrap_or_else(|_| "/data/monkey.db".to_string());
    let store = Store::new(&db_path)?;

    let counts = store.status_counts()?;
    for (status, count) in counts {
        println!("{}: {}", status, count);
    }

    Ok(())
}

fn run_cleanup(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (owner, (repo, number)) = split_target(target)?;
    let workspaces_root =
        std::env::var("MONKEY_WORKSPACES_ROOT").unwrap_or_else(|_| "/data/workspaces".to_string());
    cleanup_workspace(Path::new(&workspaces_root), &owner, &repo, number);
    println!("cleaned {}", target);
    Ok(())
}

pub fn split_target(target: &str) -> Result<(String, (String, i64)), String> {
    let (repo_part, num_part) = target.split_once('#').ok_or("missing issue number (#)")?;

    let number: i64 = num_part
        .parse()
        .map_err(|_| "invalid issue number".to_string())?;

    let (owner, repo) = repo_part.split_once('/').ok_or("missing repo")?;

    Ok((owner.to_string(), (repo.to_string(), number)))
}
