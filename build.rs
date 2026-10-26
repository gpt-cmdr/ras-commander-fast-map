use std::path::Path;
use std::process::Command;

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn vcs_sha(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".cargo_vcs_info.json")).ok()?;
    let (_, value) = text.split_once("\"sha1\"")?;
    let (_, value) = value.split_once(':')?;
    let value = value.trim_start().strip_prefix('"')?.split('"').next()?;
    valid_sha(value).then(|| value.to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=FAST_MAP_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=.cargo_vcs_info.json");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let revision = std::env::var("FAST_MAP_BUILD_COMMIT")
        .ok()
        .filter(|sha| valid_sha(sha))
        .or_else(|| vcs_sha(&root))
        .or_else(|| {
            let output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .ok()?;
            let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
            (output.status.success() && valid_sha(&sha)).then_some(sha)
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FAST_MAP_ENGINE_COMMIT={revision}");
}
