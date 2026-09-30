# Node work packages ign1t10n depends on

ign1t10n always writes the configuration keys below; the node ignores keys it
does not know (its configuration structs do not deny unknown fields), so the
keys take effect as each package lands. `versions.toml [f1r3node].has` records
which the pinned revision contains.

## N1: bind addresses

`protocol-server.bind-address` and `peers-discovery.bind-address`, so that the
transport and discovery listen on 127.0.0.1 rather than 0.0.0.0. Until then
the supervisor's `lsof` audit reports the exposed sockets as Degraded
("listening off loopback") and the smoke job warns.

## N2: vendored OpenSSL

A `vendored-openssl` feature on the `crypto` crate (`openssl = { features =
["vendored"] }`), so the macOS binary links only system libraries. The release
workflow builds with `--features crypto/vendored-openssl`.

## N4: storage format

`/api/status` reports `storageFormat: u32`, bumped whenever an older node
could not open the data directory. ign1t10n records it at genesis and refuses
to start a shard whose format is not in `compat.toml`.

## N5: refuse foreign Origin (Decision 7)

`api-server.reject-foreign-origin` (default `false`). When true, the HTTP API
answers **403** to any request carrying an `Origin` header. Browsers attach
`Origin` to every cross-origin request (and to all POSTs), so pages in other
browsers cannot use the local shard; F1R3Gaze (`gaze-net`) and Embers
(`reqwest`) send none. CORS preflights (`OPTIONS` with `Origin`) are refused
too, so no `Access-Control-Allow-Origin` is ever granted.

Sketch against `node/src/rust/web/routes.rs` (revision fce422a):

```rust
// model.rs, struct ApiServer
#[serde(rename = "reject-foreign-origin", default)]
pub reject_foreign_origin: bool,

// routes.rs
use axum::{extract::Request, middleware::{self, Next}};

async fn refuse_foreign_origin(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return (StatusCode::FORBIDDEN, Json(json!({
            "error": "foreign_origin",
            "message": "this node does not serve browser pages from other origins"
        }))).into_response();
    }
    next.run(req).await
}

pub fn create_main_routes(reporting_enabled: bool, http_max_body_bytes: usize,
                          reject_foreign_origin: bool) -> Router<AppState> {
    // ... unchanged ...
    let router = router
        .layer(DefaultBodyLimit::max(http_max_body_bytes))
        .fallback(not_found_handler)
        .method_not_allowed_fallback(method_not_allowed_handler);
    if reject_foreign_origin {
        router.layer(middleware::from_fn(refuse_foreign_origin))  // outermost: before CORS
    } else {
        router.layer(cors)
    }
}
```

and the same for `create_admin_routes`, with `servers_instances.rs` passing
`conf.api_server.reject_foreign_origin`. The WebSocket `/ws/events` is covered:
a browser's WebSocket handshake carries `Origin`. Test: `curl -H 'Origin:
https://evil.example' http://127.0.0.1:40413/api/status` → 403; without the
header → 200.
