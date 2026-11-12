use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../VERSION");
    println!("cargo:rerun-if-changed={}", path.display());

    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let version = text.trim();
    assert!(!version.is_empty(), "{} is empty", path.display());
    assert!(
        version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')),
        "{} contains invalid characters: {version}",
        path.display()
    );

    println!("cargo:rustc-env=TF_VERSION={version}");
}
