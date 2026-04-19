use gl_core::*;
use indicatif::ProgressBar;
use std::time::Duration;

fn main() {
    let mut gm = GoopyManager::new(
        "./test-temp".into(),
        "localhost".into(),
        "bar@example.com".into(),
        32,
    );

    // let ss = vec!["foo", "bar", "hoshino", "ayumi"];
    let ss = vec!["foo"];
    let port_base = 50000;
    for (i, s) in ss.iter().enumerate() {
        gm.spawn(s.to_string(), port_base + i as u32);
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Spawning ...");
    spinner.enable_steady_tick(Duration::from_millis(100));

    while ss.iter().map(|s| gm.get(&s.to_string()).unwrap().0).any(|s| s == GlStatus::InProgress) {
        std::thread::sleep(Duration::from_secs(1));
    }

    spinner.finish_with_message("Done spawning!");

    let de_spinner = ProgressBar::new_spinner();
    de_spinner.set_message("Now despawning...");
    de_spinner.enable_steady_tick(Duration::from_millis(1000));

    for s in ss.iter() {
        let _ = gm.despawn(s.to_string());
    }

    // should there also be a status checker for the despawning jobs?
}
