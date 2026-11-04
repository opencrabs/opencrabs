use crate::brain::tools::hashline::hash::*;

#[test]
fn test_hash_deterministic() {
    let h1 = hash_line("hello world");
    let h2 = hash_line("hello world");
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), HASH_LEN);
}

#[test]
fn test_hash_chars_from_alphabet() {
    let h = hash_line("some code here");
    assert_eq!(h.len(), HASH_LEN);
    for c in h.chars() {
        assert!(
            HASH_ALPHABET.contains(&(c as u8)),
            "char '{}' not in alphabet",
            c
        );
    }
}

#[test]
fn test_different_content_different_hash() {
    let h1 = hash_line("hello");
    let h2 = hash_line("world");
    assert_ne!(h1, h2);
}

#[test]
fn test_identical_content_same_hash() {
    // Stateless hashing: identical content produces same hash
    // regardless of position. Ambiguity is handled by lazy context
    // escalation in the edit tool (issue #105).
    let h1 = hash_line("same content");
    let h2 = hash_line("same content");
    assert_eq!(h1, h2);
}

#[test]
fn test_blank_lines_same_hash() {
    // Blank lines have identical content (""), so same hash.
    // The edit tool disambiguates via line number in the HashRef.
    let h1 = hash_line("");
    let h5 = hash_line("");
    assert_eq!(h1, h5);
}

#[test]
fn test_hash_all_lines() {
    let content = "line one\nline two\nline three";
    let hashes = hash_all_lines(content);
    assert_eq!(hashes.len(), 3);
    assert_eq!(hashes[0].0, 1);
    assert_eq!(hashes[1].0, 2);
    assert_eq!(hashes[2].0, 3);
    assert_eq!(hashes[0].1.len(), HASH_LEN);
}

#[test]
fn test_format_hashline() {
    let tag = "VKQMZP";
    let formatted = format_hashline(12, tag, "function hello() {");
    assert_eq!(formatted, format!("{tag}|function hello() {{"));
}

#[test]
fn test_empty_content() {
    let h = hash_line("");
    assert_eq!(h.len(), HASH_LEN);
}

#[test]
fn test_unicode_content() {
    let h = hash_line("héllo wörld 🦀");
    assert_eq!(h.len(), HASH_LEN);
}

#[test]
fn test_wider_hash_reduces_false_collisions() {
    // Guard against a silent narrowing of HASH_LEN (#573, #2202). A 2-char
    // (256-value) tag turned ~40 distinct lines into ~37 distinct hashes; the
    // current width keeps 40 distinct lines distinct.
    use std::collections::HashSet;
    let distinct: HashSet<String> = (0..40)
        .map(|i| hash_line(&format!("let value_{i} = compute({i});")))
        .collect();
    assert!(
        distinct.len() >= 39,
        "40 distinct lines produced only {} distinct hashes, tag too narrow",
        distinct.len()
    );
}

#[test]
fn test_hash_len_survives_a_large_file() {
    // #2202: the tag is 4 bits per character, so a 4-char tag has 65,536
    // values and a 7,400-line file expects ~418 false colliding pairs. Two
    // logged failures had nine lines sharing one tag. 6 chars is the floor
    // that keeps a large file's false collisions near zero.
    //
    // Measured from a real tag rather than from the constant, so this is a
    // behavioural pin: narrowing HASH_LEN fails here instead of being
    // optimised away as a constant assertion.
    let width = hash_line("let value = compute(1);").len();
    assert!(
        width >= 6,
        "a line tag is {width} chars (HASH_LEN={HASH_LEN}); large files will \
         produce false collisions"
    );
}

#[test]
fn test_no_false_collisions_across_five_thousand_lines() {
    // Deterministic corpus, so this is a fixed fact rather than a sample:
    // 5,000 distinct lines must produce 5,000 distinct tags at the current
    // width. At 4 chars this fails by construction (expected ~190 pairs).
    use std::collections::HashMap;
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    let mut collisions = 0;
    for i in 0..5_000 {
        let line = format!("    let value_{i} = compute({i}, \"row {i}\");");
        if first_seen.insert(hash_line(&line), i).is_some() {
            collisions += 1;
        }
    }
    assert_eq!(
        collisions, 0,
        "{collisions} colliding tags across 5,000 distinct lines, tag too narrow"
    );
}
