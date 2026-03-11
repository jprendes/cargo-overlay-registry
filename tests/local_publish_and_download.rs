mod common;

use std::path::PathBuf;
use std::process::Command;

use common::ProxyTestHelper;

#[test]
fn test_local_publish_and_download() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let quote_dir = manifest_dir.join("example").join("quote");
    let consumer_dir = manifest_dir.join("example").join("test-consumer");

    let proxy = ProxyTestHelper::new("publish");

    // Step 1: Publish the quote crate
    let publish_output = proxy
        .cargo_command()
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&quote_dir)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Step 2: Build the test-consumer crate that depends on the published quote crate
    let build_output = proxy
        .cargo_command()
        .args(["build"])
        .current_dir(&consumer_dir)
        .output()
        .expect("Failed to run cargo build");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);
    println!("=== Consumer build stdout ===\n{}", stdout);
    println!("=== Consumer build stderr ===\n{}", stderr);

    assert!(
        build_output.status.success(),
        "Consumer crate build failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Step 3: Run the consumer binary and verify it works
    let consumer_binary = manifest_dir
        .join("example")
        .join("target")
        .join("debug")
        .join("test-consumer");

    assert!(
        consumer_binary.exists(),
        "Consumer binary not found at {:?}",
        consumer_binary
    );

    let run_output = Command::new(&consumer_binary)
        .output()
        .expect("Failed to run consumer binary");

    let run_stdout = String::from_utf8_lossy(&run_output.stdout);
    println!("=== Consumer run output ===\n{}", run_stdout);

    assert!(run_output.status.success(), "Consumer binary failed to run");
    assert!(
        run_stdout.contains("Steve Jobs"),
        "Expected quote output, got: {}",
        run_stdout
    );

    println!("Publish/download integration test passed!");
}
