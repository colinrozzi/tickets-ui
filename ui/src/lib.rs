//! tickets-ui: server-rendered HTML UI on top of tickets-acceptor's API.
//!
//! Single actor. Binds a TCP listener at init, then for each inbound
//! connection reads one HTTP request, routes it, builds an HTML response,
//! and closes — the actor stays up to serve the next connection.
//!
//! Wire shape (write path): HTTP POST over loopback to the tickets-acceptor
//! API on `api_addr` (plaintext; tickets-acceptor is on 127.0.0.1:8443 with
//! no TLS), Authorization: Bearer <api_token>. Endpoints + JSON shapes per
//! tickets-dev's wire-format reply 2026-06-05.
//!
//! Wire shape (read path): same transport as writes — HTTP GET over
//! loopback against the tickets API (`GET /v1/tickets`,
//! `GET /v1/tickets/<id>`). Flip from store-direct was signed off by
//! Colin 2026-06-05 (manager id=14).
//!
//! Initial state (JSON in Value::String):
//!   {
//!     "api_addr":    "127.0.0.1:8443",   // tickets-acceptor API
//!     "api_token":   "<bearer>",         // for outbound writes
//!     "listen_addr": "127.0.0.1:9444"    // optional; this default
//!   }

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, GraphValue, Value};
use serde::{Deserialize, Serialize};

packr_guest::setup_guest!();

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:9444";
const VALID_STATUSES: &[&str] = &["open", "in-progress", "done", "closed"];

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
            connect: func(address: string) -> result<string, string>,
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

#[import(module = "theater:simple/tcp", name = "connect")]
fn tcp_connect(address: String) -> Result<String, String>;

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
// API request/response shapes — must match tickets-handler's serde structs
// (ticket-handler/src/lib.rs:100-152).
// ============================================================================

#[derive(Deserialize)]
struct Ticket {
    id: u64,
    title: String,
    body: String,
    reporter: String,
    assignee: String,
    status: String,
    created_at: u64,
    #[serde(default)]
    comments: Vec<Comment>,
}

#[derive(Deserialize)]
struct Comment {
    author: String,
    body: String,
    created_at: u64,
}

#[derive(Deserialize)]
struct TicketsList {
    tickets: Vec<Ticket>,
}

#[derive(Serialize)]
struct NewTicketBody<'a> {
    title: &'a str,
    body: &'a str,
    reporter: &'a str,
    assignee: &'a str,
}

#[derive(Serialize)]
struct NewCommentBody<'a> {
    author: &'a str,
    body: &'a str,
}

#[derive(Serialize)]
struct SetStatusBody<'a> {
    status: &'a str,
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

fn route(state: &UiState, request: &[u8]) -> Vec<u8> {
    let request_str = match core::str::from_utf8(request) {
        Ok(s) => s,
        Err(_) => return http_response(400, "text/plain", b"bad request\n".to_vec()),
    };

    let request_line = request_str.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");
    let (path, query) = match raw_path.split_once('?') {
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

        ("GET", "/") => render_list_view(state, query),
        ("GET", "/new") => render_new_view(),
        ("GET", p) if p.starts_with("/t/") => {
            let rest = &p["/t/".len()..];
            if rest.is_empty() || rest.contains('/') {
                return not_found();
            }
            render_detail_view(state, rest)
        }

        ("POST", "/new") => handle_create(state, request_str),
        ("POST", p) if p.starts_with("/t/") && p.ends_with("/comments") => {
            let id_str = &p["/t/".len()..p.len() - "/comments".len()];
            if id_str.is_empty() || id_str.contains('/') {
                return not_found();
            }
            handle_add_comment(state, id_str, request_str)
        }
        ("POST", p) if p.starts_with("/t/") && p.ends_with("/status") => {
            let id_str = &p["/t/".len()..p.len() - "/status".len()];
            if id_str.is_empty() || id_str.contains('/') {
                return not_found();
            }
            handle_set_status(state, id_str, request_str)
        }

        _ => not_found(),
    }
}

// ============================================================================
// Write handlers — parse form body, call the tickets API, 303 on success,
// render an error page on failure.
// ============================================================================

fn handle_create(state: &UiState, request_str: &str) -> Vec<u8> {
    let form = match extract_form(request_str) {
        Ok(f) => f,
        Err(msg) => return render_error(400, "bad form body", &msg),
    };

    let title = form_get(&form, "title").unwrap_or("");
    let body_text = form_get(&form, "body").unwrap_or("");
    let reporter = form_get(&form, "reporter").unwrap_or("");
    let assignee = form_get(&form, "assignee").unwrap_or("");

    if title.is_empty() || reporter.is_empty() || assignee.is_empty() {
        return render_error(
            400,
            "missing required fields",
            "title, reporter, and assignee are required.",
        );
    }

    let json = match serde_json::to_string(&NewTicketBody {
        title,
        body: body_text,
        reporter,
        assignee,
    }) {
        Ok(s) => s,
        Err(e) => return render_error(500, "encode failed", &e.to_string()),
    };

    match api_post(state, "/v1/tickets", &json) {
        Ok((status, _body)) if (200..300).contains(&status) => redirect_303("/"),
        Ok((status, body)) => render_api_error(status, &body),
        Err(e) => render_error(502, "upstream unavailable", &e),
    }
}

fn handle_add_comment(state: &UiState, id_str: &str, request_str: &str) -> Vec<u8> {
    let id: u64 = match id_str.parse() {
        Ok(n) => n,
        Err(_) => return render_error(400, "bad ticket id", id_str),
    };

    let form = match extract_form(request_str) {
        Ok(f) => f,
        Err(msg) => return render_error(400, "bad form body", &msg),
    };

    let author = form_get(&form, "author").unwrap_or("");
    let body_text = form_get(&form, "body").unwrap_or("");

    if author.is_empty() || body_text.is_empty() {
        return render_error(
            400,
            "missing required fields",
            "author and body are required.",
        );
    }

    let json = match serde_json::to_string(&NewCommentBody {
        author,
        body: body_text,
    }) {
        Ok(s) => s,
        Err(e) => return render_error(500, "encode failed", &e.to_string()),
    };

    let path = format!("/v1/tickets/{}/comment", id);
    match api_post(state, &path, &json) {
        Ok((status, _body)) if (200..300).contains(&status) => {
            redirect_303(&format!("/t/{}", id))
        }
        Ok((status, body)) => render_api_error(status, &body),
        Err(e) => render_error(502, "upstream unavailable", &e),
    }
}

fn handle_set_status(state: &UiState, id_str: &str, request_str: &str) -> Vec<u8> {
    let id: u64 = match id_str.parse() {
        Ok(n) => n,
        Err(_) => return render_error(400, "bad ticket id", id_str),
    };

    let form = match extract_form(request_str) {
        Ok(f) => f,
        Err(msg) => return render_error(400, "bad form body", &msg),
    };

    let status = form_get(&form, "status").unwrap_or("");
    if !VALID_STATUSES.iter().any(|s| *s == status) {
        return render_error(
            400,
            "bad status",
            "valid values: open, in-progress, done, closed",
        );
    }

    let json = match serde_json::to_string(&SetStatusBody { status }) {
        Ok(s) => s,
        Err(e) => return render_error(500, "encode failed", &e.to_string()),
    };

    let path = format!("/v1/tickets/{}/status", id);
    match api_post(state, &path, &json) {
        Ok((http_status, _body)) if (200..300).contains(&http_status) => {
            redirect_303(&format!("/t/{}", id))
        }
        Ok((http_status, body)) => render_api_error(http_status, &body),
        Err(e) => render_error(502, "upstream unavailable", &e),
    }
}

// ============================================================================
// View renderers — read-side stays placeholder until the read-path-flip lands.
// ============================================================================

fn render_list_view(state: &UiState, query: &str) -> Vec<u8> {
    let tickets = match load_tickets(state, query) {
        Ok(t) => t,
        Err(e) => return render_error(502, "couldn't load tickets", &e),
    };

    let (status_filter, assignee_filter) = parse_list_filters(query);
    let filter_form = format!(
        "<form method=\"get\" action=\"/\" class=\"filters\">\
           <label>status<br>\
             <select name=\"status\">\
               <option value=\"\">any</option>\
               {status_options}\
             </select>\
           </label>\
           <label>assignee<br><input name=\"assignee\" value=\"{assignee_val}\"></label>\
           <button type=\"submit\">filter</button>\
         </form>",
        status_options = status_options_html(status_filter.as_deref()),
        assignee_val = html_escape(assignee_filter.as_deref().unwrap_or("")),
    );

    let rows = if tickets.is_empty() {
        String::from("<tr><td colspan=\"5\" class=\"placeholder\">no tickets match</td></tr>")
    } else {
        let mut s = String::new();
        for t in &tickets {
            s.push_str(&format!(
                "<tr>\
                   <td>#{}</td>\
                   <td><a href=\"/t/{id}\">{title}</a></td>\
                   <td>{status}</td>\
                   <td>{assignee}</td>\
                   <td>{reporter}</td>\
                 </tr>",
                t.id,
                id = t.id,
                title = html_escape(&t.title),
                status = html_escape(&t.status),
                assignee = html_escape(&t.assignee),
                reporter = html_escape(&t.reporter),
            ));
        }
        s
    };

    let body = page(
        "tickets",
        &format!(
            "<h1>tickets</h1>\
             {filter_form}\
             <table>\
               <thead><tr><th>id</th><th>title</th><th>status</th><th>assignee</th><th>reporter</th></tr></thead>\
               <tbody>{rows}</tbody>\
             </table>\
             <p><a href=\"/new\">+ new ticket</a></p>",
            filter_form = filter_form,
            rows = rows,
        ),
    );
    http_response(200, "text/html; charset=utf-8", body.into_bytes())
}

fn render_detail_view(state: &UiState, id_str: &str) -> Vec<u8> {
    let id: u64 = match id_str.parse() {
        Ok(n) => n,
        Err(_) => return render_error(400, "bad ticket id", id_str),
    };

    let ticket = match load_ticket(state, id) {
        Ok(Some(t)) => t,
        Ok(None) => return not_found(),
        Err(e) => return render_error(502, "couldn't load ticket", &e),
    };

    let comment_action = format!("/t/{}/comments", id);
    let status_action = format!("/t/{}/status", id);

    let comments_html = if ticket.comments.is_empty() {
        String::from("<p class=\"placeholder\">no comments yet</p>")
    } else {
        let mut s = String::from("<ol class=\"thread\">");
        for c in &ticket.comments {
            s.push_str(&format!(
                "<li>\
                   <header><strong>{author}</strong> <span class=\"ts\">at {ts}</span></header>\
                   <p>{body}</p>\
                 </li>",
                author = html_escape(&c.author),
                ts = c.created_at,
                body = html_escape_pre(&c.body),
            ));
        }
        s.push_str("</ol>");
        s
    };

    let status_options = status_options_html(Some(&ticket.status));

    let body = page(
        &format!("ticket #{}: {}", ticket.id, &ticket.title),
        &format!(
            "<p><a href=\"/\">&larr; back</a></p>\
             <h1>#{id}: {title}</h1>\
             <dl class=\"meta\">\
               <dt>status</dt><dd>{status}</dd>\
               <dt>assignee</dt><dd>{assignee}</dd>\
               <dt>reporter</dt><dd>{reporter}</dd>\
               <dt>created</dt><dd>{created}</dd>\
             </dl>\
             <section><h2>body</h2><p>{body_text}</p></section>\
             <section>\
               <h2>comments</h2>\
               {comments_html}\
               <h3>add comment</h3>\
               <form method=\"post\" action=\"{comment_action}\">\
                 <label>author<br><input name=\"author\" required></label>\
                 <label>body<br><textarea name=\"body\" rows=\"4\" required></textarea></label>\
                 <button type=\"submit\">add</button>\
               </form>\
             </section>\
             <section>\
               <h2>set status</h2>\
               <form method=\"post\" action=\"{status_action}\">\
                 <label>status<br>\
                   <select name=\"status\">{status_options}</select>\
                 </label>\
                 <button type=\"submit\">update</button>\
               </form>\
             </section>",
            id = ticket.id,
            title = html_escape(&ticket.title),
            status = html_escape(&ticket.status),
            assignee = html_escape(&ticket.assignee),
            reporter = html_escape(&ticket.reporter),
            created = ticket.created_at,
            body_text = html_escape_pre(&ticket.body),
            comments_html = comments_html,
            comment_action = comment_action,
            status_action = status_action,
            status_options = status_options,
        ),
    );
    http_response(200, "text/html; charset=utf-8", body.into_bytes())
}

fn parse_list_filters(query: &str) -> (Option<String>, Option<String>) {
    let mut status = None;
    let mut assignee = None;
    if query.is_empty() {
        return (status, assignee);
    }
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        let v = url_decode(v);
        if v.is_empty() {
            continue;
        }
        match k {
            "status" => status = Some(v),
            "assignee" => assignee = Some(v),
            _ => {}
        }
    }
    (status, assignee)
}

fn status_options_html(selected: Option<&str>) -> String {
    let mut out = String::new();
    for s in VALID_STATUSES {
        let sel = if selected == Some(*s) { " selected" } else { "" };
        out.push_str(&format!("<option value=\"{s}\"{sel}>{s}</option>", s = s, sel = sel));
    }
    out
}

fn render_new_view() -> Vec<u8> {
    let body = page(
        "new ticket",
        "<p><a href=\"/\">&larr; back</a></p>\
         <h1>new ticket</h1>\
         <form method=\"post\" action=\"/new\">\
           <label>title<br><input name=\"title\" required></label>\
           <label>body<br><textarea name=\"body\" rows=\"6\"></textarea></label>\
           <label>reporter<br><input name=\"reporter\" required></label>\
           <label>assignee<br><input name=\"assignee\" required></label>\
           <button type=\"submit\">create</button>\
         </form>",
    );
    http_response(200, "text/html; charset=utf-8", body.into_bytes())
}

fn not_found() -> Vec<u8> {
    let body = page("not found", "<h1>not found</h1><p><a href=\"/\">home</a></p>");
    http_response(404, "text/html; charset=utf-8", body.into_bytes())
}

fn render_error(status: u16, title: &str, detail: &str) -> Vec<u8> {
    let body = page(
        title,
        &format!(
            "<h1>{}</h1>\
             <p>{}</p>\
             <p><a href=\"/\">home</a></p>",
            html_escape(title),
            html_escape(detail),
        ),
    );
    http_response(status, "text/html; charset=utf-8", body.into_bytes())
}

fn render_api_error(status: u16, body: &str) -> Vec<u8> {
    render_error(
        status,
        "upstream returned an error",
        &format!("HTTP {} from tickets API: {}", status, body),
    )
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

/// Like `html_escape` but turns newlines into `<br>` so multi-line ticket
/// bodies and comments render with their line breaks intact (we're not
/// running these through markdown for v0 — explicit scope cut).
fn html_escape_pre(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
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
        502 => "Bad Gateway",
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
// HTTP/1.1 client for the tickets API.
// Plaintext (tickets-acceptor is 127.0.0.1:8443 with no TLS — phase 1
// deferred TLS to a reverse proxy that isn't here yet). Bearer auth.
// Lifted directly from tickets-handler/src/lib.rs:613-683.
// ============================================================================

fn api_post(state: &UiState, path: &str, body: &str) -> Result<(u16, String), String> {
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        path,
        state.api_addr,
        state.api_token,
        body.len(),
        body
    );
    api_request(state, req)
}

fn api_get(state: &UiState, path: &str) -> Result<(u16, String), String> {
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
        path, state.api_addr, state.api_token,
    );
    api_request(state, req)
}

fn api_request(state: &UiState, req: String) -> Result<(u16, String), String> {
    let conn = tcp_connect(state.api_addr.clone())
        .map_err(|e| format!("connect {}: {}", state.api_addr, e))?;
    tcp_send(conn.clone(), req.into_bytes()).map_err(|e| format!("send: {}", e))?;

    let mut all = Vec::new();
    let mut body_start: Option<usize> = None;
    let mut content_length: Option<usize> = None;

    loop {
        if let (Some(hs), Some(cl)) = (body_start, content_length) {
            if all.len() >= hs + cl {
                break;
            }
        }
        let chunk = match tcp_receive(conn.clone(), 65536) {
            Ok(c) => c,
            Err(_) => break,
        };
        if chunk.is_empty() {
            break;
        }
        all.extend_from_slice(&chunk);

        if body_start.is_none() {
            if let Some(idx) = find_subseq(&all, b"\r\n\r\n") {
                body_start = Some(idx + 4);
                let header_str = core::str::from_utf8(&all[..idx]).unwrap_or("");
                for line in header_str.split("\r\n") {
                    if let Some((name, value)) = line.split_once(':') {
                        if name.trim().eq_ignore_ascii_case("content-length") {
                            if let Ok(n) = value.trim().parse::<usize>() {
                                content_length = Some(n);
                            }
                        }
                    }
                }
                if content_length.is_none() {
                    content_length = Some(usize::MAX);
                }
            }
        }
    }
    let _ = tcp_close(conn);

    let text = String::from_utf8(all).map_err(|_| String::from("non-utf8 response"))?;
    let status = parse_status_line(&text).unwrap_or(0);
    let start = body_start.unwrap_or_else(|| text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0));
    let end = match content_length {
        Some(n) if n != usize::MAX => start + n.min(text.len().saturating_sub(start)),
        _ => text.len(),
    };
    Ok((status, text[start..end].to_string()))
}

fn load_tickets(state: &UiState, query: &str) -> Result<Vec<Ticket>, String> {
    let path = if query.is_empty() {
        String::from("/v1/tickets")
    } else {
        // Whatever the browser sent (status=…&assignee=…) maps 1:1 onto
        // the API's filter params; pass it through unchanged.
        format!("/v1/tickets?{}", query)
    };
    let (status, body) = api_get(state, &path)?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {} from tickets API: {}", status, body));
    }
    let list: TicketsList = serde_json::from_str(&body)
        .map_err(|e| format!("deserialize tickets list: {} (body: {})", e, body))?;
    Ok(list.tickets)
}

fn load_ticket(state: &UiState, id: u64) -> Result<Option<Ticket>, String> {
    let path = format!("/v1/tickets/{}", id);
    let (status, body) = api_get(state, &path)?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {} from tickets API: {}", status, body));
    }
    let ticket: Ticket = serde_json::from_str(&body)
        .map_err(|e| format!("deserialize ticket: {} (body: {})", e, body))?;
    Ok(Some(ticket))
}

fn parse_status_line(text: &str) -> Option<u16> {
    let line = text.lines().next()?;
    let mut parts = line.split_ascii_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse().ok()
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ============================================================================
// Form-urlencoded parsing (browser <form method=post>).
// ============================================================================

fn extract_form(request_str: &str) -> Result<Vec<(String, String)>, String> {
    let body_start = request_str
        .find("\r\n\r\n")
        .ok_or_else(|| String::from("no request body"))?;
    let body = &request_str[body_start + 4..];

    // Trim any padding past Content-Length — if the client kept the
    // connection alive we'd see following bytes here. Single-shot recv
    // makes this mostly theoretical for v0 form posts but be defensive.
    let body = body.trim_end_matches(|c: char| c == '\0');

    let mut out = Vec::new();
    if body.is_empty() {
        return Ok(out);
    }
    for pair in body.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.push((url_decode(k), url_decode(v)));
    }
    Ok(out)
}

fn form_get<'a>(form: &'a [(String, String)], name: &str) -> Option<&'a str> {
    form.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            let hi = hex_digit(bytes[i + 1]);
            let lo = hex_digit(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        }
        if b == b'+' {
            out.push(' ');
        } else {
            out.push(b as char);
        }
        i += 1;
    }
    out
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ============================================================================
// Embedded static assets
// ============================================================================

const STYLE_CSS: &str = include_str!("../static/style.css");
