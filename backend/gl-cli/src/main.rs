use gl_core::*;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::goopy_provisioner::ghost_local_provisioner::GhostLocalProvisioner;
use indicatif::{MultiProgress, ProgressBar};
use std::path::PathBuf;
use std::time::Duration;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "Goopy-Life CLI")]
#[command(version = "0.1")]
#[command(about = "Mainly a quick playground for now. Would it become a real CLI tool? Who knows.", long_about = None)]
struct Cli {
    /// Path to the config file
    #[arg(long, default_value = "./config.toml")]
    config: std::path::PathBuf,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Spawn one or more goopies
    Spawn {
        /// Number of instances to spawn
        #[arg(default_value = "1")]
        count: u32,
    },
    /// Despawn one or more goopies
    Despawn {
        #[arg(num_args = 1.., required = true)]
        slugs: Vec<String>,
    },
    /// List all the available goopies
    List {},
    /// Allocate storage at the given path (PlainDirAllocator smoke test)
    Alloc {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Release storage at the given path (PlainDirAllocator smoke test)
    Dealloc {
        #[arg(long)]
        path: std::path::PathBuf,
    },
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let (db_path, base_dir, port_range_start, port_range_end) = if cli.config.exists() {
        match gl_core::Config::from_file(&cli.config) {
            Ok(cfg) => {
                tracing::info!("loaded config from {}", cli.config.display());
                (cfg.registry.path, cfg.base_dir, cfg.port_range_start, cfg.port_range_end)
            }
            Err(e) => {
                tracing::error!("Error loading config: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        tracing::warn!(
            "config file not found: {}; using ./registry.db",
            cli.config.display()
        );
        // Dev-fallback port range: 50000–51000 (1000 ports for local testing)
        (PathBuf::from("./registry.db"), PathBuf::from("./test-temp"), 50000, 51000)
    };

    let registry = SqliteRegistry::new(&db_path)
        .expect("failed to open SQLite registry");

    let mut gm = GoopyManager::new(
        base_dir,
        "localhost".into(),
        "bar@example.com".into(),
        32,
        port_range_start,
        port_range_end,
        registry,
        GhostLocalProvisioner::new(),
    );
    let mp = MultiProgress::new();
    let mut spinners = vec![];
    let mut jobs = vec![];

    match cli.command {
        Cmd::Spawn { count } => {
            for _ in 0..count {
                let spinner = mp.add(ProgressBar::new_spinner());
                spinner.set_message("Spawning ...".to_string());
                spinner.enable_steady_tick(Duration::from_millis(100));

                match gm.spawn() {
                    Ok((slug, port, job_id)) => {
                        spinner.set_message(format!("Spawning {slug} ..."));
                        println!("spawned: {slug} (port {port})");
                        jobs.push(job_id);
                    }
                    Err(e) => {
                        println!("Spawn failed: {:?}", e);
                        spinner.finish_with_message(format!("Failed due to: {:?}", e));
                    }
                }
                spinners.push(spinner);
            }
        }
        Cmd::Despawn { slugs } => {
            for s in slugs.iter() {
                let spinner = mp.add(ProgressBar::new_spinner());
                spinner.set_message(format!("Despawning {} ...", s));
                spinner.enable_steady_tick(Duration::from_millis(100));

                match gm.despawn(s.to_string()) {
                    Ok(job_id) => jobs.push(job_id),
                    Err(e) => {
                        println!("Despawn failed: {:?}", e);
                        std::process::exit(1);
                    }
                }
                spinners.push(spinner);
            }
        }
        Cmd::Alloc { path } => {
            match PlainDirAllocator.allocate(&path) {
                Ok(()) => println!("allocated: {}", path.display()),
                Err(e) => {
                    eprintln!("alloc failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Cmd::Dealloc { path } => {
            match PlainDirAllocator.release(&path) {
                Ok(()) => println!("released: {}", path.display()),
                Err(e) => {
                    eprintln!("dealloc failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Cmd::List {} => {
            match gm.list() {
                Ok(goopies) => {
                    for gp in goopies {
                        println!("{:?}", gp);
                    }
                }
                Err(e) => {
                    println!("List failed: {:?}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    while jobs.iter().map(|job_id| gm.is_job_finished(job_id).unwrap()).any(|s| s == false) {
        std::thread::sleep(Duration::from_secs(1));
    }

    spinners.iter().for_each(|s| s.finish_with_message(format!("{} done!", s.message())));
}
