//! Cargo Overlay Registry - A proxy registry for crates.io with local publishing support.
//!
//! This library provides the core functionality for running a cargo registry proxy that:
//! - Proxies requests to crates.io (or another upstream registry)
//! - Supports local publishing of crates
//! - Can act as an HTTP/HTTPS forward proxy for cargo
//! - Supports MITM interception for transparent proxying

use std::sync::Arc;

use axum::routing::{get, put};
use axum::Router;

pub mod endpoints;
pub mod http_proxy;
pub mod registry;
pub mod state;
pub mod tls;
pub mod types;

pub use endpoints::{
    handle_api_download, handle_api_publish, handle_api_search, handle_config, handle_index_1char,
    handle_index_2char, handle_index_3char, handle_index_4plus,
};
pub use http_proxy::{handle_proxy_connection, HttpProxyState};
pub use state::{MitmCa, ProxyState};
pub use tls::generate_self_signed_cert;

/// Build the standard registry router with all endpoints configured.
pub fn build_registry_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        // Index config endpoint
        .route("/config.json", get(handle_config))
        // Index files for 1-char package names: /1/{name}
        .route("/1/{name}", get(handle_index_1char))
        // Index files for 2-char package names: /2/{name}
        .route("/2/{name}", get(handle_index_2char))
        // Index files for 3-char package names: /3/{first_char}/{name}
        .route("/3/{first_char}/{name}", get(handle_index_3char))
        // Index files for 4+ char package names: /{first_two}/{second_two}/{name}
        .route("/{first_two}/{second_two}/{name}", get(handle_index_4plus))
        // API: Search crates
        .route("/api/v1/crates", get(handle_api_search))
        // API: Publish crate
        .route("/api/v1/crates/new", put(handle_api_publish))
        // API: Download crate
        .route(
            "/api/v1/crates/{crate_name}/{version}/download",
            get(handle_api_download),
        )
        .with_state(state)
}
