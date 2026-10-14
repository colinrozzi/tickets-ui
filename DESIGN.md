# tickets-ui — v0 design (refreshed to current deployed reality)

**Status:** proposal, awaiting sign-off from Colin. No UI code lands until this doc is merged.
**Refreshed:** 2026-10-01 — updated to the deployed-backend shape (:8456 loopback API, caddy-fronted), the fleet-services-supervisor deploy model, and the current fleet ABI (packr 0.24 / theater `d1a9f270`, graph wire v3). Supersedes the 2026-07 draft.

This is the design for the v0 web UI on top of the tickets actor system. It honors the architectural decisions locked in by manager + Colin (Theater-native wasm actor, supervisor-managed deploy, API-over-loopback for both reads and writes). What follows is the choices that were *not* locked, with justifications and explicit scope cuts — plus an appendix scoping the packr 0.11 → 0.24 rebuild the actor needs before any of this can ship.

## 0. What changed since the last draft

The architecture is intact; four facts about the surrounding fleet moved:

| Was (2026-07 draft) | Now (2026-10-01) |
|---|---|
| tickets API at `127.0.0.1:8445` | tickets backend deployed at **`127.0.0.1:8456`**, bearer-authed, **caddy-fronted**, on a dedicated fleet-services supervisor (linode) |
| Public exposure via a future **frontdoor SNI actor** (per-backend in-actor TLS) | Public exposure via **caddy** on the fleet-services box (TLS terminated by caddy, reverse-proxied to loopback). The UI is exposed the same way the backend already is. The frontdoor-actor story is retired for this deploy. |
| "sentinel spawns it as a sibling child" | **supervisor-managed actor** on the fleet-services supervisor. Manager runs the **initial deploy**, then hands me a **scoped push key**; thereafter I redeploy by **pushing** the wasm + sub-manifest over the supervisor's management port (push-spawn model). |
| packr 0.11.0 / theater `73a4540b` (0.3.9) | Current fleet ABI: **packr 0.24 / theater `d1a9f270`**, graph wire **v3**. The repo's last build is packr 0.11.0 and must take the full 0.24 cutover wave (see Appendix A). |

Everything in §1–§6 below is written against this current reality.

## 1. View inventory

Three screens. Nothing else in v0.

| Path | Purpose |
|---|---|
| `GET /` | Ticket list. Server-rendered table. Filters via query string: `?assignee=…&status=…`. No client-side sorting/filtering. |
| `GET /t/<id>` | Ticket detail. Header (id, title, body, status, assignee, created), comment thread in chronological order, comment-add form, status-transition control. |
| `GET /new` + `POST /new` | Compose a new ticket. Plain form: title, body, reporter, assignee. |

Plus `GET /healthz` → 200 (caddy / supervisor liveness probe) and `GET /static/style.css` (one embedded stylesheet).

**Comment add** and **status transition** post to dedicated paths (`POST /t/<id>/comments`, `POST /t/<id>/status`) and 303-redirect back to the detail view (post/redirect/get).

That is the entire URL surface for v0. No `/me`, no `/search`, no `/api/*` from the UI actor — the UI is a renderer, not an API.

## 2. Listener strategy

**Choice:** the UI actor binds its own loopback port (**`127.0.0.1:9445`** as deployed — `:9444` was already held by another theater process on the fleet-services box and crash-looped the unit, so the live deploy moved to `:9445`; `listen_addr` is manifest-configurable, so this was a one-line change, never hardcoded per-deploy), exposed publicly via **caddy** on the fleet-services supervisor box at a dedicated hostname (e.g. `tickets.colinrozzi.com` or `tickets-ui.colinrozzi.com` — Colin / supervisor-dev's call). The tickets backend stays on `127.0.0.1:8456`; the UI actor calls into it over loopback for every read and write.

**Tradeoff considered:**

|  | Separate port (chosen) | Sub-route of the backend's :8456 |
|---|---|---|
| Deploy independence | UI and backend release on their own cycles | Coupled — the backend would have to proxy to the UI or absorb HTML rendering |
| Surface separation | Backend stays a pure JSON API | Adds HTML rendering / routing to the API actor |
| Public exposure | One extra caddy route (same pattern the backend already uses) | Same caddy work either way |
| Auth boundary | UI holds the bearer server-side; browser never sees it | Same |

Separate port wins: deploy independence + a clean backend API surface stack, and the caddy route is one-time work that mirrors what the backend already has. The backend being loopback-only *reinforces* this — the UI is the browser-facing front, the backend stays private behind loopback + caddy.

**Exposure (current reality):** the fleet-services supervisor box runs **caddy** terminating TLS and reverse-proxying public hostnames to loopback backends. tickets-ui plugs in the same way: add a caddy site block for the UI hostname → `127.0.0.1:9445`. The UI actor itself binds **plain HTTP** on loopback — caddy owns TLS. No in-actor TLS, no SNI-peeking frontdoor actor for this deploy.

**Open coordination (not blocking sign-off):** hostname assignment + the caddy site block are a manager / supervisor-dev / Colin decision. v0 is reachable via SSH tunnel to `127.0.0.1:9445` before the caddy route lands; public HTTPS lands when the route is added. This doc takes no position on the exact hostname.

## 3. Wire shape — reads and writes

**Both reads and writes go through the tickets API over loopback** (plaintext HTTP to `127.0.0.1:8456`, `Authorization: Bearer <api_token>`). Decided 2026-06-05 (Colin sign-off on manager's flip); still the right call.

Why API-over-loopback for reads rather than reading the store directly:

- The `tickets` store holds one opaque blob (`serde_json::to_vec(&Vec<Ticket>)`) under a single label — a store-direct read deserializes the whole corpus and couples the UI to the backend's storage layout.
- The backend's roadmap may change that layout. API-over-loopback insulates the UI: one transport, one bearer, one error model, and no cutover handshake when the store shape changes.
- The price is one loopback round-trip per view — negligible.

### Reads (`GET` to `127.0.0.1:8456`, bearer)

| View | UI call | Upstream |
|---|---|---|
| List | `GET /` (optional `?assignee=…&status=…`) | `GET /v1/tickets[?assignee=…&status=…]` → `{tickets: [Ticket, …]}` |
| Detail | `GET /t/<id>` | `GET /v1/tickets/<id>` → `Ticket` (with `comments` inline) |

### Writes (`POST` to `127.0.0.1:8456`, bearer)

| UI route | Upstream | Body |
|---|---|---|
| `POST /new` | `POST /v1/tickets` | `{title, body, reporter, assignee}` |
| `POST /t/<id>/comments` | `POST /v1/tickets/<id>/comment` | `{author, body}` |
| `POST /t/<id>/status` | `POST /v1/tickets/<id>/status` | `{status}` |

All five carry the bearer. The actor loads `{api_addr, api_token, listen_addr?}` from its JSON `initial_state`; the browser never sees the token. Browsers submit plain `<form>`s, the actor turns them into authenticated API calls server-side, and 303-redirects back to the right view. No `/api` from the UI actor; no fetch/optimistic-update JS in v0.

> **Confirm with tickets-dev before the rebuild PR:** that the deployed backend on `:8456` keeps these exact paths + JSON shapes under packr 0.24 (the UI's serde structs mirror `ticket-handler`'s). The wire is JSON-over-HTTP, so the packr ABI of the two actors is independent — but the request/response *contract* must still match.

## 4. Actor decomposition

**Choice:** one wasm actor for v0.

It handles inbound HTTP, outbound API calls over loopback, and HTML rendering. Per-request state is tiny; the store (behind the API) is the persistence layer. A per-connection actor split is more theater-idiomatic for isolating session state, but v0 has none (single bearer, single shared user, no per-user prefs). Splitting one actor into two later is easier than collapsing two into one, so defer the split.

## 5. Framework / build

**Server-rendered HTML, vanilla JS (only where needed), hand-written CSS.** No SPA, no framework, no build step beyond `cargo` + `wasm32-unknown-unknown` through the nix flake.

| Concern | Choice | Why |
|---|---|---|
| HTML rendering | Rust `format!`/`write!` string templating (the v0 actor already does this) | ~3 screens doesn't justify a templating crate or its wasi-p2 compat risk. A `minijinja` swap stays an optional follow-up. |
| CSS | One hand-written `static/style.css`, embedded via `include_str!`, served at `GET /static/style.css` | No PostCSS / Tailwind for 3 screens. |
| JS | Vanilla, ideally zero — plain forms, full-page reload | Progressive enhancement is an explicit scope cut |
| Guest ABI | **packr-guest 0.24 + packr-abi 0.24 + theater-guest @ `d1a9f270`**, in-module-state (`StateCell`), plain cdylib | Matches the current fleet ABI. See Appendix A. |
| Build | `nix build` producing the wasm, release artifact `release-YYYYMMDD-<sha>` (wasm + sub-manifest TOML) | Standard fleet release shape; this is what the supervisor push-spawn consumes. |

**Design language / coordination with inbox-ui-dev:** mild divergence is fine for v0 (per CLAUDE.md); converge later. Once this doc is signed I'll compare notes with inbox-ui-dev on palette + CSS conventions and update where it's cheap. For the broader UX collaboration Colin wants (tickets + chat UIs together), I'll also sync with chat-dev.

## 6. Deploy model

The UI ships as a **supervisor-managed actor** on the dedicated fleet-services supervisor (linode), alongside the tickets backend.

- **Artifact:** a release tag `release-YYYYMMDD-<sha>` carrying the built wasm + a sub-manifest TOML (the shape tickets/inbox already use).
- **Initial deploy:** manager does the first deploy onto the supervisor and injects the real `initial_state` (`api_addr = 127.0.0.1:8456`, the shared bearer, `listen_addr = 127.0.0.1:9445`).
- **Thereafter:** manager hands me a **scoped push key**; I redeploy by **pushing** the wasm + config over the supervisor's management port (push-spawn — the bare box fetches nothing). No separate deploy pipeline.
- **Manifest:** `[[handler]] type = "self"` + `[[handler]] type = "tcp"`. No `supervisor` handler — the UI spawns no children. Secrets in `initial_state` are redacted from theater logs (runtime #222).
- **Exposure:** caddy site block on the fleet-services box → `127.0.0.1:9445` (see §2).

**Blocked-until pieces (coordination, not design):** (a) GitHub push credentials for this container so I can open the rebuild PR; (b) manager's initial deploy + the scoped push key; (c) hostname + caddy route. None of these block design sign-off.

## 7. What v0 is NOT

- **No authentication / user identity in the UI.** Single bearer held server-side; humans share the credential. Per-user auth is a separate design.
- **No search.** Filtering is `?assignee=…&status=…` on the list page only.
- **No attachments / uploads.** Plain text bodies + comments.
- **No realtime.** No WebSocket, no SSE. Refresh the page. (A later UX pass with chat-dev may revisit live updates.)
- **No edit / delete.** Status transitions are the only post-creation mutation.
- **No markdown rendering.** Plain text + line breaks (`\n` → `<br>`). `pulldown-cmark` is a one-line follow-up.
- **No mobile-optimized layout.** Sensible defaults only.
- **No client-side state.** Filters live in the URL.
- **No notifications.**

## 8. Open questions for review

Non-blocking for sign-off; some block the first implementation PR:

1. **Hostname + caddy route** for the UI port — manager / supervisor-dev / Colin. Blocks public HTTPS, not v0 (SSH tunnel bridges it).
2. **Backend contract under 0.24** — confirm with tickets-dev the `:8456` paths + JSON shapes are unchanged post-cutover (§3 note).
3. **`get-state` exposure** — the UI's state holds the bearer token. v0 therefore uses `StateCell<UiState>` *directly* (no `#[derive(State)]`, no `get-state` export) so `runtime.get-actor-state` can't dump the token. Confirm that's acceptable (it means the running actor isn't state-inspectable; the store behind the API is the inspectable truth anyway). See Appendix A.
4. **Push key scope** — what the scoped supervisor push key is allowed to do (this actor only).

---

## Appendix A — packr 0.11 → 0.24 rebuild scope

The actor is one small crate (`ui/`, ~920 lines, 3 fields of state, no map/set types). It already uses the pact macro surface (`pack_types!` / `setup_guest!` / `#[export]` / `#[import]`), so the wit→pact axis is **already done**. The remaining work is the same cutover wave chat and inbox ran — applied to a much smaller actor.

**Four axes (one commit):**

1. **wit → pact — DONE.** No `wit!` / `.wit`; `pack_types!` is already in use. Nothing to change.

2. **runtime → self.** The only import from `theater:simple/runtime` is `log`. Move it to `theater:simple/self`:
   - `pack_types!` imports block: `theater:simple/runtime { log }` → `theater:simple/self { log }`.
   - `#[import(module = "theater:simple/runtime", name = "log")]` → `theater:simple/self`.
   - The six `theater:simple/tcp` imports are unchanged. No `shutdown`/`self`/`runtime` system calls are used.

3. **packr-guest 0.11 → 0.24.**
   - `Cargo.toml`: `packr-guest = { version = "0.24", features = ["derive"] }`; add `packr-abi = { version = "0.24", default-features = false }`.
   - `UiState`: drop `#[graph(crate = "packr_guest::composite_abi")]` (0.24 defaults the graph crate to `packr_abi` via the direct dep). Keep `#[derive(Clone, GraphValue)]`.
   - **No map/set codegen churn** — `UiState` is three `String`s, so the 0.24 first-class map/set break doesn't touch us (unlike chat's `set`/`map` fields). This is the big simplifier vs the chat/inbox cutovers.

4. **in-module-state migration.** The exports currently thread state (`init` returns `(UiState, ())`; `handle-connection` takes/returns `UiState`). Under in-module-state, state lives in a module cell and never crosses the boundary:
   - Add `theater-guest = { git = "https://github.com/colinrozzi/theater", rev = "d1a9f270" }`; `use theater_guest::...`.
   - Hold state in `static STATE: StateCell<UiState> = StateCell::new();` **directly** (no `#[derive(State)]`, no `get-state` export — deliberate, to keep the bearer token out of `get-actor-state`; see Open Q 3).
   - `init(config: value) -> result<_, string>`: parse config, `tcp_listen`, then `STATE.set(UiState{…})` and return `ok`/`ok_unit()` — no `(UiState, ())` tuple.
   - `handle-connection(connection-id: string) -> result<_, string>`: drop the `state` param + return; read config via `STATE.with(|s| …)`. The per-connection logic (`try_handle` / `route` / renderers / `api_*`) is unchanged except it takes `&UiState` from the cell instead of an arg.
   - `pack_types!` exports block: `init: func(state: value) -> result<_, string>` and `handle-connection: func(connection-id: string) -> result<_, string>` — drop the `ui-state` param/return from both.

**Manifest + flake:**
- `ui/manifest.toml`: `[[handler]] type = "runtime"` → `type = "self"`. Re-point `package` at the pushed/built wasm (or lean on the release artifact under push-spawn); `static_package` semantics revisited for the push-spawn path. The `tcp` handler stays.
- `flake.nix`: bump the `theater` input `73a4540b` → **`d1a9f270`**; add/point the `theater-guest` git dep to the same rev. The plain-cdylib link flags (`--export-memory`, `--no-entry`) carry over (0.11+ already retired composition). `Cargo.lock` regen (mixed-workspace `cargo update`, not `--precise`) + `flake.lock` update.

**Risk / unknowns:**
- Low surface: no map/set types, no RPC decode (the UI speaks JSON-over-HTTP to the backend, not packr `rpc.call`), no supervisor/spawn imports, no store reads. The thorniest parts of the chat/inbox cutovers (map/set codegen, the outer-Result/inner-Variant rpc decode landmine, store v2→v3 migration) **do not apply here**.
- The `StateCell`-direct pattern (vs `#[derive(State)]`) is exactly what inbox's `mailbox` actor used; it's a proven shape.
- Confirm the exact `ok_unit()` return-shape / helper against the current theater-guest reference actor at `d1a9f270` at implementation time (the one place I'd verify against source rather than memory).

**Build/deploy split:** I write + push the code changes. Per the fleet pattern (and because this container builds native but WASM builds go through manager), the `nix build` + `Cargo.lock`/`flake.lock` regen + the mandatory local spawn-gate on `d1a9f270` + initial prod deploy run on manager's dev box. I'm on-call for any spawn/decode surprise.

**Estimate:**

| Phase | Effort |
|---|---|
| Source cutover (axes 2–4 + manifest + flake) on a branch | **~0.5 day** — small actor, no map/set, no rpc, recipe is known |
| Build + `Cargo.lock`/`flake.lock` regen + spawn-gate on `d1a9f270` (manager) | ~0.5 day incl. one round-trip |
| Spawn/decode fix round-trip buffer | ~0.5 day |
| **Total** | **~1–1.5 days** once the rebuild PR is unblocked (GitHub creds) and Colin has signed this design |

The source cutover can start the moment push credentials are provisioned; it does **not** need to wait on the design sign-off, since the ABI rebuild is mechanical and independent of the (unchanged) architecture. Shipping it to prod waits on sign-off + manager's initial deploy.
