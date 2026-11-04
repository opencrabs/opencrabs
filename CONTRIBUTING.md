# Contributing to OpenCrabs

Thank you for your interest in contributing to OpenCrabs! We welcome contributions from the community — but we have clear standards to keep the project moving forward efficiently.

## Before You Do Anything

**Read this entire document.** PRs that ignore these guidelines will be closed without review.

## Types of Contributions

### 1. Bug Reports (Issues Only)

Found a bug? Open an issue — **do not submit a PR yet**. The bug report form
(`.github/ISSUE_TEMPLATE/bug_report.yml`) asks for every item below, so use it
rather than a blank issue: it is the fastest route to a confirmed, fixable
report.

**Required information:**
- Clear, descriptive title
- Steps to reproduce (exact commands, config, inputs)
- Expected vs. actual behavior
- Environment: OS, Rust version (`rustc --version`), OpenCrabs version (`opencrabs --version`)
- Full error messages and logs (redact API keys)
- Screenshots if it's a TUI/visual issue

### 2. Feature Requests (Issues Only — No Code)

Have an idea for a new feature? Open an issue with the `feature` label. The
feature request form (`.github/ISSUE_TEMPLATE/feature_request.yml`) asks for
exactly this information.

**What to include:**
- What problem does this solve?
- How should it work from the user's perspective?
- Why is this useful to OpenCrabs users broadly (not just your use case)?

**What NOT to do:**
- Do not submit a PR with stub/placeholder code for a feature that doesn't exist yet
- Do not submit empty implementations with `todo!()`, `vec![]`, or `unimplemented!()`
- Do not submit PRs that add files with no actual logic, no tests, and no integration

**Stub PRs will be closed immediately.** If you want a feature built but don't have the skills to implement it, that's totally fine — open an issue, describe what you need, and the community or maintainers will pick it up. A well-written issue is 10x more valuable than a stub PR.

### 3. Code Contributions (PRs)

PRs are welcome for:
- Bug fixes (reference the issue number)
- Feature implementations (must have a linked issue approved by a maintainer first)
- Performance improvements (with benchmarks showing the improvement)
- Test coverage improvements
- Documentation fixes

## Issues Must Be Atomic

**One issue = one atomic piece of work.** A feature can have dozens of issues — each tracking one bug, one sub-task, or one improvement. Don't bundle multiple pieces of work into a single issue.

**Why:** Atomic issues are easier to track, prioritize, and close independently. A single "mega-issue" covering 5 bugs becomes a blocker when only 3 are fixed.

**Good:**
- `#376 — split_message byte boundary`
- `#377 — Rich API retry`
- `#378 — strip_html_tags incomplete`

**Bad:**
- `#375 — Fix all plain-text fallback bugs` (covers 3 separate issues)

**PRs can reference multiple atomic issues:**

```markdown
## Summary
Fixes message delivery cascade that caused plain-text fallback.

Fixes #376 (byte/char boundary), #377 (rich API retry), #378 (HTML tag stripping).

## Changes
- `split_message()` now uses `chars().count()` instead of `.len()`
- Added `RetryAfter` handling to rich path
- Completed `strip_html_tags()` to handle all HTML elements
```

This lets you batch related fixes in one PR while keeping the issue tracker clean and atomic.

### Issue Titles

Issue titles use the same [Conventional Commits](https://www.conventionalcommits.org/) shape as commit messages: `<type>(<scope>): <what is wrong or wanted>`. The type is one of `fix`, `feat`, `docs`, `refactor`, `test`, `perf`, `build`, `ci`, `revert`. There is no `chore`: dependency work is `build`, CI work is `ci`, formatting sweeps are `refactor`, and anything that fits no type gets a plain explanatory title instead. Mark breaking changes with `!` after the type (`feat!: remove /rebuild`) and describe what breaks in the body. The scope is the module or surface (`core`, `tui`, `provider`, `rsi`, `telegram`, `memory`).

```
fix(tui): copy-to-clipboard notice shifts the chat history three rows
feat(provider): send tool_stream to z.ai so tool-call arguments stream
docs: README test counts are stale
refactor(memory): mod.rs is declarations-only
```

A bare area prefix (`TUI:`, `z.ai:`, `Reasoning stream:`) is not the convention. The type makes the tracker filterable and lets the fixing commit reuse the title verbatim. Add labels on creation (`--label bug --label tui`), one for the type and one for the area.

Both halves of the shape are enforced in CI: `.github/workflows/title-lint.yml` runs on issue and pull request open/edit and fails the check on a bare area prefix, using the same rule (`governance.toml` `[titles]`) that `scripts/policy-lint.py` applies to commit subjects. A title that is already wrong cannot become a compliant commit, because the fixing commit reuses it verbatim.

## Where Issues Go

OpenCrabs is developed alongside a **dev factory** — the tooling, CI, and automation that build, test, and release the binary. Issues are split by what they describe:

- **The binary** — runtime behaviour, channels, providers, TUI, memory, tools — belongs in this repository.
- **The dev factory** — its tooling, CI, release automation, and process — belongs in [`leshchenko1979/opencrabs-dev-factory`](https://github.com/leshchenko1979/opencrabs-dev-factory).

The historical issue portfolio lives at `leshchenko1979/opencrabs` and is read-only; new issues do not go there.

### Sweep for duplicates first

**Search before you file** — open *and* closed, issues *and* PRs:

```bash
gh search issues --repo opencrabs/opencrabs "<terms>" --state all
gh search prs --repo opencrabs/opencrabs "<terms>" --state all
```

A closed hit that already fixed the same defect means *reference it*, not *re-file it*. A duplicate costs a review cycle and splits the record across two threads.

### Mark core-surface changes

A change that touches a core runtime surface — the agent loop, provider layer, channels, memory, config, migrations — is reviewed more carefully than docs or tooling. Mark it in the title so triage can route it:

```
fix core(provider): tool-call arguments drop the final delta
feat core(memory): expose vector index stats to /doctor
```

`fix core` and `feat core` are the two core prefixes; every other change uses the plain type.

### Issue references in code

A code comment explains what the code does and why, in words. An issue number can be added to that explanation, but it cannot replace it: a reader should understand the code without opening the ticket.

- A bare `#N` in source means an issue on `opencrabs/opencrabs`. Check that the issue exists there before you cite it.
- Do not cite numbers from the historical fork tracker. If the history behind a comment matters, recreate that issue on `opencrabs/opencrabs` and cite the new number.

```rust
// Before: the number is the only explanation
// #2027: the per-row cache-read count behind the By Model cache share.

// After: the reason is in the comment; the number is optional
// The per-row cache-read count behind the By Model cache share. Rows
// written before the column existed carry no count, so they are left out.
```

## Issue & PR Lifecycle

1. **File one atomic issue** — one issue, one piece of work (see above).
2. **Wait for a maintainer to confirm** it is real and not a duplicate.
3. **Get assigned** — assignment is the public signal that the issue is claimed. Do not comment "I'll take this"; the assignment says it.
4. **Branch and fix** — a short-lived branch off `main` in your fork, with atomic commits.
5. **Open the PR** — reference the issue in the body, explain *why* the change is needed, and add the `core` prefix if it applies.
6. **Merge closes the issue** — because the issue and the PR live in the same repository, `Fixes #N` closes it on merge.

**Commenting:** the only comments expected on an issue are the approved design (for non-trivial work) and per-commit implementation notes. Status chatter — "working on it", "+1", "any update?" — belongs nowhere.

## Step-by-Step: Submitting a Bug Fix

1. **Find or create the issue** — Check existing issues first. If none exists, create one.
2. **Wait for confirmation** — A maintainer will confirm it's a real bug and not a duplicate.
3. **Fork and branch** — Fork the repo, create a branch from `main` (not `master`).
4. **Fix the bug** — Keep changes minimal. Fix the bug, nothing more.
5. **Add a test** — Write a test that fails without your fix and passes with it.
6. **Run CI checks locally** (see below).
7. **Submit the PR** — Reference the issue, explain what you changed and why.

## Step-by-Step: Submitting a Feature

1. **Open an issue first** — Describe the feature, get maintainer approval.
2. **Discuss the design** — For non-trivial features, discuss the approach in the issue before writing code.
3. **Fork and branch** from `main`.
4. **Implement fully** — The feature must work end-to-end. No stubs, no placeholders, no "TODO: implement later".
5. **Add tests** — Unit tests at minimum, integration tests for complex features.
6. **Run CI checks locally** (see below).
7. **Submit the PR** — Reference the issue, include before/after screenshots for UI changes.

## Development Setup

### Prerequisites

- **Rust** 1.95 or later (edition 2024; source of truth: `rust-version` in `Cargo.toml`)
- **SQLite** (bundled via `rusqlite`)
- **Git**

### Toolchain

`rust-toolchain.toml` pins an exact compiler version, and all 11 `toolchain:` inputs in `.github/workflows/` name the same version. Your local `cargo` and CI therefore compile with the same rustc, and a new Rust release cannot change the shipped binary without a commit here.

The pin is deliberate. The glibc floor of the Linux binary is a property of the toolchain that built it, so a floating `stable` can raise that floor the same way a rolled runner image did in v0.5.13 (#2206).

To bump it, in one commit:

1. Set `channel` in `rust-toolchain.toml` to the new version.
2. Set every `toolchain:` value in `.github/workflows/` to match (`grep -rn 'toolchain:' .github/workflows/` finds all of them).
3. Update `rust-version` in `Cargo.toml` only if the new compiler raises the minimum the code needs.
4. Run `cargo fmt --all -- --check`, the clippy gate below, and `cargo test --all-features`.

Those four places are the whole bump, so nothing is left floating after it.

### Build & Test

```bash
# Clone your fork
git clone https://github.com/YOUR_USERNAME/opencrabs.git
cd opencrabs

# Iterate locally
cargo clippy --all-features         # USE THIS — never `cargo check` or `cargo build`
cargo test --all-features            # run the suite (incl. your new tests)
cargo fmt --all                      # auto-format before committing

# Run the EXACT CI checks before submitting a PR
cargo fmt --all -- --check                # soft-fail in CI today, still expected on your diff
cargo clippy --locked --lib --bins --tests --examples --all-features -- -D warnings
cargo test --locked --profile ci --all-features --verbose
```

**`cargo clippy` and `cargo test` are hard CI gates: they must pass.** PRs with failing CI will not be merged. We'll comment on the PR explaining what's failing and how to fix it. Push the fix, wait for CI to go green, and the PR will be reviewed. The `cargo fmt --check` step is a soft-fail in CI today (`continue-on-error: true` in `ci.yml`): legacy unformatted code would otherwise block every PR until a fmt sweep lands on main, after which it becomes a hard gate. Format the code you touch anyway; an unformatted diff gets bounced in review.

`cargo clippy` is the lint pass we trust — `cargo check` only type-checks and misses the lint rules CI enforces. Iterate with clippy locally so you don't burn a CI run discovering a `-D warnings` failure.

#### Why clippy, not `cargo check`

Most Rust users learned `check` before they met clippy, so the habit dies hard. The distinction is not style policing: clippy runs rustc itself (so it reports everything `cargo check` does) plus ~700 lints, and the [`clippy::correctness` group](https://doc.rust-lang.org/clippy/lints.html) is deny-by-default and aborts compilation: "if you see a correctness lint, it means that your code is outright wrong or useless". Those lints are deliberately low-FP, and `#[allow]`-ing them is discouraged. A clean `cargo check` still passes code like:

- `let _ = parking_lot_mutex.lock();` - the guard drops on the next line, so the "locked" section protects nothing. `clippy::let_underscore_lock` (correctness) aborts; `cargo check` is silent.
- `#[derive(PartialOrd)]` alongside a hand-written `impl Ord` that disagrees with it. Sorting and `BTreeMap` keys then violate their ordering invariants at runtime. `clippy::derive_ord_xor_partial_ord` (deny-by-default) catches it; `cargo check` is silent.
- `assert_eq!(a, a)`, the test that passes whatever the code does. `clippy::eq_op` flags identical operands.
- Quadratic loops and needless clones: "code that can be written to run faster" is `clippy::perf`'s job. Neither check nor the test suite notices until the profiler does, months later.

Even rustc admits its own gap: per the [cargo book](https://doc.rust-lang.org/stable/cargo/commands/cargo-check.html), some diagnostics are only emitted during code generation, which `cargo check` skips. That is why the CI gate is `cargo clippy ... -D warnings` (`.github/workflows/ci.yml`), and why this repo's own tests carry deliberate `#[allow(clippy::field_reassign_with_default)]` suppressions in `src/tests/onboarding_*_test.rs`: the lint fires here, and each allow is a conscious call, not an oversight.

A codebase built with `cargo check` alone can report *thousands* of clippy findings on its first run, so the time to start is the first commit. If you're inheriting a backlog, `cargo clippy --fix` mechanically applies the machine-applicable suggestions, so the wall is smaller than it looks.

### Running the App While You Iterate

**Use `cargo run`. Do NOT `cargo build --release` for normal development.**

```bash
cargo run --all-features                 # default debug build, fastest iteration
cargo run --all-features -- /sessions    # pass CLI flags after `--`
cargo run --all-features -- -p hermes    # named profile
```

`cargo run` compiles a debug build and launches the TUI directly. Debug builds compile ~5x faster than release, and the extra instrumentation (full backtraces, debug assertions) makes any regression you introduce surface immediately. The debug binary is what you want when chasing a bug — release optimizations strip frames that make panics impossible to read.

**Brain files just work.** `cargo run` resolves `~/.opencrabs/` (or `~/.opencrabs/profiles/<name>/` with `-p`) the same way the installed binary does. Your existing config, brain files, sessions, and memory are all picked up — no copy step, no env var override needed.

#### `target/` Grows Fast on the Debug Loop

The `cargo run` loop is cheap in time and expensive in disk. Nothing in the toolchain prunes stale artifacts, so `target/` accumulates across every build, every feature combination, and every dependency bump. A maintainer checkout measured **33 GB** in September 2026: 29 GB in `target/debug`, 4.3 GB from a separate CI profile.

`--all-features` is the multiplier. The local STT/TTS, `pdfium`, and browser dependency trees are large, and building them debug-mode with full debug info is what fills the volume.

Keep it under control:

```bash
du -sh target                  # check before it surprises you
cargo clean                    # nuke the whole tree (next build is a full rebuild)
cargo clean -p opencrabs       # drop only this crate's artifacts, keep deps compiled
```

`cargo clean -p opencrabs` is the one you usually want: it keeps the dependency graph compiled, so the next `cargo run` recompiles our crate only and takes seconds instead of minutes.

If your home volume is small, move the tree somewhere bigger instead of cleaning it constantly:

```bash
export CARGO_TARGET_DIR=/path/to/big/volume/opencrabs-target
```

Put that in your shell profile and every `cargo run` / `cargo test` in the repo writes there instead. Release builds deserve the same treatment: run `cargo clean` before `cargo build --release` so a stale debug tree is not sitting next to the release artifacts you are about to produce.

#### When You Actually Want a Release Build

Reach for `cargo build --release` only when:

- You want to **test the optimized binary** (perf-sensitive feature work, startup time benchmarks, etc.).
- You want to **replace your installed binary with the latest from `main`** so the long-running daemon / channels keep running the new code. The flow:
  ```bash
  git pull origin main
  cargo build --release --all-features
  # macOS / Linux: replace ~/.cargo/bin/opencrabs (or wherever your install lives)
  cp target/release/opencrabs ~/.cargo/bin/opencrabs
  ```
  The release binary reads the same `~/.opencrabs/` as `cargo run`, so your brain files, sessions, and config carry over seamlessly.

For everything else — bug hunting, feature work, test-driven development — stay on `cargo run`. Release builds are a deployment step, not a dev loop.

### Where Tests Live

**Every test goes under `src/tests/` as a dedicated `*_test.rs` file**, registered in `src/tests/mod.rs`. No inline `#[cfg(test)] mod tests { ... }` blocks at the bottom of source files — they are explicitly forbidden by project policy because they hide behind the source file in IDE outlines and grow unbounded.

To add a new test file:

```bash
# 1. Create the test file
$EDITOR src/tests/my_feature_test.rs

# 2. Register it in src/tests/mod.rs (alphabetical-ish neighbourhood)
echo "pub mod my_feature_test;" >> src/tests/mod.rs   # or insert manually

# 3. If the test needs internal helpers from the module under test,
#    bump those helpers from `fn` / `pub(super)` to `pub(crate)` so the
#    test file can reach them without weakening the public API.

# 4. Verify
cargo test --all-features my_feature_test
```

If you find an existing inline `#[cfg(test)] mod tests` while working on a file, move it into `src/tests/` as part of your change. Leaving the violation in place will fail review.

### Tests Never Touch the Live Config or Keys

**No test may write `~/.opencrabs/config.toml`, `keys.toml`, or anything else in the live default home.** Any test that reaches a config or keys writer (the onboarding wizard's save, `Config::write_key`, `Config::write_array`, `save_keys`, a migration) must run under a home override so the write lands in a throwaway directory:

```rust
let dir = tempfile::tempdir().unwrap();
let home = dir.path().join(".opencrabs");
std::fs::create_dir_all(&home).unwrap();
crate::config::profile::with_home_override(home, || {
    // drive the wizard / call the writer here
});
```

A test binary logs nowhere, so a leaking test silently rewrites the developer's real settings with fixture defaults. Two wizard tests did exactly that and disabled voice on every `cargo test` run for an evening before anyone found the writer (#1399). `atomic_write` now refuses a path directly in the live default home under `cfg(test)`; if your test hits that refusal, the test is wrong, not the guard. Never disable or work around it.

### Commit Discipline — Atomic Commits

**Always do atomic commits:** repository-wide atomic commits (grouping all files changed for a single logical change) combined with short-lived feature branches or stacked pull requests.

Repository-wide means the unit is the logical change, not the file. Every file a change touches lands in the same commit, so the tree builds and the tests pass at every commit and a revert or bisect can land on a single sha. Three unrelated edits in one file are three commits; one change spread across ten files is one commit. Each commit branches off `main` on a short-lived branch and lands as its own PR, or as one PR in a stack when later commits depend on earlier ones.

- **Don't bundle** `cargo fmt` drift with feature work. Run fmt in its own commit (`refactor: cargo fmt`).
- **Don't bundle** rename / move / restructure with logic changes. The reviewer cannot tell what's mechanical and what's behavioural.
- **Split test additions from production fixes only if the test would compile against the un-fixed code.** Otherwise commit them together so the test demonstrates the fix.
- **Commit message body explains the WHY**, not the diff. The diff already shows what changed; the message should answer "why was that wrong?" and "what would break if we reverted this?".
- **Never add `[skip ci]` to a commit.** The pipeline is fast and cheap to run; a skipped commit is an unverified commit, and on a release commit `[skip ci]` also skips the release workflow.
- **Never add `Co-Authored-By` lines** to commit messages. Project policy.

### Project Structure

```
src/
├── main.rs              # Binary entry point
├── lib.rs               # Crate root
├── app/                 # Application lifecycle / startup
├── brain/               # AI agent core
│   ├── agent/           # Agent orchestration, tool loop, context management
│   ├── provider/        # LLM providers (Anthropic, OpenAI-compatible, Copilot, custom)
│   ├── tools/           # Built-in tools (bash, edit, browser, memory_search, etc.)
│   ├── goal/            # Autonomous goal-completion loop
│   └── mission_control/ # Data services behind the Mission Control TUI panels
├── channels/            # Messaging + voice (Telegram, WhatsApp, Discord, Slack, Trello, voice)
├── a2a/                 # Agent-to-Agent protocol (agent card, JSON-RPC task API, HTTP gateway)
├── cli/                 # Command-line interface (Clap)
├── config/              # Configuration (config.toml + keys.toml)
├── cron/                # Cron scheduler — polls cron_jobs and runs due jobs
├── db/                  # SQLite database layer (SQLx)
├── error/               # Error types (OpenCrabsError, ErrorCode)
├── eval/                # Offline evaluation harness (context/memory quality; feature-gated)
├── logging/             # Conditional logging system
├── memory/              # Long-term memory (FTS5 + vector search via qmd)
├── migrations/          # SQLite migrations
├── rtk/                 # Rust Token Killer — compresses bash output to save context tokens
├── services/            # Business logic (Session, Message, File, Plan)
├── tui/                 # Terminal UI (ratatui)
├── usage/               # Usage analytics dashboard
├── utils/               # Shared utilities
├── tests/               # All tests, one *_test.rs per module (see TESTING.md)
├── benches/             # Criterion benchmarks
├── assets/              # Icons, screenshots, visual assets
├── scripts/             # Build and setup scripts
├── docker/              # Dockerfile + compose.yml
├── evals/               # Eval datasets / fixtures
└── docs/                # Documentation templates + reference/ architecture docs
```

## Coding Standards

### Rust Conventions

- **Files**: `snake_case.rs` — never PascalCase, never camelCase
- **Structs/Enums**: `PascalCase`
- **Functions/Variables**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Error handling**: `anyhow::Result` for application errors, `thiserror` for typed errors
- **Async**: `tokio` runtime — never block in async functions
- **`mod.rs` is for module declarations ONLY — functions NEVER live in `mod.rs`. Ever.** A `mod.rs` file may contain exactly: the module doc comment, `mod`/`pub mod`/`pub(crate) mod` declarations, and `pub(crate) use` re-exports of the submodule surface. Zero `fn` definitions. When a function grows in `mod.rs`, that is the signal it belongs in a named submodule (`detect.rs`, `gate.rs`, whatever the cohesion says) — create the file, move the fn, re-export it from `mod.rs` so call sites don't change. Reference implementation: `src/channels/telegram/rich/mod.rs`. If you find functions in a `mod.rs` while working on it, moving them out is part of your change — leaving the violation in place will fail review.

### What We Value

- **Working code** over clever code
- **Minimal diffs** — change only what's needed
- **Tests that prove the fix** — not tests for the sake of coverage
- **Comments that explain why**, not what

### Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/), the same shape as issue titles (see **Issue Titles** above):

```
feat: add voice message support for Discord channel
fix: prevent duplicate message rendering for CLI providers
refactor: simplify tool loop iteration tracking
```

## AI Agents and Agentic Contributions

Agent-written code is welcome. Agent-run accounts are not.

**We reject AI agent profiles.** An account that exists to run an agent is not a contributor: vendor bot identities, `[bot]` accounts, and personal accounts created only to relay an agent's output all fall under this. Issues and PRs opened from one are closed without review. There is no separate agent profile to register, no bot badge, and no shared automation account that can stand in for a person.

**A named agent account is refused even when a human vouches for it.** Some coding agents ship their own GitHub identity. Work arriving from that account is closed no matter what the linked issue or PR body says about who directed it, because the account is not attached to an owner: there is nobody accountable behind it, nobody to answer a review question, and nobody to fix it. An endorsement written in the description does not convert an unowned account into an owned one. Commit from your own profile instead, and the same diff is welcome.

**Agentic commits are accepted under the human's profile.** If an agent produced the diff, the commit still carries the name and email of the person who owns that agent, and that person opens the PR from their own GitHub account. Issues follow the same rule: file them yourself, from your own account.

**The owning profile carries the full responsibility.** Submitting agent-written work under your profile states that you read it, you understand it, you ran the checks, and you will answer for it in review. "The agent wrote it" is not an explanation, a defense, or a mitigation. A regression, a security hole, or a wrong claim in an issue is yours, and a maintainer asking why a line exists expects an answer from you, not a transcript.

In practice:

- No `Co-Authored-By` trailer naming an agent or a model, same as any other co-author line (see **Commit Discipline**).
- No "generated by" footer, badge, or attribution in commit bodies, PR descriptions, or issue bodies.
- No agent-written PR you have not run locally. CI is not your reviewer of first resort, and a green pipeline does not excuse a diff nobody read.
- A PR you cannot explain in your own words is closed, however good the code is.

## What Gets Your PR Closed

To be transparent, here's what will get your PR closed immediately:

- **Stub/placeholder code** — Empty implementations, `todo!()`, functions that return hardcoded empty values
- **No linked issue** — Feature PRs without an approved issue
- **Fails CI** — If `cargo clippy` or `cargo test` fail (the hard gates; `cargo fmt --check` is a soft-fail in CI today, but an unformatted diff still gets bounced in review)
- **Unrelated changes** — Reformatting files you didn't modify, drive-by "improvements"
- **No tests** — Bug fixes without a regression test, features without any tests
- **Tests that write the live config** — A test that saves `config.toml` or `keys.toml` outside a home override
- **AI-generated spam** — PRs that look like they were generated by an LLM with no understanding of the codebase
- **An AI agent profile**: issues and PRs opened from an account that exists to run an agent (see **AI Agents and Agentic Contributions**)

## Don't Know How to Code?

That's completely fine. You can still contribute meaningfully:

- **Report bugs** with detailed reproduction steps
- **Request features** with clear descriptions of the problem you're trying to solve
- **Improve documentation** — fix typos, clarify confusing sections, add examples
- **Test pre-release builds** and report issues
- **Answer questions** in GitHub Discussions

A well-written bug report or feature request is worth more than a stub PR. Seriously.

## Workflow policy: `governance.toml`

The machine-checkable workflow rules (child issues not sibling PRs, no
co-author trailers, conventional commit titles, and a diff-size warning)
live in [`governance.toml`](governance.toml) and are enforced by the
`Policy lint` CI job (`scripts/policy-lint.py`). That file is the source
of truth; this page is its human mirror. Everything the fence cannot see
(tone, scope creep, "is this really one problem?") stays with human review
by design.

## License

By contributing to OpenCrabs, you agree that your contributions will be licensed under the MIT License. See [LICENSE.md](LICENSE.md) for details.
