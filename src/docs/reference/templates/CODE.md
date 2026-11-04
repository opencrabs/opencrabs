# CODE.md — How You Write Code

> **Owns:** how you write code — standards, file organization, testing, security-first, and the Rust-First Policy.

*You build things. Build them right.*

---

## Philosophy

**Single binary. Run it. Delete the mess. Move on.**

You are not a framework junkie. You don't leave build artifacts rotting on disk. You compile, verify it works, clean up, and ship. If something needs to change — recompile from scratch. Binaries are disposable. Source is sacred.

**Rust first. Always.** When choosing a language, Rust wins unless there's a concrete reason it can't (browser JS, platform SDK requirement, etc.). Native, safe, fast, single binary. No runtime dependencies. No "just install Node and Python and Java and..." — one binary, done.

**Small is beautiful.** Every line of code is a liability. Less code = less bugs = less maintenance. If you can solve it in 50 lines, don't write 200. If a dependency does it well, don't rewrite it. But if a dependency drags in half the internet, write it yourself.

---

## File Organization

### Hard Limits

- **No file over 500 lines.** If you're approaching that, you've already waited too long to split.
- **No file over 300 lines** without a damn good reason and a plan to refactor.
- **Target: 100-250 lines per file.** That's the sweet spot. Easy to read, easy to review, easy to test.

### Structure

Every module follows this pattern — **separate concerns into separate files:**

```
feature/
├── mod.rs          # Public API, re-exports (thin — just wiring)
├── types.rs        # Structs, enums, type aliases
├── handler.rs      # Request handling / business logic
├── utils.rs        # Helper functions (only if needed)
└── tests/
    ├── mod.rs      # Test module aggregator
    └── feature_test.rs
```

**Types live in `types.rs`.** Not scattered across handler files. Not inline in the module root. Dedicated file.

**Handlers handle.** They don't define types, don't contain utilities, don't hold test code. They receive, process, respond.

**One responsibility per file.** If you can't describe what a file does in one sentence, it does too much.

### When to Split

- File crosses 250 lines → think about splitting
- File crosses 400 lines → split now, no excuses
- You're adding a second "section" with its own types/logic → new file
- You find yourself scrolling to find things → too long

### Anti-Patterns

- **God files** — one file that does everything. Split it.
- **Never silent drop failures/errors:** — One example is "let _ = call.await" its is forbidden across the project; logging on failure is non-negotiable even for best-effort operations.
- **Copy-paste walls** — 10+ duplicated lines. Extract a function.
- **Inline type definitions** — types buried inside handler functions. Pull them out to `types.rs`.
- **"I'll refactor later"** — no you won't. Do it now while context is fresh.

---

## Testing

### Tests Live in Dedicated Files

**Tests go in a `tests/` directory, not at the bottom of source files.**

```
src/
├── feature/

│   ├── types.rs
│   ├── handler.rs
│   └── tests/          # ← tests here
│       ├── mod.rs
│       └── handler_test.rs
├── tests/              # ← or project-level test directory
│   ├── mod.rs
│   └── feature_test.rs
```

- **Naming:** `*_test.rs` — always. Consistent, searchable, obvious.
- **Shared test helpers** go in `test_utils` module. Never duplicate mock setups across test files.
- **Duplication:** Check for duplications and merge them under its directory if exists. Do not duplicate tests that lives on its own directory.

### Test Every Build

**You don't get to say "it compiles, ship it."**

1. **Write the code.**
2. **Write tests for it** — in a dedicated test file.
3. **Run the full test suite.** Not just your new tests — everything. Regressions are real.
4. **If tests fail, fix before moving on.** No "I'll fix that later" — later is now.

### What to Test

- **Every public function** gets at least one test.
- **Every error path** gets a test. Happy path alone is not coverage.
- **Edge cases** — empty input, max values, Unicode, concurrent access.
- **Integration points** — where your code talks to external systems, mock and test the boundary.

### What NOT to Test

- Private implementation details that change often.
- Trivial getters/setters with no logic.
- Third-party library internals — trust or replace, don't test.

---

## Security-First

### Non-Negotiable

- **Validate all external input.** User input, API responses, file contents, environment variables — anything from outside your control gets validated before use.
- **No hardcoded secrets.** Not in source, not in tests, not in comments. Use environment variables, config files (chmod 600), or secret managers.
- **No `unwrap()` on user data** (Rust). Use `?`, `.unwrap_or()`, or proper error handling. Panics on bad input = denial of service.
- **Sanitize output.** HTML, SQL, shell commands — if user data touches these, escape it.
- **Principle of least privilege.** Don't request permissions you don't need. Don't read files you don't need. Don't open ports you don't need.

### Dependency Hygiene

- **Audit before adding.** Check the crate/package: maintainer reputation, recent activity, dependency tree, security advisories.
- **Minimal dependencies.** Every dependency is an attack surface. If you can write it in 20 lines, don't add a crate for it.
- **Pin versions.** Lock files exist for a reason. No floating versions in production.
- **No yanked/deprecated packages.** Check before adding.

### Code Patterns

- **Fail explicitly.** Return errors, don't swallow them. `let _ = dangerous_thing()` is a bug.
- **No shell injection.** Never pass unsanitized strings to `Command::new()` or `exec()` or `system()`. Use argument arrays.
- **Constant-time comparison** for secrets (tokens, passwords, API keys).
- **Bound all buffers.** No unbounded reads from network or files. Set limits.

---

## Build Workflow

### The Loop

```
1. Write code
2. Build → single binary
3. Run binary, verify it works
4. Run tests, verify coverage
5. Delete build artifacts (target/, dist/, node_modules/, __pycache__/, etc.)
6. If changes needed → go to 1
```

### Language-Specific

**Rust (preferred):**
```bash
cargo clippy --all-features          # lint — NOT cargo check
cargo test --all-features            # test everything
cargo fmt                            # format

# Before ANY release build, clear the artifacts first — target/ grows into
# tens of GB otherwise, and stale artifacts accumulate across builds:
cargo clean
cargo build --release --all-features # release binary
```

> **Exception — OpenCrabs' own source.** The commands above are for YOUR
> projects; build them however they are normally built. On this project, never
> run `cargo build --release` inline — it takes many minutes and times out. To
> verify a change, clippy + test + fmt is the answer; a release build proves
> nothing they do not.

**Go:**
```bash
go vet ./...
go test ./...
go build -o binary .
# clean: rm binary
```

**Python (when unavoidable):**
```bash
python3 -m pytest tests/
python3 -m py_compile script.py
# no build artifacts, but clean __pycache__/
```

**TypeScript/JavaScript (when unavoidable):**
```bash
npm test
npm run build
# clean: rm -rf dist/ node_modules/
```

### Clean Up After Yourself

Build artifacts are temporary. Don't commit them. Don't leave them. The repo should be source code and nothing else.

---

## Rust

- **Do NOT use unwraps or anything that can panic in Rust code, handle errors.** Obviously in tests unwraps and panics are fine!
- In Rust code prefer using `crate::` to `super::`; don't use `super::`. If you see a lingering `super::` from someone else clean it up.
- Avoid `pub use` on imports unless you are re-exposing a dependency so downstream consumers do not have to depend on it directly.
- Skip global state via `lazy_static!`, `Once`, or similar; prefer passing explicit context structs for any shared state.
- **No mock tests.** Unit tests and e2e tests hit real implementations (real DB, real structs). Mocks hide bugs — if the mock passes but prod breaks, the test was worthless.
- All tests live in `src/tests/` as dedicated `*_test.rs` files — not inline, not scattered.
- **Rust `if let` edition trap (crates on edition < 2024):** scrutinee temporaries (lock guards, borrows, owned values) live until the END of the whole `if let ... else`, so a guard is still HELD inside `else`; re-locking the same mutex there deadlocks. If `else` touches the same value, bind the scrutinee to a named `let` above the `if let` or drop it explicitly. Rust 2024 (RFC 3606) shortened the drop; models miss this (Opus caught it in 3 min, DeepSeek not at all, user report 2026-10-30). Check `edition` in Cargo.toml BEFORE editing any `if let ... else`.

---

## Architecture Principles

### Read Before Writing

**Understand the existing codebase before adding code.** Read the module structure. Follow the patterns already established. Don't introduce a new convention when one exists.

### Match Existing Patterns

If the project uses X pattern, use X pattern. Consistency beats "better" in isolation. If you genuinely think the existing pattern is wrong, flag it — don't silently introduce a competing pattern.

### No Premature Abstraction

Three similar lines of code is fine. Don't create a `GenericHandlerFactoryBuilder` for two use cases. Abstract when you have 3+ concrete cases that genuinely share logic.

### Error Handling

- **Return errors up the call stack.** Let the caller decide what to do.
- **Log at the boundary, not deep inside.** One log per error, not five as it propagates.
- **Typed errors over strings.** `enum MyError { NotFound, InvalidInput(String) }` beats `"something went wrong"`.
- **No silent failures.** If something can fail, handle it or propagate it. Never ignore it.

### Comments

- **Code should be self-documenting.** Good names > good comments.
- **Comment the "why", never the "what".** `// increment counter` is noise. `// retry because the API returns stale data on first call` is useful.
- **Don't add comments to code you didn't write or change.**
- **TODO comments need context:** `// TODO(name): reason, ticket/issue if exists`

---

## Problem Solving

### Never Give Up

**If a solution doesn't work, try another approach.** Then another. Then search the web. Read docs. Read source code. Read issues. There is always a fix — find it.

- **Don't settle for the first approach.** If it's ugly, fragile, or hacky — step back and think harder.
- **Use every resource available.** Web search, official docs, GitHub issues, source code of dependencies. The answer exists somewhere.
- **Try different angles.** If the direct approach fails, come at it sideways. Rethink the problem. Question your assumptions.
- **Never declare something impossible** without exhausting alternatives. "I can't" means "I haven't tried enough approaches yet."

### Never Suppress Errors

**`#[allow(lint)]` is not a fix. It's duct tape over a wound.**

- If clippy or the compiler complains, **fix the underlying code**, not the warning.
- If two lints contradict each other, restructure the code so neither triggers.
- `let _ = result;` is hiding a bug. Handle the error or propagate it.
- `#[allow(dead_code)]` means you have code that shouldn't exist. Delete it.

### Dead Code Dies

**If code is unused, delete it. Period.**

- Don't comment it out "for later." Git remembers. You won't need it.
- Don't add `#[allow(dead_code)]` to keep it around. If nothing calls it, it's dead weight.
- Don't re-export unused items just to silence warnings. Remove the item.
- Don't add `_` prefixes to mask unused variables — either use them or remove them.
- **The codebase should compile clean with zero warnings.** Every warning is a conversation you're avoiding.

---

## Bug Fixes & Improvements — Tracking Workflow (Hard Rule)

**Every bug fix and improvement MUST be tracked.** Use **issues for smaller fixes**, **PRs for larger changes**. No exceptions.

### When `gh` CLI is authenticated:
1. **Open the issue/PR FIRST** with initial findings: what's broken, how to reproduce, root cause, and fix plan. Use `gh issue create` (smaller) or `gh pr create --draft` (larger).




### When `gh` CLI is NOT authenticated:


### Commit Discipline:



---

## Hard Rules (Non-Negotiable)

1. **No 5,000-line files.** Ever. For any reason. Split or die.
2. **No tests at file bottom** beyond 30 lines. Dedicated test files or nothing.
3. **No code without tests.** If you built it, prove it works.
4. **No build artifacts in the repo.** `.gitignore` exists. Use it.
5. **No hardcoded secrets.** Not even "temporarily."
6. **No suppressing warnings.** Fix the code, not the lint. No `#[allow()]` unless you can explain exactly why the lint is wrong and the code is right.
7. **No `unsafe` without a comment explaining why** it's necessary and why it's sound.
8. **Run the full test suite before declaring anything done.**
9. **Clean up after builds.** Source is permanent. Binaries are disposable.
10. **Rust first.** Always. Unless you can't. And you probably can.
11. **No dead code.** If it's unused, delete it. Git has history. You don't need commented-out code.
12. **Never give up on a problem.** Research, web search, try different approaches. The fix exists.

---

## Change Discipline — Verify Before You Ship

**Every change must pass this checklist before committing:**

### 1. Match the Request
- Read the user's exact words. What did they *actually* ask for?
- If they said "add X", don't remove Y in the same diff unless explicitly requested
- Flag scope creep in your own work — if you're tempted to refactor beyond the ask, don't

### 2. Surgical Diffs
- **Diff size as a red flag.** If your diff touches more files than the user mentioned, pause and audit
- Every changed file must directly serve the stated goal — no "while I'm here" cleanups
- No removing dead code unless it's **obviously** dead code from the same feature you're modifying, not unrelated code that happens to have a warning
- **Before committing:** `git diff --stat` — does the file count match your mental model?

### 3. No Silent Removals
- **Never remove a public function, endpoint, or configuration option** without the user explicitly asking
- If a change requires removing something, state what and why before doing it
- When in doubt: deprecate + warn > delete

### 4. Test Discipline — Non-Negotiable
- **Before modifying any source file:** check if a corresponding test exists in `src/tests/`
- If no test exists for the feature you're touching → **create one** before committing
- Test the behavior, not just compilation — test the actual runtime contract
- Rate limiters need tests for: concurrent access, sleep behavior, shared state across instances
- If test creation is complex, at minimum add a `#[test]` for the happy path

### 5. Build + Lint Before Commit
- `cargo clippy --all-features` — zero warnings
- `cargo test --all-features` — relevant tests pass
- **Always run `cargo test --all-features`**, never just `cargo test` — this catches missing feature-gated test coverage
- **Match CI checks locally.** Check `.github/` for CI workflow definitions and run the same checks (clippy, tests, fmt, etc.) that CI runs. User correction at 20:58: "run all commands such as clippy, tests and etc matching what gh expects, they are inside .github dir"

### 7. Always File Issues or PRs — Every Bug Fix and Improvement Is Tracked

**No invisible fixes.** Every bug fix and improvement must be tracked as a GitHub issue (smaller scope) or PR (larger scope, multi-file features). This creates a permanent, searchable record linking every fix to its issue, commit, and rationale.

**When `gh` is authenticated:**
1. Open the issue/PR first with initial findings or plan
2. Fix the code, run clippy + tests, and commit
3. Comment on the issue/PR with the fix details: commit hash, root cause, what changed, files modified
4. Close the issue with `gh issue close <number> --reason completed`

**When `gh` is NOT authenticated (no GitHub auth configured):**
- You cannot create issues or PRs directly
- Inform the user that the bug/fix should be reported to the maintainer (or filed manually on GitHub)
- Include enough detail in your message that the user can copy-paste it into a GitHub issue if they want

**Issue vs PR:**
- Issue: single-file fixes, config changes, small bugs, documentation corrections
- PR: multi-file features, new tools, refactors spanning multiple modules


---

## Hard Rules — Test Discipline

**No inline tests in source files.** No `#[cfg(test)] mod tests` blocks inside `src/`. Tests belong in `src/tests/`, one file per module, registered in `src/tests/mod.rs`. If you catch yourself writing a test block inside a source file, stop — create `src/tests/<feature>_test.rs` instead. Source files are production code only. Tests are separate.

**Always verify your diff before committing.** Run `git diff` and check:
1. Does the diff match what was requested? No more, no less.
2. Did you accidentally remove existing functionality? Every deleted line must be intentional.
3. Are you adding tests for new code? If not, you're shipping blind.

**Tests catch real bugs.** The rate limiter had a production bug where `Instant::now().elapsed()` near startup was ~0, causing every first request to incorrectly sleep. Tests caught it immediately. This is why they exist.


## Parameter Naming — Use Descriptive Names, Never Numeric Indices

**Never use positional/numeric parameter names** (e.g., `param_0`, `param_1`, `field_2`) in any code — API clients, provider implementations, config structs, function signatures, or test fixtures.

**Why:** Numeric indices are fragile — reordering breaks everything silently, tests become unreadable, and adding a new provider becomes a nightmare of off-by-one errors. Descriptive names are self-documenting and reorder-safe.

**❌ Wrong:**
```rust
let params = vec![
    ("0", Value::String(model)),    // What is this?
    ("1", Value::String(voice)),    // What is this?
    ("2", Value::Number(speed)),    // What is this?
];
```

**✅ Right:**
```rust
let params = json!({
    "model": model,
    "voice": voice,
    "speed": speed,
});
```

This applies everywhere: API request bodies, struct field definitions, query parameters, CLI arguments, test fixtures, and configuration objects. Always use human-readable, descriptive names.

## Release Note Format — NEVER USE TABLES

**CRITICAL: Release notes are plain text paragraphs. NEVER tables. NEVER markdown tables. NEVER pipe-separated rows.**

The v0.3.11 format is the ONLY correct reference. v0.3.12 and later entries were incorrectly formatted as tables — DO NOT copy them.

**Before writing ANY changelog entry:**
1. `head -100 /Users/adolfousierstudio/srv/rs/opencrabs/CHANGELOG.md`
2. Find the v0.3.11 entry specifically
3. Match that format exactly — plain text with bullet points, no tables
4. The v0.3.12+ table entries are WRONG. They must be fixed to match v0.3.11 style.

**User has corrected this 2 times. Tables for release notes = immediate user frustration.**

**Each release note must have UNIQUE wording.** Never reuse the same generic phrases across releases (e.g., "it keeps getting tougher", "another milestone", "we're excited to announce"). Every release is different — write fresh, specific, concise sentences that reflect what actually changed. Keep it short. User correction May 18 12:53: "we need to stop saying it keeps getting tougher on all releases the same wording."

.



- **When adding new features, tools, or changes, ALWAYS update all relevant documentation: README, CHANGELOG, and any reference lists.** Do not consider a feature "done" until its documentation is complete. Check: (1) Is the new feature/tool mentioned in README? (2) Is it in the CHANGELOG with proper description? (3) Are all URL references and links updated? Missing documentation means the feature is effectively invisible. User correction May 20 14:22: "did you check if the new tools, or auto title generation feature or paste-by-default, are on readme file? you was completely missinng adding the url reference in the changelog bottom url list, so almost incomplete."

.


.

## Release Discipline — Non-Negotiable Checklist

When preparing a release, ALL of the following MUST be completed before committing:

1. **Changelog entry** — Every open issue that was addressed MUST have a changelog entry. If an issue isn't fixed, don't claim it is. If it IS fixed, it MUST appear in the changelog.

2. **URL path reference at bottom of changelog** — Every feature/tool mentioned in the changelog MUST have its URL path reference listed at the bottom. No orphan references. Check: does every link in the bottom list correspond to something in the changelog? Does every feature mentioned in the changelog have a link in the bottom list?

3. **Cargo version bump** — Update the version in `Cargo.toml` to match the new release tag. Do not forget this step.

4. **Commit message MUST be similar to last release and comprehensive** — Look at the last release commit message. Match its style, length, and structure. The commit message should comprehensively describe what changed. Do not write a generic or terse commit message for a release. Mention the changes explicitly, not vaguely.

5. **Documentation completeness** — README, CHANGELOG, and any reference lists must ALL be updated before the release commit. A feature is not "done" until its documentation is visible in all relevant places.

6. **NEVER create GitHub releases manually.** The Release CI workflow creates releases automatically: tag push triggers `wait-for-ci → build-release → publish-crate → create-release`. The `create-release` job auto-generates release notes from CHANGELOG.md. After pushing a tag, WAIT for the full pipeline to finish, then VERIFY the release exists on GitHub. Do NOT use `gh release create` — this creates duplicate releases that you then have to delete. User correction May 21 23:12: agent created release manually before CI finished, had to delete it.









































































### Rust-First Policy

This is the single home for the Rust-first rule (AGENTS.md and BOOT.md point here).

When searching for new integrations, libraries, or adding new features, **always prioritize Rust-based crates** over wrappers, FFI bindings, or other-language alternatives. Performance is non-negotiable — native Rust keeps the stack lean, safe, and fast. Only fall back to non-Rust solutions when no viable crate exists.

---


## Project-Specific Standards

### HeyIolo Backend (Node.js/TypeScript)

**Database migrations: ALWAYS inline, NEVER standalone SQL files.**

New columns and tables MUST be added inline in the `runMigrations()` function in `~/srv/js/heyiolo-backend-frontend/backend/src/config/database.ts`. This function auto-fires on server startup, so changes take effect automatically on next deploy.

**NEVER create standalone `.sql` files** for schema changes. They don't run automatically and require manual execution, which Adolfo will not do. If the agent creates a standalone SQL file, the migration simply won't happen.

adolfo correction Jun 25 12:53: "Why the migration wasn't set automatically to run on next build is the question?" — agent wrote columns to a standalone SQL file in /tmp instead of adding them to the inline runMigrations() function.

## Brain File Writing: Protocols, Not Essays

**Every brain file entry must be a terse execution directive, not an explanation.** The model already knows BDI, OODA, testing patterns, security concepts from training data. Brain files ACTIVATE patterns, they don't TEACH them.

- Bad: "OODA stands for Observe, Orient, Decide, Act. It was developed by John Boyd for military strategy. The observe phase involves..."
- Good: "Before acting: OBSERVE → ORIENT → DECIDE → ACT → VERIFY → UPDATE"

**Rule of thumb:** if a brain file entry explains WHAT something is, replace it with a directive that says WHEN and HOW to use it. The training data has the "what." The brain file has the "when" and "how."

**Max 3 lines per rule.** If it needs more than3 lines, it's an essay, not a rule. Consolidate.

**gh issue create: use `--body-file`, never heredocs.** Heredoc bodies with special characters (backticks, quotes, pipes) consistently break shell quoting — 6 failures on 2026-08-21/22 alone (`unexpected EOF while looking for matching`). Instead: write the body to a temp file first, then pass it: `cat > /tmp/issue_body.md << 'EOF' ... EOF` then `gh issue create --title "..." --body-file /tmp/issue_body.md`. This avoids all quoting/escaping issues. Violations: 6, last: 2026-08-22.

- New tool checklist must include `KNOWN_TOOL_NAMES` in `src/brain/provider/custom_openai_compatible.rs` (leaked-tool-call extractor gate) alongside catalog/tool_setup/mod.rs registration. Missed it in 2c3fbf58 (#1161); fixed by 0c86df9e.

- Never trust a lint's suggested fix without compiling it: clippy's `explicit_auto_deref` suggestion (`*p` → `p`) itself failed E0277 on a double-ref + `Borrow` bound. Apply lint fixes, then `cargo check` IMMEDIATELY before stacking more work. The lint's "try this" is a hypothesis, not a fix. (Sep 8 2026, zai rename: followed the suggestion blind, cost two clippy cycles; correct fix was derefing in the loop pattern, `for &p in path`.)
- Test-count derivations for TESTING.md/README: anchor on line-start attributes (`^\s*#\[(tokio::)?test`), NEVER loose `grep -c '#\[test'` (matches comment mentions, inflated counts), and subtract `#[cfg(windows)]` twins so rows reflect what runs on this machine (violations: 2026-09-10, auto_title_e2e "3" was 2 comment matches; tool_loop_helpers "42" ignored 2 cfg'd-out twins → 40).
- Source-scan sentinels (tests that `include_str!` a source file and `.split()`/`split_once()` on literal markers, e.g. src/tests/phantom_*.rs) DIE when a refactor renames their marker. Restructuring a hot file (tool_loop.rs...) → grep src/tests for its include_str and re-anchor every marker literal IN THE SAME COMMIT; targeted test runs stay green while the sentinel is broken, only the full suite catches it. Evidence: 2026-09-11, #1506's fired_branches rewrite removed the `if phantom_retries_used < MAX_PHANTOM_RETRIES` marker and silently broke phantom_unbacked_facts_test.
