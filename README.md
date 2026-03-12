# cargo-overlay-registry

A read-write overlay layer for cargo registries. Like an overlay filesystem, it provides a writable local layer on top of a read-only upstream registry (e.g., crates.io). Publishes go to the local layer, while reads fall through to the upstream when not found locally.

## How It Works

```
┌─────────────────────────────────────────┐
│            cargo build/publish          │
└─────────────────┬───────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────┐
│         Overlay Registry (local)        │  ◄── Writes go here
│    - Stores published crates locally    │
│    - Merges index with upstream         │
└─────────────────┬───────────────────────┘
                  │ (fallback)
                  ▼
┌─────────────────────────────────────────┐
│      Upstream Registry (crates.io)      │  ◄── Read-only
└─────────────────────────────────────────┘
```

- **Publish**: Crates are stored in the local overlay
- **Download**: Local crates are served first; missing crates fall through to upstream
- **Index**: Local and upstream indexes are merged transparently

## Use Case: Dry-Run Publishing

Testing multi-crate publish workflows is challenging — you can't publish crate B that depends on crate A until A is actually on crates.io. The overlay solves this by capturing publishes locally while still resolving real dependencies from upstream.

## Installation

```bash
cargo install --path .
```

## Quick Start

### 1. Start the overlay

```bash
cargo-overlay-registry --ca-cert-out ./ca-cert.pem
```

### 2. Configure cargo

```bash
export CARGO_HTTP_PROXY="http://127.0.0.1:8080"
export CARGO_HTTP_CAINFO="./ca-cert.pem"
export CARGO_REGISTRY_TOKEN=dummy
```

### 3. Use cargo normally

```bash
# Builds use local crates + upstream fallback
cargo build

# Publishes go to the local overlay
cargo publish --allow-dirty
```

## cargo publish-dry-run

For quick dry-run testing, use the `cargo-publish-dry-run` binary. It automatically starts a temporary overlay registry, configures cargo, runs `cargo publish`, and cleans up:

```bash
# Install
cargo install --path . --bin cargo-publish-dry-run

# Use as a cargo subcommand
cargo publish-dry-run --allow-dirty

# Or run directly
cargo-publish-dry-run --allow-dirty

# Publish a workspace
cargo publish-dry-run --workspace --allow-dirty
```

All arguments are forwarded to `cargo publish`. Metadata validation is enforced (same as crates.io), so you can verify your crate meets publishing requirements before actually publishing.

## Options

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--port` | `-p` | 8080 | Server port (registry + proxy) |
| `--host` | `-H` | 0.0.0.0 | Host to bind to |
| `--registry-path` | `-r` | (temp dir) | Local overlay storage |
| `--no-proxy` | | | Disable proxy mode |
| `--ca-cert-out` | | | Export CA certificate for HTTPS interception |
| `--upstream-index` | | https://index.crates.io | Upstream index URL |
| `--upstream-api` | | https://crates.io | Upstream API URL |
| `--permissive-publishing` | | | Skip crates.io metadata validation |
| `--tls` | | | Enable HTTPS (use with `CARGO_HTTP_PROXY=https://...`) |
| `--tls-cert` | | | TLS certificate file (PEM) |
| `--tls-key` | | | TLS private key file (PEM) |

## Example: Publishing Dependent Crates

```bash
# Start the overlay
cargo-overlay-registry --ca-cert-out ./ca-cert.pem

# In another terminal, configure cargo
export CARGO_HTTP_PROXY="http://127.0.0.1:8080"
export CARGO_HTTP_CAINFO="./ca-cert.pem"
export CARGO_REGISTRY_TOKEN=dummy

# Publish my-core (stored locally, not on crates.io)
cd my-core
cargo publish --allow-dirty

# Publish my-app which depends on my-core
# The overlay serves my-core from the local layer
cd ../my-app
cargo publish --allow-dirty

# Build a project that uses my-app
# The overlay serves my-app and my-core locally,
# fetches other dependencies from crates.io
cd ../test-project
cargo build
```

## Technical Details

### MITM TLS Interception

The overlay intercepts HTTPS traffic to crates.io domains using MITM TLS:
- Generates certificates on-the-fly signed by the overlay's CA
- Non-registry traffic passes through unmodified
- Use `CARGO_HTTP_CAINFO` to trust the CA certificate

### Storage Layout

```
{registry-path}/
├── crates/{name}/{version}.crate    # Published .crate files
└── index/{path}/{name}              # Index entries (JSON lines)
```

### Registry API

Implements the [Cargo Registry HTTP API](https://doc.rust-lang.org/cargo/reference/registry-web-api.html):
- `GET /config.json` — Registry configuration
- `GET /{first-two}/{second-two}/{crate}` — Index lookup (merged with upstream)  
- `GET /api/v1/crates/{name}/{version}/download` — Crate download (local-first)
- `PUT /api/v1/crates/new` — Publish (stored locally)

## License

Apache-2.0
