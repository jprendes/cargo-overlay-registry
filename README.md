# cargo-proxy-registry

A local cargo registry proxy that sits between your project and crates.io. It proxies requests to crates.io while also allowing you to publish crates locally, making it perfect for dry-run testing of publish workflows.

## Features

- **Transparent HTTPS interception**: Routes all crates.io traffic (including HTTPS) through the proxy using MITM TLS
- **Local publishing**: Publish crates locally without affecting the real crates.io
- **Seamless fallback**: Local crates are served first, with automatic fallback to crates.io
- **HTTP proxy mode**: Works with `CARGO_HTTP_PROXY` for easy integration

## Use Case: Dry-Run Publishing with Dependencies

When you have multiple crates where one depends on another, testing the publish workflow can be challenging. You can't publish crate B that depends on crate A until crate A is actually published to crates.io.

This proxy solves that problem by letting you:

1. Publish crate A locally to the proxy
2. Publish crate B (which depends on A) locally to the proxy
3. Build projects that depend on either crate, with the proxy serving your local versions while proxying everything else from crates.io

## Installation

```bash
cargo install --path .
```

## Usage

### Start the Proxy with HTTP Proxy Mode

```bash
cargo-proxy-registry \
  --port 8080 \
  --http-proxy-port 8081 \
  --ca-cert-out ./ca-cert.pem \
  --registry-path ./my-registry
```

Options:
- `--port` / `-p`: Port for the registry server (default: 8080)
- `--host` / `-H`: Host to bind to (default: 0.0.0.0)
- `--registry-path` / `-r`: Directory to store published crates and index (default: ./local-registry)
- `--base-url` / `-b`: Base URL for the registry (default: http://{host}:{port})
- `--http-proxy-port`: Port for the HTTP forward proxy (enables MITM interception)
- `--ca-cert-out`: Export CA certificate for trusting the proxy's TLS certificates
- `--tls`: Enable HTTPS with self-signed certificate
- `--tls-cert`: Path to TLS certificate file (PEM format)
- `--tls-key`: Path to TLS private key file (PEM format)
- `--upstream-index`: Upstream registry index URL (default: https://index.crates.io)
- `--upstream-api`: Upstream registry API URL (default: https://crates.io)

### Configure Cargo to Use the Proxy

Set these environment variables before running cargo commands:

```bash
export CARGO_HTTP_PROXY="http://127.0.0.1:8081"
export CARGO_HTTP_CAINFO="./ca-cert.pem"
```

Now `cargo build` will route all HTTP/HTTPS traffic through the proxy. The proxy intercepts crates.io traffic and serves local crates when available.

### Publish a Crate Locally

Publish directly to the proxy's index:

```bash
cargo publish \
  --index "sparse+http://127.0.0.1:8080/" \
  --token dummy \
  --allow-dirty
```

## Example: Publishing Dependent Crates

Suppose you have two crates:
- `my-core` (v0.1.0) - a library
- `my-app` (v0.1.0) - depends on `my-core = "0.1"`

### Step 1: Start the proxy

```bash
cargo-proxy-registry \
  --port 8080 \
  --http-proxy-port 8081 \
  --ca-cert-out ./ca-cert.pem \
  --registry-path ./test-registry
```

### Step 2: Set up environment

```bash
export CARGO_HTTP_PROXY="http://127.0.0.1:8081"
export CARGO_HTTP_CAINFO="./ca-cert.pem"
```

### Step 3: Publish `my-core` locally

```bash
cd my-core
cargo publish --index "sparse+http://127.0.0.1:8080/" --token dummy --allow-dirty
```

### Step 4: Publish `my-app` locally

```bash
cd my-app
cargo publish --index "sparse+http://127.0.0.1:8080/" --token dummy --allow-dirty
```

### Step 5: Verify the workflow

Create a test project that depends on `my-app`:

```bash
mkdir test-project && cd test-project
cargo init

# Add dependency (no special registry annotation needed!)
echo 'my-app = "0.1"' >> Cargo.toml

# Build - the proxy intercepts crates.io requests and serves local versions
cargo build
```

The proxy will serve `my-app` and `my-core` from your local registry, and fetch any other dependencies from the real crates.io.

## How It Works

### Registry Server

The proxy implements the [Cargo Registry HTTP API](https://doc.rust-lang.org/cargo/reference/registry-web-api.html):

- **Index requests** (`/config.json`, `/{first-two}/{second-two}/{crate}`): Merges local index with upstream crates.io index
- **Download requests** (`/api/v1/crates/{name}/{version}/download`): Serves local crates if available, otherwise proxies to crates.io
- **Publish requests** (`/api/v1/crates/new`): Stores crates locally in the registry directory

### HTTP Proxy with MITM TLS

When `--http-proxy-port` is specified, the proxy also runs an HTTP forward proxy that:

1. **HTTP requests**: Intercepts and rewrites crates.io URLs to route through the registry server
2. **HTTPS requests (CONNECT)**: Performs MITM TLS interception for `*.crates.io` domains, generating certificates on-the-fly signed by the proxy's CA
3. **Other HTTPS**: Passes through directly without interception

Use `CARGO_HTTP_CAINFO` to make cargo trust the proxy's CA certificate for intercepted HTTPS connections.

### Storage

Published crates are stored in:
- `{registry-path}/crates/{name}/{version}.crate` - the .crate files
- `{registry-path}/index/{path}/{name}` - the index entries (one JSON line per version)

## License

Apache-2.0

