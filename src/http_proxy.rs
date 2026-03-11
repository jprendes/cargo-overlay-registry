use crate::state::MitmCa;
use log::{debug, error, info};
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

/// Run the HTTP proxy server
pub async fn run_http_proxy(
    bind_addr: &str,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
    upstream_hosts: Vec<String>,
) {
    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind HTTP proxy to {}: {}", bind_addr, e);
            return;
        }
    };

    info!("HTTP proxy listening on {}", bind_addr);
    let upstream_hosts = Arc::new(upstream_hosts);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                debug!("HTTP proxy: connection from {}", addr);
                let main_host = main_proxy_host.to_string();
                let main_port = main_proxy_port;
                let ca = mitm_ca.clone();
                let hosts = upstream_hosts.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_http_proxy_connection(stream, &main_host, main_port, ca, hosts).await
                    {
                        debug!("HTTP proxy connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("HTTP proxy accept error: {}", e);
            }
        }
    }
}

/// Handle a single HTTP proxy connection (supports keep-alive for multiple requests)
async fn handle_http_proxy_connection(
    stream: TcpStream,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
    upstream_hosts: Arc<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(read_half);

    // Create a shared reqwest client for all requests on this connection
    let client = reqwest::Client::builder()
        .user_agent("cargo-proxy-registry-http-proxy/0.1.0")
        .build()?;

    loop {
        // Read the HTTP request line
        let mut request_line = String::new();
        match buf_reader.read_line(&mut request_line).await {
            Ok(0) => break, // Connection closed
            Ok(_) => {}
            Err(e) => {
                debug!("Error reading from HTTP proxy stream: {}", e);
                break;
            }
        }

        if request_line.trim().is_empty() {
            continue;
        }

        debug!("HTTP proxy request: {}", request_line.trim());

        let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
        if parts.len() < 3 {
            write_half
                .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                .await?;
            break;
        }

        let method = parts[0].to_string();
        let target = parts[1].to_string();

        // Read headers
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if line.trim().is_empty() {
                        break;
                    }
                    headers.push(line.trim().to_string());
                }
                Err(_) => break,
            }
        }

        if method == "CONNECT" {
            // For CONNECT, we need to reunite the stream and hand off to tunnel handler
            // This consumes the connection, so we return after handling
            let inner = buf_reader.into_inner();
            let stream = inner.unsplit(write_half);
            return handle_connect_tunnel(
                stream,
                &target,
                main_proxy_host,
                main_proxy_port,
                mitm_ca,
                upstream_hosts,
            )
            .await;
        }

        // Handle regular HTTP request with keep-alive support
        let should_close = handle_http_forward_request(
            &mut buf_reader,
            &mut write_half,
            &client,
            &method,
            &target,
            &headers,
            main_proxy_host,
            main_proxy_port,
            &upstream_hosts,
        )
        .await?;

        if should_close {
            break;
        }
    }

    Ok(())
}

/// Handle CONNECT method (HTTPS tunneling)
/// For upstream registry domains, we perform MITM TLS interception to route through our proxy
async fn handle_connect_tunnel(
    stream: TcpStream,
    target: &str,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
    upstream_hosts: Arc<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse target as host:port
    let (host, port) = if let Some(colon_pos) = target.rfind(':') {
        let h = &target[..colon_pos];
        let p: u16 = target[colon_pos + 1..].parse().unwrap_or(443);
        (h, p)
    } else {
        (target, 443u16)
    };

    // Check if this is an upstream registry domain that we should intercept
    let should_intercept = upstream_hosts.iter().any(|upstream_host| {
        host == upstream_host || host.ends_with(&format!(".{}", upstream_host))
    });

    if should_intercept {
        info!(
            "HTTP proxy CONNECT MITM interception for {}:{}",
            host, port
        );
        handle_connect_mitm(stream, host, main_proxy_host, main_proxy_port, mitm_ca).await
    } else {
        info!("HTTP proxy CONNECT tunnel to {}:{}", host, port);
        handle_connect_passthrough(stream, host, port).await
    }
}

/// Handle CONNECT with MITM TLS interception for upstream registry domains
async fn handle_connect_mitm(
    mut stream: TcpStream,
    host: &str,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Generate a certificate for the target domain, signed by our CA
    let (cert_pem, key_pem) = mitm_ca.sign_domain_cert(host)?;

    // Parse the certificate and key
    let certs =
        rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
    let key =
        rustls_pemfile::private_key(&mut key_pem.as_slice())?.ok_or("No private key found")?;

    // Build TLS server config
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_config));

    // Send 200 Connection Established before TLS handshake
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    // Perform TLS handshake with client
    let tls_stream = match acceptor.accept(stream).await {
        Ok(s) => s,
        Err(e) => {
            error!("TLS handshake failed for {}: {}", host, e);
            return Err(e.into());
        }
    };

    info!("  TLS handshake completed for {}", host);

    // Create a shared reqwest client for all requests on this connection
    let client = reqwest::Client::builder()
        .user_agent("cargo-proxy-registry-mitm/0.1.0")
        .build()?;

    // Split the TLS stream for reading and writing
    let (read_half, mut write_half) = tokio::io::split(tls_stream);
    let mut buf_reader = BufReader::new(read_half);

    // Now handle HTTP requests over the TLS connection
    loop {
        // Read the HTTP request line
        let mut request_line = String::new();

        match buf_reader.read_line(&mut request_line).await {
            Ok(0) => break, // Connection closed
            Ok(_) => {}
            Err(e) => {
                debug!("Error reading from TLS stream: {}", e);
                break;
            }
        }

        if request_line.trim().is_empty() {
            // Empty line might just be keep-alive probe, continue
            continue;
        }

        // Parse request line
        let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
        if parts.len() < 3 {
            debug!("Invalid request line: {}", request_line.trim());
            break;
        }

        let method = parts[0];
        let path = parts[1];

        // Read headers
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if line.trim().is_empty() {
                        break;
                    }
                    headers.push(line.trim().to_string());
                }
                Err(_) => break,
            }
        }

        // Check for Expect: 100-continue header
        let expects_continue = headers.iter().any(|h| {
            h.to_lowercase().starts_with("expect:") && h.to_lowercase().contains("100-continue")
        });

        // Send 100 Continue response if requested before reading body
        if expects_continue {
            tokio::io::AsyncWriteExt::write_all(&mut write_half, b"HTTP/1.1 100 Continue\r\n\r\n")
                .await?;
            tokio::io::AsyncWriteExt::flush(&mut write_half).await?;
            debug!("  Sent 100 Continue for {}", path);
        }

        // Get content length
        let content_length: usize = headers
            .iter()
            .find(|h| h.to_lowercase().starts_with("content-length:"))
            .and_then(|h| h.split(':').nth(1))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        // Read body if present
        let body = if content_length > 0 {
            let mut body = vec![0u8; content_length];
            tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body).await?;
            body
        } else {
            Vec::new()
        };

        info!("  MITM {} https://{}{}", method, host, path);

        // Rewrite to our proxy
        let proxy_url = format!("http://{}:{}{}", main_proxy_host, main_proxy_port, path);
        info!("    -> Rewriting to: {}", proxy_url);

        let request = match method {
            "GET" => client.get(&proxy_url),
            "POST" => client.post(&proxy_url).body(body),
            "PUT" => client.put(&proxy_url).body(body),
            "DELETE" => client.delete(&proxy_url),
            "HEAD" => client.head(&proxy_url),
            _ => {
                let response = b"HTTP/1.1 405 Method Not Allowed\r\n\r\n";
                tokio::io::AsyncWriteExt::write_all(&mut write_half, response).await?;
                continue;
            }
        };

        // Forward relevant headers
        let mut request = request;
        for header in &headers {
            if let Some(colon_pos) = header.find(':') {
                let name = header[..colon_pos].trim();
                let value = header[colon_pos + 1..].trim();
                if !["host", "connection", "content-length"]
                    .contains(&name.to_lowercase().as_str())
                {
                    request = request.header(name, value);
                }
            }
        }

        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let mut response_headers = String::new();

                for (key, value) in response.headers().iter() {
                    if key != "transfer-encoding" && key != "connection" {
                        response_headers.push_str(&format!(
                            "{}: {}\r\n",
                            key,
                            value.to_str().unwrap_or("")
                        ));
                    }
                }

                let body = response.bytes().await.unwrap_or_default();

                let status_line = format!(
                    "HTTP/1.1 {} {}\r\n",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("OK")
                );
                let content_length_header = format!("content-length: {}\r\n", body.len());

                tokio::io::AsyncWriteExt::write_all(&mut write_half, status_line.as_bytes())
                    .await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, response_headers.as_bytes())
                    .await?;
                tokio::io::AsyncWriteExt::write_all(
                    &mut write_half,
                    content_length_header.as_bytes(),
                )
                .await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, b"connection: keep-alive\r\n")
                    .await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, b"\r\n").await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, &body).await?;
                tokio::io::AsyncWriteExt::flush(&mut write_half).await?;

                info!("    <- {} ({} bytes)", status.as_u16(), body.len());
            }
            Err(e) => {
                error!("    <- Error: {}", e);
                let response =
                    b"HTTP/1.1 502 Bad Gateway\r\nconnection: keep-alive\r\ncontent-length: 0\r\n\r\n";
                tokio::io::AsyncWriteExt::write_all(&mut write_half, response).await?;
                tokio::io::AsyncWriteExt::flush(&mut write_half).await?;
            }
        }

        // Check for Connection: close
        let should_close = headers.iter().any(|h| {
            h.to_lowercase().starts_with("connection:") && h.to_lowercase().contains("close")
        });

        if should_close {
            break;
        }
    }

    Ok(())
}

/// Handle CONNECT with direct passthrough (no interception)
async fn handle_connect_passthrough(
    mut stream: TcpStream,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let upstream_addr = format!("{}:{}", host, port);
    match TcpStream::connect(&upstream_addr).await {
        Ok(upstream) => {
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;

            let (mut client_read, mut client_write) = tokio::io::split(stream);
            let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);

            tokio::select! {
                result = tokio::io::copy(&mut client_read, &mut upstream_write) => {
                    if let Err(e) = result {
                        debug!("CONNECT tunnel client->upstream error: {}", e);
                    }
                }
                result = tokio::io::copy(&mut upstream_read, &mut client_write) => {
                    if let Err(e) = result {
                        debug!("CONNECT tunnel upstream->client error: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            error!("HTTP proxy: failed to connect to {}: {}", upstream_addr, e);
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
        }
    }

    Ok(())
}

/// Handle regular HTTP request (forward proxy) - returns true if connection should close
async fn handle_http_forward_request<R, W>(
    buf_reader: &mut tokio::io::BufReader<R>,
    write_half: &mut W,
    client: &reqwest::Client,
    method: &str,
    target: &str,
    headers: &[String],
    main_proxy_host: &str,
    main_proxy_port: u16,
    upstream_hosts: &[String],
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    // Parse the target URL
    let url = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        // Relative URL - need Host header
        let host = headers
            .iter()
            .find(|h| h.to_lowercase().starts_with("host:"))
            .and_then(|h| h.split(':').nth(1))
            .map(|s| s.trim())
            .unwrap_or("localhost");
        format!("http://{}{}", host, target)
    };

    info!("HTTP proxy {} request to {}", method, url);

    // Check for Expect: 100-continue header and respond before reading body
    let expects_continue = headers.iter().any(|h| {
        h.to_lowercase().starts_with("expect:") && h.to_lowercase().contains("100-continue")
    });

    if expects_continue {
        write_half
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await?;
        write_half.flush().await?;
        debug!("  Sent 100 Continue");
    }

    // Check if this is an upstream registry URL and rewrite it
    let should_rewrite = url::Url::parse(&url).ok().map_or(false, |parsed| {
        parsed.host_str().map_or(false, |url_host| {
            upstream_hosts
                .iter()
                .any(|upstream_host| {
                    url_host == upstream_host || url_host.ends_with(&format!(".{}", upstream_host))
                })
        })
    });

    let final_url = if should_rewrite {
        // Rewrite to main proxy
        if let Ok(parsed) = url::Url::parse(&url) {
            let path = parsed.path();
            let query = parsed
                .query()
                .map(|q| format!("?{}", q))
                .unwrap_or_default();
            let rewritten = format!(
                "http://{}:{}{}{}",
                main_proxy_host, main_proxy_port, path, query
            );
            info!("  -> Rewriting to: {}", rewritten);
            rewritten
        } else {
            url.clone()
        }
    } else {
        url.clone()
    };

    // Get content length if present
    let content_length: usize = headers
        .iter()
        .find(|h| h.to_lowercase().starts_with("content-length:"))
        .and_then(|h| h.split(':').nth(1))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // Read body if present
    let body = if content_length > 0 {
        let mut body = vec![0u8; content_length];
        tokio::io::AsyncReadExt::read_exact(buf_reader, &mut body).await?;
        body
    } else {
        Vec::new()
    };

    let request = match method {
        "GET" => client.get(&final_url),
        "POST" => client.post(&final_url).body(body),
        "PUT" => client.put(&final_url).body(body),
        "DELETE" => client.delete(&final_url),
        "HEAD" => client.head(&final_url),
        _ => {
            write_half.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nconnection: keep-alive\r\ncontent-length: 0\r\n\r\n").await?;
            return Ok(false);
        }
    };

    // Forward relevant headers
    let mut request = request;
    for header in headers {
        if let Some(colon_pos) = header.find(':') {
            let name = header[..colon_pos].trim();
            let value = header[colon_pos + 1..].trim();
            // Skip hop-by-hop headers and host (we're rewriting)
            if ![
                "host",
                "connection",
                "proxy-connection",
                "proxy-authorization",
                "te",
                "trailer",
                "transfer-encoding",
                "upgrade",
                "expect",
            ]
            .contains(&name.to_lowercase().as_str())
            {
                request = request.header(name, value);
            }
        }
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let status_line = format!(
                "HTTP/1.1 {} {}\r\n",
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK")
            );
            write_half.write_all(status_line.as_bytes()).await?;

            // Write response headers
            for (key, value) in response.headers().iter() {
                if key != "transfer-encoding" && key != "connection" {
                    let header_line = format!("{}: {}\r\n", key, value.to_str().unwrap_or(""));
                    write_half.write_all(header_line.as_bytes()).await?;
                }
            }

            // Get body
            let body = response.bytes().await?;

            // Write content-length, connection keep-alive and body
            let cl_header = format!(
                "content-length: {}\r\nconnection: keep-alive\r\n\r\n",
                body.len()
            );
            write_half.write_all(cl_header.as_bytes()).await?;
            write_half.write_all(&body).await?;
            write_half.flush().await?;
        }
        Err(e) => {
            error!("HTTP proxy: upstream request failed: {}", e);
            write_half.write_all(b"HTTP/1.1 502 Bad Gateway\r\nconnection: keep-alive\r\ncontent-length: 0\r\n\r\n").await?;
            write_half.flush().await?;
        }
    }

    // Check for Connection: close
    let should_close = headers
        .iter()
        .any(|h| h.to_lowercase().starts_with("connection:") && h.to_lowercase().contains("close"));

    Ok(should_close)
}
