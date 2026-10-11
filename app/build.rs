//! Embed the short git commit in the binary (`CANSTUDIO_GIT_SHA`).

fn main() {
    let sha = std::env::var("CANSTUDIO_GIT_SHA").ok().or_else(|| {
        let out = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    println!("cargo:rustc-env=CANSTUDIO_GIT_SHA={}", sha.unwrap_or_else(|| "unknown".into()));
    println!("cargo:rerun-if-env-changed=CANSTUDIO_GIT_SHA");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
}
