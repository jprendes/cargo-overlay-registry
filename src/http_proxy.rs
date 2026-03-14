use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder;
use log::{debug, error};
use rustls::ServerConfig;
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tower::Service;

use crate::endpoints::{handle_internal_request, InternalResponse};
use crate::state::{GenericProxyState, MitmCa, RegistryState};

/// Shared state for the HTTP proxy functionality
#[derive(Clone)]
pub struct HttpProxyState<S: RegistryState + Clone = GenericProxyState> {
    pub proxy_state: Arc<S>,
    pub mitm_ca: Arc<MitmCa>,
    pub upstream_hosts: Arc<Vec<String>>,
}

/// Handle incoming requests - routes CONNECT and proxy-style requests
pub async fn handle_proxy_request<R: RegistryState + Clone + 'static>(
    State(state): State<HttpProxyState<R>>,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();

    if method == Method::CONNECT {
        // Handle CONNECT request for HTTPS tunneling
        handle_connect(state, request).await
    } else if uri.scheme().is_some() {
        // Proxy-style request with absolute URL (e.g., GET http://example.com/path)
        handle_forward_request(state, request).await
    } else {
        // This shouldn't happen - regular requests go to axum routes
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid proxy request"))
            .unwrap()
    }
}

/// Check if a request should be handled by the proxy layer
pub fn is_proxy_request(request: &Request) -> bool {
    request.method() == Method::CONNECT || request.uri().scheme().is_some()
}

/// Handle CONNECT method for HTTPS tunneling
async fn handle_connect<R: RegistryState + Clone + 'static>(
    state: HttpProxyState<R>,
    request: Request,
) -> Response {
    let target = request.uri().to_string();

    // Parse target as host:port
    let (host, port) = if let Some(authority) = request.uri().authority() {
        let h = authority.host().to_string();
        let p = authority.port_u16().unwrap_or(443);
        (h, p)
    } else if let Some(colon_pos) = target.rfind(':') {
        let h = target[..colon_pos].to_string();
        let p: u16 = target[colon_pos + 1..].parse().unwrap_or(443);
        (h, p)
    } else {
        (target.clone(), 443u16)
    };

    // Check if this is an upstream registry domain that we should intercept
    let should_intercept = state.upstream_hosts.iter().any(|upstream_host| {
        host == upstream_host.as_str() || host.ends_with(&format!(".{}", upstream_host))
    });

    if should_intercept {
        debug!("HTTP proxy CONNECT MITM interception for {}:{}", host, port);
    } else {
        debug!("HTTP proxy CONNECT tunnel to {}:{}", host, port);
    }

    // Spawn task to handle the upgraded connection
    let host_clone = host.clone();
    tokio::spawn(async move {
        match hyper::upgrade::on(request).await {
            Ok(upgraded) => {
                // TokioIo wraps the upgraded connection to implement tokio's AsyncRead/AsyncWrite
                let stream = TokioIo::new(upgraded);

                let result = if should_intercept {
                    handle_connect_mitm(stream, &host_clone, state.proxy_state, state.mitm_ca).await
                } else {
                    handle_connect_passthrough(stream, &host_clone, port).await
                };

                if let Err(e) = result {
                    debug!("CONNECT tunnel error: {}", e);
                }
            }
            Err(e) => {
                error!("Connection upgrade failed: {}", e);
            }
        }
    });

    // Return 200 Connection Established
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::empty())
        .unwrap()
}

/// Handle CONNECT with MITM TLS interception for upstream registry domains
async fn handle_connect_mitm<S, R>(
    stream: S,
    host: &str,
    state: Arc<R>,
    mitm_ca: Arc<MitmCa>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    R: RegistryState + 'static,
{
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Generate a certificate for the target domain, signed by our CA
    let (cert_pem, key_pem) = mitm_ca.sign_domain_cert(host)?;

    // Parse the certificate and key
    let certs = rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
    let key =
        rustls_pemfile::private_key(&mut key_pem.as_slice())?.ok_or("No private key found")?;

    // Build TLS server config
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_config));

    // Wrap the stream for TLS
    // Note: For upgraded connections, we need to use a type-erased wrapper
    let tls_stream = match acceptor.accept(stream).await {
        Ok(s) => s,
        Err(e) => {
            error!("TLS handshake failed for {}: {}", host, e);
            return Err(e.into());
        }
    };

    debug!("  TLS handshake completed for {}", host);

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
            continue;
        }

        // Parse request line
        let parts: Vec<&str> = request_line.split_whitespace().collect();
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

        debug!("  MITM {} https://{}{}", method, host, path);

        // Convert headers to the format expected by handle_internal_request
        let header_pairs: Vec<(String, String)> = headers
            .iter()
            .filter_map(|h| {
                let pos = h.find(':')?;
                Some((h[..pos].trim().to_string(), h[pos + 1..].trim().to_string()))
            })
            .collect();

        // Handle internally
        debug!("    -> Handling internally");
        let response =
            handle_internal_request(state.as_ref(), method, path, &header_pairs, &body).await;

        // Write response
        let status_line = format!("HTTP/1.1 {} OK\r\n", response.status);
        tokio::io::AsyncWriteExt::write_all(&mut write_half, status_line.as_bytes()).await?;

        for (name, value) in &response.headers {
            let header_line = format!("{}: {}\r\n", name, value);
            tokio::io::AsyncWriteExt::write_all(&mut write_half, header_line.as_bytes()).await?;
        }

        let content_length_header = format!("content-length: {}\r\n", response.body.len());
        tokio::io::AsyncWriteExt::write_all(&mut write_half, content_length_header.as_bytes())
            .await?;
        tokio::io::AsyncWriteExt::write_all(&mut write_half, b"connection: keep-alive\r\n").await?;
        tokio::io::AsyncWriteExt::write_all(&mut write_half, b"\r\n").await?;
        tokio::io::AsyncWriteExt::write_all(&mut write_half, &response.body).await?;
        tokio::io::AsyncWriteExt::flush(&mut write_half).await?;

        debug!("    <- {} ({} bytes)", response.status, response.body.len());

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
async fn handle_connect_passthrough<S>(
    stream: S,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let upstream_addr = format!("{}:{}", host, port);
    match TcpStream::connect(&upstream_addr).await {
        Ok(upstream) => {
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
            // Can't really send a response here since client expects raw tunnel
            // Just close the connection
        }
    }

    Ok(())
}

/// Handle regular HTTP proxy request (forward proxy with absolute URL)
async fn handle_forward_request<R: RegistryState + Clone + 'static>(
    state: HttpProxyState<R>,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let url = request.uri().to_string();

    debug!("HTTP proxy {} request to {}", method, url);

    // Check if this is an upstream registry URL that should be intercepted
    let should_intercept = url::Url::parse(&url).ok().is_some_and(|parsed| {
        parsed.host_str().is_some_and(|url_host| {
            state.upstream_hosts.iter().any(|upstream_host| {
                url_host == upstream_host.as_str()
                    || url_host.ends_with(&format!(".{}", upstream_host))
            })
        })
    });

    // Get the body
    let (parts, body) = request.into_parts();
    let body_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Failed to read body"))
                .unwrap();
        }
    };

    if should_intercept {
        // Handle internally
        let parsed = match url::Url::parse(&url) {
            Ok(u) => u,
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from("Invalid URL"))
                    .unwrap();
            }
        };
        let path = parsed.path();
        let query = parsed
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let full_path = format!("{}{}", path, query);

        debug!("  -> Handling internally: {}", full_path);

        // Convert headers
        let header_pairs: Vec<(String, String)> = parts
            .headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()))
            .collect();

        let internal_response = handle_internal_request(
            state.proxy_state.as_ref(),
            method.as_str(),
            &full_path,
            &header_pairs,
            &body_bytes,
        )
        .await;

        convert_internal_response(internal_response)
    } else {
        // Passthrough to upstream
        let client = state.proxy_state.client();

        let request_builder = match method {
            Method::GET => client.get(&url),
            Method::POST => client.post(&url).body(body_bytes),
            Method::PUT => client.put(&url).body(body_bytes),
            Method::DELETE => client.delete(&url),
            Method::HEAD => client.head(&url),
            _ => {
                return Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .body(Body::empty())
                    .unwrap();
            }
        };

        // Forward relevant headers
        let mut request_builder = request_builder;
        for (name, value) in parts.headers.iter() {
            let name_str = name.to_string().to_lowercase();
            // Skip hop-by-hop headers
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
            .contains(&name_str.as_str())
                && let Ok(val_str) = value.to_str()
            {
                request_builder = request_builder.header(name.clone(), val_str);
            }
        }

        match request_builder.send().await {
            Ok(upstream_response) => {
                let status = upstream_response.status();
                let mut response_builder = Response::builder().status(status);

                // Copy headers
                for (key, value) in upstream_response.headers().iter() {
                    if key != "transfer-encoding" && key != "connection" {
                        response_builder = response_builder.header(key.clone(), value.clone());
                    }
                }

                match upstream_response.bytes().await {
                    Ok(body_bytes) => response_builder.body(Body::from(body_bytes)).unwrap(),
                    Err(e) => {
                        error!("Failed to read upstream response: {}", e);
                        Response::builder()
                            .status(StatusCode::BAD_GATEWAY)
                            .body(Body::empty())
                            .unwrap()
                    }
                }
            }
            Err(e) => {
                error!("HTTP proxy: upstream request failed: {}", e);
                Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::empty())
                    .unwrap()
            }
        }
    }
}

/// Convert InternalResponse to axum Response
fn convert_internal_response(internal: InternalResponse) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(internal.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));

    for (name, value) in internal.headers {
        builder = builder.header(name, value);
    }

    builder.body(Body::from(internal.body)).unwrap()
}

/// Serve HTTP requests on any stream type with proxy support
pub async fn serve_stream<S, R>(stream: S, app: Router, proxy_state: HttpProxyState<R>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    R: RegistryState + Clone + 'static,
{
    use std::convert::Infallible;

    use hyper::service::service_fn;
    use hyper_util::rt::TokioExecutor;

    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
        let mut app = app.clone();
        let proxy_state = proxy_state.clone();

        async move {
            let (parts, body) = request.into_parts();
            let body = Body::new(body);
            let request = Request::from_parts(parts, body);

            if is_proxy_request(&request) {
                let response = handle_proxy_request(State(proxy_state), request).await;
                Ok::<_, Infallible>(response)
            } else {
                let response = app.call(request).await.into_response();
                Ok::<_, Infallible>(response)
            }
        }
    });

    let io = TokioIo::new(stream);
    if let Err(e) = Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, service)
        .await
    {
        debug!("Connection error: {}", e);
    }
}

/// Handle a proxy connection, optionally with TLS
pub async fn handle_proxy_connection<R>(
    stream: TcpStream,
    app: Router,
    proxy_state: HttpProxyState<R>,
    tls_acceptor: Option<TlsAcceptor>,
) where
    R: RegistryState + Clone + 'static,
{
    if let Some(tls_acceptor) = tls_acceptor {
        let tls_stream = match tls_acceptor.accept(stream).await {
            Ok(s) => s,
            Err(e) => {
                debug!("TLS handshake error: {}", e);
                return;
            }
        };
        serve_stream(tls_stream, app, proxy_state).await;
    } else {
        serve_stream(stream, app, proxy_state).await;
    }
}
