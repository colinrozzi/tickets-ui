//! tickets-ui: server-rendered HTML UI on top of tickets-acceptor's API.
//!
//! Single actor. Binds a TCP listener at init, then for each inbound
//! connection reads one HTTP request, routes it, builds an HTML response,
//! and closes — the actor stays up to serve the next connection.
//!
//! Wire shape (write path) per design §3: HTTP POST over loopback to the
//! tickets-acceptor API on `api_addr` (plaintext; tickets-acceptor is on
//! 127.0.0.1:8443 with no TLS), Authorization: Bearer <api_token>.
//!
//! Wire shape (read path): TBD. The original v0 design said reads go
//! straight to the shared `tickets` store; tickets-dev's wire-format reply
//! surfaced that the store is one opaque blob owned by the backend, and
//! manager has asked Colin whether to flip the read path to
//! GET /v1/tickets over loopback. The handlers in this scaffold call
//! `load_tickets()` (currently stubbed) so flipping is one function-body
//! swap when the decision lands.
//!
//! Initial state (JSON in Value::String):
//!   {
//!     "api_addr":    "127.0.0.1:8443",   // tickets-acceptor API
//!     "api_token":   "<bearer>",         // for outbound writes
//!     "listen_addr": "127.0.0.1:8081"    // optional; this default
//!   }

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, GraphValue, Value};
use serde::Deserialize;

packr_guest::setup_guest!();

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8081";

#[derive(Clone, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct UiState {
    pub listener_id: String,
    pub api_addr: String,
    pub api_token: String,
}

pack_types! {
    imports {
        theater:simple/runtime {
            log: func(msg: string),
        }
        theater:simple/tcp {
            listen: func(address: string) -> result<string, string>,
            receive: func(connection-id: string, max-bytes: u32) -> result<list<u8>, string>,
            send: func(connection-id: string, data: list<u8>) -> result<u64, string>,
            close: func(connection-id: string) -> result<_, string>,
        }
    }
    exports {
        theater:simple/actor.init: func(state: value) -> result<ui-state, string>,
        theater:simple/tcp-client.handle-connection: func(state: ui-state, connection-id: string) -> result<ui-state, string>,
    }
}

#[import(module = "theater:simple/runtime", name = "log")]
fn log(msg: String);

#[import(module = "theater:simple/tcp", name = "listen")]
fn tcp_listen(address: String) -> Result<String, String>;

#[import(module = "theater:simple/tcp", name = "receive")]
fn tcp_receive(connection_id: String, max_bytes: u32) -> Result<Vec<u8>, String>;

#[import(module = "theater:simple/tcp", name = "send")]
fn tcp_send(connection_id: String, data: Vec<u8>) -> Result<u64, String>;

#[import(module = "theater:simple/tcp", name = "close")]
fn tcp_close(connection_id: String) -> Result<(), String>;

#[derive(Deserialize)]
struct Config {
    api_addr: String,
    api_token: String,
    #[serde(default)]
    listen_addr: Option<String>,
}

// ============================================================================
// Actor entry points
// ============================================================================

#[export(name = "theater:simple/actor.init")]
fn init(state: Value) -> Result<(UiState, ()), String> {
    log(String::from("[tickets-ui] init"));

    let raw = match state {
        Value::String(s) if !s.is_empty() => s,
        _ => {
            return Err(String::from(
                "tickets-ui needs initial_state as a non-empty JSON string \
                 ({api_addr, api_token, listen_addr?})",
            ))
        }
    };

    let cfg: Config = serde_json::from_str(&raw)
        .map_err(|e| format!("initial_state is not valid JSON Config: {}", e))?;

    if cfg.api_addr.is_empty() {
        return Err(String::from("api_addr must be non-empty"));
    }
    if cfg.api_token.is_empty() {
        return Err(String::from("api_token must be non-empty"));
    }

    let listen_addr = cfg
        .listen_addr
        .unwrap_or_else(|| String::from(DEFAULT_LISTEN_ADDR));

    let listener_id = tcp_listen(listen_addr.clone())
        .map_err(|e| format!("listen on {} failed: {}", listen_addr, e))?;
    log(format!(
        "[tickets-ui] HTTP listening on {} (id={})",
        listen_addr, listener_id
    ));

    Ok((
        UiState {
            listener_id,
            api_addr: cfg.api_addr,
            api_token: cfg.api_token,
        },
        (),
    ))
}

#[export(name = "theater:simple/tcp-client.handle-connection")]
fn handle_connection(
    state: UiState,
    connection_id: String,
) -> Result<(UiState, ()), String> {
    // Always return Ok — a single bad request must not kill the actor (which
    // would tear down the entire supervision subtree). Log + serve the
    // canned 500 + carry on.
    if let Err(e) = try_handle(&state, &connection_id) {
        log(format!(
            "[tickets-ui] handle-connection failed (conn={}): {}",
            connection_id, e
        ));
        let _ = tcp_send(connection_id.clone(), canned_500());
        let _ = tcp_close(connection_id);
    }
    Ok((state, ()))
}

fn try_handle(state: &UiState, connection_id: &str) -> Result<(), String> {
    let request = tcp_receive(connection_id.to_string(), 65536)
        .map_err(|e| format!("receive: {}", e))?;
    let response = route(state, &request);
    tcp_send(connection_id.to_string(), response)
        .map_err(|e| format!("send: {}", e))?;
    tcp_close(connection_id.to_string())
        .map_err(|e| format!("close: {}", e))?;
    Ok(())
}

// ============================================================================
// Routing
// ============================================================================

fn route(_state: &UiState, request: &[u8]) -> Vec<u8> {
    let request_str = match core::str::from_utf8(request) {
        Ok(s) => s,
        Err(_) => return http_response(400, "text/plain", b"bad request\n".to_vec()),
    };

    let request_line = request_str.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");
    let (path, _query) = match raw_path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (raw_path, ""),
    };

    match (method, path) {
        ("GET", "/healthz") => http_response(200, "text/plain", b"ok\n".to_vec()),
        ("GET", "/static/style.css") => http_response(
            200,
            "text/css; charset=utf-8",
            STYLE_CSS.as_bytes().to_vec(),
        ),

        ("GET", "/") => render_list_view(),
        ("GET", "/new") => render_new_view(),
        ("GET", p) if p.starts_with("/t/") => {
            let rest = &p["/t/".len()..];
            if rest.is_empty() || rest.contains('/') {
                return not_found();
            }
            render_detail_view(rest)
        }

        // Write paths — stubbed in the scaffold. Real handlers will POST
        // to {api_addr}/v1/tickets[/...] using state.api_token and 303 back
        // to the appropriate view.
        ("POST", "/new") => stub_post("create ticket", "/"),
        ("POST", p) if p.starts_with("/t/") && p.ends_with("/comments") => {
            let id_str = &p["/t/".len()..p.len() - "/comments".len()];
            if id_str.is_empty() || id_str.contains('/') {
                return not_found();
            }
            stub_post("add comment", &format!("/t/{}", id_str))
        }
        ("POST", p) if p.starts_with("/t/") && p.ends_with("/status") => {
            let id_str = &p["/t/".len()..p.len() - "/status".len()];
            if id_str.is_empty() || id_str.contains('/') {
                return not_found();
            }
            stub_post("set status", &format!("/t/{}", id_str))
        }

        _ => not_found(),
    }
}

// ============================================================================
// View renderers — stub HTML for v0 scaffold.
//
// These currently render placeholder bodies. Once Colin's call lands on the
// read-path question (store-direct vs GET /v1/tickets over loopback), the
// load_tickets() helper below gets its implementation and these renderers
// stop being stubs.
// ============================================================================

fn render_list_view() -> Vec<u8> {
    let body = page(
        "tickets",
        "<h1>tickets</h1>\
         <p class=\"placeholder\">List view — pending data wiring.</p>\
         <p><a href=\"/new\">+ new ticket</a></p>",
    );
    http_response(200, "text/html; charset=utf-8", body.into_bytes())
}

fn render_detail_view(id_str: &str) -> Vec<u8> {
    let body = page(
        &format!("ticket #{}", html_escape(id_str)),
        &format!(
            "<p><a href=\"/\">&larr; back</a></p>\
             <h1>ticket #{}</h1>\
             <p class=\"placeholder\">Detail view — pending data wiring.</p>",
            html_escape(id_str)
        ),
    );
    http_response(200, "text/html; charset=utf-8", body.into_bytes())
}

fn render_new_view() -> Vec<u8> {
    let body = page(
        "new ticket",
        "<p><a href=\"/\">&larr; back</a></p>\
         <h1>new ticket</h1>\
         <form method=\"post\" action=\"/new\">\
           <label>title<br><input name=\"title\" required></label><br>\
           <label>body<br><textarea name=\"body\" rows=\"6\"></textarea></label><br>\
           <label>reporter<br><input name=\"reporter\" required></label><br>\
           <label>assignee<br><input name=\"assignee\" required></label><br>\
           <button type=\"submit\">create</button>\
         </form>",
    );
    http_response(200, "text/html; charset=utf-8", body.into_bytes())
}

fn stub_post(action: &str, redirect_to: &str) -> Vec<u8> {
    log(format!(
        "[tickets-ui] stub: {} -> would redirect to {}",
        action, redirect_to
    ));
    redirect_303(redirect_to)
}

fn not_found() -> Vec<u8> {
    let body = page("not found", "<h1>not found</h1><p><a href=\"/\">home</a></p>");
    http_response(404, "text/html; charset=utf-8", body.into_bytes())
}

// ============================================================================
// HTML helpers
// ============================================================================

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\
         <html lang=\"en\">\
         <head>\
         <meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title>\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         </head>\
         <body><main>{}</main></body>\
         </html>",
        html_escape(title),
        body,
    )
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ============================================================================
// HTTP response helpers
// ============================================================================

fn http_response(status: u16, content_type: &str, body: Vec<u8>) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        303 => "See Other",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(&body);
    out
}

fn redirect_303(location: &str) -> Vec<u8> {
    let header = format!(
        "HTTP/1.1 303 See Other\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        location
    );
    header.into_bytes()
}

fn canned_500() -> Vec<u8> {
    let body = b"<!doctype html><title>error</title><h1>internal error</h1>".to_vec();
    http_response(500, "text/html; charset=utf-8", body)
}

// ============================================================================
// Embedded static assets
// ============================================================================

const STYLE_CSS: &str = include_str!("../static/style.css");
