//! Runs the notch chat module's node tests, so its behaviour is checked by `cargo test` too.
//! Skipped (with a note) when node is not installed.

use std::process::Command;

#[test]
fn the_notch_chat_module_passes_its_node_tests() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/gnome-shell/divisi-notch@nbr.company");
    let Ok(out) = Command::new("node").arg("chat.test.mjs").current_dir(dir).output() else {
        eprintln!("node is not installed; skipping the notch chat module tests");
        return;
    };
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("all checks passed"));
}

#[test]
fn the_notch_chat_row_shows_replies_through_the_tested_cut() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/gnome-shell/divisi-notch@nbr.company/extension.js")).unwrap();
    assert!(src.contains("text: Chat.shownText(entry)"), "_chatRow must label rows with Chat.shownText");
    assert!(!src.contains("(rules only)"), "the rules-only note belongs to chat.js, not extension.js");
}
