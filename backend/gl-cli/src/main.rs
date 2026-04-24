use gl_core::*;
use gl_core::goopy_store::simple_fs_store::SimpleFsStore;
use indicatif::ProgressBar;
use std::time::Duration;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "Goopy-Life CLI")]
#[command(version = "0.1")]
#[command(about = "Mainly a quick playground for now. Would it become a real CLI tool? Who knows.", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Spawn one or more goopies test test
    Spawn {
        #[arg(num_args = 1.., required = true)]
        slugs: Vec<String>,
    },
    /// Despawn one or more goopies
    Despawn {
        #[arg(num_args = 1.., required = true)]
        slugs: Vec<String>,
    }
}

fn main() {
    let mut gm = GoopyManager::new(
        "./test-temp".into(),
        "localhost".into(),
        "bar@example.com".into(),
        32,
        SimpleFsStore::new("./test-temp"),
    );

    let cli = Cli::parse();

    match cli.command {
        Cmd::Spawn { slugs } => {
            let port_base = 50000;
            let mut jobs = vec![];
            for (i, s) in slugs.iter().enumerate() {
                match gm.spawn(s.to_string(), port_base + i as u32) {
                    Ok(job_id) => jobs.push(job_id),
                    Err(e) => {
                        println!("Spawn failed: {:?}", e);
                        std::process::exit(1);
                    }
                }
            }

            let spinner = ProgressBar::new_spinner();
            spinner.set_message("Spawning ...");
            spinner.enable_steady_tick(Duration::from_millis(100));

            while jobs.iter().map(|job_id| gm.is_job_finished(job_id).unwrap()).any(|s| s == false) {
                std::thread::sleep(Duration::from_secs(1));
            }

            spinner.finish_with_message("Done spawning!");
        }
        Cmd::Despawn { slugs } => {
            let mut jobs = vec![];

            for s in slugs.iter() {
                match gm.despawn(s.to_string()) {
                    Ok(job_id) => jobs.push(job_id),
                    Err(e) => {
                        println!("Despawn failed: {:?}", e);
                        std::process::exit(1);
                    }
                }
            }

            let spinner = ProgressBar::new_spinner();
            spinner.set_message("Now despawning...");
            spinner.enable_steady_tick(Duration::from_millis(1000));

            while jobs.iter().map(|job_id| gm.is_job_finished(job_id).unwrap()).any(|s| s == false) {
                std::thread::sleep(Duration::from_secs(1));
            }

            spinner.finish_with_message("Done depawning!");
        }
    }
}
