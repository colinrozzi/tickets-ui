# tickets-ui

Theater-native web UI for the [tickets](https://github.com/colinrozzi/tickets) actor system.

Owned by **tickets-ui-dev@colinrozzi.com**.

See [DESIGN.md](./DESIGN.md) for the v0 architecture.

## Shape

Single wasm actor (`packr-guest`, `no_std + alloc`) that:
- binds a TCP listener (default `127.0.0.1:9444`) in `init`,
- serves each connection in-place — no per-connection child spawn,
- renders server-side HTML with `format!` strings + hand-written CSS embedded via `include_str!`,
- reads *and* writes go through the tickets API on loopback (default `127.0.0.1:8443`, plaintext, `Authorization: Bearer …`).

Both paths use the API over loopback — the store-direct read option from the
original DESIGN was flipped to `GET /v1/tickets` and signed off 2026-06-05
(see DESIGN.md §3). List + detail renderers are wired against it.

Wire shape per `tickets-handler`'s 2026-06-05 reply:

| UI route | Upstream | Body |
|---|---|---|
| `GET /` (with optional `?status=…&assignee=…`) | `GET /v1/tickets[?status=…&assignee=…]` | — |
| `GET /t/<id>` | `GET /v1/tickets/<id>` | — |
| `POST /new` | `POST /v1/tickets` | `{title, body, reporter, assignee}` |
| `POST /t/<id>/comments` | `POST /v1/tickets/<id>/comment` | `{author, body}` |
| `POST /t/<id>/status` | `POST /v1/tickets/<id>/status` | `{status}` |

## Build

```sh
nix build                      # produces result/tickets_ui.wasm
nix run .#release              # tag + push release-YYYYMMDD-<sha7>; CI builds + uploads
theater spawn ui/manifest.toml # bring the actor up against a running tickets-acceptor
```

`ui/manifest.toml` takes JSON `initial_state`:

```json
{
  "api_addr":    "127.0.0.1:8443",
  "api_token":   "<bearer the tickets API accepts>",
  "listen_addr": "127.0.0.1:9444"
}
```

## Deploy

v0 testing is **via SSH tunnel** — Colin reaches the actor's `127.0.0.1:9444` from a local browser through the tunnel.

Public HTTPS lands with [frontdoor](https://github.com/colinrozzi/frontdoor), the SNI-routing Theater actor that owns `:443` on the VPS and forwards SNI-matched encrypted streams to backends on loopback. When frontdoor is in place, the sentinel template for tickets-ui adds a `server_tls` config block alongside the `tcp` handler and frontdoor SNI-routes `tickets-ui.colinrozzi.com` to it. No coupling to the v0 actor code — the listener already supports an additional TLS server config.
