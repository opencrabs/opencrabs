# Command Templates (opt-in, LLM-flavored)

Copy-paste `commands.toml` fragments for workflows that genuinely need the
LLM. Everything in here costs tokens on every invocation: that is the point
of this directory. The mechanical, zero-cost versions of these commands
(`/architecture`, `/attach`, `/audit`) ship **inside the binary** and need no
install.

This directory follows the same opt-in tier as `templates/cron/` and
`templates/optional-skills/`: nothing here is embedded, seeded, or wired into
runtime. Who wants it, copies it. (`templates/skills/` is the opposite tier:
those are compiled into the binary and reach every user.)

## Install

Append a fragment to `~/.opencrabs/commands.toml` (create the file if it does
not exist), then restart or reload:

```toml
[[commands]]
name = "/architecture-explain"
description = "Annotate a directory tree with one-line purposes (LLM)"
action = "prompt"
prompt = "..."
```

Safer alternative that cannot corrupt formatting: `config_manager` with an
`add_command` action, or the `/models`-style config tooling in your channel.

## Uninstall

Delete the `[[commands]]` block from `~/.opencrabs/commands.toml`.

## Cost note

Every `action = "prompt"` command is a full LLM round-trip (plus any tool
calls the model makes). The built-in mechanical commands cover the common
paths at zero cost; use these only when you want the model's judgment.

## Fragments

| File | Command | What it adds |
|------|---------|--------------|
| `architecture-explain.toml` | `/architecture-explain [path]` | Runs the built-in mechanical tree, then has the LLM annotate each entry with a one-line purpose |

Tested against: OpenCrabs v0.5.3 (2026-09-26). If a fragment references a
tool that no longer exists after an upgrade, delete the line or update the
prompt.
