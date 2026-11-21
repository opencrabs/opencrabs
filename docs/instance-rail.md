# The instance rail: several crabs in one window

A 56 px column on the far left of the dashboard that says *which crab* this
window is looking at. Pick a crab and the rail lists **that crab's** sessions
instead of the local ones; pick a session and the panel reads its transcript,
read-only, with the remote's banner on top. The composer under that panel sends
the message to that crab as an A2A task.

Three nested scopes, one per column:

| Column | Shows | Source |
|---|---|---|
| strip (56 px) | the crabs, with a health dot | `state/remote/instances.json` |
| rail | that crab's sessions | `session/list` on the remote |
| panel | that session's messages | `session/transcript` on the remote |

The local crab is one of the entries. It is the first badge, it is selected by
default, and selecting it puts the rail and the panel back on the local ledger.

## Why it exists

One crab per machine is a lie the UI told for a long time: an owner with a
laptop, a server and a VPS had three windows and no way to tell which one a
session lived in. The rail answers that with the smallest surface that can: a
column that names the machine, and the two columns that already existed
following the pick.

## What it is not

- **Not a remote shell.** The panel reads and sends one message. It does not
  drive tools, approve plans, or change settings on the other crab.
- **Not a sync.** Nothing is copied between the two ledgers; the remote renders
  its own rows.
- **Not a new transport.** It speaks the A2A JSON-RPC that `opencrabs a2a`
  already speaks, over the routes `opencrabs routes` already keeps.

## Protocol

Three read methods, all under `a2a:read`, plus the existing `message/send` for
the composer.

| Method | Params | Answer |
|---|---|---|
| `session/list` | `limit` | `{ sessions: [{ id, title, updatedAt, createdAt, model, cost, preview }] }` |
| `session/transcript` | `sessionId`, `limit` | `{ sessionId, title, messages: [{ id, role, content, createdAt }] }` |
| `tasks/list` | `limit`, optional `contextId` | `{ tasks: [...] }`, newest first |
| `message/send` | A2A message | the task id, as before |

`session/list` and `session/transcript` read the same repositories the local
window reads (`SessionRepository::list`, `MessageRepository::find_by_session`),
so a remote crab lists exactly what it would list on its own screen.

A crab whose build predates these methods answers `-32601`. The rail then falls
back to `tasks/list` and says so; it does not paint an empty list and pretend
the crab has no sessions.

## Client

`src/remote/instance_client.rs` is the wire: `Target` (url + optional token),
`probe`, `call`, `list_sessions`, `transcript`, `list_tasks`, `send`, `task`.
Every parser is pure and tested against a fixture, so a wire change fails a
test instead of a window.

`src/remote/instance_store.rs` is the disk: `state/remote/instances.json`, mode
`0600` from the first byte, written atomically. Pairing is `POST /a2a/pair`
with a one-time code; what comes back is a `ocpair_` session token scoped to
`a2a:read` (plus `message/send` where the owner granted it).

The token is plaintext at rest, and that is deliberate: a client must present
it, so hashing it would only make the file unreadable to its owner. The file is
`0600` and lives beside `routes.json`, which holds the same class of secret.

## GUI

| File | Job |
|---|---|
| `src/gui/instances.rs` | the model: entries, health, order, selection, labels. Pure. |
| `src/gui/instance_strip.rs` | the 56 px view, cached: a `StripFrame` that compares equal replays its prepaint |
| `src/gui/dashboard_instances.rs` | the wiring: load the store, the probe loop, the pick, the pairing form, the mark's menu, the sharing card, the remote panel frame |
| `src/gui/remote_panel.rs` | the panel column: banner, read-only transcript, composer |
| `src/gui/dashboard_rail.rs` | `remote_rail_rows()` swaps the rail's rows while a remote is open |
| `src/a2a/handler/sessions.rs` | the three server methods |

Two rules the paint follows:

1. **The accent belongs to the selection alone.** Ring, number and the edge bar
   are the only orange in the column. A crab that is merely online gets a green
   dot, not a glow.
2. **Health never moves a crab.** Order is the store's, so a badge cannot slide
   away between the press and the release.

## Health

There is no heartbeat in A2A, so none is invented. The probe is a `GET` to the
remote's agent card, every five seconds, on the background executor, with a
timeout. A remote that accepts the connection and never answers must not be
able to freeze a window, so a probe that times out is `Down`, not a hang.

## Tests

The three layers are testable without a window and without a network. Note
which crate each filter belongs to: `src/gui/` is a **binary** (`path =
"src/gui/main.rs"`), not part of the library, so its tests need `--bin`, and
the module path starts at the crate root (`tests::…`, not `gui::tests::…`).

```
# library
cargo test --locked --profile ci --lib remote::instance_store
cargo test --locked --profile ci --lib remote::instance_client
cargo test --locked --profile ci --lib a2a::handler::sessions

# window binary
cargo test --locked --profile ci --features gui --bin opencrabs-gui tests::instances
```

These are the exact filters the fork's GUI workflow runs before it links a
binary, so a red test names its module instead of the window. A `--lib` filter
that matches nothing exits 0 and proves nothing, which is why the window tests
are pinned to the binary.

## Handing a crab to another crab

The head of the column is the mark: the `C` that says which crab the window is
about. Right-click it and it opens three rows, because the head of the column
can do three things.

| Row | What it does |
|---|---|
| gerar código de acesso remoto | mints a single-use code and opens the sharing card |
| emparelhar um crab | the existing pairing form, for a code *this* crab was given |
| definições desta máquina | this crab's own settings |

### The code is minted here, not by the CLI

`instance_share_begin` builds a `PairingStore` on `PairingStore::default_path()`
(`state/remote/pairing.json`) and calls `mint_code` with `a2a:read` and
`a2a:send`. That is the same file the A2A gateway reads, so a code minted by the
window is a code the gateway honours, with no round trip through a terminal.
The code expires after `CODE_TTL_SECS` (300 s, `src/remote/pairing.rs`).

Closing the card is **not** a revoke. The store has no revoke path a card could
call, so the card says the code lives until it expires and stops there.

### The card

Top to bottom: the QR, this crab's address, the code, any warning, and three
buttons (copiar link, copiar código, novo código).

The QR is painted **white on black whatever the theme is**, at 4 px per module.
A scanner needs contrast, and a dark card with dark modules is a pretty square
that no camera reads. A link too long for the largest QR version comes back
empty (`qr_modules` returns `(vec![], 0)`), and the card says so and points at
the copy button instead of drawing half a square.

### The link

```
opencrabs://pair?url=<percent-encoded>&code=<code>&label=<hostname>
```

`parse_pair_link` reads it back into `(url, code, Option<label>)`, and the
pairing form's `absorb_link` fills all three fields from one paste: pasting the
link beats typing an IP and a code by hand. The round trip is pinned by a test,
so a change to either encoder fails in the binary rather than in a window.

### The address, and when it is useless

`share_target` decides what to hand out, and refuses to be optimistic:

1. `[a2a] advertise_url`, if set, wins. It is the owner's explicit answer to
   "how do I reach this box".
2. `[a2a] enabled = false` → the card says the gateway is off and how to turn it
   on. A code for a listener that is not running is a dead code.
3. bind is loopback (`127.0.0.1`, `localhost`, `::1`) → the card says plainly
   that **no other crab can reach this machine**, and names the two keys that
   fix it (`bind = "0.0.0.0"`, `advertise_url`). Saying "here is a code" while
   the listener is loopback is how an hour disappears into a pairing that could
   never have worked.
4. Otherwise the LAN address, found by binding a UDP socket and asking the
   kernel which interface it would route through: no crate, no DNS, no packet
   sent.
5. With no `[a2a] api_key` the card warns that anything on the network can call
   `/a2a/v1`.

### The badge's colour

Each badge tints itself from its own `host_id` (`badge_tint`), stable across
restarts and bounded so a column of crabs never collapses into five identical
grey circles. Two tests hold it: one pins stability and the bound, one pins
that a realistic column of hosts stays distinguishable.

## Known limits

- The panel follows nothing: a `message/send` returns a task id and the panel
  says so. Streaming a remote task's progress needs `message/stream` wired into
  the same frame, which is not done.
- A remote with no `session/list` shows tasks, not sessions. That is a smaller
  view of the same crab, and the banner says which one is on screen.
- Pairing needs a URL the client can reach: a quick tunnel changes its hostname
  on every restart, so LAN, tailnet or a named tunnel is what keeps an entry
  alive.
- The QR carries whatever `share_target` decided. On a machine reachable only
  through a tailnet or a VPN, the LAN guess is the wrong address: set
  `advertise_url` and the card hands that out instead.
- A code minted while `[a2a] enabled = false` is dead on arrival. The card says
  so before the other crab tries it, but it cannot fix the config for you.
