use std::path::PathBuf;
use std::process::Command;

const DEPLOYMENT_TARGET: &str = "26.0";

fn main() {
    let source = "swift/Bridge.swift";
    println!("cargo:rerun-if-changed={source}");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let optimize = if std::env::var("PROFILE").as_deref() == Ok("release") { "-Osize" } else { "-Onone" };
    let status = Command::new("xcrun")
        .args(["swiftc", "-parse-as-library", "-emit-library", "-static", "-module-name", "TfBridge"])
        .args(["-target", &format!("arm64-apple-macosx{DEPLOYMENT_TARGET}"), optimize])
        .arg("-o")
        .arg(out.join("libTfBridge.a"))
        .arg(source)
        .status()
        .expect("failed to run swiftc");
    assert!(status.success(), "swiftc failed");

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=TfBridge");
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-lib=framework=FoundationModels");
    println!("cargo:rustc-link-lib=framework=Speech");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
}
