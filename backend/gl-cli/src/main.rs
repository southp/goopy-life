use gl_core::*;
use indicatif::ProgressBar;
use std::time::Duration;
use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Spawn one or more goopies
    Spawn {
        slugs: Vec<String>,
    },
    /// Despawn one or more goopies
    Despawn {
        slugs: Vec<String>,
    }
}

fn main() {
    let mut gm = GoopyManager::new(
        "./test-temp".into(),
        "localhost".into(),
        "bar@example.com".into(),
        32,
    );

    let cli = Cli::parse();

    match cli.command {
        Cmd::Spawn { slugs } => {
            let port_base = 50000;
            for (i, s) in slugs.iter().enumerate() {
                gm.spawn(s.to_string(), port_base + i as u32);
            }

            let spinner = ProgressBar::new_spinner();
            spinner.set_message("Spawning ...");
            spinner.enable_steady_tick(Duration::from_millis(100));

            while slugs.iter().map(|s| gm.get(&s.to_string()).unwrap().0).any(|s| s == GlStatus::InProgress) {
                std::thread::sleep(Duration::from_secs(1));
            }

            spinner.finish_with_message("Done spawning!");
        }
        Cmd::Despawn { slugs } => {
            let de_spinner = ProgressBar::new_spinner();
            de_spinner.set_message("Now despawning...");
            de_spinner.enable_steady_tick(Duration::from_millis(1000));

            for s in slugs.iter() {
                let _ = gm.despawn(s.to_string());
            }

            while slugs.iter().map(|s| gm.get(&s.to_string()).unwrap().0).any(|s| s == GlStatus::InDestructing) {
                std::thread::sleep(Duration::from_secs(1));
            }

            de_spinner.finish_with_message("Done depawning!");

        }
    }
}
