# TUI ↔ Channel slash-command parity

Inventory taken from source on 2026-10-31 (v0.5.15 tree); refreshed for
upstream main on 2026-11-23.

- **TUI autocomplete list:** upstream moved it to `src/ui_core/commands.rs` →
  `SLASH_COMMANDS` (36 entries, now including the GUI setup screens
  `/projects`, `/tasks`, `/workspace`, `/channels`, `/voice`, `/image`,
  `/daemon`, `/brain`, `/settings`; `/rebuild` was retired upstream)
- **TUI-visible channel-only list:** `src/tui/app/state.rs` → `CHANNEL_COMMANDS` (3 entries)
- **Channel dispatcher:** `src/channels/commands.rs` → `handle_command()` (plus `/cowork`, intercepted earlier in `src/channels/telegram/handler.rs`)
- **Telegram menu catalog:** `src/channels/telegram/agent.rs` → `collect_command_catalog()`

Total TUI command surface: **37** (34 + 3). Every one appears below.

## Legend

- **TUI** — handled by `src/tui/app/messaging.rs` (`handle_slash`).
- **TG handler** — accepted by the channel dispatcher, i.e. it does something useful in chat.
- **TG menu** — published in Telegram's `/` picker by `collect_command_catalog()`.
- **Gap** — what is missing.

## Gap table

| Command | TUI | TG handler | TG menu | Gap / needs-what |
|---|---|---|---|---|
| `/new` | yes | yes (`NewSession`) | yes | — |
| `/cd` | yes | yes (`/cd [path]`) | yes | — |
| `/sessions` | yes | yes (`/sessions[:query]`) | yes | — |
| `/compact` | yes | yes (`Compact`) | yes | — |
| `/clear` | yes | yes (`ClearContext`) | yes | — |
| `/theme` | yes | no | no | TUI-only: terminal colour presets have no chat meaning. Needs a polite "TUI-only" reply instead of "Unknown command". |
| `/stop` | yes | yes (`Stop`) | yes | — |
| `/plan` | yes | yes (`PlanMode`) | yes | — |
| `/show-plan` | yes | yes (`/show-plan`, `/show_plan`, `/showplan`) | yes (`show_plan`) | — |
| `/execute` | yes | yes (`Execute`) | yes | — |
| `/discard` | yes | yes (`Discard`) | yes | — |
| `/help` | yes | yes (`Help`) | yes | — |
| `/models` | yes | yes (`/models`, `/models <provider/model>`) | yes | — |
| `/usage` | yes | yes (`/usage`, `provider`, `model`, `7d`) | yes | — |
| `/mission-control` | yes | yes (`/mission-control`, `/mission_control`) | yes (`mission_control`) | — |
| `/onboard` | yes | no | no | TUI-only: multi-screen wizard (provider/workspace/channels/voice/image/brain) with pickers. Needs a "run it in the TUI, or use /models + edit config" reply. |
| `/onboard:provider` | yes | no | no | Same as `/onboard`. |
| `/onboard:workspace` | yes | no | no | Same as `/onboard`. |
| `/onboard:channels` | yes | no | no | Same as `/onboard`. |
| `/onboard:voice` | yes | no | no | Same as `/onboard`. |
| `/onboard:image` | yes | no | no | Same as `/onboard`. |
| `/onboard:brain` | yes | no | no | Same as `/onboard`. |
| `/doctor` | yes | yes (`Doctor`) | yes | — |
| `/cowork` | yes | yes (`telegram/handler.rs`, DM-only intercept) | yes | — |
| `/approve` | yes | **no** | no | **Real gap.** Tool approval policy is global config (`agent.approval_policy`); chat has no way to read or set it. Needs a handler: show policy, accept `ask`/`auto-session`/`auto-always`. |
| `/rebuild` | yes | no | no | TUI-only: needs a local Rust toolchain and restarts the running process; irresponsible to expose from chat. Needs a "TUI-only" reply. |
| `/evolve` | yes | yes (`Evolve`) | yes | — |
| `/restart` | yes | **yes (`Restart`)** | **no** | Handler exists, chip missing from the catalog. |
| `/exit` | yes | **yes (`Exit`)** | **no** | Handler exists (also `/quit`), chip missing from the catalog. |
| `/whisper` | yes | no | no | TUI-only: clipboard capture. Needs a "TUI-only" reply. |
| `/skills` | yes | **no** | no | **Real gap.** Skills are listed in the menu already (auto-registered from `SKILL.md`), but there is no command that enumerates them with descriptions in chat. Needs a read-only listing handler. |
| `/rtk` | yes | yes (`Rtk`) | yes | — |
| `/goal` | yes | yes (`/goal <args>`) | yes | — |
| `/profiles` | yes | yes (`Profiles`) | yes | — |
| `/respond_to` | channel-only | yes (`/respond_to [all\|mention\|auto]`) | yes | — (already documented under CHANNEL COMMANDS) |
| `/redact` | channel-only | yes (`/redact <scope> <on\|off>`) | yes | — |
| `/rename` | channel-only | **yes (`/rename <title>`)** | **no** | Handler exists, chip missing from the catalog. |

Count check: 37 rows, matching 34 `SLASH_COMMANDS` + 3 `CHANNEL_COMMANDS`.

## Summary of actual work

| Class | Commands | Action |
|---|---|---|
| Already complete | 22 | none |
| Handler exists, missing from the Telegram menu | `/restart`, `/exit`, `/rename` | add catalog entries |
| Missing handler, meaningful in chat | `/approve`, `/skills` | implement in `src/channels/commands.rs` |
| Missing handler, TUI-only by nature | `/theme`, `/whisper`, `/rebuild`, `/onboard` + 6 sub-screens | reply with a "TUI-only" explanation instead of "Unknown command" |

## Blocking bug found while inventorying

`register_scoped_menus()` (`agent.rs`) and both `menu_auto.rs` call sites resolve the owner as:

```rust
tg.allowed_users.first().and_then(|s| s.parse::<u64>().ok())
```

With the shipped convention that `allowed_users` may start with the wildcard `"*"` (e.g. `["*", "1608111860"]`), `.first()` yields `"*"`, the parse fails, and the function returns **before publishing any owner menu at all** — the Telegram `/` picker is left showing only `/start`. Owner detection must skip non-numeric entries instead of trusting index 0.

Second structural limit: unconfigured groups only receive the owner catalog when the group is *solo-owner* (`menu_auto::evaluate_solo_group`). A group with one owner plus other humans gets nothing, even though the catalog is published under a per-user `ChatMember` scope that only the owner can see. The scope is already zero-leak, so the solo gate is stricter than the security model requires.

## Resolution (implemented)

| Change | File |
|---|---|
| `owner_user_id()` — first *parseable* allow-list entry wins, `"*"` and junk skipped | `src/channels/telegram/menu_scope.rs` |
| Owner menu no longer dies on a wildcard-prefixed allow-list | `agent.rs`, `menu_auto.rs`, `menu_refresh.rs`, `member_events.rs` (5 call sites) |
| Owner catalog published in non-solo groups too (per-user scope, zero leak), decision cached | `menu_auto.rs` |
| Menu chips added for `/approve`, `/skills`, `/rename`, `/restart`, `/exit` | `agent.rs` → `collect_command_catalog()` |
| New handlers: `/approve` (read/set `agent.approval_policy`), `/skills` (skill listing) | `commands.rs` |
| TUI-only commands answer with an explanation, not "unknown command" | `commands.rs` → `is_tui_only_command()` / `tui_only_notice()` |
| Regression coverage | `src/tests/telegram_command_parity_test.rs` |

Remaining by design: `/theme` (terminal colour presets), `/whisper` (clipboard),
`/rebuild` (local toolchain + process restart) and `/onboard` + its six screens
(multi-screen wizard) stay TUI-only, and now say so.