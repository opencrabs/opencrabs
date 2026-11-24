# GUI/backend protocol

How a surface (the desktop window, a mobile app, a script) asks a crab to do
things. Written for a client that is not Rust and does not read the code.

Every operation a surface can ask is a named, serializable value declared in
`src/backend/`. The local window runs it in process; a remote client sends the
same value to the crab's gateway, which runs it through the same
`opencrabs::backend::dispatch_as`. A local answer and a remote answer differ
only by the transport.

`tests/gui_backend_protocol_test.py` keeps this file honest: every operation
in an `ops!{}` must have a row in the table below with the same wire name and
scope, no row may name an operation the code does not declare, and every event
and error kind must be named here.

## Status of each piece

Read this first. Some of the wire is in code, some is the agreed design that
lands in wave 2.

| Piece | Where | Status |
|---|---|---|
| Operation names, scopes, params and replies | `src/backend/*.rs`, `Op` trait in `op.rs` | in code |
| Error body (`BackendError`) | `src/backend/error.rs` | in code |
| Event body (`Event`) | `src/backend/event.rs` | in code |
| Capabilities (`Cap`) | `src/backend/cap.rs` | in code |
| JSON-RPC method `gui/call` on `POST /a2a/v1` | `src/a2a/` | designed, not in code yet |
| SSE method `gui/events` | `src/a2a/` | designed, not in code yet |
| Blob download `gui/blob` | `src/backend/blob.rs` | designed, not in code yet |
| `gui-backend/<n>` in the agent card, the handshake op | `src/a2a/agent_card.rs`, `src/backend/hello.rs` | designed, not in code yet |
| Rail snapshot op (one round trip for the whole rail) | `src/backend/sessions.rs` | designed, not in code yet |

A "designed" row describes the shape the plan fixes. Its exact names land with
the package that writes it, which also updates this file in the same commit.

## Transport

The crab's existing A2A gateway, default port `18790`.

- `POST /a2a/v1`, JSON-RPC 2.0, `Content-Type: application/json`.
- `Authorization: Bearer <token>`. The token is either the crab's
  `[a2a].api_key` (every scope) or an `ocpair_...` token from pairing (the
  scopes its pairing code carried). With no api_key configured the gateway
  only binds loopback and grants every scope.
- A missing or wrong token answers HTTP 401 with JSON-RPC error `-32001`.

### Calling an operation: `gui/call`

```json
{"jsonrpc": "2.0", "id": 1, "method": "gui/call",
 "params": {"name": "sessions.rename",
            "params": {"id": "6f1c...", "title": "new title"}}}
```

`params.name` is the wire name from the table below. `params.params` is the
operation's own params object, exactly as its fixture shows. An operation
with no params takes `{}`.

On success, `result` is the operation's reply, exactly as its fixture shows.
A reply of `()` in Rust is JSON `null`.

On failure, a JSON-RPC `error` whose `data` is the `BackendError` body below.

### Errors

`BackendError` travels as `{"kind": <kind>, "detail": <detail>}`:

| kind | detail | meaning |
|---|---|---|
| `unknown_op` | the name asked | no domain declares it; an older crab answers this for a newer op |
| `forbidden` | `{"scope": "a2a:admin"}` | the token lacks the operation's scope; checked before params are read |
| `bad_params` | text | `params.params` did not decode as the operation |
| `not_ready` | absent | the crab is still booting |
| `busy` | absent | the crab cannot take this now; retry later |
| `failed` | text | the operation ran and failed; show the text as is |

The gateway's own JSON-RPC codes still apply around it: `-32004` insufficient
scope on the method itself, `-32601` unknown method (a crab without `gui/*`),
`-32600` wrong `jsonrpc` version.

### Events: `gui/events`

A server-sent event stream on the same endpoint. Each `data:` line is a
JSON-RPC response whose `result` is one `Event`, tagged by `kind`:

| kind | fields | a surface |
|---|---|---|
| `turn` | `session_id`, `progress` | renders the step; `progress` is a `ProgressEvent`, tagged `{"kind", "data"}` |
| `turn_done` | `session_id` | ends the live turn, re-reads the transcript |
| `turn_failed` | `session_id`, `message` | ends the live turn, shows `message` |
| `turn_cancelled` | `session_id` | ends the live turn |
| `rail_changed` | none | reloads the rail, never patches it |
| `transcript_changed` | `session_id` | reloads that transcript |
| `tasks_changed` | none | reloads the background tasks |

### Capabilities

`Cap` names what only some backends can do. A remote client answers `false`
for `open_terminal` (the folder is on the other machine) and `true` for
`open_external` (it opens a downloaded copy). Capabilities are local to the
client and never travel in a call.

## Scopes

Flat and exact: `a2a:admin` does not imply `a2a:read`.

| scope | covers | in a pairing by default |
|---|---|---|
| `a2a:read` | rail, transcript, facts, tasks, allowed files, usage | yes |
| `a2a:send` | send or cancel a turn, attach | yes |
| `a2a:manage` | rename, close, favourites, topics, create session, clear, undo | no |
| `a2a:admin` | model, working directory, settings, secrets (write only), shell, delete | no |

## Operations

One row per operation the code declares. The fixture
`tests/fixtures/gui_backend/<Type>.json` is the contract for params and reply;
it is written by the package that adds the operation, from the handler's real
output.

| wire name | type | scope | what it does |
|---|---|---|---|
| `sessions.load` | `LoadSessions` | `a2a:read` | the rail's sessions, newest first, at most 200 rows; params `{}`, reply a list of session rows |
| `sessions.rename` | `RenameSession` | `a2a:manage` | rename a session or topic; params `{"id", "title"}`, reply `null` |
| `session_ops.clear_messages` | `ClearMessages` | `a2a:manage` | delete one session's messages, keep the session; params `{"session_id"}`, reply `null` |

### Fixture shape

```json
{
  "op": "sessions.rename",
  "params": {"id": "00000000-0000-0000-0000-000000000001", "title": "after"},
  "reply": null
}
```

`params` and `reply` are required; `op`, when present, must equal the wire
name. Ids are UUID strings, timestamps RFC 3339 strings, durations serde's
`{"secs", "nanos"}`.

## Compatibility

- A new field always carries `#[serde(default)]`, so an older client's call
  still decodes and an older crab ignores what it does not know.
- A newer client asking an older crab for an operation it lacks gets
  `unknown_op`, and disables that action with the reason instead of failing.
  The handshake op returns the operations the crab knows, so a client can
  decide before asking.
- Secrets are never read over the wire, only written.

## A client in 50 lines

`scripts/client-gui-backend.py` speaks this protocol with the Python standard
library only: the handshake, then the rail snapshot.

```sh
python3 scripts/client-gui-backend.py --url http://127.0.0.1:18790 --token "$TOKEN"
```
