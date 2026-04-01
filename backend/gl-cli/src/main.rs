use gl_core::*;

fn main() {
    let gm = GoopyManager {
        base_dir: "./test-temp".into(),
        domain: "localhost".into(),
        ssl_email: "bar@example.com".into(),
    };

    gm.spawn(&Goopy {
        slug: "foobar".to_string(),
        life_in_days: 3,
        created_at: chrono::Utc::now(),
    });
}
