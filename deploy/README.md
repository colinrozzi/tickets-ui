# tickets-ui deploy artifacts

Staged, git-tracked build artifacts for the box-local deploy onto the
tickets-supervisor (sibling of tickets-acceptor). Git-tracked deliberately so a
`nix store gc` on the builder can't evict the `result` symlink's target and lose
the wasm.

| File | What |
|---|---|
| `wasm/tickets_ui.wasm` | The built + spawn-gated actor. **sha256 `e82610ee020a6acf7fe4afbe7faf16a371c218fe3b51d963bfc93a6e8edf0546`** |
| `tickets-ui.toml` | Sub-manifest (self + tcp handlers). `initial_state` injected at deploy. |

## Provenance

- Built from the merged packr-0.24 / theater `d1a9f270` cutover (PR #22) via `nix build .#default` — byte-reproducible from the same flake + lock.
- Import surface: host-only (`theater:simple/self` log + 6× `theater:simple/tcp`). Exports `actor.init` + `tcp-client.handle-connection`, **no `actor.get-state`** (bearer stays un-dumpable, DESIGN Q3).
- Spawn-gated on the embedded theater `d1a9f270` (`supervisor` v0.4.2): interface-hash verify passed, `/healthz` 200, `/static` 200, `/` 502 pre-backend.

## Deploy (box-local push-spawn)

```
supervisor push --manifest deploy/tickets-ui.toml --wasm deploy/wasm/tickets_ui.wasm ...
```

`initial_state`: `{"api_addr":"127.0.0.1:8456","api_token":"<shared tickets bearer>","listen_addr":"127.0.0.1:9445"}`.
Authorize control pubkey `b1e64dedbcd2f36062ae7a55f948621e44567096d9eb4d8787ecf08b205ddada` for subsequent push redeploys.
