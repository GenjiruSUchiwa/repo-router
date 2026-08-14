use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=RR_GIT_HASH");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    let git_hash = match std::env::var("RR_GIT_HASH") {
        Ok(hash) if !hash.trim().is_empty() => hash.trim().to_string(),
        _ => {
            let output = Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output();

            match output {
                Ok(out) if out.status.success() => {
                    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if sha.is_empty() {
                        "unknown".to_string()
                    } else {
                        sha
                    }
                }
                _ => "unknown".to_string(),
            }
        }
    };

    println!("cargo:rustc-env=RR_GIT_HASH={git_hash}");
}
