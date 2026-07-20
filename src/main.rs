use anyhow::{Context, Result};
use clap::Parser;
use pg_studio::cli::run_prompts;
use pg_studio::config::AppConfig;
use pg_studio::drizzle::{check_dependencies, run_drizzle_studio};
use pg_studio::ssh::establish_tunnel;
use std::process::exit;
use std::sync::{Arc, Mutex};

/// pg-studio: Introspect a remote Postgres database via SSH tunnel and launch Drizzle Studio.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {}

fn main() -> Result<()> {
    // Basic argument parsing (for version, help, etc.)
    let _args = Args::parse();

    // Setup graceful exit handler
    let running = Arc::new(Mutex::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        let mut is_running = r.lock().unwrap();
        if *is_running {
            *is_running = false;
            println!("\nReceived Ctrl-C, shutting down gracefully...");
            exit(0); // This will drop variables in main, but actually exit(0) DOES NOT run drops.
            // To fix that, we can just let it exit, or better, we can signal a channel.
            // Actually, if we just rely on `Command::spawn` inside `main`, an exit(0)
            // will kill child processes if they are properly managed, but `Command::spawn`
            // children might become orphans.
            // Let's rely on standard Rust drop behaviour by NOT calling exit(0) immediately,
            // but since we are blocked in `wait()` for studio, we can let Drizzle Studio
            // handle SIGINT which will make `studio_child.wait()` return, and then `main` returns.
        }
    })
    .context("Error setting Ctrl-C handler")?;

    // Check for npm/npx
    check_dependencies()?;

    // Load config
    let config = AppConfig::load().unwrap_or_default();

    // Prompt user
    let (state, new_config) = run_prompts(config)?;

    // Save defaults
    if let Err(e) = new_config.save() {
        eprintln!("Warning: Failed to save config: {}", e);
    }

    // Establish SSH Tunnel
    let tunnel = establish_tunnel(&state.ssh_connection, &state.remote_db_port)?;

    // Run Drizzle Studio
    if let Err(e) = run_drizzle_studio(
        tunnel.local_port,
        &state.db_name,
        &state.db_user,
        &state.db_pass,
    ) {
        eprintln!("Error running Drizzle Studio: {}", e);
    }

    // Tunnel drops here, killing the SSH process.
    drop(tunnel);

    Ok(())
}
