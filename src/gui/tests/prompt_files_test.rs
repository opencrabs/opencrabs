//! The markdown a folded prompt leaves behind (#3064).
//!
//! The card on the row names a file, so the file has to exist and its name has
//! to be the one the card prints. These checks cover the naming and the write,
//! which are the two things that can go wrong without a window: two prompts
//! landing in one file, and a reopen rewriting what is already there.

use uuid::Uuid;

use crate::prompt_files::{PromptDocument, file_name, write};

fn document(name: &str, content: &str) -> PromptDocument {
    PromptDocument {
        session: Uuid::new_v4(),
        id: Uuid::new_v4(),
        name: name.to_string(),
        content: content.to_string(),
    }
}

/// A directory of its own per test, so two runs never share a file.
fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("opencrabs-prompt-docs-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

#[test]
fn the_file_is_named_after_the_row_and_the_card() {
    let doc = document("deploy-log.md", "x");
    assert_eq!(file_name(&doc), format!("{}-deploy-log.md", doc.id));
}

#[test]
fn two_prompts_that_open_with_the_same_words_keep_one_file_each() {
    // The card name is built from the message's own opening words, so two
    // pastes that start the same way produce the same name. The row id in
    // front is what stops the second one clobbering the first.
    let first = document("deploy-log.md", "first body");
    let second = document("deploy-log.md", "second body");
    assert_ne!(file_name(&first), file_name(&second));
}

#[test]
fn a_written_prompt_holds_the_whole_body() {
    let dir = scratch();
    let body = (0..200)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let doc = document("deploy-log.md", &body);
    let path = write(&dir, &doc).expect("the document is written");
    assert_eq!(path, dir.join(file_name(&doc)));
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads back"),
        body,
        "the file holds every line the row folded away"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_reopen_leaves_a_file_of_the_same_size_alone() {
    // A row's content never changes, so a same-size file is the same file and
    // a reopen is one metadata call. The check is written as the tradeoff it
    // is: a same-size file with different bytes is left alone too, because
    // re-reading every prompt on every reopen buys nothing.
    let dir = scratch();
    let doc = document("deploy-log.md", "the body the row holds");
    let path = dir.join(file_name(&doc));
    std::fs::write(&path, "x".repeat(doc.content.len())).expect("a same-size file");

    assert_eq!(write(&dir, &doc), Some(path.clone()));
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file still reads"),
        "x".repeat(doc.content.len()),
        "the same-size file was not rewritten"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_of_the_wrong_size_is_written_again() {
    // The one failure the size check cannot hide: a truncated write leaves a
    // different length behind, so the next install repairs it.
    let dir = scratch();
    let doc = document("deploy-log.md", "the body the row holds");
    let path = dir.join(file_name(&doc));
    std::fs::write(&path, "cut").expect("a truncated file");

    assert_eq!(write(&dir, &doc), Some(path.clone()));
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads back"),
        doc.content,
        "a short file is replaced with the whole body"
    );
    std::fs::remove_dir_all(&dir).ok();
}
