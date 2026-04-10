use gl_core::*;

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

    while ss.iter().map(|s| gm.get(s.to_string()).unwrap().0).any(|s| s == GlStatus::InProgress) {
        println!("Someone is still in-progress...");

        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
