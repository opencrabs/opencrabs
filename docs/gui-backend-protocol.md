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
  only binds loopback and grants `a2a:read` and `a2a:send`, never an
  elevated scope.
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
| `forbidden` | `{"scope": "a2a:admin"}` | the token lacks the operation's scope; checked before params are read. Also the answer of the remote door to what only the crab itself does (`turn.resume`, `mission.decide` with `Apply`) and to a principal acting on another's turn (`turn.approval_answer`, `turn.cancel`): those name `admin`, which no wire credential carries |
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
| `approval_requested` | `session_id`, `request_id`, `tool_name`, `tool_description`, `tool_input`, `capabilities` | asks its user, then answers with `turn.approval_answer` |

### Tool approval

A turn this client started by `gui/call` (`turn.send`, `turn.resume`) that
reaches a tool needing approval, under a policy that does not grant it,
publishes `approval_requested` on its session and waits. `tool_input` is the
JSON the model gave the tool; `capabilities` lists what the tool may touch.

The client answers with `turn.approval_answer`, params
`{"session_id", "request_id", "approved", "always"}`, the ids copied from the
event. `always: true` approves this call and every later tool of the same
turn. The reply is `true` when the waiting tool took the answer and `false`
when it reached nothing: already answered, timed out, or a `session_id` that
is not the request's.

- With no subscriber on the session when the tool asks, or no answer within
  300 s, the tool is denied and the turn goes on with that denial.
- The policy is the one a channel reads: `auto-session` and `auto-always`
  grant without asking, and no event is sent.
- Only a turn that came in by `gui/call` from a caller holding `a2a:manage`
  asks. With `a2a:send` alone (the open loopback posture included) the tool
  is denied, as for the window. A turn the crab's own window started never
  sends `approval_requested`, even while a client watches its session.
- Answering needs `a2a:manage`, and only the principal that started the
  turn answers it: another principal gets `forbidden` and the request keeps
  waiting. Today the gateway hands `gui/call` the credential's scopes but
  not which credential it was, so every remote caller is one principal.

### The remote door

A turn started by `gui/call` belongs to the caller, not to the window:

- `turn.send`: the server ignores `display` and `voice` (history keeps
  `content`, the answer is text) and runs the turn as the `a2a` channel. Any
  turn already on the session, a channel's included, answers `busy`.
- `turn.cancel`, params `{"session_id"}`, scope `a2a:send`: stops the
  remote turn on that session. Reply `true` when one was running, `false`
  when none was. Only the principal that started the turn stops it.
- `turn.resume`: local only; the remote door answers `forbidden`.
- `speak.attach_voice_reply`: local only; the remote door answers `forbidden`, and a `path` with `<<`, `>>` or a control character is `bad_params`.
- `mission.decide` with `Apply` installs a tool the crab runs: the remote
  door needs `a2a:admin` for it. `Reject` stays at `a2a:manage`.
- `session_ops.track_files`: a path the blob door would not serve is not
  tracked and comes back as an untracked line.

### Capabilities

`Cap` names what only some backends can do. A remote client answers `false`
for `open_terminal` (the folder is on the other machine) and `true` for
`open_external` (it opens a downloaded copy). Capabilities are local to the
client and never travel in a call.
`setup` (the `/onboard` wizard, daemon install) and `logs` (the log viewer) are local only: they act on this machine.

## Scopes

Flat and exact: `a2a:admin` does not imply `a2a:read`.

| scope | covers | in a pairing by default |
|---|---|---|
| `a2a:read` | rail, transcript, facts, tasks, allowed files, usage | yes |
| `a2a:send` | send or cancel a turn, attach | yes |
| `a2a:manage` | rename, close, favourites, topics, create session, clear, undo, answer a tool approval | no |
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
| `turn.approval_answer` | `ApprovalAnswer` | `a2a:manage` | answer one `approval_requested`; params `{"session_id", "request_id", "approved", "always"}`, reply `true` if a waiting tool took it |
| `turn.cancel` | `TurnCancel` | `a2a:send` | stop the remote turn on a session; params `{"session_id"}`, reply `true` if one was running |
| `settings.load` | `SettingsLoad` | `a2a:admin` | the settings modal's rows read from the crab's config; params `{}`, reply `{"values", "credentials"}`, a credential only `{"section", "label", "configured"}`, never a key |
| `settings.write` | `SettingWrite` | `a2a:admin` | write one settable key on the crab; params `{"section", "key", "write"}`, a key outside the settable table or a value of the wrong kind is `bad_params`; reply the value as written |
| `settings.credential_write` | `CredentialWrite` | `a2a:admin` | write one provider's `api_key` in the crab's `keys.toml`; params `{"section", "api_key"}`, an unknown provider or empty key is `bad_params`; reply `null`, the key is never echoed |
| `shell.run` | `ShellRun` | `a2a:admin` | the window's `!cmd`, run through the platform shell in the crab's launch directory; params `{"cmd"}`, reply the chat's text: the command line, then its output, each stream capped |

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
