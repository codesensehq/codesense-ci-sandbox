// SPDX-License-Identifier: MIT
//! Memorable 2-word names for terminal sessions whose launcher did not provide
//! one. Deterministic from a seed (a session id, a handle, or any string): the
//! same input always yields the same name, so a session's label is stable
//! across restarts even though the layout itself is not persisted.
//!
//! The lists are curated: visual / sensory adjectives, friendly concrete nouns,
//! short, non-corporate, non-offensive. 80 × 80 = 6400 combinations — plenty of
//! headroom against collisions for a human-sized fleet.

use blake3::Hasher;

const ADJECTIVES: &[&str] = &[
    "Amber", "Azure", "Blue", "Bold", "Brave", "Breezy", "Bright", "Brisk", "Cheery", "Clever",
    "Cosmic", "Crimson", "Crystal", "Dapper", "Deep", "Dusty", "Eager", "Earthy", "Ember",
    "Emerald", "Fancy", "Fiery", "Fluffy", "Fond", "Frosty", "Gentle", "Giddy", "Gilded", "Golden",
    "Grand", "Happy", "Hazel", "Humble", "Icy", "Indigo", "Iron", "Ivory", "Jade", "Jolly", "Keen",
    "Kind", "Lemon", "Lively", "Lucky", "Lunar", "Merry", "Minty", "Misty", "Mossy", "Noble",
    "Opal", "Peach", "Pearl", "Plucky", "Polar", "Quick", "Quiet", "Radiant", "Rapid", "Rose",
    "Royal", "Ruby", "Rustic", "Shiny", "Silent", "Silky", "Silver", "Snowy", "Solar", "Steady",
    "Stormy", "Sunny", "Swift", "Tangy", "Tidal", "Topaz", "Velvet", "Vivid", "Warm", "Witty",
];

const NOUNS: &[&str] = &[
    "Acorn", "Anchor", "Arrow", "Badger", "Beacon", "Beetle", "Berry", "Blossom", "Breeze",
    "Brook", "Canyon", "Cedar", "Cliff", "Clover", "Comet", "Coral", "Cosmos", "Delta", "Dolphin",
    "Dragon", "Dune", "Eagle", "Echo", "Falcon", "Feather", "Fern", "Finch", "Fjord", "Forest",
    "Fox", "Frost", "Garden", "Glade", "Grove", "Harbor", "Hawk", "Heron", "Island", "Lagoon",
    "Lantern", "Lark", "Leaf", "Lion", "Lotus", "Lynx", "Maple", "Meadow", "Meteor", "Nebula",
    "Oak", "Ocean", "Octopus", "Orchid", "Otter", "Owl", "Panda", "Pearl", "Pebble", "Pine",
    "Planet", "Quasar", "Raven", "Reef", "Ridge", "River", "Rocket", "Sage", "Shore", "Sparrow",
    "Spire", "Star", "Stone", "Summit", "Thunder", "Tiger", "Tulip", "Valley", "Wave", "Whale",
    "Willow",
];

const _: () = {
    assert!(ADJECTIVES.len() == 80);
    assert!(NOUNS.len() == 80);
};

pub fn memorable_name_from_seed(seed: u64) -> String {
    let adj = ADJECTIVES[(seed as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((seed >> 32) as usize) % NOUNS.len()];
    format!("{adj} {noun}")
}

pub fn memorable_name_from_str(input: &str) -> String {
    memorable_name_from_seed(seed_from_str(input))
}

fn seed_from_str(input: &str) -> u64 {
    let mut hasher = Hasher::new();
    hasher.update(b"codesense:memorable:v1\0");
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// A default name looks like a leftover placeholder the generator should
/// replace: the old sequential "Terminal N" default, or a raw session handle
/// (`term-tab-...` / `pty-session-...` / `remote-tab-...`) that leaked as a
/// label when nothing better was set.
pub fn looks_like_default_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return true;
    }
    if let Some(rest) = trimmed.strip_prefix("Terminal ") {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    const HANDLE_PREFIXES: &[&str] = &["term-tab-", "pty-session-", "remote-tab-", "remote-shell-"];
    HANDLE_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_two_title_case_words_separated_by_a_space() {
        let name = memorable_name_from_str("pty-session-abc");
        let words: Vec<&str> = name.split(' ').collect();
        assert_eq!(words.len(), 2, "{name:?} is not two words");
        for word in words {
            assert!(!word.is_empty());
            let first = word.chars().next().unwrap();
            assert!(first.is_ascii_uppercase(), "{word:?} is not title-case");
            assert!(
                word[1..].chars().all(|c| c.is_ascii_lowercase()),
                "{word:?} has non-lowercase tail"
            );
        }
    }

    #[test]
    fn is_deterministic_per_seed() {
        assert_eq!(
            memorable_name_from_str("session-a"),
            memorable_name_from_str("session-a"),
        );
        assert_eq!(memorable_name_from_seed(42), memorable_name_from_seed(42));
    }

    #[test]
    fn different_seeds_rarely_collide_in_a_small_sample() {
        let names: std::collections::HashSet<_> = (0..200)
            .map(|i| memorable_name_from_str(&format!("term-tab-{i}")))
            .collect();
        assert!(names.len() > 180, "too many collisions: {}", names.len());
    }

    #[test]
    fn recognises_the_old_sequential_default() {
        assert!(looks_like_default_name("Terminal 1"));
        assert!(looks_like_default_name("Terminal 42"));
        assert!(looks_like_default_name(""));
        assert!(looks_like_default_name("   "));
    }

    #[test]
    fn recognises_a_handle_shaped_name() {
        assert!(looks_like_default_name("term-tab-1791084011927390000-0"));
        assert!(looks_like_default_name("pty-session-abc123"));
        assert!(looks_like_default_name("remote-tab-xyz"));
    }

    #[test]
    fn leaves_a_user_chosen_name_alone() {
        assert!(!looks_like_default_name("Blue Sky"));
        assert!(!looks_like_default_name("build watch"));
        assert!(!looks_like_default_name("Terminal that fixes the bug"));
        assert!(!looks_like_default_name("Terminal"));
    }
}
