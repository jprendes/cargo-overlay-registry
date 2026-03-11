mod common;

use common::ProxyTestHelper;
use std::fs;
use std::path::PathBuf;

#[test]
fn test_publish_quote_crate() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let quote_dir = manifest_dir.join("example").join("quote");

    let proxy = ProxyTestHelper::new("quote");

    // Publish using cargo publish
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

    // Verify the crate was stored
    let stored_crate = proxy
        .registry_path
        .join("crates")
        .join("quote")
        .join("99.0.0.crate");
    assert!(
        stored_crate.exists(),
        "Crate file not found in registry at {:?}",
        stored_crate
    );

    // Verify the index was updated
    let index_file = proxy
        .registry_path
        .join("index")
        .join("qu")
        .join("ot")
        .join("quote");
    assert!(
        index_file.exists(),
        "Index file not found at {:?}",
        index_file
    );

    let index_content = fs::read_to_string(&index_file).expect("Failed to read index file");
    println!("Index content:\n{}", index_content);
    assert!(
        index_content.contains("\"name\":\"quote\""),
        "Index doesn't contain quote"
    );
    assert!(
        index_content.contains("\"vers\":\"99.0.0\""),
        "Index doesn't contain version 99.0.0"
    );

    println!("Quote crate publish test passed!");
}
