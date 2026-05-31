# tickets-ui — v0 design proposal

**Status:** proposal, awaiting sign-off from Colin (via manager). No UI code lands until this doc is merged.

This is the design for the v0 web UI on top of the tickets actor system. It honors the architectural decisions already locked in by manager + Colin (Theater-native wasm actor, sentinel-managed deploy, reads from the shared store, writes through the tickets HTTPS API). What follows is the choices that were *not* yet locked, with justifications and explicit scope cuts.

## 1. View inventory

Three screens. Nothing else in v0.

| Path | Purpose |
|---|---|
| `GET /` | Ticket list. Server-rendered table. Filters via query string: `?assignee=…&status=…`. No client-side sorting/filtering. |
| `GET /t/<id>` | Ticket detail. Header (id, title, body, status, assignee, created/updated), comment thread in chronological order, comment-add form, status-transition control. |
| `GET /new` + `POST /new` | Compose a new ticket. Plain form: title, body, assignee, initial status (default `open`). |

Plus a single `GET /healthz` that returns 200 — useful for sentinel/proxy probes.

**Comment add** and **status transition** post to dedicated paths (`POST /t/<id>/comments`, `POST /t/<id>/status`) and 303-redirect back to the detail view (post/redirect/get).

That's the entire URL surface for v0. No `/me`, no `/search`, no `/api/*` from the UI actor — the UI is a renderer, not an API.

## 2. Listener strategy

**Choice:** the UI actor binds its own port (proposed: `127.0.0.1:8081`), exposed publicly via the VPS reverse proxy at a dedicated hostname (e.g. `tickets-ui.colinrozzi.com` or whatever the host setup ends up being — leaving this to sentinel-dev / Colin). tickets-acceptor stays on `127.0.0.1:8443` and the UI actor calls into it over loopback when it needs to write.

**Tradeoff considered:**

|  | Separate port (chosen) | Sub-route of tickets-acceptor:8443 |
|---|---|---|
| Deploy independence | UI and API actors release on their own cycles | Coupled — either tickets-acceptor proxies to UI actor (extra hop, extra wiring), or UI lives inside tickets-acceptor (collapses two actors into one — gives up the decomposition) |
| Surface separation | tickets-acceptor stays a pure JSON API | Adds HTML rendering responsibility to the API actor or routing complexity to it |
| Public exposure | One reverse-proxy entry to add | Either tickets-acceptor gets exposed (it's not today) or the same reverse-proxy work happens |
| Auth boundary | UI holds the bearer token server-side; browser never sees it | Same, doesn't change much |

Separate port wins because the deploy-independence + clean-API-surface arguments stack, and the public-exposure work is one-time either way. tickets-acceptor being localhost today actually pushes *toward* a separate port: we don't have to change tickets-acceptor's exposure model to ship the UI.

**Exposure (clarified after manager's sign-off ask):** today inbox-acceptor terminates TLS *in-actor* at `mail.colinrozzi.com:443` — there is no nginx/caddy reverse proxy in front of it. Exposing tickets-ui externally over HTTPS therefore needs one of two paths:

  **(a)** Introduce a small reverse proxy (caddy or nginx) on the VPS fronting both inbox-acceptor and tickets-ui, with TLS termination at the proxy. New infrastructure, but once it's there both actors benefit and future siblings get cheap onboarding.

  **(b)** The tickets-ui actor terminates its own TLS in-actor (mirroring inbox-acceptor's pattern) with its own cert and a separate `:443` vhost / hostname. No new infrastructure, but duplicates the in-actor TLS plumbing inbox-acceptor already carries — and every future UI actor pays the same cost.

This design doc takes **no position** on (a) vs (b); both are compatible with the separate-UI-port choice above. It's a sentinel-dev / Colin call and is **not** a blocker on architectural sign-off — it blocks v0 deploy, not v0 design. Final hostname + cert delivery: same channel.

## 3. Wire shape — reads vs writes

**Reads go to the store directly. Writes go through the API.** Per the locked decisions.

### Reads (`theater:simple/store`, `store_id = "tickets"`)

| View | What the actor reads |
|---|---|
| List | Enumerate ticket records (assume an index entry or prefix scan — exact shape TBD with tickets-dev); filter in-actor by query params; render table |
| Detail | Read the single ticket record + its comment thread (whether one document or many keys is tickets-dev's call — see open questions below) |

Storage layout is owned by tickets-acceptor / tickets-dev. The UI reads whatever shape is there; this doc does **not** prescribe schema. The expected read APIs in the WIT are the standard `theater:simple/store` operations.

### Writes (HTTPS to `127.0.0.1:8443` over loopback, with bearer auth)

| Action | Roughly maps to |
|---|---|
| Create ticket | `POST /tickets` |
| Add comment | `POST /tickets/<id>/comments` |
| Transition status | `POST /tickets/<id>/status` (or `PATCH /tickets/<id>`) |

These map ~1:1 to the existing `tickets` CLI subcommands. Exact paths, methods, and request/response bodies will be confirmed with tickets-dev as a follow-up — they're the source of truth on the wire format. The UI actor holds the bearer token (loaded from env or sentinel-injected config); the browser never sees it.

**No `/api` from the UI actor.** Browsers submit plain forms, the UI actor turns them into authenticated API calls server-side, and 303-redirects back to the detail page. Progressive enhancement (fetch + optimistic update) is explicitly out of scope.

## 4. Actor decomposition

**Choice:** one wasm actor for v0.

It handles inbound HTTP, store reads, outbound API calls, and HTML rendering. Theater's handler model accommodates this within a single actor; per-request state is small and the store is the persistence layer.

**Considered alternative:** per-connection actor (a dispatcher spawns a child for each HTTP request). More theater-idiomatic for isolating session state, but the UI has no per-user session state in v0 (single bearer token, single user, no per-user preferences). The split is an optimization we can take later if we need it — splitting one actor in two is easier than collapsing two into one.

## 5. Framework / build

**Server-rendered HTML, vanilla JS (only where needed), hand-written CSS.** No SPA, no React, no build step beyond `cargo` + `wasm32-wasip2` (or whatever target tickets-acceptor uses — match it).

| Concern | Choice | Why |
|---|---|---|
| HTML rendering | Rust string templating via a small crate (likely `minijinja` if it compiles cleanly to wasi-preview2; fallback to `format!`/`write!`) | Avoids learning a framework for ~5 templates. Minijinja is the smallest "real" templating crate I'd consider. |
| CSS | One hand-written `style.css`, embedded in the wasm and served at `GET /static/style.css` (or sidecar — TBD per Theater conventions) | No PostCSS / Tailwind. v0 has ~3 screens; that doesn't justify a build pipeline. |
| JS | Vanilla, ideally zero — plain forms with full-page reload | Progressive enhancement (inline comment add, live list refresh) is an explicit v0 scope cut |
| Build | `nix flake` building the wasm, matching tickets-acceptor's flake layout | Standard for the workspace; release artifact shape is `release-YYYYMMDD-<sha>` per CLAUDE.md |
| Templating data shape | The actor builds typed Rust structs per view, hands them to the template | Keeps the template "dumb" |

**On coordinating with inbox-ui-dev**: CLAUDE.md says mild divergence is OK for v0; we can converge later. If inbox-ui-dev has already settled on a palette / templating choice and shared it, this doc will be updated in review to match where it makes sense. Otherwise: pick something defensible now, share back via the inbox, and converge in v0.1.

## 6. What v0 is NOT

Explicit scope cuts so this doesn't sprawl:

- **No authentication / user identity in the UI.** Single bearer token held server-side. If multiple humans use this, they share the credential. Per-user auth is a separate design.
- **No search.** Filtering is limited to `?assignee=…&status=…` on the list page. No fuzzy / full-text.
- **No attachments / file uploads.** Plain text bodies and comments only.
- **No realtime updates.** No WebSocket, no SSE. The user refreshes the page.
- **No edit / delete.** Tickets and comments are immutable; status transitions are the only mutation past creation. (Editing a ticket title is a follow-up design.)
- **No markdown rendering** in ticket bodies or comments. Plain text + line breaks. (Easy to add later — pulling in `pulldown-cmark` is one line, but skipping for v0.)
- **No mobile-optimized layout.** Sensible defaults; nothing more.
- **No client-side state.** Filters live in the URL, not localStorage.
- **No notifications** (in-app, email, anything).

## Open questions for review

These don't block this design doc, but block the first implementation PR:

1. **Wire format** of the write API — tickets-dev to confirm paths, methods, request/response bodies.
2. **Storage schema** in the `tickets` store — tickets-dev to confirm whether tickets and comments are co-located or separately keyed, and how to enumerate them for the list view.
3. **Public hostname + TLS** for the UI port — sentinel-dev / Colin to pick between §2(a) (introduce a reverse proxy fronting inbox-acceptor + tickets-ui) and §2(b) (UI actor terminates its own TLS in-actor on a separate `:443` vhost). Not blocking design sign-off; blocking v0 deploy.
4. **Bearer token delivery** to the UI actor — env var? sentinel-injected manifest config? Match how tickets-acceptor / inbox actors do it today.
5. **Templating crate** wasi-preview2 compatibility — verify `minijinja` builds cleanly; fall back to `format!` if not.
6. **Shared design conventions with inbox-ui-dev** — open the conversation once this doc is merged so we don't pre-empt before the architecture is approved.

## Why this shape

The locked-in constraints (Theater actor, sentinel-managed, store-reads / API-writes) already make most of the hard calls. What was left to decide were:

- **Listener** → separate port (deploy independence + leaves tickets-acceptor untouched)
- **Decomposition** → single actor (no session state to isolate; split later if needed)
- **Framework** → vanilla + hand-rolled (~3 screens does not justify a build pipeline)
- **Scope** → narrow (3 views, 0 realtime, 0 search, 0 attachments)

The thing we're optimizing for in v0 is: *ship something usable from a browser fast, without committing to anything that's expensive to undo later*. Server-rendered HTML in a wasm actor is the smallest viable shape; everything else (SPA, realtime, search, multi-user) can be added incrementally without breaking what's here.
