use std::fs;
use std::path::Path;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    println!("cargo:rerun-if-env-changed=BRAINPOD_VERSION");

    let version = std::env::var("BRAINPOD_VERSION")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(development_version);
    println!("cargo:rustc-env=BRAINPOD_VERSION={version}");

    let descriptors = protox::compile(
        [
            "proto/brainpod/tunnel/v1/broker.proto",
            "proto/brainpod/tunnel/v1/tunnel.proto",
        ],
        ["proto"],
    )?;

    tonic_build::configure()
        .build_server(false)
        .build_transport(false)
        .compile_fds(descriptors)?;

    Ok(())
}

/// Version for builds the release workflow did not stamp, carrying the commit
/// it was built from when git metadata is reachable, so a local build is never
/// mistaken for a published release.
fn development_version() -> String {
    let base = format!(
        "{}-dev",
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned())
    );

    match commit_sha() {
        Some(sha) => format!("{base}+{sha}"),
        None => base,
    }
}

fn commit_sha() -> Option<String> {
    watch_git_head(Path::new(".git"));

    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let sha = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}

fn watch_git_head(git_dir: &Path) {
    let head = git_dir.join("HEAD");
    if !head.exists() {
        return;
    }

    println!("cargo:rerun-if-changed={}", head.display());

    let Ok(contents) = fs::read_to_string(&head) else {
        return;
    };
    if let Some(reference) = contents.strip_prefix("ref: ") {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(reference.trim()).display()
        );
    }
}
