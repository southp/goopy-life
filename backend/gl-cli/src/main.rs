use gl_core::*;

fn main() {
    let mut gm = GoopyManager::new(
        "./test-temp".into(),
        "localhost".into(),
        "bar@example.com".into()
    );

    gm.spawn(&Goopy {
        slug: "foobar".to_string(),
        life_in_days: 3,
        created_at: chrono::Utc::now(),
    });
}
