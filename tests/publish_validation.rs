mod common;

use common::ProxyTestHelper;
use std::fs;
use std::path::PathBuf;

/// Test that validation rejects crates missing required metadata (enabled by default)
#[test]
fn test_rejects_incomplete_metadata() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = manifest_dir.join("target");
    let test_crate_dir = target_dir.join("test-strict-incomplete");

    // Clean up and create test crate directory
    let _ = fs::remove_dir_all(&test_crate_dir);
    fs::create_dir_all(test_crate_dir.join("src")).expect("Failed to create test crate dir");

    // Create a minimal Cargo.toml WITHOUT description or license (should fail validation)
    fs::write(
        test_crate_dir.join("Cargo.toml"),
        r#"[package]
name = "test-strict-incomplete"
version = "0.1.0"
edition = "2021"
# Missing: description, license
"#,
    )
    .expect("Failed to write Cargo.toml");

    // Create a minimal lib.rs
    fs::write(test_crate_dir.join("src/lib.rs"), "// empty\n")
        .expect("Failed to write lib.rs");

    // Start proxy (validation is on by default)
    let proxy = ProxyTestHelper::new("strict-incomplete");

    // Try to publish - should fail
    let publish_output = proxy
        .cargo_command()
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&test_crate_dir)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    // Should fail due to validation
    assert!(
        !publish_output.status.success(),
        "cargo publish should have failed due to missing metadata"
    );

    // Check that the error mentions the missing fields
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("description") || combined.contains("license"),
        "Error should mention missing description or license, got:\n{}",
        combined
    );

    println!("Validation rejection test passed!");
}

/// Test that validation accepts crates with complete metadata
#[test]
fn test_accepts_complete_metadata() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = manifest_dir.join("target");
    let test_crate_dir = target_dir.join("test-strict-complete");

    // Clean up and create test crate directory
    let _ = fs::remove_dir_all(&test_crate_dir);
    fs::create_dir_all(test_crate_dir.join("src")).expect("Failed to create test crate dir");

    // Create a complete Cargo.toml with all required fields
    fs::write(
        test_crate_dir.join("Cargo.toml"),
        r#"[package]
name = "test-strict-complete"
version = "0.1.0"
edition = "2021"
description = "A test crate with complete metadata"
license = "MIT"
"#,
    )
    .expect("Failed to write Cargo.toml");

    // Create a minimal lib.rs
    fs::write(test_crate_dir.join("src/lib.rs"), "// empty\n")
        .expect("Failed to write lib.rs");

    // Start proxy (validation is on by default)
    let proxy = ProxyTestHelper::new("strict-complete");

    // Try to publish - should succeed
    let publish_output = proxy
        .cargo_command()
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&test_crate_dir)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish should have succeeded with complete metadata:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify the crate was actually published
    let crate_file = proxy
        .registry_path
        .join("crates")
        .join("test-strict-complete")
        .join("0.1.0.crate");
    assert!(
        crate_file.exists(),
        "Published crate file should exist at {:?}",
        crate_file
    );

    println!("Validation acceptance test passed!");
}

/// Test that too many keywords are rejected
#[test]
fn test_rejects_too_many_keywords() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = manifest_dir.join("target");
    let test_crate_dir = target_dir.join("test-strict-keywords");

    // Clean up and create test crate directory
    let _ = fs::remove_dir_all(&test_crate_dir);
    fs::create_dir_all(test_crate_dir.join("src")).expect("Failed to create test crate dir");

    // Create Cargo.toml with too many keywords (max 5)
    fs::write(
        test_crate_dir.join("Cargo.toml"),
        r#"[package]
name = "test-strict-keywords"
version = "0.1.0"
edition = "2021"
description = "A test crate"
license = "MIT"
keywords = ["one", "two", "three", "four", "five", "six"]
"#,
    )
    .expect("Failed to write Cargo.toml");

    // Create a minimal lib.rs
    fs::write(test_crate_dir.join("src/lib.rs"), "// empty\n")
        .expect("Failed to write lib.rs");

    // Start proxy (validation is on by default)
    let proxy = ProxyTestHelper::new("strict-keywords");

    // Try to publish - should fail
    let publish_output = proxy
        .cargo_command()
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&test_crate_dir)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    // Should fail due to too many keywords
    assert!(
        !publish_output.status.success(),
        "cargo publish should have failed due to too many keywords"
    );

    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("keywords") || combined.contains("too many"),
        "Error should mention keywords issue, got:\n{}",
        combined
    );

    println!("Keywords validation test passed!");
}

/// Test that --permissive-publishing allows incomplete metadata
#[test]
fn test_permissive_publishing_allows_incomplete() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = manifest_dir.join("target");
    let test_crate_dir = target_dir.join("test-permissive");

    // Clean up and create test crate directory
    let _ = fs::remove_dir_all(&test_crate_dir);
    fs::create_dir_all(test_crate_dir.join("src")).expect("Failed to create test crate dir");

    // Create a minimal Cargo.toml WITHOUT description or license
    fs::write(
        test_crate_dir.join("Cargo.toml"),
        r#"[package]
name = "test-permissive"
version = "0.1.0"
edition = "2021"
# Missing: description, license - but --permissive-publishing allows it
"#,
    )
    .expect("Failed to write Cargo.toml");

    // Create a minimal lib.rs
    fs::write(test_crate_dir.join("src/lib.rs"), "// empty\n")
        .expect("Failed to write lib.rs");

    // Start proxy with --permissive-publishing to skip validation
    let proxy = ProxyTestHelper::with_args("permissive", &["--permissive-publishing"]);

    // Try to publish - should succeed despite missing metadata
    let publish_output = proxy
        .cargo_command()
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&test_crate_dir)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish should have succeeded with --permissive-publishing:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify the crate was actually published
    let crate_file = proxy
        .registry_path
        .join("crates")
        .join("test-permissive")
        .join("0.1.0.crate");
    assert!(
        crate_file.exists(),
        "Published crate file should exist at {:?}",
        crate_file
    );

    println!("Permissive publishing test passed!");
}
