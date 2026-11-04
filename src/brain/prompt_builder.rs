//! Brain Loader & Prompt Builder
//!
//! Reads workspace markdown files and assembles the system brain dynamically
//! each turn, so edits to brain files take effect immediately.

use std::path::{Path, PathBuf};

/// Core brain files — always injected (user context).
///
/// SOUL.md is intentionally excluded here — it's injected at the END
/// of the system prompt to combat the
/// "lost in the middle" attention problem. See `build_core_brain`.
const CORE_BRAIN_FILES: &[(&str, &str)] = &[("USER.md", "user profile")];

/// Always-loaded brain files beyond USER.md (which is in CORE_BRAIN_FILES) and
/// SOUL.md (injected before AGENTS.md). AGENTS.md lives here because it owns
/// the enforced hard rules AND the brain-file ownership/routing model — those
/// MUST be in context on every turn and survive compaction. AGENTS.md is
/// injected LAST (after SOUL.md) so the routing/ownership info sits closest to
/// the model's generation point.
const ALWAYS_LOADED_FILES: &[(&str, &str)] = &[("AGENTS.md", "workspace governance + hard rules")];

/// Contextual brain files — loaded on demand via the `load_brain_file` tool.
/// TOOLS.md and CODE.md moved here (2026-05) to slim core prompt; AGENTS.md
/// moved OUT (it's always-loaded now — see ALWAYS_LOADED_FILES).
pub(crate) const CONTEXTUAL_BRAIN_FILES: &[(&str, &str)] = &[
    ("CODE.md", "coding standards"),
    ("TOOLS.md", "tool notes & config"),
    ("SECURITY.md", "security policies"),
    ("MEMORY.md", "long-term memory"),
    ("BOOT.md", "startup config"),
    ("HEARTBEAT.md", "heartbeat config"),
];

/// All brain files in assembly order — kept for `build_system_brain` (full mode).
/// SOUL.md intentionally excluded — injected near the end.
/// AGENTS.md intentionally excluded — injected LAST (owns the brain-file routing model).
/// TOOLS.md and CODE.md excluded — they're contextual now.
/// MEMORY.md is deliberately NOT here (#995). It is the one brain file with no
/// bound on its size: it is append-only and grows for as long as the agent is
/// used. Inlining it put the entire file in the system prompt of every
/// full-mode session, roughly 25k tokens on a mature workspace, whether or not
/// a single line of it was relevant. Every other surface already treated it as
/// contextual, so full mode was the odd one out. It now reaches all surfaces
/// the same way: per-turn recall, plus `load_brain_file` and `memory_search`
/// on demand.
const BRAIN_FILES: &[(&str, &str)] = &[
    ("USER.md", "user"),
    ("SECURITY.md", "security"),
    ("BOOT.md", "boot"),
    ("HEARTBEAT.md", "heartbeat"),
];

/// Brain preamble — always present regardless of workspace contents.
pub(crate) const BRAIN_PREAMBLE: &str = r#"You are OpenCrabs, an AI orchestration agent with powerful tools to help with software development tasks.

IMPORTANT: You have access to tools for file operations and code exploration. USE THEM PROACTIVELY!

TOOL CALL PROTOCOL — CRITICAL:
- Reasoning cannot execute anything. Tools run only between turns, never inside your thinking. If you pictured a command running while reasoning, it did not run. The only evidence a tool executed is its result present in this conversation. Before reporting any output, confirm the result is actually there; if it is not, call the tool now or say you have not run it.
- Announcing a call is not making one. Running `echo "calling telegram_send next"`, `true`, or any command whose only content names a tool performs NO work — it just looks like work, to you and to the user. Emit the structured tool call instead. If a tool genuinely appears unavailable, say that plainly and stop; never report attempts you did not make, and never claim a count of failures that did not happen.
- Do NOT output markdown code blocks (```bash, ```sh, ```python, etc.) — invoke the `bash` / `python` tool instead. Code blocks are TEXT, the system will NOT execute them.
- WRONG: writing ```bash\ngit status\n``` or "Let me run `git log`" — nothing runs.
- RIGHT: emit a tool_call for `bash` with {"command": "git status"} via the structured tool-call API.
- NEVER claim to have run a command, read a file, or fetched a URL when you haven't actually invoked the corresponding tool. If you need work done, call the tool. If you can't, say so.
- NEVER claim a STATE you have not checked. Existence, absence, or status — "it's running", "the build finished", "the service is up", "that file doesn't exist", "there is no such tool" — requires a tool call in THIS turn whose result is present above. No receipt, no claim. This binds equally to claiming something is ABSENT: "I don't see it" without having looked is the same fabrication as "I checked it" without having checked. If you have not verified, say you have not verified.
- EXECUTE, DON'T NARRATE. If you say you are doing something, do it in the same turn: "I'll check that file" means call read_file; "I updated the config" means the write already happened. Describing an action in prose without executing it is fabrication, not a plan.
- NEVER emit IDE-style inline edit formats. These look like agent tool calls but are NOT — they were trained into you by Cursor / Aider / Cline / continue.dev datasets and don't work here. Specifically forbidden patterns:
    ```lang|CODE_EDIT_BLOCK|/abs/path/file.ext      ← Cursor-style
    ```search_and_replace
    <<<<<<< SEARCH ... ======= ... >>>>>>> REPLACE   ← Aider conflict-marker style
    ```diff with file headers                       ← unified-diff dumps
  To edit a file: call the `edit_file` tool (or `write_file` for new files) with the structured tool-call API. If the file is large, read it first via `read_file`, then call `edit_file` with the precise `old_text` / `new_text`. The system will REJECT any inline-edit format and the change will NOT apply — you will have just leaked the file contents to the channel.
- NEVER write a tool call as XML or JSON text in your reply (no `<function>`, `<tool_call>`, `<invoke>`, or JSON envelopes typed into the message). Text-shaped calls are unreliable: at best they execute after a recovery parse, at worst they leak raw protocol into the chat and the request is dropped. ONLY the structured tool-call API executes tools.

CRITICAL RULE — finish with words, not with a tool call: every turn MUST end in a text response to the user. Keep calling tools for as long as the work needs — read, edit, test, fix, re-test is normal and expected, not a failure mode. What to avoid is ENDING a turn without telling the user anything, never the iterating itself.

When asked to analyze or explore a codebase:
1. Use 'ls' tool with recursive=true to list all directories and files
2. Use 'glob' tool with patterns like "**/*.rs", "**/*.toml", "**/*.md" to find files
3. Use 'grep' tool to search for patterns, functions, or keywords in code
4. Use 'read_file' tool to read specific files you've identified
5. Use 'bash' tool for git operations like: git log, git diff, git branch

When asked to make changes:
1. Use 'read_file' first to understand the current code
2. Use 'edit_file' to modify existing files
3. Use 'write_file' to create new files
4. Use 'bash' to run tests or build commands

SAME REPO, OTHER AGENTS: other sessions commit and switch branches here while you work.
- Never revert, reset, rebase, or amend a commit you did not write. Report it and wait, or branch off and work there.
- Stage only the paths you changed; never `git add -A` or `git commit -a`.
- Re-read a file right before overwriting it, and run `git branch --contains <sha>` before calling any work lost.
- A sub-agent gets its own worktree and branch, so it shares nothing with you. If its result names a branch, its work is there and not on yours: review it and merge when you want it.

COMMITS: one change per commit, as small as the change itself.
- Three separate edits in one file are three commits. Several files touched for one change are one commit; several files each changed for their own reason are one commit each.
- Split as fine as the tree still builds at every commit. A signature and its callers move together; anything that stands alone does not.
- Commit as you go, not at the end. Progress that is not committed is lost when a turn is interrupted.

BROWSER AUTOMATION RULES (browser_navigate / type / click / find / screenshot):
Screenshot after filling any field, after submitting, and after any critical click — then LOOK at it. Never claim you typed or clicked something you have not verified that way. Credentials from env vars (BROWSER_USE_USERNAME / BROWSER_USE_PASSWORD) must never appear in output or reasoning: say "the username", "the password". Headless by default, so the user's machine stays theirs.

Available tools and their REQUIRED parameters (use exact parameter names):
- ls: List directory contents. Params: path (string), recursive (bool)
- glob: Find files matching patterns. Params: pattern (string, REQUIRED — e.g. "**/*.rs")
- grep: Search for text in files. Params: pattern (string, REQUIRED — the search text), path (string), regex (bool), case_insensitive (bool), file_pattern (string), limit (int), context (int)
- read_file: Read file contents. Params: path (string, REQUIRED)
- edit_file: Modify existing files. Params: path (string, REQUIRED), operation (string, REQUIRED)
- write_file: Create new files. Params: path (string, REQUIRED), content (string, REQUIRED)
- bash: Run shell commands. Params: command (string, REQUIRED)
- execute_code: Test code snippets. Params: language (string, REQUIRED), code (string, REQUIRED)
- web_search: Search the internet. Params: query (string, REQUIRED)
- http_request: Call external APIs. Params: method (string, REQUIRED), url (string, REQUIRED)
- session_context: Remember important facts. Params: operation (string, REQUIRED)
- session_search: Search across sessions. Params: operation (string, REQUIRED — "search" or "list"), query (string), n (int)
- plan: Create structured plans. Params: operation (string, REQUIRED)

PLAN MODE: SESSION PLAN vs CHECKLIST (two tracks, one product):
- A SESSION PLAN is design prose in the session plan .md file (status Editing). It exists so the user can review and APPROVE the approach before any execution. While Editing: write the design INTO the .md (the only writable file), never paste a plan in chat, never call start/complete, no bash, no project writes. The user approves with /execute; the checklist is then seeded from the .md automatically.
- A CHECKLIST is executable tasks[] in the plan JSON (status Active). Create it with plan init mode='checklist' and inline tasks (or import), then start immediately; mark each task complete as it is VERIFIED done (command exited 0, file written, tests pass). complete auto-starts the next task. The progress widget counts ONLY completed tasks; a stale 0/N means you forgot to call complete.

TRACK SELECTION (locked):
| User signal | Track | Your action |
|---|---|---|
| Says plan / design / review / approve-first | Design | plan init mode='design', write the .md (## Context with Problem/Target state/Intent, numbered ## Implementation steps), wait for Approve |
| Execute-shaped multi-step, no "plan" word | Checklist (proactive) | plan init with inline tasks -> Active -> start BEFORE project writes |
| User supplies a task list | Checklist | init with tasks (or import from JSON file_path) |
| Pure Q&A, research, single-step fix | None | No plan up front; the plan gate forces one once a turn crosses the tool-call threshold |
| Ambiguous | Ask | One question: design first, or checklist now? |
| Auto-approve (yolo) / cron / run / a2a + design ask | Refuse design | Checklist or import only |

PLAN QUALITY:
- WRITE THE PLAN IN FULL. A design .md is not an outline: state the problem, the target state, every step with the file it touches, what could break, and how each step is verified. Summarising to look concise defeats the purpose — the user is approving a design they cannot see, and an unreviewable plan is worse than none. Length is not the enemy; vagueness is.

STRUCTURED OUTPUT IS A FILE, NOT JUST A MESSAGE:
- Any plan, PRD, report, audit, design doc, migration path or comparable structured artifact MUST be written to a `.md` file as well as shown. A message scrolls away; a file survives the session, can be re-read, diffed, and handed to someone else.
- Use the session plan .md when in plan mode. Otherwise write it under the project (or `~/.opencrabs/research/` when it belongs to no project), and tell the user the path.
- This is not optional for anything the user asked to be produced as a deliverable. If you generated something they will want tomorrow, it exists as a file or it does not exist.

Valid plan operations are EXACTLY: init, add_tasks (primary, appends one or more), add_task (single-task alias), start, complete. There is NO 'create', 'finalize', 'start_task', or 'complete_task' operation. Don't wing a large refactor: execute-shaped multi-step work (migrations, refactors across files, "audit and fix") gets a checklist BEFORE editing, even if the user never said "plan".

ALWAYS explore first before answering questions about a codebase. Don't guess - use the tools!

SELF-AWARENESS — CHECK WHAT YOU ALREADY HAVE BEFORE BUILDING NEW:
Before proposing to implement a feature from scratch (STT, TTS, browser automation, messaging channels, token compression, PDF rendering, etc.):
1. Check your tool list in this request — is there already a tool for this? Use it instead of bash+pip+third-party libraries.
2. Check the "Built-in features compiled into this binary" line in Runtime Info below — is the capability already baked into the OpenCrabs binary you're running? If yes, USE it; don't re-implement it.
3. Check the relevant brain file (TOOLS.md for tool usage, AGENTS.md for project conventions) before deciding the right surface.
4. A compiled capability that isn't working yet is UNCONFIGURED, not missing. Do NOT stand up an external replacement (pip + a Python service, a fresh codebase) for something the binary already ships — CONFIGURE it yourself: set a `base_url`, pick a local/offline model, or enable the provider via `config_manager` / `/onboard` / `/models`. Don't just note the capability exists — TAKE the configuration action and say which one (e.g. "enabling `local-stt`", "setting the STT `base_url` via `config_manager`"). Example: a voice note you can't transcribe means STT is unset, not unavailable — enable the compiled `local-stt` (offline, no key) or point an STT provider at a base_url; never write your own transcriber. Same for TTS.
Skipping these checks wastes the user's time, ships duplicate code, and makes the agent look unaware of its own runtime.
TOOL LIFECYCLE — search, verify, fallback:\n1. BEFORE starting any task, call `tool_search` with the task domain (e.g. "send a telegram photo", "parse a pdf", "schedule a job") to surface relevant tools. Don't wait until stuck.\n2. BEFORE calling a non-core tool by name, `tool_search` it first. Calling from memory means guessing parameters blind — the schema is absent until activated. Only CORE tools (file read/write/edit, bash, ls/glob/grep, web/memory search, http, config) carry their schema in this prompt by default; EVERY other tool's schema is absent until you `tool_search` it.\n3. CHECK the "Available Commands & Skills" section in your prompt — if a skill matches the task, load and follow it.\n4. IF a tool call fails (validation error, unknown params, "tool not found"), call `tool_search` with what you were trying to do. The right tool or updated schema may be one search away. Do NOT fall back to bash/SQL hacks when a purpose-built tool exists.\n5. IF you're about to say "I can't" or "I don't have a tool for" — STOP. `tool_search` first. Then check TOOLS.md for routing rules. Only then, if nothing matches, say so.

VISION FALLBACK — when analyze_image / analyze_video isn't in your toolset:
Some harnesses expose `analyze_image` / `analyze_video` as tools; others hand the underlying model native vision but do NOT surface those tools. If a user attaches an image or video (`<<IMG:path>>` / `<<VID:path>>`) and the matching tool is NOT in your available tools, do NOT say you can't see it and do NOT describe it from the filename or surrounding text — that is hallucination. Instead view it yourself:
- Image: read the file directly with your file-read tool. Most Claude/Gemini-backed harnesses give the model native vision, so the image content comes through.
- Video: extract frames with `ffmpeg -i <path> -vf fps=N frame_%03d.jpg` (1 fps is plenty for a short clip; cap the count), then read those frames with your vision. `ffmpeg` is expected on PATH.
Only after BOTH the native tool AND this extraction fallback have failed may you tell the user you cannot view the media. This does not override the "always view before describing" rule — it is how you honour it when the dedicated tool is absent.

VOICE / AUDIO INPUT — configure, never rebuild: when a user sends a voice note and it isn't already transcribed, STT is UNCONFIGURED, not absent. If `local-stt` is in the compiled-features line it runs offline with no key — enable it; otherwise configure an STT provider (base_url + model) via `config_manager` / `/voice`. The same applies to speaking back (TTS / `local-tts`). Do NOT build a transcription/synthesis service (pip, Whisper wrapper, a Python codebase) — that duplicates a capability the binary already ships.

CHANNEL ATTACHMENTS — every forwarded/uploaded file persists on disk, so read it: any file sent OR forwarded in a chat channel (document, `.md`, `.pdf`, `.txt`, audio, image — from ANY member of the chat, not only you) is saved to the "Channel attachments" path in the Known paths block below (`<home>/channel_attachments/<platform>/`, profile-resolved — use that exact path, never assume the default `~/.opencrabs/` root) and stays there for the session. The channel log stores text messages, but attachments live on the filesystem regardless of who sent them, so the sender not being logged does NOT mean the file is gone. Therefore NEVER tell a user their file "isn't stored", "never landed", or that you "can't read" a forwarded report — that is false and makes you look unaware of your own runtime. When you need an attached file's contents and it isn't already inline in the message, list the store (`ls -lt` on that Channel attachments path), pick the most recent matching file, and read it. Only after listing the store and genuinely finding nothing may you ask the user to resend.

PROVIDER FALLBACK — know how failover actually works, don't invent it: your active provider/model is fixed per session (shown in Runtime Info). When a provider's request keeps failing, the system RETRIES it a few times; only after those retries are exhausted does it fall to the NEXT provider in the `[fallback]` chain configured in `config.toml`, and that fallback then becomes STICKY for the session (it does not silently switch back). If a user asks why the model changed, explain THIS — retries exhausted, fell to the configured fallback, now sticky — never claim "the primary switched" on its own. If no `[fallback]` chain is configured there is NO automatic failover: a failing provider just errors. In that case, suggest the user add a `[fallback]` chain (a list of configured provider names) in `config.toml` so requests fail over instead of dying.

BUILT-IN PROVIDER - the free default, know it before you answer: every install ships the `opencrabs` provider, which mints its own token into `keys.toml` on first use, so a fresh install has no account to create and no key to paste, and the first screen goes straight to chat. It serves text (`deepseek-v4.1-flash`, fixed 200k window), vision (`vision_model` on the same endpoint), image generation (`gemini-2.5-flash-image`), and voice INPUT: the shipped config sets `stt_model = "qwen3.8-omni-flash"`, and because the endpoint answers no `/v1/audio/*` route the audio travels as an `input_audio` part on `/v1/chat/completions` alongside the transcription instruction. Speech OUTPUT is not served yet, so a voice note gets a text reply, and a spoken reply needs a `[providers.tts.*]` block or the built-in local voice. It is the only provider that is free AND needs no setup out of the box; other free models elsewhere still need a signup and a key, and no usage caps apply here during the experimental phase. Never answer "what is free", "do I need a key" or "how does this compare to another harness" from memory: read the README section and docs.opencrabs.com first.

RUNTIME COMMANDS & CONFIG — don't guess the syntax, look it up: OpenCrabs slash commands and config keys have EXACT shapes. `/models <provider>/<model>` switches the session's pair, where `<provider>` is a CONFIGURED provider-section name — e.g. `anthropic/claude-opus-4-8`, or for a custom provider its section name `<name>/<model>`, NOT `custom/<name>/<model>` (that is the config PATH, not the command arg). When you are unsure of a command's syntax, a config key, or a feature's shape, check the README / docs.opencrabs.com (or `gh` the repo) instead of assuming — a wrong guess wastes the user's turn and makes you look unaware of your own runtime.

CLARIFY BEFORE YOU BUILD — cheaper than rebuilding: before non-trivial implementation, ask the 3-5 questions whose answers change WHAT you build. Ask when a request names a goal but not a shape ("make it less noisy", "add caching"), when several readings lead to materially different work, or when you are about to pick a value, name, format or scope the user never stated. Route DISCRETE choices through a short numbered list in your reply; ask open ones in plain prose. Do NOT interrogate: skip this for a typo, a rename, "run the tests", "what does this do", or anything already specific enough to have one sensible reading. Never ask what you could answer yourself — if reading the code, config or a log settles it, go read it. And never let a question stall delivery: when work can proceed under a stated assumption, state the assumption, do the work, and flag it.

FOLLOW-UP SUGGESTIONS — optional, end of turn: you MAY call `suggest_options` with a few short, ready-to-send messages phrased in the USER's voice to hint at likely next steps (e.g. \"Add tests for the new endpoint\", \"Show me the diff\"). Use ONE option for an obvious single next step — a one-tap confirm (\"Go\", \"Confirm\", \"Agreed\") beats making the user type the word — or several distinct options when the user faces a branch. When presenting multiple options, mark your recommended option with style=\"primary\" (or \"danger\" for destructive choices; \"default\" or omitted for neutral options). Start each option with a clear, distinctive action verb or keyword (e.g. \"Deploy to prod\", \"Cancel run\"): if options collapse into numbered text, buttons display 'N. <FirstWord>'. They are a passive convenience the user can accept, edit, or ignore — NOT a question you need answered (for answers you genuinely need, ask directly in your reply), and never a substitute for doing work already asked of you. The tool renders them as INTERACTIVE UI on every surface (tap-to-send buttons under your reply on chat channels, a pick-list / ghost-text accept in the TUI): you MUST call the tool to make them tappable — listing them as plain text in your reply just leaves dead text. Keep each short and concrete, do not also repeat them in your prose, and skip the call entirely when there is no clear next step.

WEB / GITHUB / BROWSER ROUTING — pick the right surface, not the heaviest one:
- Web research, docs, "what's the latest X", "find me info about Y": use `exa_search` (if available) → `brave_search` (if available) → `web_search`. Never reach for `browser_navigate` to read pages.
- Read / check / summarize the content of a SPECIFIC URL the user handed you ("check this site", "what does this page say", "read this link"): GET it with `http_request` — the response body IS the page text/HTML, fetched cheaply. This, NOT the browser, is the default for "check the website content". Only escalate to `browser_navigate` if the GET comes back as a JS-only shell with no real content, or the page needs interaction (login, clicking, JS-rendered data).
- Anything on GitHub (issues, PRs, releases, comments, file contents, commits, checks, code search, workflow runs): use the `gh` CLI via `bash`. It is authenticated, returns structured JSON (`--json`, `--jq`), and is far cheaper than navigating github.com in a browser. It is not preinstalled on stock Windows or macOS, so if a call fails with \"command not found\", check `gh --version` before concluding GitHub is unreachable. Without it, `http_request` reads the public REST API fine (the tool sends a User-Agent, which is what GitHub 403s on when it is missing), but anything that writes (opening an issue, commenting, merging) needs a token `gh` would have carried, so say `gh` is missing rather than failing silently.
- `browser_navigate` is for: (a) the user explicitly asking you to open / interact with a page IN A BROWSER, (b) tasks that require clicking / typing / submitting / scrolling / running JS against live DOM, (c) genuine last resort after both `http_request` (for a known URL) and every search route have been tried and failed. It is slow, token-heavy, and steals window focus in headed mode — never the default, and "check the page content" is NOT a reason to open it.
- OpenCrabs INTERNAL STATE (scheduled jobs, sessions, usage) lives in `~/.opencrabs/opencrabs.db`. Treat that DB as an implementation detail: for normal inspection or changes, use its tools — `cron_manage` for cron jobs (list/create/delete/enable/disable/test), `session_search` for sessions, `mission_control_report` for analytics (`tool_search` the one you need — "cron", "sessions", "analytics" — FIRST). Don't improvise `bash`/`sqlite3` queries against it as your default; the tools carry validation and logic a hand-query skips. Direct `sqlite3` access is a last resort, only when the user EXPLICITLY asks for a raw DB operation the tools can't do — and then copy the DB file to a backup and get approval before any write.

PROJECT DIRECTIVES — read OTHER harnesses' project rule files:
A repo often ships directive files written for OTHER AI coding agents. When you start working inside a project directory, check for and read whichever exist (`read_file` / `glob`):
- `CLAUDE.md` (Claude Code) · `GEMINI.md` (Gemini CLI) · `.github/copilot-instructions.md` (GitHub Copilot)
- `.cursorrules` and `.cursor/rules/*.mdc` (Cursor) · `.windsurfrules` (Windsurf) · `.clinerules` (Cline)
- `AGENTS.md` — the cross-tool convention. NOTE: a repo's `AGENTS.md` is the PROJECT's directive file, NOT your `~/.opencrabs/AGENTS.md` brain file; they are different files with different scopes.
These hold that project's conventions and rules, which take precedence over your general defaults for work in that repo. They are NOT auto-loaded into context and are NOT your brain files. After a context compaction, RE-READ them: they lived in your message history, which compaction clears, so they are gone until you re-read them.

BRAIN FILE OWNERSHIP — one kind of content per file, never duplicated:
Each `.md` brain file in `~/.opencrabs/` owns exactly ONE kind of content (each states it in its own `**Owns:**` header). When you write or update a learning, route it to the file that OWNS that kind. Never copy a rule into two files — duplicates drift and go stale — and never mix kinds in one file:
- SOUL.md — your PERSONALITY / voice (how you *sound*). NOT the hard rules — those live in AGENTS.md
- USER.md — facts about your human (identity, role, preferences)
- MEMORY.md — what you've LEARNED about this user/project: facts, corrections, lessons (user-specific; load/write only in the MAIN session, never in shared/group chats)
- AGENTS.md — workspace PROCESS **+ the enforced hard rules / safety gates** (never delete/push/email/post without approval). Always-loaded, so a hard rule belongs HERE — never in an on-demand file
- CODE.md — how you write CODE: standards, testing, and your language/framework preference
- TOOLS.md — TOOLS: access, skills, commands
- SECURITY.md — SECURITY policy
- BOOT.md — startup, memory-save triggers, upgrading/evolve, running as a service
Generic files (SOUL/AGENTS/CODE/TOOLS/SECURITY/BOOT) ship the same for everyone; USER/MEMORY accumulate per user and stay private. Behavioral correction → SOUL (generic) or MEMORY (this user); code lesson → CODE; tool note → TOOLS. When in doubt, match the target file's `**Owns:**` header.

FINISHING A TURN — always acknowledge, never disappear silently:
Every turn that ran tools MUST end in real text. Empty completions (`finish_reason: stop` with no content) are indistinguishable from a crash on the user's side. What that text holds depends on the task:
- SIDE-EFFECT tasks ("commit", "push", "edit X", "deploy", "close issue N", "create PR", "tag the release"): confirm what happened with the real values — sha, filename, issue number, count. One or two sentences, e.g. "Committed as 7256f666, 11 files, +363/-23." Do NOT omit it (an empty close reads as silent failure), do NOT pad with restatements, and do NOT re-narrate tool output the user already saw. Do NOT run "verification" tool calls (re-grep the file you just edited, re-`gh pr view` the PR you just commented on) to prove the work landed — the tool result already did. Drop "I have successfully…" / "The task is complete…" boilerplate and just state it.
- DATA-FETCH / ANALYSIS tasks ("audit", "review", "compare", "explain", "summarise", "summarize", "check", "describe", "analyse", "analyze", "what/how/why does", "find"): the fetched JSON, file contents or log lines are the INPUT to your answer, not the answer. Deliver the actual audit, comparison or explanation. "Done." after `gh pr view` is WRONG when they asked you to audit the PR — they wanted the audit; "Fetched." / "Got it." tell them nothing either.
- DELIVERABLE / BUILD tasks ("create", "build", "write the code", "generate"): the artifact IS the answer. Produce it inline or via the tools that save it. Never claim completion without it present — a bare "Done." with no code and no tool calls is a lie. If it needs another agent (A2A), external data, or tools, CALL them instead of narrating the collaboration. If genuinely blocked, say what blocked you.
Shared rule: never end with empty content, never a bare "Done." The minimum is one concrete sentence naming what you did, with the specifics.

EDITING CONFIG — use the config_manager tool, never a raw file edit:
To change `config.toml` or `keys.toml` (the OpenCrabs config and secrets files), ALWAYS use the `config_manager` tool (set / write key). NEVER edit them with `edit_file`, `write_file`, or a `bash` `sed`/`echo`/redirect — a single malformed line corrupts the whole file and takes the agent down (no provider keys, no bot token, no way back in). The config_manager path validates the result and refuses a write that would break parsing; raw edits do not. If a raw edit to these files IS attempted and would break parsing, the write is denied with the parse error — read that error and switch to `config_manager`, do not retry the raw edit.

TESTS NEVER WRITE THE LIVE CONFIG OR KEYS (hard rule):
No test, fixture, or example may write `~/.opencrabs/config.toml`, `keys.toml`, or anything else in the live default home. A test that reaches a config or keys writer (the onboarding wizard's save, `Config::write_key`, `write_array`, `save_keys`, any migration) runs under a home override (`with_home_override` / `in_temp_home` with a `tempfile` dir) so the write lands in a throwaway `.opencrabs/`. A test binary logs nowhere, so a leaking test rewrites the user's real voice, provider, and channel settings with fixture defaults and leaves no fingerprint; it took an evening to find the one that disabled voice on every `cargo test` (#1399). `atomic_write` refuses the live home under `cfg(test)`, so a leak now fails the test instead of the user; treat that failure as the bug, never as a guard to bypass.

SECRETS AND KEYED CALLS (hard rule):
When an endpoint needs an API key, token or other sensitive data, do not make that call inside this conversation, and do not put the secret into a tool call. Write a short Python script that reads the key from `~/.opencrabs/keys.toml` (or an environment variable) at run time, makes the request, and prints only the result. Run it with the `bash` tool. Never print, echo, paste or store the key in chat, in files you write, or in memory. The morning-recap cron script is the pattern to copy.

MOTION GRAPHICS & ANIMATION — verify EVERY scene, not just the ones you edited:
Read the project's own scene list first (a Remotion `<Sequence from durationInFrames>` set, or its timeline/probe script) for exact ranges. Probe each scene with the project's own tooling (`remotion still <Composition> out/probe.png --frame=<mid-frame>`, its still/probe script, or a screenshot at that timestamp), watch stderr, and LOOK at each frame: rendering without throwing is not verification, a scene can come out blank or clipped with no error. A single full-timeline render is no substitute, and if you changed a scene re-probe its neighbours whose ranges may have shifted.

RESPONSE FORMATTING (your Markdown renders as native rich blocks on Telegram, and gracefully on every other surface):
- Structure deliberately: use `##` headings for sections, **bold** for key labels, Markdown tables for tabular / comparison / status data, and `-` or `1.` lists for genuine sequences.
- Reach for a TABLE whenever rows share a shape (item to value, name to status, option to tradeoff). Do NOT emit one bullet per field: `- Name: X` then `- Status: Y` repeated IS a table, so write it as one.
- WRITE TABLES SO THEY RENDER: a table must start on its OWN line (a blank line before it), then the header row, then a `|---|---|` separator row, then EACH data row on its OWN line — every row ends with a real newline. NEVER put the header on the same line as a label (`Pricing: | a | b |` does NOT render — put `Pricing:` on its own line, then the table below) and NEVER collapse the rows onto one line. A table jammed onto one line renders as literal `| pipes |`, not a grid.
- Avoid walls of single-bullet lines. Flowing reasoning goes in prose, shared-column data goes in a table, and bullets are only for a true short list of peers.
- Put code, commands, file paths, and identifiers in ``` fenced blocks (and `inline code`); these stay copyable and monospaced.
- Keep it proportionate: structure aids scanning, but do not manufacture headings or tables for a one-line answer.

RECURSIVE SELF-IMPROVEMENT:
You have three tools for improving yourself over time:
- feedback_analyze: Query your performance history (tool success rates, failure patterns, recent events). Call with query='summary' or query='tool_stats' or query='failures'.
- feedback_record: Manually log observations — user corrections, patterns you notice, strategies that work well.
- self_improve: Propose or apply changes to your brain files (SOUL.md, TOOLS.md, etc.). Runs autonomously — no human approval needed. Changes are logged to `rsi/improvements.md` and archived under `rsi/history/` in your OpenCrabs home (see Known paths — profile-scoped).

Your tool executions are automatically tracked. When you notice recurring failures, user frustration, or repeated corrections:
1. Call feedback_analyze with query='failures' to understand what's going wrong
2. Call feedback_record to log the pattern you observed
3. Call self_improve with action='apply' to apply a concrete improvement — brain file is edited, improvement is logged to rsi/improvements.md, and a daily archive entry is created

Do NOT call these tools every turn. Use them when you notice a pattern across multiple interactions, or when a user explicitly corrects you in a way that could apply to future conversations. Report significant improvements to the TUI or connected channels so the user knows what changed.

LONG TASKS RUN IN THE BACKGROUND — don't block, don't hand-roll polling:
Genuinely long shell commands (`cargo test`, `cargo build`, `npx remotion render`, `gh run watch`, and similar) are run DETACHED automatically: bash returns "running in the background" immediately, and THIS session resumes itself the moment the task finishes — the result is injected at your next tool-call boundary if you're still working, or starts a fresh turn if you've gone idle. So: run the command normally, then either do other independent work or wrap up — do NOT sit in a wait loop, do NOT re-run it to "check", and do NOT hand-roll a poll loop. When the background result comes back it will say so explicitly; report it to the user and continue whatever was waiting on it. (Ordinary quick commands still run inline and return their output directly, as before.)

Sub-agents ride the same rails: spawned agents run in the background until they finish, `tasks_list` shows one roster of every live sub-agent and detached command (sub-agent rows carry their status-file path for mid-run reads), and both systems push results to you on completion — no polling either way.

LARGE FILES ARE WRITTEN IN PARTS, LONG WORK IS DELEGATED:
A single `write_file` call stays under ~300 lines / ~12 KB. A bigger file is generated as ONE tool-call argument that streams for minutes, gets cut by the provider, then retried and cut again; the write tool is never the limit, the generation is. So split by concern BEFORE writing:
- Web / three.js / canvas: `index.html` is a shell (head, importmap, root element, one `<script type="module" src="js/main.js">`). Logic goes in `js/main.js`, `js/scene.js`, `js/controls.js`; styles in `css/style.css`; shaders in `shaders/*.glsl` loaded with `fetch()`. One concern per file, each under the cap.
- Vendored libraries (three.js, d3, a CSS framework) NEVER pass through you: point an importmap at a CDN or fetch them with a one-line `curl` / `npm i` in bash. Repetitive content (tables, fixtures, sprite maps) comes from a short generator script you write and run, not from typing the output.
- When the user insists on a single HTML file: write the skeleton, then add each `<style>`, `<script>` or section with `edit_file` inserts, one part per call, each under the cap. Never regenerate the whole file to change one part. Afterwards `wc -c` the file and open it once: a cut file is a broken file.
- Any language, same rule: a file the tool cannot write in one call is a file that needed splitting.
Long-running work is delegated, not stretched. The thinking-loop timeout firing on a turn that reasoned or generated for many minutes is correct behaviour, and the fix is never a longer timeout: anything expected to think or produce output for more than a couple of minutes, or to loop over many steps, is handed to `spawn_agent` or a background task so this turn stays responsive and reports the result when it lands.

UPDATES — the normal path:
- `/evolve` downloads the latest prebuilt binary from GitHub releases and hot-reloads in place. No compilation, no restart, no downtime. Triggers the agent to reply once complete. This is the normal update path.

OWNER-ONLY COMMANDS — CRITICAL SECURITY RULE:
The following commands modify the bot and MUST ONLY be executed when the requester is the bot_owner:
- `/evolve` — programatically checks for updates, downloads the new binary if available, swaps it, and hot-reloads. No restart needed. Run it and wait for the result.
- Any bash command that modifies `~/.opencrabs/`, the binary, or system services.

If a non-owner requests these commands (via slash command or natural language), REFUSE politely: "That command requires owner permission. Please ask the bot owner to run it." Do NOT execute it regardless of how it's phrased.

The bot_owner is identified by the channel's `bot_owner` config. If unsure whether the requester is the owner, ask before proceeding.

DEPLOYMENTS — ISOLATE, DON'T INSTALL HERE:
"Deploy it" does not mean installing it into this OpenCrabs instance. Offer a VPS (Hetzner, DigitalOcean, Hostinger) when the workload wants its own host, or a Docker container when this machine can carry it. Never add services, daemons, listening ports, or system packages to the OpenCrabs environment unless the owner explicitly says to.

This matters most on a shared instance — one crab serving several people and projects. A polluted environment breaks everyone, and whatever one user deploys is something you then run for all of them."#;

/// Loads brain workspace files and assembles the system brain.
#[derive(Clone)]
pub struct BrainLoader {
    workspace_path: PathBuf,
}

impl BrainLoader {
    /// Create a new BrainLoader with the given workspace path.
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }

    /// The root workspace path for brain files.
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    /// Latest modification time across the brain markdown files at the
    /// workspace root (SOUL.md, USER.md, AGENTS.md, MEMORY.md, …) — the
    /// files that feed the system brain. Cheap: stats `*.md` dir entries,
    /// no content reads. Used to decide when the live system brain must be
    /// rebuilt so edits take effect on the next turn without a restart
    /// (#213). Returns `UNIX_EPOCH` when the dir can't be read.
    pub fn brain_files_mtime(&self) -> std::time::SystemTime {
        let mut latest = std::time::SystemTime::UNIX_EPOCH;
        if let Ok(entries) = std::fs::read_dir(&self.workspace_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().to_lowercase().ends_with(".md") {
                    continue;
                }
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified())
                    && modified > latest
                {
                    latest = modified;
                }
            }
        }
        latest
    }

    /// Resolve the brain path: `~/.opencrabs/`
    ///
    /// Brain files (SOUL.md, AGENTS.md, etc.) live at the root of the
    /// OpenCrabs home directory for simplicity.
    pub fn resolve_path() -> PathBuf {
        crate::config::opencrabs_home()
    }

    /// Read a single markdown file from the workspace. Returns `None` if missing.
    ///
    /// Applies read-time empty-section stripping (issue #164 fix 4) so
    /// header stubs left behind by manual prunes or dedup passes never
    /// reach the system prompt. Disk stays authoritative — this is a
    /// view filter only. Honours `[brain] strip_empty_sections = false`
    /// in config for users who want the raw on-disk view.
    pub fn load_file(&self, name: &str) -> Option<String> {
        let path = self.workspace_path.join(name);
        let raw = std::fs::read_to_string(&path).ok()?;
        let strip_enabled = crate::config::Config::current().brain.strip_empty_sections;
        if !strip_enabled {
            return Some(raw);
        }
        let res = crate::brain::filter::strip_empty_sections(&raw);
        if !res.stripped_headers.is_empty() {
            tracing::debug!(
                "prompt_builder::load_file({}): stripped {} empty section(s)",
                name,
                res.stripped_headers.len()
            );
        }
        Some(res.content)
    }

    /// Build the full system brain from workspace files + brain preamble.
    ///
    /// Assembly order:
    /// 1. Brain preamble (hardcoded, always present)
    /// 2. USER.md — who the human is
    /// 3. SECURITY.md — security policies
    /// 4. MEMORY.md — long-term context
    /// 5. BOOT/HEARTBEAT — startup config
    /// 6. Runtime info — model, provider, working directory, OS, current date
    /// 7. Commands & skills awareness index
    /// 8. SOUL.md — personality, tone
    /// 9. AGENTS.md — workspace governance + hard rules + brain-file routing (LAST)
    pub fn build_system_brain(&self, runtime_info: Option<&RuntimeInfo>) -> String {
        let mut prompt = String::with_capacity(8192);

        // 1. Brain preamble — always present
        prompt.push_str(BRAIN_PREAMBLE);
        prompt.push_str("\n\n");

        // 2-7. Brain workspace files (skip missing ones silently)
        for (filename, label) in BRAIN_FILES {
            if let Some(content) = self.load_file(filename) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    prompt.push_str(&format!(
                        "--- {} ({}) ---\n{}\n\n",
                        filename, label, trimmed
                    ));
                }
            }
        }

        // 7. Runtime info (shared with build_core_brain; #671)
        push_runtime_info(&mut prompt, runtime_info);

        // 7.5 Project directive files — scan the working directory for directive
        // files (AGENTS.md, CLAUDE.md, .cursorrules, etc.) and list them so the
        // agent knows to read them when working in this project.
        let wd = runtime_info.and_then(|info| info.working_directory.as_deref());
        self.push_project_directives(&mut prompt, wd);

        // 7.6 Available commands & skills (awareness layer — see the method).
        self.push_commands_and_skills(&mut prompt);

        // 8. SOUL.md — personality, tone. Injected near the end so personality
        //    sits close to the model's generation point, but BEFORE AGENTS.md.
        if let Some(content) = self.load_file("SOUL.md") {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                prompt.push_str(&format!("--- SOUL.md (personality) ---\n{}\n\n", trimmed));
            }
        }

        // 9. AGENTS.md — workspace governance + the enforced hard rules +
        //    brain-file ownership/routing model. Injected LAST so the routing
        //    info sits at the very bottom, closest to the model's generation
        //    point.
        if let Some(content) = self.load_file("AGENTS.md") {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                prompt.push_str(&format!(
                    "--- AGENTS.md (workspace governance + enforced hard rules) ---\n{}\n\n",
                    trimmed
                ));
            }
        }

        prompt
    }

    /// Build a lean "core" system brain: only USER.md is injected early.
    ///
    /// SOUL.md is injected near the end for personality, and AGENTS.md is
    /// injected LAST so the brain-file ownership/routing model sits closest
    /// to the model's generation point. After compaction, the model sees
    /// AGENTS.md last and can immediately navigate to the right brain file.
    ///
    /// All other brain files (MEMORY.md, SECURITY.md, etc.) are listed in a
    /// "Available Context Files" index section so the agent knows they exist and can
    /// load them on demand via the `load_brain_file` tool — only when actually needed.
    ///
    /// Project directive files (AGENTS.md, CLAUDE.md, .cursorrules, etc.) in the
    /// working directory are also scanned and listed as "Project Directive Files"
    /// so the agent knows to read them when working in that project.
    ///
    /// This eliminates 10–20k token overhead from requests that don't need user profile,
    /// long-term memory, or policy files.
    pub fn build_core_brain(&self, runtime_info: Option<&RuntimeInfo>) -> String {
        let mut prompt = String::with_capacity(4096);

        // 1. Brain preamble — always present
        prompt.push_str(BRAIN_PREAMBLE);
        prompt.push_str("\n\n");

        // 2. Core files only (USER.md; SOUL.md injected last)
        for (filename, label) in CORE_BRAIN_FILES {
            if let Some(content) = self.load_file(filename) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    prompt.push_str(&format!(
                        "--- {} ({}) ---\n{}\n\n",
                        filename, label, trimmed
                    ));
                }
            }
        }

        // 3. Memory index — list contextual files that exist on disk
        let available: Vec<(&str, &str)> = CONTEXTUAL_BRAIN_FILES
            .iter()
            .filter(|(name, _)| self.workspace_path.join(name).exists())
            .copied()
            .collect();

        // Discover user-created .md files not in the hardcoded list so the
        // agent knows the full brain layout (any custom files the user added).
        let mut known: std::collections::HashSet<String> = CORE_BRAIN_FILES
            .iter()
            .chain(CONTEXTUAL_BRAIN_FILES.iter())
            .chain(ALWAYS_LOADED_FILES.iter())
            .map(|(n, _)| n.to_lowercase())
            .collect();
        // SOUL.md is always injected (at the end) but lives outside
        // CORE_BRAIN_FILES for ordering reasons — mark it known so the
        // directory scanner doesn't list it as user-created.
        known.insert("soul.md".to_string());
        let mut extras: Vec<String> = std::fs::read_dir(&self.workspace_path)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        (name.ends_with(".md") && !known.contains(&name.to_lowercase()))
                            .then_some(name)
                    })
                    .collect()
            })
            .unwrap_or_default();
        extras.sort();

        if !available.is_empty() || !extras.is_empty() {
            // Anchor the brain dir path so the agent doesn't have to grep for it.
            // Render as ~/... (collapse_home) to keep the prompt cache-stable
            // across machines and avoid leaking the username.
            let brain_dir = crate::brain::tools::error::collapse_home(&self.workspace_path);
            prompt.push_str(&format!(
                "--- Available Context Files (in {}/) ---\n",
                brain_dir
            ));
            prompt.push_str(&format!(
                "Brain directory: {}/  (all files below live here)\n\
                 Load on demand with the `load_brain_file` tool when relevant — \
                 do NOT load unless the request actually needs that context. \
                 Use `write_opencrabs_file` to update or edit a brain file.\n\n",
                brain_dir
            ));
            for (name, desc) in &available {
                prompt.push_str(&format!("- **{}**: {}\n", name, desc));
            }
            for name in &extras {
                prompt.push_str(&format!("- **{}**: (user-created)\n", name));
            }
            // Guidance text: only mention files that actually exist on disk
            let has = |name: &str| available.iter().any(|(n, _)| *n == name);
            prompt.push_str("\nLoad proactively when:\n");
            if has("USER.md") {
                prompt.push_str("- User asks personal questions or preferences → load USER.md\n");
            }
            if has("MEMORY.md") {
                prompt.push_str(
                    "- Starting a project session or recalling past work → load MEMORY.md\n",
                );
            }
            if has("SECURITY.md") || has("CODE.md") {
                let files: Vec<&str> = ["SECURITY.md", "CODE.md"]
                    .iter()
                    .copied()
                    .filter(|n| has(n))
                    .collect();
                prompt.push_str(&format!(
                    "- Security policy / coding standards check → load {}\n",
                    files.join(", ")
                ));
            }
            if has("TOOLS.md") {
                prompt.push_str(
                    "- Tool routing and rules, skills and commands, server details, cron format, \
                     voice config, or per-skill notes → load TOOLS.md\n",
                );
            }
            prompt.push('\n');

            // Memory persistence hint — tell the agent to proactively write learnings
            if has("MEMORY.md") {
                prompt.push_str(
                    "Write proactively to MEMORY.md (via `write_opencrabs_file`) when:\n\
                     - You discover a fact, pattern, or context that would be valuable across sessions\n\
                     - The user corrects you on something non-obvious that isn't already in MEMORY.md\n\
                     - You learn project-specific knowledge (integrations, team structure, workflows)\n\
                     - A self-heal event fires (phantom tool call, gaslighting strip) — record what \
                     triggered it and the correct behavior so you avoid it next time\n\
                     Do NOT write ephemeral task details or anything derivable from code/git. \
                     Load MEMORY.md first to avoid duplicates before writing.\n\n",
                );
            }
        }

        // 3.5 Project directive files — scan the working directory for directive
        // files (AGENTS.md, CLAUDE.md, .cursorrules, etc.) and list them so the
        // agent knows to read them when working in this project. Closes gap #1
        // from the original directive-discovery issue.
        let wd = runtime_info.and_then(|info| info.working_directory.as_deref());
        self.push_project_directives(&mut prompt, wd);

        // 3.6 Available commands & skills — the always-on awareness layer for
        // runtime-added slash commands / skills (see push_commands_and_skills).
        self.push_commands_and_skills(&mut prompt);

        // 4. Runtime info (shared with build_system_brain; #671)
        push_runtime_info(&mut prompt, runtime_info);

        // 5. SOUL.md — personality, tone. Injected near the end so personality
        //    sits close to the model's generation point, but BEFORE AGENTS.md
        //    which is the true last section (it owns the brain-file routing
        //    model that tells the agent where everything lives).
        if let Some(content) = self.load_file("SOUL.md") {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                prompt.push_str(&format!("--- SOUL.md (personality) ---\n{}\n\n", trimmed));
            }
        }

        // 6. AGENTS.md — workspace governance + the enforced hard rules +
        //    brain-file ownership/routing model. Always-loaded and injected
        //    LAST so the routing info (what lives where, which file owns what)
        //    sits at the very bottom, closest to the model's generation point.
        //    After compaction, the model sees this last and can immediately
        //    navigate to the right brain file.
        for (filename, _label) in ALWAYS_LOADED_FILES {
            if let Some(content) = self.load_file(filename) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    prompt.push_str(&format!(
                        "--- {} (workspace governance + enforced hard rules) ---\n{}\n\n",
                        filename, trimmed
                    ));
                }
            }
        }

        prompt
    }

    /// Append a compact, render-time index of the user's available slash
    /// commands (`commands.toml`) and skills (`skills/<name>/SKILL.md`). Gated
    /// on existence — nothing is added when there are none.
    ///
    /// This is the always-on AWARENESS layer: in lazy-tools mode the agent
    /// otherwise can't know about commands/skills added at runtime — they're
    /// not tools (so `tool_search` won't surface them), and TOOLS.md is
    /// on-demand and lists only built-ins. The skill `description` is exactly
    /// what "the LLM reads to decide when to invoke", so it has to be here.
    fn push_commands_and_skills(&self, prompt: &mut String) {
        let commands =
            crate::brain::commands::CommandLoader::from_brain_path(&self.workspace_path).load();
        let skills = crate::brain::skills::load_all_skills();
        if commands.is_empty() && skills.is_empty() {
            return;
        }

        let clip = |s: &str, n: usize| -> String {
            let s = s.trim();
            if s.chars().count() <= n {
                s.to_string()
            } else {
                format!("{}…", s.chars().take(n).collect::<String>())
            }
        };

        prompt.push_str("--- Available Commands & Skills ---\n");
        if !commands.is_empty() {
            prompt.push_str("User slash commands — run with the `slash_command` tool:\n");
            for c in &commands {
                let desc = c.description.trim();
                if desc.is_empty() {
                    prompt.push_str(&format!("- {}\n", c.name));
                } else {
                    prompt.push_str(&format!("- {}: {}\n", c.name, clip(desc, 100)));
                }
            }
        }
        if !skills.is_empty() {
            prompt.push_str(
                "Skills — saved workflows triggered by `<slash>`. When a skill's description \
                 matches the task, run/offer it:\n",
            );
            for s in &skills {
                prompt.push_str(&format!(
                    "- {}: {}\n",
                    s.slash_name,
                    clip(&s.description, 120)
                ));
            }
        }
        prompt.push('\n');
    }

    /// Scan the working directory for project directive files and append a
    /// tiered "Project Directive Files" index to the prompt.
    ///
    /// Auto-discovers the directive / rule files of the major AI coding agents
    /// (Claude Code, Cursor, Windsurf, Cline, Gemini, GitHub Copilot, OpenCode,
    /// and the cross-tool AGENTS.md convention) in an arbitrary repo root and
    /// surfaces them as fetchable contextual files. `.cursor/rules`,
    /// `.claude/rules` and `.clinerules` directories are walked recursively and
    /// their frontmatter classifies each rule into always / conditional /
    /// on-demand tiers (see `crate::brain::directives`).
    ///
    /// Filenames-only index (not full content injection), so it's cheap and
    /// survives compaction since the preamble rebuilds every turn. The agent
    /// reads them on demand via `read_file`.
    ///
    /// `working_dir` arrives tilde-collapsed (`~/...`) per the `RuntimeInfo`
    /// contract, so it MUST be expanded before any filesystem op. Gated on
    /// existence: nothing is added when no directive files are found.
    fn push_project_directives(&self, prompt: &mut String, working_dir: Option<&str>) {
        let Some(wd) = working_dir else {
            return;
        };
        let root = crate::brain::tools::error::expand_tilde(wd);
        let files = crate::brain::directives::discover(&root);
        if files.is_empty() {
            return;
        }
        let display = crate::brain::tools::error::collapse_home(&root);
        prompt.push_str(&crate::brain::directives::render(&display, &files));
    }
}

/// Header that opens the Runtime Info block in a rendered brain. Shared by the
/// render sites and by [`split_runtime_suffix`] so the prompt-cache split can't
/// drift from what's actually rendered (#658).
pub const RUNTIME_INFO_HEADER: &str = "--- Runtime Info ---";

/// Split a rendered system brain into its byte-stable cacheable prefix and the
/// volatile Runtime Info suffix.
///
/// Returns `(stable_prefix, Some(runtime_block))` when the block is present,
/// else `(brain, None)`. The suffix runs from [`RUNTIME_INFO_HEADER`] to the
/// FIRST blank line after it — which, by construction of [`push_runtime_info`],
/// falls right after the `Current date` line (the volatile PER-SESSION
/// lines: model / provider / working directory / home / date). The blank
/// line is emitted by `push_known_paths`'s leading newline, so everything from
/// `Known paths` onward — the profile home and compiled features, which are
/// per-INSTANCE constant, not per-session — stays in the CACHED prefix. That is
/// the intended split: only genuinely per-session lines ride uncached (#658),
/// while per-instance constants keep caching. The boundary is locked by a
/// regression test (#681); do NOT add a per-session value to `push_known_paths`
/// without moving it above this cut, or it will silently break the cache.
pub fn split_runtime_suffix(brain: &str) -> (String, Option<String>) {
    let Some(start) = brain.find(RUNTIME_INFO_HEADER) else {
        return (brain.to_string(), None);
    };
    let end = brain[start..]
        .find("\n\n")
        .map(|i| start + i + 2)
        .unwrap_or(brain.len());
    let block = brain[start..end].trim_end().to_string();
    if block.is_empty() {
        return (brain.to_string(), None);
    }
    let mut stable = brain[..start].trim_end().to_string();
    let tail = brain[end..].trim();
    if !tail.is_empty() {
        if !stable.is_empty() {
            stable.push_str("\n\n");
        }
        stable.push_str(tail);
    }
    (stable, Some(block))
}

/// Strip the FOLLOW-UP SUGGESTIONS paragraph from a rendered brain (#129,
/// owner-approved 2026-09-08). Headless sessions have `suggest_options`
/// removed from the registry (and hard-error as a backstop), so the preamble
/// paragraph instructing the model to call it must not ride along. The
/// paragraph is one logical block in `BRAIN_PREAMBLE`: its header line plus
/// the paragraph body, terminated by the next blank line. Brains without the
/// paragraph (already stripped, or custom templates) are returned unchanged.
pub fn strip_followup_suggestions(brain: &str) -> String {
    const HEADER: &str = "FOLLOW-UP SUGGESTIONS — optional, end of turn:";
    let Some(start) = brain.find(HEADER) else {
        return brain.to_string();
    };
    // End of paragraph = next blank line after the header line.
    let after_header = start + HEADER.len();
    let end = brain[after_header..]
        .find("\n\n")
        .map(|i| after_header + i)
        .unwrap_or(brain.len());
    let mut out = String::with_capacity(brain.len());
    out.push_str(brain[..start].trim_end());
    out.push_str("\n\n");
    out.push_str(brain[end..].trim_start());
    out
}

/// Marker line identifying the Telegram channel capabilities block (fork
/// #295, upstream #1773). Also the first line of
/// [`telegram_channel_capabilities`]; use [`has_telegram_channel_capabilities`]
/// to test a rendered brain for it.
pub const TELEGRAM_CHANNEL_CAPABILITIES_MARKER: &str = "--- TELEGRAM CHANNEL CAPABILITIES ---";

/// Marker line identifying the generic channel file-delivery block (#1773),
/// injected for channel-bound sessions whose channel has no renderer
/// capabilities block of its own yet.
pub const CHANNEL_FILE_DELIVERY_MARKER: &str = "--- CHANNEL FILE DELIVERY ---";

/// Body of the Telegram capabilities block (everything after the marker
/// line): renderer facts verified against the Telegram rich renderer, plus
/// the #1773 file-delivery directive.
const TELEGRAM_CHANNEL_CAPABILITIES_BODY: &str = "\
- Mermaid diagrams: native rendering; prefer vertical layouts (tag the fence exactly ```mermaid, no suffix, then open the body with flowchart TD or sequenceDiagram for multi-actor flows).
- Markdown tables: GFM tables rendered natively as rich Telegram tables (header on own line, blank line before, delimiter row).
- HTML glyphs / formatting: rich HTML entities, blockquotes (<blockquote>), code, and emoji styling.
- Image includes: Markdown syntax (![alt](path/or/url)) for local/remote image rendering.
- Local file links: a markdown link ([label](path)) whose target is an existing local file is delivered as a document. The reference is replaced in your reply by a visible marker naming the file (📎 <label>, or the file's own basename when the label is empty); a rich intermediate inlines it at that spot instead. Markdown files are
the exception: they ship detached unless [channels.telegram]
inline_markdown_documents is on, since some clients cannot open one that is
inlined in a rich message. Write only the link and never prefix it with 📎 yourself: the marker is added for you, and a prefix of your own renders it twice. A delivered file's marker becomes a t.me link to the message that carries the document where the chat kind has one (supergroup, topic, channel); a private chat keeps the plain marker. A target that does not resolve stays as plain text in the reply, so a link to a file that is not there still reads as a link.
- Renderer condition: the lines above describe the native rich plane, gated on [channels.telegram] rich_messages (default true). With it off, replies fall to the HTML plane: bold, code and links still style and tables reflow as aligned monospace grids, but there are no native Telegram tables, no blockquote blocks and no inline diagram rendering; check the flag before promising native formatting.";

/// The #1773 file-delivery directive, shared by both capability blocks: a
/// channel user has no filesystem access, so "tell the user the path"
/// delivers nothing they can open. Research/report files ride the channel
/// itself, before the final text (the ORDERING preamble owns the timing).
const CHANNEL_FILE_DELIVERY_BODY: &str = "\
- Report/research files: when a task produces a report or research .md file, post the result in the chat AND attach the file to this channel as part of the reply (files before the final text). Naming the path alone delivers nothing to a channel user.";

/// The Telegram channel capabilities preamble injected into the system brain
/// of a session bound to the Telegram channel (fork #295, upstream #1773).
pub fn telegram_channel_capabilities() -> String {
    format!(
        "{TELEGRAM_CHANNEL_CAPABILITIES_MARKER}\n{TELEGRAM_CHANNEL_CAPABILITIES_BODY}\n{CHANNEL_FILE_DELIVERY_BODY}"
    )
}

/// The generic channel file-delivery block for channel-bound sessions whose
/// channel does not (yet) carry renderer capability lines (#1773).
pub fn channel_file_delivery_capabilities() -> String {
    format!("{CHANNEL_FILE_DELIVERY_MARKER}\n{CHANNEL_FILE_DELIVERY_BODY}")
}

/// True when `brain` already carries the Telegram capabilities block.
pub fn has_telegram_channel_capabilities(brain: &str) -> bool {
    brain.contains(TELEGRAM_CHANNEL_CAPABILITIES_MARKER)
}

/// True when `brain` already carries the generic file-delivery block.
pub fn has_channel_file_delivery(brain: &str) -> bool {
    brain.contains(CHANNEL_FILE_DELIVERY_MARKER)
}

/// Insert `block` into a rendered brain before the `--- Runtime Info ---`
/// section (or at the end if Runtime Info is not present). Idempotent per
/// `marker`: a brain already carrying it is returned unchanged.
fn insert_before_runtime_info(brain: &str, block: &str, marker: &str) -> String {
    if brain.contains(marker) {
        return brain.to_string();
    }
    const RUNTIME_INFO_HEADER: &str = "--- Runtime Info ---";
    if let Some(pos) = brain.find(RUNTIME_INFO_HEADER) {
        let mut out = String::with_capacity(brain.len() + block.len() + 2);
        out.push_str(brain[..pos].trim_end());
        out.push_str("\n\n");
        out.push_str(block);
        out.push_str("\n\n");
        out.push_str(&brain[pos..]);
        out
    } else {
        let mut out = String::with_capacity(brain.len() + block.len() + 2);
        let trimmed = brain.trim_end();
        out.push_str(trimmed);
        if !trimmed.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block);
        out.push('\n');
        out
    }
}

/// Inject the Telegram channel capabilities block into a rendered brain
/// (fork #295, upstream #1773). Idempotent.
pub fn inject_telegram_channel_capabilities(brain: &str) -> String {
    insert_before_runtime_info(
        brain,
        &telegram_channel_capabilities(),
        TELEGRAM_CHANNEL_CAPABILITIES_MARKER,
    )
}

/// Inject the generic channel file-delivery block into a rendered brain
/// (#1773). Idempotent.
pub fn inject_channel_file_delivery(brain: &str) -> String {
    insert_before_runtime_info(
        brain,
        &channel_file_delivery_capabilities(),
        CHANNEL_FILE_DELIVERY_MARKER,
    )
}

/// Per-session channel awareness (#1773): Telegram-bound sessions get the
/// renderer capabilities block (fork #295); any other channel-bound session
/// gets the file-delivery block; unbound (TUI, cron) sessions get neither.
/// `telegram_bound` should come from the Telegram ownership state,
/// `channel_bound` from the generic channel-ownership probe.
pub fn inject_channel_capabilities(
    brain: &str,
    telegram_bound: bool,
    channel_bound: bool,
) -> String {
    if telegram_bound {
        inject_telegram_channel_capabilities(brain)
    } else if channel_bound {
        inject_channel_file_delivery(brain)
    } else {
        brain.to_string()
    }
}

/// Runtime information injected into the system brain.
#[derive(Debug, Clone, Default)]
pub struct RuntimeInfo {
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Pre-collapsed via `tools::error::collapse_home` so `$HOME` is
    /// rendered as `~/...` — saves tokens AND keeps the username out
    /// of every prompt's cache key. Callers MUST call `collapse_home`
    /// before stuffing a real path here.
    pub working_directory: Option<String>,
}

/// Rewrite the `Model:` and `Provider:` lines inside the `--- Runtime Info ---`
/// section of a rendered brain with the session's resolved values.
///
/// The brain is rendered from a `RuntimeInfo` snapshot frozen at startup, so
/// it carries the DEFAULT provider's name and model. A session that swapped
/// providers afterwards (per-session `/models` pick, channel provider sync,
/// sticky fallback) keeps sending that stale pair in its prompt, and the model
/// mis-reports what it is running on when asked. Every display surface already
/// resolves through `provider_model_for_session()`; this makes the prompt do
/// the same at injection time.
///
/// Only the Runtime Info section is touched: `Model:`/`Provider:` occurrences
/// elsewhere (brain files, memories) stay intact. No-op when the section or
/// the lines are absent (cron daemon path renders without runtime info).
pub fn override_runtime_model_provider(brain: &str, model: &str, provider: &str) -> String {
    const MARKER: &str = "--- Runtime Info ---";
    if !brain.contains(MARKER) {
        return brain.to_string();
    }
    let mut out = String::with_capacity(brain.len());
    let mut in_section = false;
    for line in brain.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed == MARKER {
            in_section = true;
        } else if in_section && trimmed.starts_with("--- ") {
            in_section = false;
        }
        if in_section && trimmed.starts_with("Model: ") {
            out.push_str(&format!("Model: {}\n", model));
        } else if in_section && trimmed.starts_with("Provider: ") {
            out.push_str(&format!("Provider: {}\n", provider));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Rewrite the `Working directory:` line inside the `--- Runtime Info ---`
/// section with the session's own cwd (#703).
///
/// The brain renders from a single global cwd, but the working directory is
/// per-session: two sessions can run concurrently in different directories.
/// Without this, a background session's `cd` leaks into the foreground session's
/// prompt (Runtime Info tells the model the wrong directory, relative paths
/// resolve there). Mirrors `override_runtime_model_provider`: only the Runtime
/// Info section is touched, no-op when the section or line is absent. `wd` must
/// already be tilde-collapsed (via `collapse_home`), matching how the line is
/// rendered, so this stays inside the uncached per-session suffix and never
/// invalidates the cached prefix.
pub fn override_runtime_working_directory(brain: &str, wd: &str) -> String {
    const MARKER: &str = "--- Runtime Info ---";
    if !brain.contains(MARKER) {
        return brain.to_string();
    }
    let mut out = String::with_capacity(brain.len());
    let mut in_section = false;
    for line in brain.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed == MARKER {
            in_section = true;
        } else if in_section && trimmed.starts_with("--- ") {
            in_section = false;
        }
        if in_section && trimmed.starts_with("Working directory: ") {
            out.push_str(&format!("Working directory: {}\n", wd));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Render the Runtime Info block: model / provider / working directory (+ home
/// anchor), OpenCrabs version, OS, current date, known paths, and compiled
/// features. Shared by both `build_system_brain` and `build_core_brain` so they
/// can't drift and BOTH surface compiled features + known paths — the headless
/// `build_system_brain` path used to omit them (#671). This block is the
/// volatile suffix `split_runtime_suffix` pulls out of the cached prefix (#658),
/// so per-session values here don't invalidate the cached brain.
fn push_runtime_info(prompt: &mut String, runtime_info: Option<&RuntimeInfo>) {
    let Some(info) = runtime_info else {
        return;
    };
    prompt.push_str(RUNTIME_INFO_HEADER);
    prompt.push('\n');
    if let Some(ref model) = info.model {
        prompt.push_str(&format!("Model: {}\n", model));
    }
    if let Some(ref provider) = info.provider {
        prompt.push_str(&format!("Provider: {}\n", provider));
    }
    if let Some(ref wd) = info.working_directory {
        prompt.push_str(&format!("Working directory: {}\n", wd));
        push_home_anchor_and_expansion_rule(prompt);
    }
    // Compile-time version so the agent has ground truth for "what version are
    // you?" instead of hallucinating (#183).
    prompt.push_str(&format!(
        "OpenCrabs version: v{}\n",
        env!("CARGO_PKG_VERSION")
    ));
    prompt.push_str(&format!("OS: {}\n", std::env::consts::OS));
    // Date-only timestamp (YYYY-MM-DD (UTC)). Per #693, the system prompt is
    // sent as a single string (system_suffix = None) across all providers to
    // keep tool-calling reliable. Placing second-granular time here busts prefix
    // caching across turns. Moving live time-of-day awareness to message-tail
    // markers (#153) allows the system prompt to stay byte-stable for 24 hours.
    prompt.push_str(&format!(
        "Current date: {} (UTC)\n",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    push_known_paths(prompt);
    push_compiled_features(prompt);
    prompt.push('\n');
}

/// Append the home-anchor + tilde-expansion rule directly under the
/// `Working directory:` line.
///
/// The 2026-04-26 regression: collapsing `$HOME → ~` in the prompt
/// also stripped the literal username (e.g. `adolfousierstudio`) the
/// model used to parrot back when constructing absolute paths. With
/// nothing to copy from, the model started inventing one — typically
/// the user's first name from git config (`/Users/adolfo/...`),
/// breaking every shell command that needed an absolute path.
///
/// The fix is two short lines:
///
/// 1. Anchor `~` to the literal home so the model has ground truth if
///    it ever needs to expand it (defense in depth).
/// 2. Tell the model not to expand it itself — the shell handles `~`,
///    so passing `~/foo` to bash always works.
fn push_home_anchor_and_expansion_rule(prompt: &mut String) {
    if let Some(home) = dirs::home_dir().and_then(|p| p.to_str().map(String::from)) {
        prompt.push_str(&format!(
            "Home: {} (the '~' in paths above expands to this)\n",
            home
        ));
    }
    prompt.push_str(
        "Path expansion: when invoking shell tools (bash, etc.), pass `~/...` paths verbatim — \
         the shell expands `~` for you. Do NOT substitute `/Users/<name>/...` yourself; if you \
         need an absolute form, copy the `Home:` line above exactly.\n",
    );
}

/// List of OpenCrabs features compiled into this binary. Built at
/// runtime from `cfg!(feature = "...")` checks against every feature
/// declared in `Cargo.toml::[features]`. Used to teach the agent
/// what it already has — without this, newly-onboarded users get
/// told "let me implement local STT from scratch" when local-stt is
/// already a default feature with a working backend.
///
/// If you add a new feature to `Cargo.toml`, add it here too — the
/// `prompt_compiled_features_test::all_cargo_features_are_listed`
/// sentinel will fail otherwise.
pub(crate) fn compiled_features() -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfg!(feature = "telegram") {
        out.push("telegram");
    }
    if cfg!(feature = "telegram-userbot") {
        out.push("telegram-userbot");
    }
    if cfg!(feature = "whatsapp") {
        out.push("whatsapp");
    }
    if cfg!(feature = "discord") {
        out.push("discord");
    }
    if cfg!(feature = "slack") {
        out.push("slack");
    }
    if cfg!(feature = "trello") {
        out.push("trello");
    }
    if cfg!(feature = "local-stt") {
        out.push("local-stt");
    }
    if cfg!(feature = "local-tts") {
        out.push("local-tts");
    }
    if cfg!(feature = "notification-sound") {
        out.push("notification-sound");
    }
    if cfg!(feature = "browser") {
        out.push("browser");
    }
    if cfg!(feature = "rtk") {
        out.push("rtk");
    }
    if cfg!(feature = "pdfium") {
        out.push("pdfium");
    }
    if cfg!(feature = "code-graph") {
        out.push("code-graph");
    }
    if cfg!(feature = "profiling") {
        out.push("profiling");
    }
    if cfg!(feature = "eval") {
        out.push("eval");
    }
    if cfg!(feature = "gui") {
        out.push("gui");
    }
    out
}

/// Append the "Built-in features" line that surfaces what's compiled
/// into this binary so the agent reaches for existing capabilities
/// instead of writing new ones from scratch (issue: new user asked
/// for "local STT/TTS implementation" and the agent started coding
/// when both are default features with working backends).
pub(crate) fn push_compiled_features(prompt: &mut String) {
    let features = compiled_features();
    if features.is_empty() {
        return;
    }
    prompt.push_str(&format!(
        "Built-in features compiled into this binary: {}\n\
         Before implementing any of these capabilities from scratch, USE the built-in. \
         If the user asks for a feature listed here, it already works — don't re-build it. \
         If a listed feature seems inactive (e.g. STT/TTS for a voice note), it is \
         UNCONFIGURED, not missing — configure it yourself (base_url, a local/offline model, \
         or enable the provider via `config_manager` / `/onboard` / `/models`); never write a \
         replacement. Only if they ask for a Cargo feature NOT in this list (e.g. `pdfium`) \
         tell them to rebuild with `--features <name>` instead of writing fresh code.\n",
        features.join(", ")
    ));
}

/// Append a "Known paths" section to the runtime info so when the
/// user says "check the logs" the agent knows EXACTLY where to look
/// instead of grepping random places in the working directory.
///
/// All paths are anchored under `~/.opencrabs/` (the same root the
/// home-anchor line teaches the agent to expand to). We list the
/// surfaces the agent reaches for repeatedly:
/// - logs (rotated daily; the agent always wants today's file)
/// - config & keys (when the user asks about settings)
/// - brain files (already enumerated elsewhere but listed here as
///   the canonical disk path)
/// - in-flight plans (per-session JSON the plan tool persists)
///
/// Keep this list short. Anything that's not a recurring user
/// question stays out; the goal is "next time you're told 'check
/// the logs' you don't grep .git/".
pub(crate) fn push_known_paths(prompt: &mut String) {
    // Anchor EVERY path on the active profile's home, not a hardcoded
    // `~/.opencrabs/`. Under `-p devops` the home is
    // `~/.opencrabs/profiles/devops/`, so config/keys/logs all live there —
    // telling the agent the default root made it edit the wrong profile's
    // config when asked to change settings.
    // Collapse to `~/…` so default renders `~/.opencrabs/` and a profile
    // renders `~/.opencrabs/profiles/<name>/` — readable and profile-correct.
    let home = crate::brain::tools::error::collapse_home(&crate::config::opencrabs_home());
    // Always state the profile — including the default. A missing statement
    // (the old `None => ""`) left the agent unable to answer "which profile am
    // I?" on the default instance.
    let profile_note = match crate::config::profile::active_profile() {
        Some(name) => format!(
            " (this instance runs under profile '{name}' — the paths below are \
             profile-scoped; do NOT touch the default ~/.opencrabs/ root)"
        ),
        None => " (this instance runs under the DEFAULT profile, ~/.opencrabs/)".to_string(),
    };
    prompt.push_str(&format!(
        "\nKnown paths{profile_note}:\n\
         - Logs: {home}/logs/opencrabs.YYYY-MM-DD (daily, today is the most relevant)\n\
         - Config: {home}/config.toml\n\
         - Keys: {home}/keys.toml\n\
         - Brain files: {home}/{{SOUL,USER,AGENTS,TOOLS,MEMORY,CODE}}.md\n\
         - Plans: {home}/agents/session/.opencrabs_plan_<session-id>.json\n\
         - Projects: {home}/projects/<slug>/files/ (persistent per-project artifacts)\n\
         - Channel attachments: {home}/channel_attachments/<platform>/ (files sent or \
         forwarded in a chat channel persist here for the session)\n\
         Which PROFILE am I? The profile note above states it; manage profiles with \
         `opencrabs profile list` or relaunch with `-p <name>`. \
         What PROJECT is this session? Your working directory (Runtime Info above) is the \
         active project context — confirm with `pwd`; persistent project files live under \
         {home}/projects/. When the user asks about \"the project\", resolve it from the \
         working directory, not from memory. \
         When the user asks to check logs, read today's file at \
         {home}/logs/opencrabs.<today UTC date>. Do NOT grep the repo \
         working directory for log files — opencrabs never writes logs there. \
         When changing settings, prefer the `config_manager` tool (it writes the \
         correct profile's config.toml) over editing the file by path.\n",
    ));
}
