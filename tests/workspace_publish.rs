use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// Build the cargo-publish-dry-run binary (only once per test run)
fn build_publish_dry_run_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let target_dir = manifest_dir.join("target");

            let build_output = Command::new("cargo")
                .args(["build", "--release", "--bin", "cargo-publish-dry-run"])
                .current_dir(&manifest_dir)
                .output()
                .expect("Failed to build cargo-publish-dry-run");

            assert!(
                build_output.status.success(),
                "Failed to build cargo-publish-dry-run binary: {}",
                String::from_utf8_lossy(&build_output.stderr)
            );

            let binary = target_dir.join("release").join("cargo-publish-dry-run");
            assert!(
                binary.exists(),
                "cargo-publish-dry-run binary not found at {:?}",
                binary
            );

            binary
        })
        .clone()
}

#[test]
fn test_workspace_publish() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example_dir = manifest_dir.join("example");

    let binary = build_publish_dry_run_binary();

    // Use temp dirs to avoid conflicts with other builds and ensure test isolation
    let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
    let target_dir = temp_dir.path().join("target");
    let cargo_home = temp_dir.path().join("cargo-home");

    // Publish the entire workspace using cargo-publish-dry-run
    let publish_output = Command::new(&binary)
        .args(["--workspace", "--allow-dirty"])
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(&example_dir)
        .output()
        .expect("Failed to run cargo-publish-dry-run");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);

    println!("=== Workspace publish stdout ===\n{}", stdout);
    println!("=== Workspace publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo-publish-dry-run --workspace failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify expected crates were packaged (shown in output)
    assert!(
        stderr.contains("Packaging quote") || stdout.contains("Packaging quote"),
        "Expected quote to be packaged"
    );
    assert!(
        stderr.contains("Packaging hello-proxy") || stdout.contains("Packaging hello-proxy"),
        "Expected hello-proxy to be packaged"
    );
    assert!(
        stderr.contains("Packaging test-consumer") || stdout.contains("Packaging test-consumer"),
        "Expected test-consumer to be packaged"
    );

    // Verify uploads were attempted (they happen before the proxy accepts them)
    assert!(
        stderr.contains("Uploading quote") || stdout.contains("Uploading quote"),
        "Expected quote to be uploaded"
    );
    assert!(
        stderr.contains("Uploading hello-proxy") || stdout.contains("Uploading hello-proxy"),
        "Expected hello-proxy to be uploaded"
    );
    assert!(
        stderr.contains("Uploading test-consumer") || stdout.contains("Uploading test-consumer"),
        "Expected test-consumer to be uploaded"
    );

    println!("Workspace publish test passed!");
}
