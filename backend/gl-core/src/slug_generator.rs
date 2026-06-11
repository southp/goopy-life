use rand::seq::IndexedRandom;

const ADJECTIVES: &[&str] = &[
    "bold", "brave", "bright", "calm", "chill", "clean", "cool", "crisp",
    "cute", "dark", "deep", "eager", "fair", "fast", "fine", "fond",
    "free", "fresh", "glad", "gold", "grand", "green", "happy", "hardy",
    "keen", "kind", "lame", "lean", "light", "lofty", "lucky", "mild",
    "neat", "noble", "odd", "pale", "plain", "plush", "proud", "pure",
    "quick", "quiet", "rare", "rich", "ripe", "rosy", "rough", "rusty",
    "safe", "sharp", "shiny", "shy", "sleek", "slim", "slow", "snug",
    "soft", "sour", "spicy", "stark", "steep", "stout", "sunny", "sweet",
    "swift", "tall", "tame", "tart", "tasty", "tender", "thick", "thin",
    "tiny", "tough", "vast", "vivid", "warm", "wavy", "weak", "weary",
    "weird", "wide", "wild", "wise", "witty", "young", "zany", "zesty",
];

const NOUNS: &[&str] = &[
    "acorn", "anvil", "apple", "arrow", "badge", "banjo", "berry", "birch",
    "bloom", "bluff", "booth", "brook", "brush", "candy", "cedar", "chalk",
    "clam", "cliff", "cloud", "clover", "comet", "coral", "crane", "creek",
    "crown", "daisy", "dawn", "delta", "dew", "drum", "dune", "eagle",
    "ember", "fawn", "fern", "finch", "flame", "flask", "flint", "forge",
    "frost", "gale", "gem", "glen", "grove", "hare", "hawk", "hedge",
    "heron", "hill", "holly", "ivy", "jade", "lake", "lark", "leaf",
    "lily", "lodge", "lotus", "maple", "marsh", "mesa", "mist", "moon",
    "moss", "oak", "olive", "otter", "owl", "palm", "patch", "peach",
    "pearl", "pebble", "pike", "pine", "plum", "pond", "quail", "rain",
    "reef", "ridge", "robin", "sage", "seed", "shade", "shore", "slate",
    "spark", "sprout", "stone", "storm", "thorn", "tide", "trail", "tulip",
    "vale", "vine", "wren", "yarrow",
];

/// Generate a human-readable slug in the form `adjective-adjective-noun`.
pub fn generate_slug() -> String {
    let mut rng = rand::rng();
    let adj1 = ADJECTIVES.choose(&mut rng).unwrap();
    let adj2 = ADJECTIVES.choose(&mut rng).unwrap();
    let noun = NOUNS.choose(&mut rng).unwrap();
    format!("{adj1}-{adj2}-{noun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_has_three_parts() {
        let slug = generate_slug();
        let parts: Vec<&str> = slug.split('-').collect();
        assert_eq!(parts.len(), 3, "slug should have 3 parts: {slug}");
    }

    #[test]
    fn slugs_are_not_all_identical() {
        let slugs: Vec<String> = (0..10).map(|_| generate_slug()).collect();
        let unique: std::collections::HashSet<&String> = slugs.iter().collect();
        assert!(unique.len() > 1, "10 slugs should not all be identical");
    }

    #[test]
    fn concurrent_generation_produces_no_panics() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let slugs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let handles: Vec<_> = (0..50)
            .map(|_| {
                let slugs = Arc::clone(&slugs);
                thread::spawn(move || {
                    let s = generate_slug();
                    slugs.lock().unwrap().push(s);
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread should not panic");
        }
        let all = slugs.lock().unwrap();
        assert_eq!(all.len(), 50, "all 50 threads should complete");
    }
}
