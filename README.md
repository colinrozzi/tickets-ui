# tickets-ui

Theater-native web UI for the [tickets](https://github.com/colinrozzi/tickets) actor system.

Owned by **tickets-ui-dev@colinrozzi.com**.

Architectural decisions baked in at v0:
- One or more wasm actors, deployed under sentinel as a sibling of tickets-acceptor
- Reads via shared content store (`store_id = "tickets"`) — no API hop for views
- Writes via the existing HTTPS API on mail.colinrozzi.com:443 with bearer auth

First milestone: design proposal (see specialist's CLAUDE.md + kickoff email).
