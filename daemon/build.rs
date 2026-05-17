use std::process::Command;

fn main() {
    let output = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output()
        .expect("Failed to execute git command");

    let git_hash = String::from_utf8(output.stdout).unwrap_or_default();
    let git_hash = git_hash.trim();

    let git_hash = if git_hash.is_empty() {
        "unknown"
    } else {
        git_hash
    };

    println!("cargo:rustc-env=SHBOT_GIT_HASH={}", git_hash);
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads/");
}
