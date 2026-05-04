use gl_core::*;
use gl_core::goopy_store::simple_fs_store::SimpleFsStore;
use gl_core::goopy_provisioner::ghost_local_provisioner::GhostLocalProvisioner;
use indicatif::{MultiProgress, ProgressBar};
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
    },
    /// List all the available goopies
    List {}
}

fn main() {
    let mut gm = GoopyManager::new(
        "./test-temp".into(),
        "localhost".into(),
        "bar@example.com".into(),
        32,
        SimpleFsStore::new("./test-temp"),
        GhostLocalProvisioner::new(),
    );

    let cli = Cli::parse();
    let mp = MultiProgress::new();
    let mut spinners = vec![];
    let mut jobs = vec![];

    match cli.command {
        Cmd::Spawn { slugs } => {
            let port_base = 50000;

            for (i, s) in slugs.iter().enumerate() {
                let spinner = mp.add(ProgressBar::new_spinner());
                spinner.set_message(format!("Spawning {} ...", s));
                spinner.enable_steady_tick(Duration::from_millis(100));

                match gm.spawn(s.to_string(), port_base + i as u32) {
                    Ok(job_id) => jobs.push(job_id),
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
