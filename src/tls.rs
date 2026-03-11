use rcgen::{generate_simple_self_signed, CertifiedKey};

/// Generate a self-signed certificate for the registry server
pub fn generate_self_signed_cert(hostname: &str) -> Result<(Vec<u8>, Vec<u8>), rcgen::Error> {
    let subject_alt_names = vec![
        hostname.to_string(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];

    let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)?;

    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();

    Ok((cert_pem, key_pem))
}
