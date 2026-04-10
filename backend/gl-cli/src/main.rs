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

    let ss = vec!["foo", "bar", "hoshino", "ayumi"];
    for s in ss.iter() {
        gm.spawn(s.to_string());
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Spawning ...");
    spinner.enable_steady_tick(Duration::from_millis(100));

    while ss.iter().map(|s| gm.get(s.to_string()).unwrap().0).any(|s| s == GlStatus::InProgress) {
        std::thread::sleep(Duration::from_secs(1));
    }

    spinner.finish_with_message("Done!");
}
