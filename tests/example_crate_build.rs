mod common;

use common::ProxyTestHelper;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn test_example_crate_build() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example_dir = manifest_dir.join("example").join("hello-proxy");
    let target_dir = manifest_dir.join("target");

    let proxy = ProxyTestHelper::new("example");

    // Build the example crate using only the HTTP proxy
    let build_output = proxy
        .cargo_command()
        .args(["build"])
        .current_dir(&example_dir)
        .env("RUST_LOG", "debug")
        .output()
        .expect("Failed to run cargo build on example");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);

    println!("=== Example build stdout ===\n{}", stdout);
    println!("=== Example build stderr ===\n{}", stderr);

    assert!(
        build_output.status.success(),
        "Example crate build failed!\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify the example binary was created
    let example_binary = target_dir.join("debug").join("hello-proxy");
    assert!(
        example_binary.exists(),
        "Example binary not found at {:?}",
        example_binary
    );

    // Run the example and verify output
    let run_output = Command::new(&example_binary)
        .output()
        .expect("Failed to run example binary");

    let run_stdout = String::from_utf8_lossy(&run_output.stdout);
    println!("=== Example run output ===\n{}", run_stdout);

    assert!(run_output.status.success(), "Example binary failed to run");
    assert!(
        run_stdout.contains("Hello, World!"),
        "Expected 'Hello, World!' in output, got: {}",
        run_stdout
    );

    println!("Integration test passed!");
}
