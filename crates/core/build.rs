//! Builds the release key into Klefki, so it can check the signature on its own updates (`src/update.rs`).
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // The folder, not the file: a file that does not exist yet would make every build run this again.
    println!("cargo:rerun-if-changed={}", root.display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("minisign.pub").display()
    );
    let Ok(text) = std::fs::read_to_string(root.join("minisign.pub")) else {
        return;
    };
    // minisign.pub: an untrusted comment line, then the key in base64.
    let key = text.lines().nth(1).map(str::trim).unwrap_or_default();
    assert!(
        !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
        "minisign.pub must hold a minisign public key on its second line"
    );
    println!("cargo:rustc-env=KLEFKI_RELEASE_KEY={key}");
}
