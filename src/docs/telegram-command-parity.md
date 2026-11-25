# TUI ↔ Channel slash-command parity

Inventory taken from source on 2026-10-31 (v0.5.15 tree); refreshed for
upstream main on 2026-11-23.

- **TUI autocomplete list:** `src/ui_core/commands.rs` → `SLASH_COMMANDS`
  (**35 entries**, including the setup screens `/projects`, `/tasks`,
  `/workspace`, `/channels`, `/voice`, `/image`, `/daemon`, `/brain`;
  `/rebuild` was retired upstream). The desktop window adds its own
  `GUI_ONLY_COMMANDS` (`/settings`) and the alias table
  (`/themes`, `/showplan`, `/show_plan`).
- **TUI-visible channel-only list:** `src/tui/app/state.rs` → `CHANNEL_COMMANDS` (3 entries)
- **Channel dispatcher:** `src/channels/commands.rs` → `handle_command()` (plus `/cowork`, intercepted earlier in `src/channels/telegram/handler.rs`)
- **Telegram menu catalog:** `src/channels/telegram/agent.rs` → `collect_command_catalog()`

Total TUI command surface: **39** (35 `SLASH_COMMANDS` + 1 `GUI_ONLY_COMMANDS`
+ 3 `CHANNEL_COMMANDS`). Every one appears below.

## Legend

- **TUI**  -  handled by `src/tui/app/messaging.rs` (`handle_slash`).
- **TG handler**  -  accepted by the channel dispatcher, i.e. it does something useful in chat.
- **TG menu**  -  published in Telegram's `/` picker by `collect_command_catalog()`.
- **Gap**  -  what is missing.

## Gap table

| Command | TUI | TG handler | TG menu | Gap / needs-what |
|---|---|---|---|---|
| `/new` | yes | yes (`NewSession`) | yes |  -  |
| `/cd` | yes | yes (`/cd [path]`) | yes |  -  |
| `/sessions` | yes | yes (`/sessions[:query]`) | yes |  -  |
| `/projects` | yes | yes (`TuiOnly` explanation) | no | Setup screen; chat gets a "TUI-only" reply instead of "Unknown command". |
| `/compact` | yes | yes (`Compact`) | yes |  -  |
| `/clear` | yes | yes (`ClearContext`) | yes |  -  |
| `/theme` | yes | yes (`TuiOnly` explanation) | no | Terminal colour presets have no chat meaning. |
| `/stop` | yes | yes (`Stop`) | yes |  -  |
| `/plan` | yes | yes (`PlanMode`) | yes |  -  |
| `/show-plan` | yes | yes (`/show-plan`, `/show_plan`, `/showplan` aliases) | yes (`show_plan`) |  -  |
| `/tasks` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/execute` | yes | yes (`Execute`) | yes |  -  |
| `/discard` | yes | yes (`Discard`) | yes |  -  |
| `/help` | yes | yes (`Help`) | yes |  -  |
| `/models` | yes | yes (`/models`, `/models <provider/model>`) | yes |  -  |
| `/usage` | yes | yes (`/usage`, `provider`, `model`, `7d`) | yes |  -  |
| `/mission-control` | yes | yes (`/mission-control`, `/mission_control`) | yes (`mission_control`) |  -  |
| `/onboard` | yes | yes (`TuiOnly` explanation) | no | Multi-screen wizard. Legacy `/onboard:<step>` spellings still resolve and get the same reply. |
| `/workspace` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/channels` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/voice` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/image` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/daemon` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/brain` | yes | yes (`TuiOnly` explanation) | no | Setup screen. |
| `/doctor` | yes | yes (`Doctor`) | yes |  -  |
| `/cowork` | yes | yes (`telegram/handler.rs`, DM-only intercept) | yes |  -  |
| `/approve` | yes | **yes (`Approve`)** | **yes** | Implemented: read/set `agent.approval_policy`; accepts `approve-only` / `auto-session` / `auto-always`, the same three values the TUI slash-command tool accepts. Owner-gated. |
| `/evolve` | yes | yes (`Evolve`) | yes |  -  |
| `/restart` | yes | **yes (`Restart`)** | **yes** | Implemented: chip added to the catalog. |
| `/exit` | yes | **yes (`Exit`)** | **yes** | Implemented: chip added to the catalog (also `/quit`). |
| `/whisper` | yes | yes (`TuiOnly` explanation) | no | Clipboard capture; a voice note is the chat equivalent. |
| `/skills` | yes | **yes (`Skills`)** | **yes** | Implemented: enumerates every loaded skill with the `/name` that runs it. Owner-gated. |
| `/rtk` | yes | yes (`Rtk`) | yes |  -  |
| `/goal` | yes | yes (`/goal <args>`) | yes |  -  |
| `/profiles` | yes | yes (`Profiles`) | yes |  -  |
| `/settings` | window-only (`GUI_ONLY_COMMANDS`) | yes (`TuiOnly` explanation) | no | Desktop settings screen; no terminal equivalent upstream. |
| `/respond_to` | channel-only | yes (`/respond_to [all\|mention\|auto]`) | yes |  -  |
| `/redact` | channel-only | yes (`/redact <scope> <on\|off>`) | yes |  -  |
| `/rename` | channel-only | **yes (`/rename <title>`)** | **yes** | Implemented: chip added to the catalog. |

Count check: **39 rows**  -  35 `SLASH_COMMANDS` + 1 `GUI_ONLY_COMMANDS`
(`/settings`) + 3 `CHANNEL_COMMANDS`, in table order.

## Summary of actual work

| Class | Commands | Action |
|---|---|---|
| Already complete | 22 | none |
| Handler existed, missing from the Telegram menu | `/restart`, `/exit`, `/rename` | add catalog entries |
| Missing handler, meaningful in chat | `/approve`, `/skills` | implement in `src/channels/commands.rs` |
| Missing handler, TUI-only by nature | `/theme`, `/whisper`, `/rebuild`, `/onboard` (+ legacy steps), `/projects`, `/tasks`, `/workspace`, `/channels`, `/voice`, `/image`, `/daemon`, `/brain`, `/settings` | reply with a "TUI-only" explanation instead of "Unknown command" |

## Blocking bug found while inventorying

`register_scoped_menus()` (`agent.rs`) and both `menu_auto.rs` call sites resolve the owner as:

```rust
tg.allowed_users.first().and_then(|s| s.parse::<u64>().ok())
```

With the shipped convention that `allowed_users` may start with the wildcard `"*"` (e.g. `["*", "1608111860"]`), `.first()` yields `"*"`, the parse fails, and the function returns **before publishing any owner menu at all**  -  the Telegram `/` picker is left showing only `/start`. Owner detection must skip non-numeric entries instead of trusting index 0.

Second structural limit: unconfigured groups only received the owner catalog when the group is *solo-owner* (`menu_auto::evaluate_solo_group`). A group with one owner plus other humans got nothing, even though the catalog is published under a per-user `ChatMember` scope that only the owner can see. The scope is already zero-leak, so the solo gate was stricter than the security model requires.

## Resolution (implemented)

| Change | File |
|---|---|
| `resolve_owner_id()`  -  explicit `bot_owner` first, then the first *parseable* allow-list entry; `"*"` and junk skipped, leading `+` normalised (#277) | `src/config/owner.rs` |
| `owner_user_id()` delegates to `resolve_owner_id`, so the per-user menu and the command-level owner gate resolve ownership identically | `src/channels/telegram/menu_scope.rs` |
| Owner menu no longer dies on a wildcard-prefixed allow-list (5 call sites) | `agent.rs`, `menu_auto.rs`, `menu_refresh.rs`, `member_events.rs` |
| Owner catalog published in non-solo groups too (per-user scope, zero leak), decision cached | `menu_auto.rs` |
| Menu chips added for `/approve`, `/skills`, `/rename`, `/restart`, `/exit` | `agent.rs` → `collect_command_catalog()` |
| New handlers: `/approve` (read/set `agent.approval_policy`), `/skills` (skill listing) | `commands.rs` |
| TUI-only commands (12-entry list + `/onboard*`) answer with an explanation, not "unknown command" | `commands.rs` → `is_tui_only_command()` / `tui_only_notice()` |
| Regression coverage | `src/tests/telegram_command_parity_test.rs`, `src/tests/owner_resolve_test.rs` |

Remaining by design: `/theme` (terminal colour presets), `/whisper`
(clipboard), `/rebuild` (local toolchain + process restart) and the setup
screens (`/projects`, `/tasks`, `/workspace`, `/channels`, `/voice`, `/image`,
`/daemon`, `/brain`, `/settings`) stay TUI-only, and now say so.
