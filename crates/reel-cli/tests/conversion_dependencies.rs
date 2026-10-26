#[path = "../build_support/locked_dependencies.rs"]
mod locked_dependencies;

use locked_dependencies::LockedDependencies;

const LOCK: &str = r#"
version = 4
[[package]]
name = "converter"
version = "1.0.0"
dependencies = ["codec 2.0.0"]
[[package]]
name = "codec"
version = "1.0.0"
checksum = "unused-version"
[[package]]
name = "codec"
version = "2.0.0"
checksum = "encoded-payload"
dependencies = ["codec-sys"]
[[package]]
name = "codec-sys"
version = "1.0.0"
source = "git+https://example.org/codec#aaa"
[[package]]
name = "editor"
version = "1.0.0"
checksum = "editor-only"
"#;

fn identity(lock: &str) -> blake3::Hash {
    LockedDependencies::parse(lock)
        .unwrap()
        .fingerprint(&["converter"])
        .unwrap()
}

#[test]
fn unrelated_changes_preserve_converter_identity() {
    let before = identity(LOCK);
    let edited = LOCK
        .replace("editor-only", "new-editor")
        .replace("unused-version", "old-codec-updated");
    assert_eq!(before, identity(&edited));
    assert_eq!(
        before,
        identity(&format!(
            "{edited}\n[[package]]\nname = 'renderer'\nversion = '2.0.0'\n"
        ))
    );
}

#[test]
fn codec_transitive_identity_and_version_selection_invalidate_cache() {
    let before = identity(LOCK);
    assert_ne!(
        before,
        identity(&LOCK.replace("encoded-payload", "new-payload"))
    );
    assert_ne!(before, identity(&LOCK.replace("codec#aaa", "codec#bbb")));
    assert_ne!(
        before,
        identity(&LOCK.replace("[\"codec 2.0.0\"]", "[\"codec 1.0.0\"]"))
    );
}
