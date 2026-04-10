use gl_core::*;

fn main() {
    let mut gm = GoopyManager::new(
        "./test-temp".into(),
        "localhost".into(),
        "bar@example.com".into(),
        32,
    );

    let s1 = "foobar".to_string();
    gm.spawn(s1);
}
