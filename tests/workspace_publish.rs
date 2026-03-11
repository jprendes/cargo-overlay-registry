mod common;

use common::ProxyTestHelper;
use std::fs;
use std::path::PathBuf;

#[test]
fn test_workspace_publish() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example_dir = manifest_dir.join("example");

    let proxy = ProxyTestHelper::new("workspace");

    // Publish the entire workspace using cargo publish --workspace
    // Use --no-verify because cargo verifies packages before uploading them all,
    // which fails when test-consumer needs quote v99.0.0 that hasn't been uploaded yet
    let publish_output = proxy
        .cargo_command()
        .args(["publish", "--workspace", "--allow-dirty", "--token", "dummy", "--no-verify"])
        .current_dir(&example_dir)
        .output()
        .expect("Failed to run cargo publish --workspace");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);

    println!("=== Workspace publish stdout ===\n{}", stdout);
    println!("=== Workspace publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish --workspace failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify quote was published
    let quote_crate = proxy
        .registry_path
        .join("crates")
        .join("quote")
        .join("99.0.0.crate");
    assert!(
        quote_crate.exists(),
        "quote crate file not found in registry at {:?}",
        quote_crate
    );

    // Verify hello-proxy was published
    let hello_crate = proxy
        .registry_path
        .join("crates")
        .join("hello-proxy")
        .join("0.1.0.crate");
    assert!(
        hello_crate.exists(),
        "hello-proxy crate file not found in registry at {:?}",
        hello_crate
    );

    // Verify test-consumer was published
    let consumer_crate = proxy
        .registry_path
        .join("crates")
        .join("test-consumer")
        .join("0.1.0.crate");
    assert!(
        consumer_crate.exists(),
        "test-consumer crate file not found in registry at {:?}",
        consumer_crate
    );

    // Verify index files were created
    let quote_index = proxy
        .registry_path
        .join("index")
        .join("qu")
        .join("ot")
        .join("quote");
    assert!(quote_index.exists(), "quote index file not found");

    let hello_index = proxy
        .registry_path
        .join("index")
        .join("he")
        .join("ll")
        .join("hello-proxy");
    assert!(hello_index.exists(), "hello-proxy index file not found");

    let consumer_index = proxy
        .registry_path
        .join("index")
        .join("te")
        .join("st")
        .join("test-consumer");
    assert!(consumer_index.exists(), "test-consumer index file not found");

    // Verify test-consumer's index entry has quote as a dependency
    let consumer_index_content =
        fs::read_to_string(&consumer_index).expect("Failed to read consumer index");
    println!("test-consumer index content:\n{}", consumer_index_content);
    assert!(
        consumer_index_content.contains("\"name\":\"quote\"")
            || consumer_index_content.contains("quote"),
        "test-consumer index should reference quote dependency"
    );

    println!("Workspace publish test passed!");
}
