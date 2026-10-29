# Changelog

All notable changes to OpenCrabs will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### 🔧 Fixes

- **The Windows exe starts on a clean Windows install**: v0.5.15's `opencrabs.exe` imported `VCRUNTIME140.dll`, `VCRUNTIME140_1.dll`, `MSVCP140.dll` and `VCOMP140.DLL`, none of which ship with Windows, so it died before printing a line and told the user to reinstall a program that was never the problem. A static CRT cannot fix it here: `llama.cpp`'s CMakeLists ignores `CMAKE_MSVC_RUNTIME_LIBRARY`, so its objects stay on `/MD` while Rust goes `/MT` and the link ends with unresolved `__imp_*` externals, and `rtk.exe` is a third-party prebuilt that imports `VCRUNTIME140.dll` no matter how our own binary is built. The zip now carries those four DLLs beside both exes, which is the app-local deployment Microsoft supports for the VC runtime, so the archive starts with no Visual Studio and no vc_redist (#2257)
- **`scripts/check-pe-imports.py` gates the artifact's import table**: a smoke run on `windows-latest` can never catch this, since that runner installs the vc_redist, which is exactly why the broken binary looked healthy. CI now reads the PE imports of the exe it built, and the release job reads the archive about to be uploaded, asking for closure: every non-system import must be a DLL that travels in the archive, and every carried DLL must parse as a PE image so a zero byte file cannot satisfy the name. `--self-test` proves the parser first (#2257)
- **The Windows one-liner installs the whole archive**: `install.ps1` copied `opencrabs.exe` and `rtk.exe` out of the extracted zip and then deleted the temp tree, so the runtime DLLs from the fix above never reached `%LOCALAPPDATA%\OpenCrabs\bin` and an install through the documented one-liner still died on a missing DLL. The script now takes every exe and DLL the archive ships, compares what landed against the archive's own file list, and fails loudly instead of reporting a short install. It lives in `src/scripts/install.ps1` with `src/tests/install_ps1_test.rs` as its witness, and opencrabs.com carries a byte-identical vendored copy (#2260)

## [0.5.15] - 2026-10-27

16 commits since v0.5.14. 41 files changed, +2,821 / -572 lines.

### 🔧 Fixes

- `83c8eea81` **Queue row softened to a gray hierarchy**: every span now sits under the thinking line's luma instead of above it; the amber is gone and a rendered test pins the colours, so a brightness regression fails CI instead of shipping (#2244)
- `3f3cff3fa` **A turn halted by suggest_options no longer renders intermediate narration as its answer** (#2240)
- `f8ef89d3a` **Dropped the in-flight border title from the input box**: the row claimed attention the queue row was built for (#2243)
- `59ea0421e` **History marker and splash tip now name PageUp**: both said Ctrl+O, which folds the newest turn instead of paging (#2242)
- `12ae04bde` **Ctrl+O no longer throws the viewport when the turn header is off screen**: the anchor captured a saturated row 0, so expanding a long answer jumped you to the top of the turn (#2241)
- `cc63d8296` **A caught render panic hands the terminal back**: raw mode was left off, so keystrokes echoed into the status bar and only Enter worked (#2238)
- `74499164a` **The input box says Enter queues during a turn**, and the queue row names the key that edits it (#2237)
- `5deaa70c4` **A mid-turn queued message is handed back to the input box** when the turn ends before a tool boundary drains it (#2236)
- `0b28c6354` **The built-in endpoint is matched by parsed host, not substring**: any URL that merely contained `inference.opencrabs.com` was misclassified, and every special-case keyed off that check (#2234)
- `c3a885246` **The mouse-fragment gate stops eating characters typed after a `[`** (#2233)
- `0cb652c52` **The built-in endpoint gets a 120s handshake** and stops the retry ladder after 2 silent timeouts instead of burning all 5 sends on a dead endpoint (#2232)
- `5d9f58976` **Evolve retries release-asset fetches** and splits a dropped connection from a bad checksum (#2229)
- `cf5722c8d` **A failed Telegram dispatcher startup recovers instead of panicking**, so the reconnect loop runs (#2228)

### 📖 Documentation

- `edd4e1e8e` README and architecture docs said 3x stream retries where the code retries 5 (#2231)

### 🧹 Miscellaneous

- `ea01d6e79` The phantom language scan has a work-shape guard: traversal counts pinned input-flat, mutation-tested red, so the three packs landing next cannot bend the scan input-proportional (#2245)
- `30e0b248a` The Telegram dispatcher sentinel is off the byte-slice ratchet, keeping lint green (#2230)

### 👥 Contributors

1 contributor this release:

- @adolfousier (Adolfo Usier)

### 📊 Stats

- 16 commits since v0.5.14
- 41 files changed, +2,821 / -572 lines
- 11,422 tests (11,422 passed, 0 failed, 38 ignored)


## [0.5.14] - 2026-10-25

26 commits since v0.5.13, 5 contributors. 64 files changed, +7,493 / -365 lines.

This release reaches more people and breaks fewer files. Two new language packs land next to the existing ones, Arabic and Chinese, each with its own script detection, and the non-English packs now carry intent phrases at the same order of magnitude as English instead of a third of it. European Portuguese continuous narration is detected. Four edit-tool defects are fixed: the line operations in `edit_file` no longer drop the final newline or turn a CRLF file into LF, `hashline_edit` joins its output with the file's own line ending, the fuzzy fallback keeps the file's line endings, and a retry is no longer blocked after the missing file appears. CI gained a lint that fails the build when a new byte slice can panic, and the workflow actions, the Rust toolchain and the Linux build image are now pinned to immutable versions.

### ✨ Features

- `344a1959` **phantom**: add a Chinese language pack and CJK script detection (#2223)
- `ebdc1cc9` **phantom**: add an Arabic language pack and Arabic script detection (#2222)

### 🔧 Fixes

- `855bd6f7` **tools**: join `hashline_edit` output with the file's own line ending (#2227)
- `c51b6a7e` **tools**: preserve bytes in `edit_file` line operations (#2227)
- `f07b91b5` **phantom**: bring the non-English packs to comparable coverage (#2225)
- `03ffa11a` **phantom**: detect European Portuguese continuous narration (#2224)
- `21f92922` **tasks**: settle a detached task on its row instead of announcing it twice (#2218)
- `89e6494c` **phantom**: stop the URL check slicing inside multi-byte whitespace (#2220)
- `c38bf52f` **ci**: pin the linux-amd64 release job to ubuntu-24.04 (#2206)
- `b876f7ce` **tools**: keep the file's own line endings in the fuzzy edit fallback (#2212)
- `d3e3758e` **tools**: clear the recent-failure guard when the world changed or a new turn starts (#2211)
- `688365d0` **tools**: record the post-edit content so the next `write_file` is not refused (#2210)

### 📖 Documentation

- `cd736835` docs: name the Ubuntu 24.04 runtime packages correctly (#2219)
- `dd7e5202` docs: document how to bump the pinned toolchain (#2215)
- `896a3b27` docs: add the code of conduct, issue forms and pull request template (#2213)
- `3ec99974` docs: simplify the sponsors table

### 🧹 Miscellaneous

- `097cf79d` test: format the new pack test modules and samples (#2225)
- `43687f22` refactor: resolve the language registry through one table (#2226)
- `1ecb56b5` ci: fail CI on a new `clippy::string_slice` site (#2221)
- `5d85599a` test: run the published Linux asset before uploading it (#2214)
- `87a9aab1` ci: run the test suite on Linux, not only on macOS (#2216)
- `be8e4cf1` ci: pin every workflow action to an immutable commit (#2217)
- `bb162126` ci: pin the Rust toolchain to 1.98.1 (#2215)
- `739af815` chore: bump tokio to 1.54.1 (#2205)
- `3cf5cd94` ci: fail the build when the shipped binary needs a newer glibc than we promise (#2209)
- `b4dc5c57` ci: build the prerelease linux-amd64 asset on a pinned image (#2208)

### 👥 Contributors

5 contributors this release:

- @adolfousier (Adolfo Usier)
- @adi805 (Syafriadi)
- @echo-xiangchen (Echo)
- @yimengzhiyan (beizhuo li)
- @VincentFeliciano (Vincent Feliciano)

### 📊 Stats

- 26 commits since v0.5.13
- 64 files changed, +7,493 / -365 lines
- 11,407 tests (11,369 passed, 0 failed, 38 ignored)

## [0.5.13] - 2026-10-24

61 commits since v0.5.12, 5 contributors. 188 files changed, +14,367 / -1,600 lines.

The channels carried most of this release, and the theme is one task, one place. A detached task now updates its own row instead of posting a second message on Telegram, Slack, Discord and WhatsApp, and a Telegram turn ends with its answer in one bubble rather than two. Discord interactions were hardened end to end: they obey the session turn slot, the turn group lock is no longer held across a governor sleep, reactions get the same identity check as messages, pending approvals drop on every exit from the wait, the pending select and form maps are capped, the compaction pinger stops when the turn ends, and a menu says so when it was cut to the button cap. WhatsApp folds LID-keyed sessions into their phone-number twin, queues a mid-turn message instead of killing the live turn, drops inbound status posts instead of answering them, backs off and gives up on a failing composing indicator, gates delete through the send limiter, and posts one status per recipient. A new channel session no longer inherits a provider the config disables, and the channel provider comparison reads what the session is configured with instead of the sticky fallback. A request that dies during connection setup now says whether the name failed to resolve or the connection stalled, instead of reporting both as a slow server. The TUI got full-screen sessions and help dialogs with their commands in the footer, copy that tries every clipboard backend and writes to Wayland, an editor handoff that matches nvim, helix and `$EDITOR`, and an open that reports failure instead of claiming success. The installer now leaves a callable `opencrabs` command: it writes the shell's PATH entry once, offers `--no-path`, and verifies with `command -v` before reporting success. The install identity moved to its own file so a lost token never mints a second user, an empty `stt_model` falls through instead of overriding a configured STT provider, hashline tags widened to six characters, and a scheduled cron run is exempt from the plan gate like every other headless run.

### ✨ Features

- `fded4837` **tui**: the help dialog is full screen with each column scrolling on its own (#2162)
- `f2a892e9` **tui**: the sessions dialog is full screen with its commands in the footer (#2162)

### 🔧 Fixes

- `4b4d4338` **provider**: name the phase when a connect timeout hides a DNS failure (#2204)
- `45e01b75` **install**: leave a callable opencrabs command after the install (#2203)
- `0dee0cca` **feedback**: let `feedback_analyze` filter to one tool and reach corrections (#2200)
- `687d671d` **hashline**: show real tags when a hash ref does not parse (#2201)
- `780cf330` **hashline**: widen line tags from four characters to six (#2202)
- `2a99ed3f` **voice**: an empty `stt_model` falls through instead of overriding a configured STT provider (#2185)
- `265418bc` **provider**: keep the install identity in its own file so a lost token never mints a second user (#2198)
- `1cd00208` **discord**: give `/new` the chat key so the next message lands on it (#2186)
- `80b05bbe` **whatsapp**: route the follow-up flush through the paired sanitize (#2179)
- `773b8df6` **session**: make the token counter atomic and stop calling it context (#2180)
- `ea9bf477` **whatsapp**: fold LID-keyed sessions into their phone-number twin (#2178)
- `3edc63a9` **whatsapp**: queue a mid-turn message instead of killing the live turn (#2179)
- `18c5f4ed` **discord**: record outbound messages where they leave the bot (#2196)
- `f20dcafe` **discord**: make interactions obey the session turn slot (#2191)
- `f293f403` **discord**: stop holding the turn group lock across a governor sleep (#2192)
- `e1f31f3b` **channels**: compare the session's configured provider, not the sticky fallback (#2184, #2188)
- `f1325525` **whatsapp**: post one status per recipient instead of one for the list (#2177)
- `8e9fade0` **deps**: bump `ctutils` past RUSTSEC-2026-0385 (#2197)
- `ab12d362` **channels**: stop logging "created" when a session already exists (#2183)
- `b9ee9bdb` **discord**: gate reactions with the same identity check as messages (#2195)
- `dad595cf` **discord**: drop pending approvals on every exit from the wait (#2194)
- `a6ac763b` **discord**: cap the pending select and form maps (#2193)
- `f58e9fa3` **discord**: stop the compaction pinger when the turn ends (#2189)
- `f69ec299` **discord**: say when a menu was cut to the button cap (#2190)
- `bfe3832e` **whatsapp**: drop inbound status posts instead of answering them (#2176)
- `f542b96c` **whatsapp**: back off and give up on a failing composing indicator (#2181)
- `4c643bab` **discord**: read the transcribed text in the stop gate (#2187)
- `3e5f0d0c` **whatsapp**: gate the delete action through the send limiter (#2182)
- `58817179` **tui**: the image clipboard hint works off macOS too (#2174)
- `82c5d3e2` **tui**: the editor handoff matches nvim, helix and `$EDITOR` (#2173)
- `cbfa53c1` **tui**: opening a session file no longer freezes the TUI (#2172)
- `8da38191` **tui**: copy tries every clipboard backend and writes to Wayland (#2171)
- `466b5f99` **tui**: a failed open is reported instead of claimed as success (#2170)
- `2ebe7d46` **rsi**: bound the template fetch, retry once and name the cause (#2169)
- `b445622a` **tui**: the sessions dialog insets both sides and aligns its columns (#2175)
- `f86437d4` **acp**: let the ACP surface keep `suggest_options` instead of the headless set
- `45f50285` **tui**: one chat floor for the plan card and the layout (#2168)
- `42a0f419` **tui**: the closing line no longer replaces a delivered answer (#2167)
- `e2420894` **tui**: a rule separates the sessions and help footers (#2162)
- `9f0da073` **tui/theme**: boot builds the theme pack before looking up the saved theme (#2164)
- `a693de4b` **tui/theme**: the chat history paints the theme background (#2163)
- `2420f0ed` **whatsapp**: a detached task keeps one row after the edit window closes (#2150)
- `e83ad61a` **slack**: a detached task updates its row instead of opening a second block (#2149)
- `418b823c` **discord**: a detached task updates its card instead of posting a second one (#2148)
- `8b4bba28` **telegram**: a detached task updates its row instead of posting a second message (#2147)
- `e518377e` **agent**: a detached task reports its start and finish with one id, and the TUI draws it as one row (#2145, #2146)
- `df9c49c3` **telegram**: a turn ends with its answer in one bubble, not two (#2156)
- `fe7001cd` **cron**: a scheduled run is exempt from the plan gate like every other headless run (#2161)
- `16a0dcc7` **bash**: a sudo invocation is found where the shell runs it, and the dialog opens only when it can be answered (#2157)
- `2e605cb6` **tui**: a running turn folds into its tool block like any other turn (#2160)
- `02db7d2e` **sessions**: a new channel session does not inherit a provider the config disables (#2158)
- `0ce8c4bf` **telegram**: a guest chat resolves its session from the database like a regular chat (#2159)

### 📖 Documentation

- `0ac63efa` verify external facts on the web instead of answering from memory (#2199)
- `b272466c` say how agent-written work is accepted and agent accounts are not

### 🧹 Miscellaneous

- `a18cbe72` test(session): serialize the provider sync suites on the config mirror (#2184)
- `8a06ce94` test(tui): the theme cache test pins its own pane layout
- `82006fb6` style: rustfmt the channel fix batch
- `8ae04828` style(tests): wrap long lines in the Phase A channel tests
- `2b1aa860` ci(titles): fail the title check on a bare area prefix

### 👥 Contributors

5 contributors this release:

- @adolfousier (Adolfo Usier)
- @adi805 (Syafriadi)
- @loonix (Daniel Carneiro)
- @antony87-x
- @spinecki

### 📊 Stats

- 61 commits since v0.5.12
- 188 files changed, +14,367 / -1,600 lines
- 11,333 tests (11,295 passed, 0 failed, 38 ignored)

## [0.5.12] - 2026-10-21

78 commits since v0.5.11, 6 contributors. 180 files changed, +14,993 / -3,285 lines.

Telegram guest queries went from a narrow reply path to a full surface. A tag in a chat the bot was never invited to now gets the same tools as a normal turn, the same live status block, the same rich rendering, one cancellable turn per guest session with rate limiting, and a tool set capped at what a stranger should reach. Two answer-duplication bugs went with it: a guest reply no longer carries the answer twice, and a reply with no tool calls is the answer alone. Separately, the agent now has to stop and write a plan once a turn crosses a threshold of tool calls, `browser_act` gained an `upload` action that attaches local files without putting their bytes in the context, and `bash` refuses `sleep` while a detached task is running and names the task it is waiting on. The TUI keeps the transcript visible across a session switch instead of folding every turn you had opened, and a headless install now provisions the built-in provider the way the TUI already did. Phantom detection got four corrections so a contraction, a question, a sign-off after a halt, or a denied tool call is no longer read as a promised action, and the option-surface halt path stopped issuing a follow-up request and stopped rendering the same answer twice. Usage accounting reads a top-level `cached_tokens` and records a missing cache field as not reported. Security tightened in four places: every outbound channel path sanitizes at one chokepoint, the http body is capped and its read bounded, string slicing can no longer land mid-character, and a config or keys write that would not parse is refused. Release packaging now ships `OpenCrabs.app` and a Linux desktop entry with a full-size icon ladder. The docs dropped two onboarding pages that documented a macOS GUI and a wizard this repo never had, the installer no longer prompts, and the sponsor table is generated from one ledger.

### ✨ Features

- `7c591584` **browser**: add an `upload` action to `browser_act` (#2112)
- `5daad132` **plan**: require a plan after a threshold of tool calls in one turn (#2128)
- `d04ea40d` **bash**: refuse `sleep` while a detached task is running (#2123)
- `0b8d88da` **bash**: name the running task in the sleep refusal (#2123)
- `05ece9eb` **telegram**: give a guest reply the live status block a normal session shows (#2109)
- `3758e30b` **telegram**: answer guest queries so a tag works in chats the bot is not in (#2087)
- `3373bc7d` **docs**: record the community sponsors and generate the docs from one ledger
- `b612fe57` **prompt**: keyed and sensitive calls run from a Python script, never inside the session (#2094)

### 🔧 Fixes

- `4b44a5f5` **deps**: move `rustls-webpki` and `webpki-roots` to clear the audit red
- `0d2e8faa` **provider**: provision the built-in provider on headless installs (#2151)
- `d459beb4` **tui**: keep the transcript visible across a session switch (#2155)
- `b1b6e96e` **docs**: replace the stale getting-started page and make the installer non-interactive (#2142)
- `2d10534a` **telegram**: a guest reaches only the tools a stranger should (#2134)
- `5b60f76d` **tui**: refresh whispercrabs instead of trusting the first download (#2114)
- `f6386ba0` **telegram**: a reply to media keeps the file identity (#2121)
- `f1c4be95` **agent**: nudge five times before a loop falls back (#2129)
- `d5bebea5` **phantom**: a contraction is not a completed action (#2110)
- `d473df93` **telegram**: one turn per guest session, cancellable and rate-limit aware (#2143)
- `b88ce66c` **usage**: read a top-level `cached_tokens` field when no other cache field is present (#2140)
- `f3cae9e6` **usage**: a response with no cache field is recorded as not reported (#2139)
- `29df3a89` **telegram**: give a guest turn the same tools, block chrome and rich plane as a normal one (#2138)
- `016eb2ce` **telegram**: owner-only targeted stop for another session (#2108)
- `83cf936c` **onboard**: ask the image tool's resolver for the image gap (#2106)
- `ac662707` **tools**: cap the http body and bound the read (#2104)
- `d636a33f` **channels**: sanitize every outbound path at one chokepoint (#2102)
- `e94568fc` **usage**: By Model cache share divides by prompt tokens, and DeepSeek hits are read (#2136)
- `0b756926` **telegram**: render guest rich replies on the markdown plane (#2135)
- `820aae3b` **telegram**: send and edit guest replies rich when enabled (#2133)
- `640e9d1a` **voice**: send audio on the chat route and stop offering built-in speech (#2107)
- `9bb6df69` **prompt**: drop the plan-tool prose the plan gate now enforces (#2131)
- `a281c2db` **prompt**: rejoin the stranded TRACK SELECTION table rows (#2130)
- `07a387ec` **tui**: bound cancel-persist to the in-flight turn (#2127)
- `f433b8f9` **phantom**: flag denied tool calls and unbacked identifiers (#2122)
- `d50dc3ad` **core**: stop slicing strings at byte offsets that can land mid-character (#2124)
- `ea160668` **telegram**: one file occurrence carries one link (#2125)
- `1559c9d1` **telegram**: locate a file marker by its own bytes, not a stale span
- `32111403` **vision**: stop pinning the vision output cap at 1024 (#2120)
- `a1b9487b` **release**: give the macOS icon the rounded tile shape (#2119)
- `f5fab165` **homebrew**: state the cask path and the formula's missing icon (#2118)
- `0308813f` **release**: ship a desktop entry and icon with the Linux build (#2116)
- `b4d0d1af` **release**: ship `OpenCrabs.app` so macOS has an icon to show (#2115)
- `0263383a` **assets**: rebuild `icon.icns` with the full size ladder (#2117)
- `d1f29104` **telegram**: a guest reply with no tool calls is the answer alone (#2113)
- `43ded2df` **telegram**: stop a guest reply carrying the answer twice (#2113)
- `c3eed33b` **tui**: stop the same answer rendering twice on an option-surface halt (#2111)
- `dc051e8d` **cli**: take the scheduler lock only when a scheduler is spawned (#2096)
- `09f25a46` **cli**: write the documented example config on init (#2093)
- `03e3ce6c` **profile**: stamp lock owners with their creation time (#2084)
- `87864484` **config**: refuse a config or keys write that would not parse (#2099)
- `a940d255` **phantom**: a sign-off after an option-surface halt is not a phantom (#2090)
- `d031c074` **discord**: a follow-up tap never claims the turn slot, and the cancel token still cancels a turn that already ended (#2089)
- `bbf021ce` **agent**: a `suggest_options` halt with no text still asks the model for a reply (#2088)
- `da94e7d3` **agent**: an option-surface halt ends the turn with no follow-up request (#2086)
- `470e12f4` **phantom**: a reply that asks the user something is not a promised action (#2083)

### 📖 Documentation

- `aeeeeeb9` delete the macOS GUI onboarding pages (#2154)
- `8cea1f5e` delete the onboarding wizard page that documents commands we never had (#2153)
- `f8da765f` record where each cash contribution came from
- `82651cd1` record the October 19 inference top-up
- `71aa1c02` say the built-in provider transcribes on day 0 (#2141)
- `909f1b5c` say how issues are referenced in code comments (#2137)
- `14df12ac` document the plan gate and align the TRACK SELECTION row (#2132)
- `ee0f4816` name the three zip 8.6 blockers in the audit comment (#2092)
- `3a5cc4ca` stop seeding an image id and speech fields the provider does not serve
- `3784d4a3` add the contributor count to the v0.5.0 summary line
- `aa04b7c3` add the named contributor list to v0.5.0
- `14e6b078` add the named contributor lists to v0.5.5, v0.5.4 and v0.5.1
- `cc85905e` credit the owner in the recent contributor lists

### 🧹 Miscellaneous

- `ffac6a3d` test(install): skip the bash parse check on Windows hosts
- `90c63700` test(model): stop asserting the Claude label format on non-Claude ids
- `6609b94c` chore(usage): log the key names of a provider usage object at debug (#2152)
- `062187e4` test(suite): delete the four dead modules and guard the declaration list (#2103)
- `f01ef0e0` style(plan): apply rustfmt to the plan requirement gate (#2128)
- `d0fe0c56` test(phantom): pin the accepted trade for an unbacked kill claim (#2122)
- `d8c47de6` test(bash): pin sleep running again after the task finishes (#2123)
- `02bd841e` style(tests): format the three test files the fmt gate flags
- `d44347af` test(brain): pin the anti-polling contract in the system prompt (#2101)
- `2206bb88` chore(deps): move zip to 9 and rust_xlsxwriter to 0.99 (#2092)
- `4e01b95d` ci(audit): ignore RUSTSEC-2026-0367/0368/0369 for zip until upgrades land (#2091)

### 👥 Contributors

6 contributors this release:

- @adolfousier (Adolfo Usier)
- @adi805 (Syafriadi)
- @loonix (Daniel Carneiro)
- @leshchenko1979 (Alexey Leshchenko)
- @daeho-ro (Daeho Ro)
- @moneyacademyKE (Moe)

### 📊 Stats

- 78 commits since v0.5.11
- 180 files changed, +14,993 / -3,285 lines
- 11,091 tests (11,053 passed, 0 failed, 38 ignored)

## [0.5.11] - 2026-10-16

4 commits since v0.5.10, 2 contributors. 19 files changed, +1,361 / -349 lines.

Four fixes, each one closing a hole that a shipped release exposed. A first run lands in the chat with the built-in provider already live and the agent greeting the user, instead of on a "New Chat · none / none" screen with no provider, no config.toml and no keys.toml (#2079). The agent's worker threads get a 16 MiB stack, so the first turn can no longer abort the process with a stack overflow on the default 2 MiB one (#2080). The release workflow reads the conclusion of the commit's own CI run before it publishes anything, so a red main can never ship a tag again the way v0.5.10 did (#2078). And the scheduled evolve restart no longer hands `systemctl restart` the discovery glob `opencrabs*.service`: that glob also matched the transient unit the restart runs inside, so systemd stopped its own executor 16 ms in, retried until start-limit-hit, and churned the daemon and its browser sidecar five times inside one second. The restart target is a resolved list of exact unit names now, the unit-list parser no longer drops failed units because of systemd's status glyph, and an unreadable unit list reports `Unresolved` instead of scheduling blind (#2081).

### 🔧 Fixes

- `ba8dc669` **evolve**: restart the resolved units, so the restart cannot stop itself (#2081)
- `003ee22f` **release**: gate the release tag on a green CI run for the same commit (#2078)
- `305d1944` **tui**: provision the built-in provider and greet on a fresh install (#2079)
- `e203d124` **runtime**: give the agent worker threads a 16 MiB stack (#2080)

### 👥 Contributors

2 contributors this release:

- @adolfousier (Adolfo Usier)
- @adi805 (Syafriadi)

### 📊 Stats

- 4 commits since v0.5.10
- 19 files changed, +1,361 / -349 lines
- 10,782 tests (10,744 passed, 0 failed, 38 ignored)

## [0.5.10] - 2026-10-16

99 commits since v0.5.5, 5 contributors. 213 files changed, +17,230 / -1,738 lines.

The release that works the second you install it. OpenCrabs now ships its own provider inside the binary: no account, no API key, no sign-up. It serves DeepSeek v4.1 Flash, a 200k-context multimodal model, so text and attached images both work on the first run. Onboarding was rebuilt around that first run: a quick-start welcome interviews the user, asks only for what is missing, names the session's provider and model, and offers next steps, and the built-in provider is wired into both the quick-start and the advanced path with vision and image generation on by default. Its context window is pinned at 200k and held by host, and install tokens are redacted from logs and kept out of Debug output. The provider chain got sturdier: a 429 that declares a long wait parks that provider and routes around it instead of poisoning the fallback, a lapsed cooldown no longer drops the primary from the chain, a declared-wait cooldown no longer makes the fallback sticky, every compatible provider shares one 120s stream idle default, and providers that need it now route over the Responses API. Attachments became first-class across every channel: Discord, Slack and Trello all save inbound files to the channel attachment store, Discord and Slack store DM turns in channel_messages, and the answer now posts before its suggestion buttons on Discord, Slack and WhatsApp. Discord tool cards became durable and paginated, surviving restarts in SQLite with a Log view, collapsed repeats and a colored bar. The TUI gained a full-screen responsive /usage and a /mission-control that adapts to the terminal width, /models opens on the session's provider and model and switches directly on a full provider/model argument, and the final answer stays visible when tools run after its text. Usage accounting grew a cache-read split with prices on the cards, and model costs come from the ledger rather than a cache-blind estimate. Voice serves stt_model and tts_model from any provider and splits TTS chunks on character boundaries. The plan tool locks a per-task spec check at start and runs it at completion. motion-graphics moved to an opt-in skill template. WhatsApp gained newsletter creation from the tool surface and live tool blocks on background-completion turns, and Slack got the same live tool flow on background resume.

### ✨ Features

- `ed3f5e07` **debug**: log the matched pair when the announcement-loop guard trips (#2052)
- `4dff2cd0` **tui**: make /usage a full-screen responsive screen (#2073)
- `94b9f850` **skills**: ship motion-graphics as an opt-in template, not a built-in (#1991)
- `dccd4b94` **whatsapp**: create and post to a newsletter from the tool surface (#1827)
- `d809bd05` **discord**: tool cards survive restarts, stored in SQLite (#1880)
- `0ab7dd53` **discord**: paged tool card with Log, collapsed repeats and a colored bar (#1880)
- `99681af6` **plan**: generate a per-task spec check at start and confirm before lock (#2063)
- `f68327ba` **plan**: lock a per-task spec check at start and run it at complete (#1904)
- `d03ede28` **usage**: record the cache-read split in the usage ledger (#2062)
- `069e627a` **usage**: cache breakdown with prices on the usage cards (#2027)
- `a5625f1c` **openai**: route providers that need it over the Responses API (#2059)
- `ea86c735` **telegram**: userbot-download CLI command
- `d589dfb6` **voice**: serve stt_model and tts_model from any provider
- `5819a822` **config**: set generation_model on the built-in OpenCrabs provider (#2054)
- `11ba4d6d` **onboarding**: quick-start welcome asks only about what is missing (#2048)
- `1020ccd5` **config**: enable vision on the built-in OpenCrabs provider (#2047)
- `cdf39e45` **sanitize**: redact ocp_ built-in provider install tokens (#2047)
- `75cb5f11` **onboarding**: built-in OpenCrabs provider in advanced and quick start (#2047, #2048)
- `44e71883` **provider**: list the built-in OpenCrabs model in the example config (#2047)
- `6fe59821` **provider**: register the built-in OpenCrabs provider (#2047)
- `a61e5b01` **provider**: request and store the built-in provider's install token (#2047)
- `5bfd7de9` **provider**: store the built-in provider's install ID and token (#2047)
- `2ce43d14` **provider**: install-token client for the built-in OpenCrabs provider (#2047)
- `98d4cc21` **onboarding**: quick-start welcome names the session's provider and model
- `81271c5e` **onboarding**: quick-start welcome names the default model and offers next steps (#2048)
- `884545d1` **onboarding**: quick start gets its own welcome that interviews the user
- `5b71e2f2` **generate_image**: accept several input images on Gemini and OpenAI-compatible backends (#2045)
- `54e300f9` **db**: add ToolExecutionRepository::count_for_session
- `d21300b9` **onboarding**: OpenCode asks for its Go or Zen plan before the key (#2040)
- `fa6c64d6` **whatsapp**: background-completion turns show a live tool block (#2023)
- `cc73a9d5` **slack**: background resume turns show the live tool flow (#1986)

### 🔧 Fixes

- `a2d7aae4` **cron**: apply channel converters before delivery
- `bfb090d9` **telegram**: record the scripted wait in the spacing ledger (#2076)
- `47970a5a` **tui**: draw the new full-screen screens with theme colours
- `fbf6ac6b` **tui**: make /mission-control adapt to the terminal width (#2074)
- `90102245` **tui**: unfocused split panes draw tool blocks the way the focused pane does (#2075)
- `03885a52` **db**: stop publishing GLOBAL_POOL in test builds (#2072)
- `2b11a07f` **discord**: tool rows finish their own call by id, not by name (#1880)
- `76e6712b` **provider**: hold the 200k cap on the built-in endpoint by host (#2067)
- `eda566e4` **provider**: pin the built-in OpenCrabs provider to a fixed 200k context window (#2066)
- `8c6fcf1c` **cron**: evaluate the trigger gate on the unscoped path (#2065)
- `238f984e` **tui**: /models <provider/model> switches directly instead of opening the picker (#2064)
- `33c74d5b` **voice**: split TTS chunks on char boundaries (#2051)
- `99f3d07e` **discord**: the owner's /cowork and /respond_to pass the channel gate (#2061)
- `c2594ef8` **tui**: register the /models session-pair tests and set the session pair last (#2060)
- `1dd40b36` **tui**: /models opens on the session's provider and model (#2060)
- `c37c767e` **telegram**: keep userbot downloads inside the output directory
- `2820f126` **config**: use the bare gemini-3.1-flash-image id for the built-in image model (#2054)
- `d5f02a67` **provider**: resolve the built-in OpenCrabs endpoint for image generation (#2053)
- `05085449` **provider**: resolve the built-in OpenCrabs endpoint for vision (#2047)
- `3add0609` **provider**: keep install tokens out of Debug output (#2047)
- `34a3fe0b` **provider**: name the transport cause in HTTP error messages
- `f7a8bf87` **provider**: a declared-wait cooldown does not make the fallback sticky
- `a6716300` **provider**: a lapsed cooldown window no longer drops the primary from the chain
- `9dbc9035` **provider**: park a provider that declares a long 429 wait, route around it
- `4d962a97` **phantom**: turn-end verdict exempts structured reports from phrases only (#2038)
- `a3ee4430` **phantom**: structured reports exempt phrase matchers, never fact checks (#2039)
- `e8fbe2a0` **phantom**: run the fact checker on zero-tool turns (#2038)
- `39735b7b` **phantom**: gate structured-report exemption on tools having run
- `626b37dc` **tui**: show Shift+Tab Back on the provider step and in /help (#2043)
- `fa076614` **telegram**: /models shows the live CLI list, and Current is the configured default (#2041)
- `c6f81d6d` **telegram**: apply-to-all asks first and writes only on Confirm (#2042)
- `57a7e579` **discord**: DM turns reach channel_messages on both sides (#2025)
- `afddd246` **slack**: store DM user messages in channel_messages (#2034)
- `1f378cb6` **trello**: save card attachments to the channel attachment store (#2033)
- `3ba15c9a` **slack**: save inbound files to the channel attachment store (#2032)
- `a2ebdf95` **discord**: save inbound files to the channel attachment store (#2031)
- `96ed46c8` **tui**: keep the final answer visible when tools ran after its text (#2026)
- `e4f80f6a` **whatsapp**: background-completion turns offer suggestions and post the held answer (#2030)
- `c91869d5` **slack**: post the answer before the suggestion buttons (#2029)
- `f7e00a22` **discord**: post the answer before the options buttons (#2028)
- `8a034064` **usage**: model costs come from the ledger, not the cache-blind estimate (#2024)
- `f31de15a` **onboarding**: send the welcome when /onboard completes (#2022)
- `df0e9d52` **provider**: one 120s stream idle default for every OpenAI-compatible provider (#2021)

### 📖 Documentation

- `8f134a7e` templates: correct the false tier claim in commands/README (#1991)
- `9df15404` readme: document the opt-in skill tier (#1991)
- `3a948554` tests: correct the stale GLOBAL_POOL note in auto_title_e2e_test (#2072)
- `6f76827e` readme: whatsapp_send carries the newsletter actions, and the rail that gates them (#1827)
- `4e584c30` config: the broadcast allowlist also gates the newsletter surfaces (#1827)
- `f1fda41c` readme: the Discord tool card: paging, repeats, the colored bar and Log (#1880)
- `405c7175` readme: /cowork writes respond_to = all; channel entries pass the channel scope (#2070)
- `fcf2b783` provider: the built-in provider's window is fixed and its key ignored (#2068)
- `fdee09c5` note the userbot download command
- `7bb599c1` readme: state unlimited built-in tokens with abuse protection
- `119a359f` readme: update wizard steps and provider priority for the built-in provider (#2056)
- `759fe34a` readme: image generation is on by default for the built-in provider (#2054)
- `2b607271` readme: document the built-in OpenCrabs provider and first run (#2055)
- `7958de5e` provider: describe the current vision candidate order (#1318)
- `0a09b3d5` provider: list the built-in OpenCrabs provider in the README and provider reference
- `b77faaa9` README: onboarding videos per OS, quick start and advanced

### 🧹 Miscellaneous

- `06313a0e` discord: assert tool-group retention on memory, not on a SQLite restore (#2071)
- `a4299eb0` telegram: use a portable output directory in the download test
- `cc128b6f` telegram: check the userbot download direction
- `fca50d3f` deps: bump rustls to 0.23.46, clearing four RUSTSEC advisories
- `5f1cff50` sanitize: pin redaction of 64-char hex install tokens (#2047)
- `eea82e62` onboarding: move the completion path into finish_onboarding
- `dfe8ce52` config: write several keys of one section in one atomic write
- `25a08ffa` provider: a lapsed primary that fails without a wait stays in the chain
- `5a359135` db: remove unused count_for_session (#2036)

### 👥 Contributors

5 contributors this release:

- @adolfousier (Adolfo Usier)
- @loonix (Daniel Carneiro)
- @moneyacademyKE (Moe)
- @adi805 (Syafriadi)
- @mysteriouspants (Christopher R. Miller)

### 📊 Stats

- 99 commits since v0.5.5
- 213 files changed, +17,230 / -1,738 lines
- 10,762 tests (10,724 passed, 0 failed, 38 ignored)

## [0.5.5] - 2026-10-10

283 commits since v0.5.4, 9 contributors. 471 files changed, +51,885 / -4,903 lines.

A channel-native window. Discord became a first-class surface: native polls via send_poll (#1848), TTS replies delivered as voice messages (#1849), commands.toml projected onto native slash commands (#1850), a flow group that opens at turn start with a live clock covering thinking and activity text leading the flow line (#1841, #1843, #1844, #1845), a persistent plan card riding one shared outbound write governor with 429 park (#1910, #1912), per-channel respond_to and open modes with a /respond_to command (#2014), and /cowork opening a channel or thread to its members (#2015). Slack matched the pass: throttled flow ticker (#1807), flow group opening at turn start with live activity text (#1808, #1809), and an outbound write governor with 429 observability (#2012). Telegram got observability and rate discipline: instrumented getUpdates polls and handler handoffs for stall triage, a cross-surface sub-second spacing floor with per-second admission telemetry (#1927), a per-chat 429 pause behind one Cooldown accessor, markdown links to local files delivered as documents (#1916), a flag to keep markdown documents out of the rich media array (#1968), and mermaid fences rendering inside trailer bubbles via the hooked rich send (#583). ACP implemented the five v1 session lifecycle methods and publishes the plan as a v1 plan update (#1815), and decodes image prompt blocks into IMG attachments. The TUI unified every dialog on a shared command footer with direct a/r approval verbs (#1775), made URLs and file paths clickable (#1772), redesigned onboarding around a left-side step timeline with direct setup commands (#1979, #1980, #1981, #1982), hinted when the clipboard holds a screenshot, and stamped the banner with the running binary's build time (#1965). The claude-cli provider grew background-task lifecycle surfacing into the agent mirror, process-group isolation with group kill, and post-turn completion delivery (#1774, #1776). Cron retires one-shot jobs on delivery instead of re-firing a year later (#544), stamps every delivered run with its run id (#1703), and Discord cron reports arrive as one forum post (#1851). Windows carried a LockFileEx saga of its own (9 scoped commits) and stopped telling users to run as Administrator or use WSL2. Restart and resume paint revived turns as processing and park is a third resume outcome in the boot ledger (#1951, #1952). governance.toml plus a policy-lint CI job landed (#1934). The DB snapshots the image before migrations touch it (#1779), supervised brew evolve bounds its children and reads the installed version back (#1779), session_notify got a kill switch defaulting off (#1802), and suggest_options recovers channel-safe option chips from final text (#1774).

### ✨ Features

- `185d67fe` **discord**: /cowork opens a channel or thread to its members (#2015)
- `9c4b6cd8` **slack**: outbound write governor with 429 observability (#2012)
- `f8997baa` **discord**: /respond_to in a channel or thread sets that channel's mode (#2014)
- `cd690000` **discord**: enforce per-channel respond_to and open in the message filter, ACL and command gate (#2014)
- `bc8e67b9` **config**: per-channel Discord respond_to and open under channels.discord.channels (#2014)
- `fb21a285` **governance**: governance.toml + policy-lint CI job (#1934)
- `2dd900b0` **discord**: persistent plan card on the shared governor budget (#1912)
- `11a8341d` **discord**: outbound write governor with 429 park (#1910)
- `0a407f3e` **telegram**: a flag to keep markdown documents out of the rich media array (#1968)
- `580e02de` **telegram**: deliver markdown links to local files as documents (#1916)
- `cd740363` **channels**: route /voice, /image, /channels from chat and name the direct setup commands in hints (#1981)
- `49d7c53a` **tui**: direct setup commands replace /onboard:<step> in dispatch and autocomplete (#1981)
- `ecce856b` **onboarding**: brand line into the timeline, one-line header title, spaced header rows (#1980)
- `3d7a39ec` **onboarding**: move the progress dots into a left-side step timeline (#1979)
- `1ecdd7e7` **acp**: decode image prompt blocks into IMG file attachments
- `766291b6` **1927**: per-chat 429 pause behind one Cooldown accessor
- `357e51ac` **tui**: stamp the banner with the running binary's build time (#1965)
- `cd943040` **telegram**: cross-surface sub-second spacing floor + per-second admission telemetry (#1927)
- `214b31ab` **telegram**: log every outbound request on the Telegram surface
- `73e4907b` **tools**: add maintainer rebuild skill template (#1958)
- `cd8bfa3c` **resume**: paint revived turns as processing in the restarted TUI (#1951)
- `7d8e29ba` **restart**: park is a third resume outcome in the boot ledger (#1952)
- `019a0aeb` **cron**: stamp every delivered run with its run id (#1703)
- `6f898e26` **tui**: Ctrl+C expands the active dialog's command panel (#1775)
- `56a449d7` **tui**: direct a/r verbs on the inline tool approval (#1775)
- `9b1c6bc4` **tui**: migrate every remaining dialog to the shared footer (#1775)
- `87cf8ec1` **tui**: detail popup, profiles and skills share the command footer (#1775)
- `7a4db912` **tui**: scoped dialog keymap catalog + shared footer/chrome helpers (#1775)
- `f095e1c2` **clear**: quote the last six exchanges in the /clear marker (#1905)
- `5cfa2f70` **whatsapp**: post image statuses from status_update
- `f26ab03e` **acp**: implement the five v1 session lifecycle methods (#1815)
- `506f8cdf` **acp**: name the tool and publish the plan as a v1 plan update (#1815)
- `7a8e7901` **acp**: implement the five v1 session lifecycle methods (#1815)
- `257fe32e` **discord**: default trace_narration to true (#1871)
- `d6da814a` **channels/discord**: project commands.toml onto native slash commands (#1850)
- `f4fd1f83` **cron**: deliver Discord reports as one forum post (#1851)
- `dac49692` **channels/discord**: send TTS replies as native voice messages (#1849)
- `10dd28b9` **channels/discord**: native polls via send_poll (#1848)
- `4fc124fc` **discord**: flow group opens at turn start, clock covers thinking (#1845)
- `453863f6` **discord**: live activity text leads the flow line (#1844)
- `ca794fb2` **discord**: flow line live ticker with settle-race guard (#1843)
- `7b3d9db5` **discord**: flow-group clock and settled ctx line (#1841)
- `401b337f` **tui**: vanishing hint when the clipboard holds a screenshot
- `42ce7dfe` **slack**: live activity text in the flow line (#1809)
- `1570fe77` **slack**: open flow group at turn start, delete static thinking placeholder (#1808)
- `a444a480` **slack**: throttled flow ticker keeps the clock live (#1807)
- `832fa336` **cron**: a one-shot cron job retires itself instead of re-firing a year later (#544)
- `40a28f7b` **config**: session_notify kill switch under [agent], default off (#1802)
- `9931f2bf` **telegram**: instrument getUpdates polls and handler handoffs for stall triage
- `55713984` **channels**: inject per-session channel capabilities into the system preamble
- `b6987fe1` **brain**: deliver claude-cli task completions after turn end (#1776 seam 3)
- `688f3659` **agent**: mirror claude background tasks into BackgroundTaskManager
- `e0ae338f` **feat**: recover channel-safe suggest_options from final text (#1774)
- `7b82d18e` **cli**: isolate claude_cli spawns in a process group and kill the group
- `a867a20a` **cli**: surface claude_cli background-task lifecycle events from the System arm
- `25f97826` **db**: snapshot the image before migrations touch it (#1779)
- `31f3f07e` **telegram**: #31 trailer bubble renders mermaid fences via the hooked rich send (#583)
- `95fcbae5` **tui**: open URLs and file paths on click (#1772)

### 🔧 Fixes

- `692616fc` **whatsapp**: a ban or lock alerts the owner over Telegram; five named events stop falling through (#1999)
- `1ff7ca0b` **discord**: the owner's unmentioned /respond_to passes the mention gate (#2016)
- `0485d75f` **discord**: route interaction replies through the governor (#2011)
- `acb2f919` **whatsapp**: ban, lock and connect-failure events surface to the owner (#1999)
- `d55a2406` **commands**: /respond_to outside Telegram never writes the Telegram section (#2013)
- `6d6fcc81` **discord**: the settled flow line names how the turn ended (#1911)
- `1504fa95` **agent**: the compactor is told which skills are live (#1960)
- `40f11223` **telegram**: a boot resume adopts the bubble of the turn it interrupted (#2010)
- `2f70fb7c` **agent**: a provider rotation no longer writes the user message twice (#2009)
- `1a7492bc` **tui**: a session keeps its own working directory (#2006 #2007 #2008)
- `9829980f` **telegram**: markdown documents ship detached unless inlining is opted into (#1968)
- `8257134c` **telegram**: a model-written marker prefix no longer doubles the file marker (#2001)
- `f99e61a7` **telegram**: the rich fallback respects media-bearing intermediates (#1939)
- `532a17f2` **telegram**: disarm the autolinker on a delivered file's marker (#1938)
- `5fed12b8` **telegram**: a delivered document arrives named after its file (#1937)
- `b356b762` **telegram**: a document reference never reaches the wire without its entry (#1921)
- `08cb9407` **telegram**: splice bubble links onto file markers and inline documents in rich (#1918)
- `f0b88eac` **telegram**: leave a visible marker where a delivered file link was (#1918)
- `5a71bd3c` **search**: append a deterministic Sources footer to search-backed answers (#1883)
- `331ae325` **compaction**: remove the resolve leg, its consumer is fork-only wiring
- `d8d7445a` **compaction**: strip the fork-only inventory slot and BUDGET bullets
- `e312cf16` **compaction**: drop the conflict marker left by the trim-guard pick
- `91ad9e5b` **compaction**: resolve a missing manifest fence to the pruning-safe default (#1933)
- `f510e79f` **compaction**: head-protect the continuation prompt's must-keep blocks (#1933)
- `7c6ce50a` **compaction**: anchor the loader's segment skip on the delta banner, not a substring (#1928)
- `f8ddffed` **compaction**: persist the marker the apply welded in, not a rebuilt banner (#1928)
- `bf103717` **channel_search**: the scope header count reads "N messages" (#718)
- `524cff64` **channel_search**: inherit the session's forum topic by default
- `8763420a` **compaction**: allow reasoning headroom and surface truncation (#1933)
- `eb639bc0` **compaction**: collapse the guard's nested conditionals to satisfy clippy (#1930)
- `d198f448` **compaction**: a structural trim guard enforces the continuation-document budget (#1930)
- `6df61f71` **compaction**: the continuation-document prompt states its budget and retention order (#1930)
- `79570abd` **compaction**: one budget constant for the continuation document (#1930)
- `ea349580` **db**: take the integrity scan off every database open (#459, #677)
- `07536197` **compaction**: flag artifacts a summary reports but no tool call produced (#482)
- `3f008608` **discord**: run recovered turns visibly, hold the session slot, queue follow-ups (#1990)
- `059e4a4a` **channels**: bound the typing tick and clear stuck background rows (#1984)
- `176f1fc0` **whatsapp**: give the composing indicator parity for background work and resume turns (#1989)
- `c4531aa6` **slack**: flip the waiting step group on completion, count sub-agents, keep thread
- `577cba05` **discord**: show the waiting state at settle and stop the clock on error (#1987)
- `49f46fa0` **deps**: bump rusqlite to 0.40 (SQLite 3.53.2) to remove a unix-VFS deadlock (#1982)
- `2562a62f` **tui**: scrub SGR mouse fragments at the key dispatch choke point
- `de12bc4a` **cron**: bake oc://session targets as session:<uuid> so wakes land in the addressed session (#1961)
- `3f1c8420` **cli**: classify A2A transport failures in the journal detail so the mechanism is discriminated (#1959)
- `34d314de` **memory**: enforce the shared-session gate on brain, memory, load_brain_file and recall surfaces (#1957)
- `468711df` **tests**: watchdog the test leg with thread dumps instead of a silent timeout burn (#1936)
- `4e7ee7c5` **cron**: stamp tick liveness and expose it on the daemon health endpoint (#1925)
- `239f7fe6` **tui**: shape and reorder RTL text on the render path (#1897)
- `74eab4f0` **onboarding**: pad and tint the header and footer bands (#1975)
- `5fd046cb` **onboarding**: label Esc as Exit on step 1 of a re-run (#1976)
- `4f1bdd06` **onboarding**: derive the mode select step counts from the progress totals (#1977)
- `865e11d6` **telegram**: satisfy clippy::question_mark on the owner-fallback guard
- `dc76ef32` **cron**: one un-persistable schedule advance must not abort the tick
- `e8751b70` **plan**: scope the trailing auto-complete to delivery-shaped tasks
- `0a084b8e` **plan**: expire the Active plan reminder after staleness
- `12c73700` **discord**: ack command invocations with the deferred source type
- `bf9d1140` **config**: seed the Ralph verification gate on every profile
- `4836e3f1` **brain**: disclose the rich_messages gate in the Telegram capabilities block
- `ed59b083` **telegram**: refuse owner-fallback sends for non-Telegram sessions
- `378c3af8` **tools**: thread typed args into user commands and skills (#1868)
- `842647f0` **db**: cron list_all skips unreadable rows instead of failing (#1924)
- `43695066` **onboarding**: render the wizard full-screen and responsive (#1973)
- `176ad463` **onboarding**: quit a first run on Esc or Ctrl+C, require a model on the provider step (#1973)
- `52efab90` **cron**: print the anyhow chain at the supervised-tick sites (#1972)
- `342a3e28` **trello**: route outbound card text through the strip_llm_artifacts boundary (#1970)
- `bab5acfb` **utils**: keep typographic dashes as hyphens on markdown table rows (#1969)
- `175cc0e4` **cron**: print the anyhow chain at the three supervised-tick log sites
- `6b7ed48b` **1927**: widen the surface-policy field types to the policy table's own visibility
- `0f3b81c6` **1927**: name the chat at the rich 429 sites instead of a scoped binding
- `eb8a2d6e` **tui**: restore the terminal in full on crash recovery, signals, startup (#1964)
- `b6ee1b9c` **cron**: supervise every tick and job so one panic cannot kill the scheduler (#1963)
- `20b25950` **telegram**: render the rich HTML fallback with the paragraph-wrapping renderer
- `9f53c547` **skills**: move rebuild template out of the builtin-skill directory
- `cc03e6ed` **telegram**: surface admitted_sends on the governor test Snap (#1927)
- `2ac9c06f` **core**: sweep the residual rebuild references the removal missed (#674)
- `08e06499` **resume**: chunk discord answers at the message cap instead of losing them (#1953)
- `bd5a475a` **resume**: wait for the transport, announce recovery, count outcomes after the send (#1950, #1952)
- `010313fe` **discord**: deliver CLI-provider answers in trace mode instead of dropping them (#1942)
- `aea27434` **discord**: resolve every tool-group toggle interaction with ephemeral fallbacks (#1949)
- `2785eebb` **discord**: clamp tool-group bubble to the message cap and answer every toggle (#1949)
- `7603e5d9` **tui**: give the plan seed strip breathing room (#1945)
- `27318cef` **tui**: always keep the newest message in the display-budget trim (#1944)
- `ef9577d1` **tui**: scrub SGR mouse-report garbage out of the input buffer (#1943)
- `9a3d472b` **cron**: escalate repeated tick failures to the delivery target (#1893)
- `1df0e910` **db**: skip unreadable cron_jobs rows instead of failing the whole read (#1893)
- `e33bc2d2` **cron**: log the full anyhow chain in scheduler errors (#1894)
- `cbd4beaa` **cron**: truncate delivery output at a UTF-8 char boundary (#1919)
- `e81d4e47` **cron**: compaction boundary at run start, not end (#1703)
- `322cc935` **tui**: clear the clippy errors leaving main red
- `b390daea` **cron**: a run that executed no tools earns no success (#1703)
- `14858d49` **telegram**: a notify past the classic cap loses its tail (#490)
- `3addf2e7` **provider**: tolerate usage-only stream chunk without id
- `6ae8685b` **discord**: chunk bg-resume delivery so long verdicts reach the channel
- `162b7511` **discord**: dispatch mention-only messages and fold the referenced reply (#1890)
- `b559929c` **discord**: surface forwarded message snapshots in turns and history
- `ca2d0b1e` **vision**: send opencode identity headers and gate unauthable candidates (#1792)
- `9b56f710` **vision**: send opencode identity headers and gate unauthable candidates (#1792)
- `7bb63b78` **acp**: emit official configOptions and stop echoing sessionId on load
- `40ff35ce` **acp**: move custom session methods under the _opencrabs extension prefix
- `37be9083` **acp**: keep stopReason inside the v1 enum, map StopSequence to end_turn
- `651b3c2f` **telegram**: plan cards now reach channel_messages, so the agent can read its own cards (#1684)
- `e19b4332` **agent**: fold a queued follow-up into one final reply (#1784)
- `d19ec718` **acp**: emit the schema's usage_update frame instead of an invented one
- `3734b14e` **tui**: persist the panic hook's post-mortem to the log dir (#1791)
- `cced211b` **evolve**: swap the Windows binary atomically and report what actually happened (#1820)
- `62b089e7` **tui**: give Windows a real clipboard path (#1822)
- `e99d915c` **ci**: Windows build gated at job level so a filtered run reports skipped (#1831)
- `6fc21fe7` **discord**: route follow-up taps through the tool-loop display path
- `319417ed` **slack**: route follow-up taps through the tool-loop display path
- `e2fdd447` **notify**: retire the durable twin at the delivery that replaces it (#439, #366)
- `0731e34e` **windows**: quoted triggers must reach cmd.exe verbatim
- `2a570a70` **windows**: cron trigger spawns the platform shell, not /bin/sh
- `a9f64be2` **discord**: drop ctx footer from answer messages, settled line is the only ctx surface (#1842)
- `d7f9cba1` **config**: session_notify tool registration gated on [agent] session_notify_enabled (#1840)
- `af5861ab` **telegram**: drain the whole reaction queue into one joined injection
- `f5256a0e` **provider**: bound each HTTP send, not the provider.stream() subtree
- `30eeb987` **db**: skip the pre-migration copy when nothing will write
- `24df9a9b` **acp**: emit authMethods in the initialize response (ACP v1 schema)
- `e6ebcb6a` **vision**: surface every failed analyze_image candidate in the error (#1792)
- `cc0cb03f` **profile**: pick the live lock owner by the kernel's own reading
- `9bcd8c10` **profile**: the owner map is readable from the sibling test module
- `91685cc6` **profile**: resolve a reused PID to the newest creation time
- `4929830f` **profile**: a stamped live owner must parse as an owner
- `9f524fd0` **winlock**: prove ownership by exact creation time, not a coarse mtime
- `143edef6` **winlock**: allow for a coarse mtime before calling an owner a recycled PID
- `6bd3b188` **windows**: the lock tip must compile on the target it ships for
- `a461138e` **lint**: gate the owners mut to the target that actually mutates it
- `a4bcde3b` **windows**: the kill must prove birth, and the lock must spare the stamp
- `ee2152b7` **windows**: fail-closed target verification in preempt terminate
- `3ee24c98` **windows**: real LockFileEx for instance, scheduler, registry, path locks
- `eadf0876` **memory**: default vector_enabled to false, local embeddings become opt-in (#1798)
- `f0cf5cbc` **tui**: Cmd+V pastes and SUPER chords no longer type a literal character (#1812)
- `5b64b8e6` **tui**: Ctrl+V falls back to the clipboard image when the clipboard holds no text (#1811)
- `5d2fa506` **slack**: ctx footer off every answer message, settled line only (#1806)
- `f4876ced` **slack**: consume folded narration on empty-final salvage, drop dead intermediate branch (#1805)
- `fffb7584` **sessions**: hoist persisted /cd restore above lazy cwd seed (#1810)
- `b844b213` **telegram**: get_me retry predicate for teloxide ApiError shape
- `bcbd3286` **telegram**: add retry to get_me() startup call (#1785)
- `b55394ce` **cron**: substitute the fired trigger result into the agent-path prompt (#526)
- `f83f98c9` **slack**: always-on tool counter, ctx budget and settle pass on the step group (#1797)
- `a51ac8fb` **fix**: de-dash hardcoded user-facing strings on the alert and plan paths (#1745)
- `eec82266` **fix**: normalize dashes on the SelfHealingAlert display path (#1745)
- `1c485887` **telegram**: suggestion button folds to a bare digit when its label carries an HTML entity (#396)
- `b7918f9a` **goal**: an armed goal no longer claims a driver it does not have (#480)
- `ba1c9ddb` **notify**: refuse an unbound session target instead of parking it (#574)
- `93d8b07d` **tests**: declare tokio tests as async fn
- `74c0ab19` **telegram**: suppress reaction acks while global cooldown is active
- `53a4bff1` **evolve**: bound brew children, read the installed version back, and arm the systemd restart (#1779)
- `0adc3bea` **db**: surface corruption and the restore asset outside the TUI (#1779)
- `8084f8f0` **db**: check integrity before migrations and refuse with a restore path (#1779)
- `0d79cc83` **provider**: preserve model IDs in streaming responses
- `1f668030` **provider**: preserve requested model IDs
- `f81ea664` **notify**: reject an absent session_notify target instead of promising a delivery
- `6a6f603c` **compaction**: derive section 0 obligation status instead of carrying it verbatim (#499)
- `0887a624` **timezone**: parse markdown-table rows and Russian city-name forms (#348)
- `bdb11171` **#555**: bash guard tests interactivity, not flag strings
- `336db908` **context**: serialise the session_context read-modify-write (#600)
- `c8a848aa` **tools**: keep same-path writers out of one parallel batch (#593)
- `901f7067` **telegram**: terminate a blockquote run before a block-level HTML opener (#552)
- `7cb5c1ad` **systemd**: add OOMPolicy=continue to the generated unit files
- `b7a3ceee` **telegram**: deliver a push to its session binding, not the topic that spoke last (#478)

### 📖 Documentation

- `72840322` **discord**: DISCORD_COMMANDS.md covers /respond_to, /cowork and per-channel settings (#2020)
- `38a73d72` **readme**: per-channel Discord respond_to and open (#2014)
- `855d8f26` **channels**: one copy of the governor policy matrix, under src/docs
- `c77f1418` **telegram**: teach the capabilities preamble the file-delivery shape (#1940)
- `b4599a91` **docs**: moved governor to real docs dir
- `804a7c56` **docs**: name the direct setup commands across README, docs and the TOOLS template (#1981)
- `2aeee521` **docs**: add MonoCode GUI section and opencrabs acp CLI row (#1967)
- `c4b4aefc` **discord**: stop the ack comment quoting its own banned pattern
- `ee5fccaf` **readme**: troubleshooting entry for inherited macOS malloc stderr
- `d3f02fac` **1927**: land the upstream governor policy matrix as the frozen contract
- `fb2b1910` **readme**: fix MonoCode misnaming in the ACP surfaces (#1966)
- `767069de` **contributing**: correct MSRV to 1.94 and the CI gate descriptions (#1955)
- `a996833b` **contributing**: correct the post-org repo slug in the duplicate sweep
- `85535807` **core**: drop the last /rebuild references from the shipped examples (#674)
- `5dd026d6` **config**: the typing-knob comment justified its default with a per-topic bound the code does not implement
- `dc020d21` **docs**: document issue routing, core markers, and the PR lifecycle
- `01fcb6eb` **contributing**: drop chore from commit types, document core scope and breaking marker (#1948)
- `e9fe96cd` **docs**: removed inspired by OpenClaw as it was at the very beginning, today we get inspired by all other harnesses
- `fe78f7e4` **tui**: reword the detail-popup comment to drop the phantom CLI name (#1775)
- `eb4a67c6` **discord**: document trace_narration in example config and README (#1872)
- `00319b54` **windows**: stop telling users to run as Administrator or to use WSL2
- `f37d2c2f` **config**: document [agent] session_notify_enabled kill switch (#1802)
- `1560318a` **readme**: replace the dead-end salvage recipe with the full page-1 procedure (#1779)
- `591ee54e` **readme**: document the pre-migration snapshot, the integrity refusal and the supervised brew evolve (#1779)
- `91e310cd` **tui**: /help dialog Ctrl+C scroll-to-bottom (#1771)
- `dea8ed37` **readme**: Ctrl+C scroll-to-bottom in shortcut table (#1770)

### 🧹 Miscellaneous

- `2957a2cb` **discord**: hold the cooldown lock across governor config swaps (#2018)
- `9bd88788` **discord**: the two clock-advancing governor tests hold the cooldown guard (#1910)
- `f09ff663` **channel_search**: drop the glue_target mod, it fixtures on the fork-only ship plane
- `e099e1c2` **style**: apply rustfmt across the landed lane commits
- `68bc4162` **compaction**: pin both #1933 legs, the head-protect cut and the harness default (#1933)
- `4bb426f6` **compaction**: pin the delta persist seam so the DB row must carry the sentinel banner (#1928)
- `1cc809a8` **compaction**: a five-round soak pins the continuation document to its budget (#1930)
- `5a297acc` **channels**: move background-work waiting helpers out of Telegram (#1985)
- `15bfa6c1` **onboarding**: gate build_onboarding_systemd_unit with cfg instead of allow(dead_code) (#1978)
- `4c828a82` **1927**: fix two dead summary asserts and de-flake the 429 cap check
- `0c5f7304` **1927**: extract pace_engine; four gates become thin wrappers
- `776f7fd5` **1927**: land the surface policy table as a frozen descriptor
- `06f41a61` **telegram**: cargo fmt the spacing-floor and telemetry surfaces
- `0fd5f294` **core**: remove /rebuild and the rebuild tool from the public tree (#674)
- `fb28a79b` **cli**: pin restart-resume delivery outcomes, notice, presence and chunking (#1950, #1951, #1952, #1953)
- `55375426` **discord**: skip the summary line when counting rows shown by the clamp (#1949)
- `74678cf2` **cron**: drop needless as_bytes in the delivery truncation test (#1942)
- `c4e8cb4e` **chore**: point repository references at the opencrabs org
- `69679f94` **cron**: the bare-error scan counts format strings, not comments (#1893)
- `b94e0863` **ci**: assign maintainer-opened issues to the maintainer
- `9c07495d` **tui**: point the analytics hotkey assertion at the shared footer
- `98226ada` **style**: rustfmt re-wrap of echo truncation and its tests
- `9cae24da` **tui**: drop the dead_code staircase from the #1775 helpers
- `3e7005ac` **whatsapp**: pin that alpha PNGs produce a JPEG status thumbnail
- `f65ec431` **telegram**: enforce the cooldown guard rule with a source scan (#1854)
- `aa271a0e` **windows**: execute the binary the job builds (#1823)
- `334a4d3e` **windows**: make the footgun linter a required gate (#1823)
- `ee48fbb6` **style**: rustfmt the fmt-drift files on main
- `b1716622` **test**: serialize global 429 cooldown access across parallel test modules
- `e9afd8ac` **style**: rustfmt the four merged PR trees
- `1aece86a` **telegram**: hold registry_guard in the cooldown-arming retry tests (#1832)
- `1e480a8c` **rsi**: give the self-improve apply tests per-test temp homes (#1800)
- `98164b6f` **backfill**: build the opt-in config with struct-update syntax (#1798 follow-up)
- `06d72586` **profile**: shape the reused-PID stamps to the rule each platform uses
- `83cd1438` **backfill**: assert the 300s default interval on an opted-in config (#1798)
- `a8a9693d` **test**: add em-dash guard for self-heal alert constants and renderer coverage
- `110df5c4` **provider**: relocate stable_model_id tests to src/tests per contributing.md
- `dea20183` **lint**: annotate the macOS-only launchd label parser for non-macOS builds

### 👥 Contributors

9 contributors this release:

- @adolfousier (Adolfo Usier)
- @leshchenko1979 (Alexey Leshchenko)
- @adi805 (Syafriadi)
- @kaaduu
- @Jah-yee (RoomWithOutRoof)
- @loonix (Daniel Carneiro)
- @NadLad (Nader)
- @moneyacademyKE (Moe)
- @Absolute-Law

### 📊 Stats

- 283 commits since v0.5.4
- 471 files changed, +51,885 / -4,903 lines
- 10,366 tests (10,328 passed, 0 failed, 38 ignored)


## [0.5.4] - 2026-09-26

104 commits since v0.5.3, 7 contributors. 225 files changed, +18,619 / -1,429 lines.

An agent-experience window. ACP sessions became first-class: the context meter is restored on session load, session/load replays the transcript and restores the per-session model, set_model persists across processes, session/set_mode applies the approval policy server-side, session/compact pushes, and session/new offers a live model catalog. Full-screen editors (!vi, !nano) now get the real terminal through a handoff and return to the chat on exit (#1744). Attachments went universal: paste or drop any file type through one shared router (#1740), with common-type classification (#1743). Three mechanical zero-cost commands landed compiled: /architecture, /attach (#933) and an opt-in audit trail with its /audit viewer (#1705). Search gained a serper_search engine (Google SERP) with by-URL dedup across the web_search fan-out (#1731); generate_image grew a provider chain at parity with vision (#1672, #1675); and the L1 decision cache now serves exact-repeat classification decisions without a model call (decide_cached tool, [decisions] tiers in shadow/live/off, /usage accounting, TTL sweep) (#1648). Usage billing prefers provider-reported cost over table estimates (#1707), unknown-price models are no longer billed as free, and the timeout chain resolves [providers.<name>] -> [agent] -> default across every provider family, with Anthropic and Gemini honouring it (#1688, #1697, #1689, #1698); compaction notices moved behind compaction_notice, off by default (#1686). Reliability: truncation sets inline-code parity flags regardless of last char (#1753), the plan card budgets against real terminal rows (#1750), tool success-rate health surfaces at model selection time (#1706), WhatsApp inbound attachments persist to the durable store (#1729), getUpdates 409 conflicts escalate instead of silently fighting (#1721) over an atomic insert-or-resolve for live channel sessions, the mermaid connect stage retries once (#1741), reply recovery carries an excerpt and stops chasing the topic root (#548), A2A notify retries are reserve-before-deliver idempotent (#199), phantom-announcement detectors catch marker-less bare participles in en/pt/es (#1694) and score fact detectors on mixed iterations (#1693), and the global 429 cooldown tests tolerate deschedule jitter (#1754).
### ✨ Features

- `dcc2caa4` **audit**: opt-in audit trail recording with /audit viewer (#1705)
- `188d6110` **commands**: add compiled /architecture and /attach commands (#933)
- `4c21f89b` **utils**: normalize typographic dashes out of model prose (#1745)
- `db57b38e` **tui**: terminal handoff so !vi/!nano run interactively (#1744)
- `7cfd9089` **acp**: restore context meter on session load
- `fd2d087e` **acp**: persist set_model pick on the session row for cross-process restore
- `6c70c180` **acp**: include context window size in usage updates
- `bfb97c46` **acp**: replay transcript + restore per-session model on session/load
- `e4c19918` **cli**: add --quiet to run for machine-pure stdout
- `db7485a4` **acp**: session/compact + available_commands push
- `9d00983c` **acp**: native session/set_mode with server-side approval policy
- `7f6eb6f2` **acp**: live model catalog in session/new + pair-aware set_model
- `8cf1302b` **tui**: paste and drop any file type with a shared attachment router (#1740)
- `89757c75` **tui**: classify common file types for paste and drop (#1743)
- `bcaeadf6` **tui**: byte-stream capture around redraw + ANSI strip on model text (#1719)
- `eddff5e1` **agent**: surface tool success-rate health at selection time (#1706)
- `0a64c124` **tools**: serper_search engine (Google SERP) + by-URL dedup in web_search fan-out (#1731)
- `671fd05b` **usage**: ledger prefers provider-reported cost over table estimate (#1707)
- `afaad183` **config**: put the compaction pair behind compaction_notice, off by default (#1686)
- `9bf29047` **provider**: anthropic and gemini honour the timeout chain (#1689) (#1698)
- `715edd67` **config**: resolve timeout flags [providers.<name>] -> [agent] -> default across every provider family (#1688) (#1697)
- `db27e760` **image**: provider-chain for generate_image, parity with vision (#1672) (#1675)
- `c484d19b` **tui**: expose /onboard:daemon on the surfaces its dispatch arm already serves
- `0d4ae8b3` **decisions**: /usage accounting block, mission-control counters, TTL sweep (#1648)
- `a5878e8d` **decisions**: decide_cached tool with shadow mode and [decisions] config (#1648)
- `14fd61b1` **decisions**: add decision_cache storage ring (#1648)

### 🔧 Fixes

- `ef2a939f` **telegram**: absorb deschedule jitter in global 429 cooldown wait floors (#1754)
- `e5c38676` **truncation**: inline-code parity flags regardless of last char (#1753)
- `a04cec30` **tui**: budget the plan card against real terminal rows (#1750)
- `7308b0d1` **tui**: cap expanded working-out rows behind the fold ceiling (#1747)
- `a056b6a9` **acp**: per-call approval override beats config auto-approve policy
- `8b91813a` **acp**: route session/set_mode to the mode handler
- `899db3d3` **acp**: stop doubling streamed answers as round aggregates
- `ab23ac78` **acp**: cover Segment::Blocked in replay and align fixtures with line-start marker contract
- `93c32e42` **telegram**: stop malformed react directives leaking as bubbles; thread reaction replies (#1670) (#1671)
- `f327549b` **telegram**: retry mermaid connect stage once, split transient failure headlines (#1741)
- `99a5a77b` **telegram**: do not chase the topic root in reply recovery (#548)
- `e315f5f0` **telegram**: carry an excerpt in the pruned reply pointer and de-gate reply recovery (#548)
- `ff073ac1` **a2a**: notify transport retry with reserve-before-deliver idempotency (#199)
- `99b4dc01` **channels**: persist WhatsApp inbound attachments to the durable store (#1729)
- `1248150e` **profile**: launchd-aware daemon preempt on macOS (#1726)
- `59ec0a3f` **telegram**: detect getUpdates 409 conflict and escalate instead of silently fighting (#1721)
- `4f61c285` **db**: partial unique index on live channel sessions + atomic insert-or-resolve (#1721)
- `4043b7e7` **daemon**: explicit -p scopes profile adoption; brain seeding waits for the lock (#1723)
- `3a68675b` **usage**: distinguish unknown-price models from genuinely free ones (#1717)
- `53c90256` **provider**: single-final-delta guard on [DONE] — no duplicate finalize after reported usage (#1738)
- `851f32b4` **agent**: anchored continuation nudge + bounded second attempt on degenerate truncation recovery (#1737)
- `74fedd09` **shell**: run command strings under a real bash, with honest sh fallback
- `823caa3c` **compaction**: gate WARN shows one decimal with explicit threshold label (#1733)
- `15bd5eab` **tui**: keep the final CLI answer visible when a reasoning block trails it
- `395d2e14` **doctor**: report unrecognized config keys in /doctor (#1725)
- `68609f8d` **config**: report nested stale keys in loader typo warning (#1724)
- `b5d87b69` **rsi**: read live config per cycle instead of a startup snapshot
- `c1e691c7` **telegram**: gate the compaction header pin behind compaction_notice (#1686)
- `d682cee9` **telegram**: gate forum evidence on is_topic_message, banner stops leaking reply-chain ids (#1708)
- `cd7c6539` **telegram**: TTS voice notes carry the session thread id (#1683) (#1712)
- `e80d5c83` **pdfium**: find single-digit pdftoppm page output names
- `1be75700` **pdfium**: bind the library once per process behind a shared OnceCell
- `844b392a` **compaction**: announce the gate once per compaction, not per tool iteration (#1686)
- `f48fb79f` **compaction**: receipt the after-fill on the meter's own basis (#1686)
- `cafba0a9` **compaction**: announce the fill the gate requested, not the live one (#1686)
- `ef4dc7e2` **phantom**: catch marker-less bare-participle announcements in en/pt/es (#1694) (#1710)
- `f2f9f5ca` **phantom**: score fact detectors on mixed iterations (#1693) (#1709)
- `a4e633d8` **agent**: thinking-loop guard stands down on a delivering stream and is per-provider scopable (#1690) (#1699)
- `124a3cdc` **provider**: streams must not inherit the transport total wall clock (#1687) (#1692)
- `a7372ee9` **compaction**: name the roast in every fun arm, as a protocol step (#1685)
- `6cc2094f` **compaction**: drop the brain-file weld from the marker and round the confirmation like the log (#1676)
- `d0ebd2cb` **context**: stop anchoring the ctx meter and compaction budget on Claude CLI's cache figure (#1677)
- `74d84753` **config**: drop the phantom web_search.duckduckgo table from config.toml.example (#1669)
- `92e71272` **tui**: report an unknown /onboard: suffix instead of silently opening the wizard (#1664)
- `04c14479` **provider**: sticky fallback counts rescues in a window, not in a row (#1667)
- `378e94d1` **provider**: retry banners and logs must count from MAX_STREAM_RETRIES (#1668)
- `c4cc0940` **provider**: z.ai idle default must sit above the host's own 30s cut (#1666)
- `8f3ee318` **decisions**: fold TOOLS note to fit 100-line template cap (#1648)
- `0a4d5072` **decisions**: bind decision_stats result in analytics summary (#1648)
- `c3e1f16a` **tests**: separate module-doc list from trailing prose for rustdoc lint
- `32481db3` **decisions**: merge carries the decisions overlay, stub provider completes the trait
- `a3cc31eb` **decisions**: collapse nested margin-floor check for clippy

- `0fb15d5a` **Windows release build**: the interactive editor handoff (#1744) called unix-only libc symbols (pid_t, waitpid, kill, SIGCONT/SIGTERM/SIGKILL) with no cfg gate, so the msvc leg of the release workflow failed with 13 E0425 errors; the whole editor module is now #[cfg(unix)] at the mod declaration, parking and consumption sites are gated individually, and Windows bang commands keep the v0.5.3 pipe-capture path (#1759)

### 📖 Documentation

- `4522cd9e` **audit**: audit recording section and template README for #1705
- `ca6fef49` **opt-in**: opt-in command templates and README docs for #933
- `a54e09fa` **readme**: bang operator editor handoff (#1744)
- `2dffbd2c` **readme**: document F12 mouse-capture toggle and local file drag & drop (#1739)
- `60eab754` **readme**: pin harness identity in ~/.claude/CLAUDE.md for claude-cli users (#1711)
- `43f1c465` **readme**: CLI providers are full agents running their own tools, not LLM backends (#1695)
- `0b6cf46c` **brain**: record that the compaction marker weld is deleted, not narrowed (#1676)
- `cc73d888` **provider**: document stream_idle_timeout_secs and its default tiers (#1668)
- `7dd808a3` **decisions**: [decisions] reference page, README section, TOOLS note (#1648)

### 🧹 Miscellaneous

- `0ad02c1f` **tests**: anchor chat-key migration cutoff by content not position
- `1783a542` **rebuild**: run rebuilds through detached background tasks, drop the cron special-case (#1748)
- `ca153a11` **tests**: rustfmt reflow of telegram dedup and quote-reply tests
- `5791084f` **tui**: editor handoff allowlist, decode, and reap coverage (#1744)
- `39dcd21e` **acp**: externalize catalog tests to src/tests, pin usage null-token cascade, cover --quiet parse (#1674)
- `8f2c7667` **tests**: drop wildcard field patterns clippy 1.98 rejects
- `cdf77888` **acp**: ext-prefix the steer method
- `878c4af5` **ci**: raise hosted timeout caps for fork PRs (lint 15->25, test 30->60)
- `37e591df` **usage**: pin unknown-model conservative-default billing (#1717 follow-up)
- `2f31fb8c` **config**: guard both shapes of the #1249 sweep
- `0f0ad3ab` **tests**: pin the channel half of the config freeze (#1700)
- `264aa23a` **channels**: reload factory-built agents on config change (#1700)
- `0870e9a7` **telegram**: source scans pin the thread-id invariants - no bare sends, no raw ids into rate gates (#1708)
- `29fda59a` **tui**: /doctor routes straight to the health check, drop the hidden /onboard:health arm (#1665)
- `1f039341` **theme**: serialise the process-global theme slot across test modules
- `888b2307` **decisions**: rustfmt decide_cached and its test module

### 👥 Contributors

7 contributors this release:

- @adolfousier (Adolfo Usier)
- @moneyacademyKE (Moe)
- @leshchenko1979 (Alexey Leshchenko)
- @adi805 (Syafriadi)
- @loonix (Daniel Carneiro)
- @gefsikatsinelou
- @djaafarishers-cloud

### 📊 Stats

- 104 commits since v0.5.3
- 225 files changed, +18,619 / -1,429 lines
- 9,427 tests (9,389 passed, 0 failed, 38 ignored)


## [0.5.3] - 2026-09-20

121 commits since v0.5.2, 7 contributors. 192 files changed, +12,304 / -1,709 lines.

A consolidation window: the TUI theme engine finished its sweep, every widget now resolves its colours through the theme (a suite-level guard fails on any new raw ANSI site) and themes can declare a canvas background (#1634). Windows became a first-class platform: a platform shell pair, BOM/UTF-16-aware reads, tilde and cwd fallbacks, with CI building and gating PRs on it (#627). Cron jobs can be trigger-gated - a pipeline with mechanical pre-flight and goal dispatch (#233) - and a timed-out trigger kills its shell instead of orphaning it. WhatsApp follow-up suggestions past the native button cap render as a poll, and a vote selects the option (#1616). MEMORY.md facts carry confidence and decay (#1643), with a file-level archive pass, config-driven decay and a `memory prune` CLI (#1657). Telegram gained a process-wide rate limiter and a global 429 cooldown lock (#1630). The reliability seams were tightened: the DONE-stream estimate no longer outranks reported usage and the cached prefix is netted out on compat providers (#1636), a crash-signal handler records SIGBUS/SIGSEGV diagnostics (#352), channel history is deduped and scoped across Slack, Discord and WhatsApp (#1618-#1620), stale notify_queue rows are reaped for unclaimed sessions (#182), and compaction stopped re-deriving prior summaries (#1649).

### ✨ Features

- `8960c2f8` **memory**: file-level archive pass, config-driven decay, memory prune CLI (#1657, #1658)
- `dd9cad2f` **epistemic**: wire confidence + decay to MEMORY.md facts (#1643)
- `064316e0` **tui/theme_catalog**: carry the source background through conversion (#1634)
- `d6ba4710` **tui**: themes declare an optional canvas background (#1634)
- `ca9a14c8` **windows**: utils shell_pair/push_shell_command and launch_cwd helpers
- `9d2c9645` **cron**: trigger-gated cron pipeline with mechanical pre-flight and goal dispatch (#233)
- `b42f51aa` **telegram**: process-wide rate limiter and global 429 cooldown lock
- `cb06aa0f` **whatsapp**: select a follow-up suggestion from a poll vote (#1616)
- `90221b94` **whatsapp**: render suggestion sets past the button cap as a poll (#1616)

### 🔧 Fixes

- `267967b9` **telegram**: stop General-topic history bleed in forum history injection
- `8364a62f` **prompt**: state what the gh fallback can and cannot do
- `cb24fdef` **tests**: one declaration per test module
- `2c789cef` **tests**: reattach the telegram cfg to the module it guards
- `a1267e25` **shell**: report what the process-tree sweep did instead of dropping it
- `7680b683` **tools**: a truncated UTF-16 file must not decode silently short
- `889ce7db` **tools**: a transformed read must not arm a whole-file overwrite
- `cfa3f63b` **agent**: enrich AnnouncementLoop error with diagnostic context (#1647)
- `4ceaff6d` **epistemic**: RalphLoopConfig silently drops the [epistemic] section (#1642)
- `af5ac77b` **cron**: kill a timed-out trigger's shell instead of orphaning it (#1629)
- `a0cda082` **telegram**: record 429s discovered outside wait_out (#1630)
- `14a25295` **telegram**: take the global permit before the per-chat pacing gate (#1630)
- `d8dd7b23` **logging**: stop warning at boot where the crash handler was never implemented (#1637)
- `085a9ad6` **logging**: warm the unwinder at install so the crash backtrace is signal-safe (#1637)
- `bc2c2566` **logging**: install a crash-signal diagnostic handler for SIGBUS/SIGSEGV (#352)
- `b08c3a90` **provider**: stop the DONE-stream estimate from outranking reported usage (#1636)
- `8f5ca58a` **usage**: anchor the ctx meter and compaction budget on the gross prompt (#1636)
- `fd67e49f` **usage**: net the cached prefix out of input_tokens on compat providers (#1636)
- `2773f5d1` **tui/onboarding**: resolve the voice step's colours through the theme (#1634)
- `6f4c01ce` **tui/onboarding**: resolve the wizard's colours through the theme (#1634)
- `3e3d995e` **tui/mission_control**: resolve panel error colours through the theme (#1634)
- `1f6b9634` **tui/error**: resolve severity colours through the theme (#1634)
- `afdd93d7` **tui/plan**: resolve the plan widgets' colours through the theme (#1634)
- `dd2afaec` **tui/session_files**: resolve the file list's colours through the theme (#1634)
- `b37b5280` **tui/projects**: resolve the project overlay's colours through the theme (#1634)
- `8bf81ac7` **tui/dialogs**: resolve dialog colours through the theme (#1634)
- `6bb07599` **tui/help**: resolve the help overlay's colours through the theme (#1634)
- `1a121d76` **tui/sessions**: resolve the session picker's colours through the theme (#1634)
- `5065e2e5` **tui/highlight**: resolve code-block chrome through the theme (#1634)
- `3d531a6f` **tui/title**: resolve the header rule through the theme (#1634)
- `3799cee7` **tui/panes**: resolve split-pane colours through the theme (#1634)
- `0a3bfd37` **tui/tools**: resolve tool-output colours through the theme (#1634)
- `fd5adbbb` **tui/input**: resolve the input box colours through the theme (#1634)
- `a3e9366f` **tui/chat**: resolve the transcript's colours through the theme (#1634)
- `6eb75057` **tui/markdown**: resolve chrome colours through the theme (#1634)
- `78d694d3` **tui/markdown**: give body prose a themeable foreground (#1634)
- `2a1495b1` **tui**: drop the render cache when the theme changes (#1634)
- `792503ee` **agent**: report the real stream retry count in logs (#1635)
- `cca5534e` **provider**: raise OpenAI-compatible total request timeout to 300s (#1635)
- `7c6747b4` **brain**: accept valid profiles/ paths in write_opencrabs_file
- `cb9d451d` **windows**: clippy-clean the new read/cwd helpers
- `fed7ae33` **shell**: provide the Unix kill_process_tree so the timeout tree sweep compiles off Windows
- `40f0c456` **windows**: expand_tilde handles the backslash ~ path form
- `f9d5d6b4` **windows**: session_search empty query falls back to the session list
- `a9f00ff8` **windows**: read_file decodes BOM/UTF-16 and degrades lossy instead of failing
- `2f5e809f` **windows**: fall back to home cwd when launched from a system directory
- `2eed541c` **windows**: bash tool verbatim args, timeout tree-kill, findstr/gh handling
- `20b29c93` **windows**: replace hardcoded sh -c spawns with platform shell pair
- `f511c8ed` **cron**: mark stuck cron runs as interrupted instead of error (#264)
- `5fed4735` **telegram**: guard react-only branch against dedup-suppressed final text
- `a42e664f` **title**: strip the channel-noun history block from title prompts
- `36428fda` **slack**: dedup channel history, scope it to the thread, label the sender (#1620)
- `02fc7939` **slack**: record the thread a channel message belongs to (#1620)
- `dfab8a4b` **whatsapp**: dedup group history and label the current sender (#1618)
- `5486e2c7` **discord**: dedup channel history and label the current sender (#1619)
- `4e48f47c` **notify_queue**: log reaped rows whose ids fail to parse
- `19ebf70e` **notify_queue**: reap stale rows after the redelivery pass, not before
- `504c6365` **#182**: reap stale notify_queue rows for unclaimed sessions
- `c1422802` **whatsapp**: release the outbox at turn start so a follow-up never edits the previous answer (#1614)
- `ef394406` **brain/tools**: advertise the work_status path in tasks_list (#165)

### 📖 Documentation

- `9751a14d` rewrite OPENCRABS.md to match the real binary (#1650)
- `2b0eca43` **readme**: add Epistemic Engine and 3-Tier Memory sections (#1645)
- `8710b9da` **cron**: say that a skipped trigger run is stored as success (#1629)
- `819ace0a` **db**: drop the wrong issue number from the trigger migration comment (#1629)
- `d21e5801` **logging**: say what the alternate signal stack actually guarantees (#1637)
- `495ad7d4` **telegram**: note that GLOBAL_PACER must not be held across an await (#1630)
- `ffb79c2c` **cron**: document only the run statuses something actually writes (#1628)
- `5197ec86` **contributing**: add first-run advisory with clippy --fix path
- `836d0740` **contributing**: explain what clippy catches that cargo check misses
- `5fc191e4` **contributing**: drop the stale [skip ci] instruction (#1622)
- `0311f976` **contributing**: warn that target/ grows fast on the cargo run dev loop (#1622)
- `a930c92e` **tasks_list**: correct the status-path provenance on subagent_status_file
- `9e9d5441` **changelog**: record the WhatsApp turn-boundary outbox fix (#1614)

### 🧹 Miscellaneous

- `e7ef03e5` Delta-only compaction: stop re-deriving prior summaries (#1649)
- `ac64f6b9` Add tokenized AND-match fallback to session_search (#1626)
- `206c4896` Remove phantom heartbeat subsystem from docs and templates (#1621)
- `c89d0b10` test(provider): guard the streaming total timeout and retry counters (#1635)
- `c6ee718a` style: fmt utils_cwd_test, now that it is reachable
- `39497594` test(shell): cover the Unix half of the process-tree sweep
- `92bd2c01` test(shell): register utils_shell_test so it actually runs
- `6698df6d` ci: keep windows-build out of the required gate and path-filter it
- `c452281a` style: cargo fmt sweep over the merged tree
- `b4698c56` test(cron): pin that a timed-out trigger kills its shell (#1629)
- `500db577` test(cron): lift the trigger.rs inline tests into src/tests (#1629)
- `c4341ec2` refactor(cron): delete the dead and lossy TriggerCondition::as_str (#1629)
- `9bdef2f9` test(cron): lift the target_resolver inline tests into src/tests (#1629)
- `04226dd3` style(cron): rustfmt import order across the trigger pipeline (#1629)
- `43268f1f` test(telegram): pin the global 429 propagation and the permit ordering (#1630)
- `7776360a` refactor(logging): compile the crash record formatter only where it has a consumer (#1637)
- `009ac031` test(brain): tidy the write_opencrabs_file test imports and one stale comment (#1633)
- `c7f5c667` test(telegram): lift the react-only dedup tests out of delivery.rs (#1623)
- `87321e44` refactor(telegram): name the suppressed-final vs react-only distinction (#1623)
- `b6e200a1` test(usage): guard the DONE-fallback against outranking reported usage (#1636)
- `cd067c2c` test(usage): correct the compat cache test that asserted the double count (#1636)
- `8b1c1b62` test(usage): guard the cached-prefix netting on every compat usage path (#1636)
- `378d4c27` test(tui): fail the suite on any new raw ANSI colour site (#1634)
- `081c097e` test(tui): pin the prose foreground and the theme cache sweep (#1634)
- `cf1ff38b` test(tui): pin the canvas background declarations (#1634)
- `28ed0da7` ci(windows): raise build timeout to 60m and drop dead save-if
- `5d6ec12e` ci: build on Windows and gate PRs on it (#627)
- `fc2665a4` test(windows): register shell, cwd, read-encoding, search-fallback, tilde tests
- `d5c5bf3e` test(whatsapp): pin suggestion-poll routing and vote selection (#1616)
- `f43a6d8c` test(slack): pin the history wiring against a silent fallback (#1620)
- `a616edfc` test(slack): pin thread persistence and the NULL-scoping trap (#1620)
- `da816070` test(whatsapp): pin the history wiring against a silent fallback (#1618)
- `cc8e9d41` test(discord): pin the history wiring against a silent fallback (#1619)
- `fc2e0a73` test(group_history): pin the lifted helpers against the legacy strings
- `4189237b` refactor(channels): lift group-history preamble into a shared module
- `7b22a450` test(tasks_list): pin the advertised path against a real writer file
- `0674437a` style(tasks_list): rustfmt import order after the #1615 merge
- `a36c8130` refactor(notify_queue): scope MAX_ROW_AGE_SECS and correct the stale-row doc
- `03a05aa8` style(notify_queue): rustfmt import order after the #1617 merge

### 📊 Stats

- 121 commits since v0.5.2
- 7 contributors: @adolfousier @leshchenko1979 @moe @moneyacademyKE @A2agent-ai @adi805 @carvalab
- 192 files changed, +12,304 / -1,709 lines
- 9,032 tests (8,994 passed, 0 failed, 38 ignored)

## [0.5.2] - 2026-09-16

133 commits since v0.5.1, 7 contributors. 305 files changed, +22,624 / -5,586 lines.

An integration window: ACP server mode over stdio JSON-RPC so Zed and MonoCode drive the crab directly (#1540), shadow-DOM aware browser automation, a WhatsApp surface explosion (native voice notes, inbound video/sticker/location/contact/reactions/poll votes, presence + media recovery, reaction acks, edit-in-place streaming, disappearing messages, contact blocking, pin/forward/profile, status updates, newsletter discovery, native-flow buttons, per-chat history), Discord live reply tracing with auto-threading and native markdown tables, Telegram forum topics and a /clear that starts the agent fresh without the summariser call. Along the way we found Telegram's setMyCommands silently rejects request bodies over ~7.8KB while reporting BOT_COMMANDS_TOO_MUCH - the menu-budget fix that finally got /clear onto the phone (#1613). The last inline test blocks in production files were swept into src/tests/ (#1612): 913 test files, zero inline. v0.5.1's changelog also carried a leaked release-draft block; removed here.

### ✨ Features

- `aa331182` **telegram**: queue follow-up selections instead of re-arming on mid-turn tap
- `28627b08` **telegram**: infer missing table separators in rich normalization
- `18a476b4` **browser**: composed-tree JS helpers for shadow-DOM traversal
- `3721c7dc` **session**: /clear starts the agent fresh at the current point without a summariser call (#1585)
- `428f15d0` **suggest_options**: document recommendation primary guidance and first-word folded button labels (#228)
- `1e04eefa` **whatsapp**: opt-in newsletter poller with owner digests
- `346737e1` **whatsapp**: bounded per-chat history capture with read-only search
- `a34fa80b` **whatsapp**: send follow-up suggestions as native-flow button cards
- `940663fb` **suggest_options**: polymorphic options with styles and single-option promotion
- `84021b1f` **telegram**: session bind topic instrument and create_topic bind parameter (#170)
- `1947afcd` **telegram_send**: add create_topic and rename_topic actions (#161)
- `4881e82c` **telegram**: dedup group history and prune reply quotes against live context (#133)
- `530d7bbe` **plan**: mid-list task insertion and windowed show_plan
- `9a0db943` **acp**: ACP server mode over stdio JSON-RPC for Zed/MonoCode (#1540)
- `8269270b` **tui**: log viewer in Mission Control, opened with L (#1528)
- `16c7c9f4` **tui**: search the help screen with / (#1527)
- `65773709` **channels/whatsapp**: try the native-flow path for interactive buttons (#1411)
- `ce6178c2` **channels/whatsapp**: status updates, newsletter discovery and labels (#1485)
- `cf1b320d` **channels/whatsapp**: pin, forward and programmatic profile (#1484)
- `1c538231` **channels/whatsapp**: block contacts and send disappearing messages (#1487)
- `e47c67ef` **channels/whatsapp**: send audio as a native voice note (#1486)
- `5b19ea9c` **channels/whatsapp**: handle inbound video, sticker, location, contact, reactions and poll votes (#1410, #1482, #1483)
- `157e8106` **channels/whatsapp**: publish presence and recover expired media (#1488)
- `aa3e39d4` **channels/whatsapp**: acknowledge finished turns with a reaction (#1409)
- `1a3f3c5b` **channels/whatsapp**: edit-in-place streaming instead of chunk-spam (#1408)

### 🔧 Fixes

- `836f8577` **telegram**: cap command menu payload under Telegram's ~7.8KB setMyCommands limit (#1613)
- `d643dd3a` **recovery**: expand boot wake window to 60m and classify active plan tasks as interrupted (#1605)
- `b9bd4a44` **telegram**: restore normalize_tables body and infer_missing_table_separators lost in #1595 conflict resolution
- `975db95b` **discord**: prevent double-posting final response on tool turns
- `7099a6b1` **telegram**: expand hash shield to list items, blockquotes, checkboxes
- `e7db13fd` **telegram**: balance code fences in rich text normalization
- `1126968c` **whatsapp**: gate whatsapp_history module behind whatsapp feature (#1604)
- `5fea88ae` **browser**: keep the newline escape out of Rust's hands in the text_only JS
- `5d77e6c0` **browser**: full-page text_only spans the composed tree
- `30db96a5` **browser**: text= clicks walk the composed tree too
- `2088fde1` **browser**: browser_type and scoped browser_content resolve inside shadow roots
- `040273ed` **browser**: resolve selectors across shadow roots in click/wait/screenshot/act
- `27ee0c4f` **browser**: browser_find enumerates and keeps shadow-root elements
- `dff7cf27` **tests**: isolate telegram rich edit dedup fixtures across cases (#1588)
- `e0423000` **tui**: /help enters the help screen through switch_mode so the catalog loads (#1586)
- `f89d7bc1` **tui**: treat a marker the model quoted mid-line as text on reload, not as a cut point (#1587)
- `06a97bce` **tui**: fold phantom-blocked sections into one collapsed row on reload (#1584)
- `81fad3b2` **rsi**: classify permanent cap-bails and dedup their reporting
- `a7f7e27b` **tui**: honor /cd <path> instead of always opening the picker
- `ee4f6426` **telegram**: prevent send_markdown_outbox from re-poisoning evicted dead topic
- `37963c10` **telegram**: preserve markdown dialect on queued final edit drain
- `4678856c` **ci**: give test threads a 16 MiB stack floor
- `85a56941` **cli**: surface honest parked status for surfaceless sessions (#114)
- `799c9a89` **config**: drop dead draft_streaming references after field removal
- `71890236` **telegram**: reshape over-budget button rows to column and preserve sets
- `3bf84b07` **tools**: write_file marks path fully read so subsequent writes succeed
- `7a3806be` **telegram**: delete dead draft_streaming config field (#181)
- `6ae90a2b` **mermaid**: unify nudges, readability checks, and capping-based svg vector links
- `ee9edc7d` **plan**: validate mermaid syntax in session plan markdown sync (#172)
- `e8c28abc` **compaction**: anchor the marker search on a user row that starts with it (#175)
- `cc463133` **telegram**: isolate fold list from preceding prose list counters (#207)
- `5dd0a6f9` **tools**: atomic file writes via tempfile rename in write_file and edit_file (#167)
- `ce9fda07` **brain/tools**: scope tasks_list and wait_agent to the caller (#191)
- `d4d20047` **telegram**: match code fence delimiter run length in rich markdown parser (#94)
- `38f368ce` **goal**: turn budget config, state reconciliation, and visibility (#190)
- `7db96ac4` **cli**: omit interrupt parameter in session notify unless true
- `857a04e6` **provider**: streaming chunk inactivity timeout & configurable request timeouts (#217)
- `0b8d2200` **config**: missing [channels.telegram] table defaults to rich_messages and mermaid_render (#178)
- `149f965a` **bash**: inject session identity into command environment (#166)
- `132da1fc` **loop-guard**: exempt paginated arguments from repetitive/near-match loop detection (#199)
- `1fde332b` **telegram**: surface substantial markdown status updates (#215)
- `2b1f9b04` **daemon**: await A2A gateway readiness before cron scheduler
- `ac295cf6` **cron**: seed next_run_at on create/update and backfill on scheduler start
- `08328efd` **telegram**: eliminate rich governor fail-open to prevent 429 lockouts
- `141f015e` **bash**: bound retry guard by TTL and block budget
- `5a8570a6` **telegram**: shield bare leading hashes on rich markdown wire
- `a2895a95` **bash**: make interactive command splitter quote-aware
- `f1d4681a` **telegram**: make mermaid render failures visible, retryable and escapable
- `9c1f2d06` **compaction**: align flow receipt after_pct with post-compaction token count (#211)
- `56e0b54d` **telegram**: flush queued items at resume-turn end
- `7ce65177` **agent**: the #752 turn-end verdict honors the structured-report exemption (#1541)
- `3eaf3525` **skills**: claim the boot-hydration flag after the readiness guards (#1537)
- `7d7fc03a` **brain**: a real brain file outranks the skill filename form (#1537)
- `dc10f483` **138**: explicit Result typing in SessionSkillsRepository::all()
- `85a8b133` **138**: persist seen-skills to DB and register filename-form load_brain_file
- `4ce1be10` **channels/whatsapp**: key sessions and buffers on the canonical identity (#1533)
- `07ca9bad` **channels/whatsapp**: accept either identity when gating an approval reply (#1533)
- `44898960` **channels/whatsapp**: a blocked contact could walk past the guard from a LID
- `0ddcf0ad` **tui**: clamp Help and Settings scroll to the end of the content (#1527)
- `3bcc26f7` **whatsapp**: route whatsapp_send to session chat before owner fallback and resolve LID sender_alt
- `7b55e82c` **channels/whatsapp**: create polls through the lib so votes can be decrypted (#1482)
- `f68d1bf1` **channels/whatsapp**: charge every message-delivering action to the send budget (#1407)

### 📖 Documentation

- `3df73aef` **browser**: document shadow-DOM resolution rules
- `1f9ca680` **telegram**: clarify list_topics DB-observed scope (#160)
- `6a090453` **templates**: sync CODE.md template with the updated brain file
- `cabaa75f` **readme**: correct the WhatsApp surface and add a check so it stops drifting (#1526)
- `c3660332` **readme**: describe the WhatsApp channel's new surface

### 🧹 Miscellaneous

- **release**: linux-arm64 builds with thin LTO. The fat-LTO link of the full binary drives one rustc past 14.8 GB on the 16 GB hosted arm runner, the VM freezes and GitHub kills the job with exit 143 after 25 silent minutes (measured with a sampled build). All other targets keep the fat-LTO release profile.
- **deps**: move WhatsApp crates from a git rev to crates.io whatsapp-rust/wacore/waproto 0.7.0 and drop the `crates_publish` cfg split. `cargo publish` strips git pins and verified the tarball against 0.6.0, which lacked the labels, polls and newsletter APIs the 0.5.2 surfaces use, so the publish step failed and no release shipped. Migrated proto access to buffa `MessageField`, the batched `Event::Messages`, builder-style `SendOptions`, the two-window `delete_expired_tc_tokens`. MSRV 1.94.
- `82fac8bd` **skills**: drop orphan cfg(test) Skill import left by #1612 sweep
- `48065540`: drop trailing blank line in notify_policy.rs (#1612)
- `871c51f2`: move skill_gate.rs inline tests to src/tests/skill_gate_test.rs (#1612)
- `2c7c8252`: move target_resolver.rs inline tests to src/tests/target_resolver_test.rs (#1612)
- `6ff76500`: move timezone.rs inline tests to src/tests/timezone_test.rs (#1612)
- `910afa85`: move inline.rs inline tests to src/tests/telegram_rich_inline_test.rs (#1612)
- `115cd7c0`: move panes.rs inline tests to src/tests/tui_render_panes_test.rs (#1612)
- `0dab14e3`: move mermaid.rs inline tests to src/tests/telegram_rich_mermaid_test.rs (#1612)
- `0fa234ea`: move context.rs inline tests to src/tests/agent_service_augment_user_message_test.rs (#1612)
- `bc9247cd`: fmt import order in notify_policy_test (#1612)
- `5fae72fc`: extract notify_policy inline block to src/tests (#1612)
- `df0b1188`: seed z.ai + Xiaomi endpoint types in from_config (#1596)
- `5d326aa1`: _options: reject same-first-word option sets with a rephrase nudge (#1611)
- `ab1c8451`: #1608 inline tests to src/tests/ per CONTRIBUTING.md
- `ad84601f`: live reply tracing, auto-threading, native tables, split fixes (#1608)
- `02b08061` **discord**: extract norm_key inline tests to src/tests per CONTRIBUTING policy (#1603 follow-up)
- `f69298e3`: cargo fmt — EOF newlines and line joins after #1593 merge
- `5dbcccc7` **rsi**: retire brain dedup scanner, proposal queue and dedup_intent parameter (#1593)
- `8282640f` **plan**: remove dead has_active_plan wrapper — zero callers in production or tests (#1605 follow-up)
- `baa2f07d`: cargo fmt on custom_openai_compatible.rs (line-length wrap)
- `f38cc014` **telegram**: consolidate escape_html implementations
- `960c1269` **streaming**: single-pass tail extraction, log consolidation, level demotion
- `091df907` **browser**: scope the shadow sentinels so they stop matching their own comments
- `cd6a899b` **browser**: cargo fmt the shadow-DOM changes
- `56cadfab` **browser**: inline the format arg in with_deep_helpers
- `5b2f7cd7` **browser**: e2e fixture for the shadow-DOM round trip
- `b9795331` **deps**: bump rustls 0.23.43 -> 0.23.45 (RUSTSEC-2026-0285)
- `c6c3dd7e` **telegram**: drop clear_for_test now that dedup tests isolate fixtures
- `78452fbb` **cli**: resolve full session uuid without loading session table
- `8194477f` **acp**: move inline tests to src/tests/ and close the #1540 review gaps
- `ca9192c2`: run the parallel-session cancel test on a big-stack thread
- `24c23343` **skills**: move the inline seen_skills test module under src/tests/ (#1537)
- `800bc07c` **db**: drop the redundant session_id index on session_seen_skills (#1537)
- `bfd48c6d` **db**: cargo fmt the merged session_skills repository (#1537)
- `1dcacdea` **lint**: lint the examples/ targets too (#1538)
- `3dc1ea4e` **read**: buffer the fixture writes instead of one syscall per line (#1534)
- `4570dbf7` **brain**: resolve the seed home once per test instead of twice (#1536)
- `4ac8ec6d` **subagent**: run worktree tests against an overridden profile home (#1535)
- `6eaa2033` **telegram**: run the rate-limit retry tests on a paused clock (#1532)
- `46acce75` **tui**: stop re-reading every skill and commands.toml per help frame (#1530)
- `4b7ebafd`: untrack bench scratch files absorbed by the release commit

### 📊 Stats

- 133 commits since v0.5.1
- 7 contributors: @adolfousier @leshchenko1979 @adi805 @moneyacademyKE @carvalab @loonix @A2agent-ai
- 305 files changed, +22,624 / -5,586 lines
- 8,852 tests (8,814 passed, 0 failed, 38 ignored)

[0.5.2]: https://github.com/adolfousier/opencrabs/compare/v0.5.1...v0.5.2

## [0.5.1] - 2026-09-12

213 commits since v0.5.0, 6 contributors. 373 files changed, +27,981 / -2,729 lines.

A hardening window: per-job cron sessions, a config write guard derived from the Config struct itself, a skill-globs gate that refuses tool calls until the skill is read, malformed plan designs refused at write time, temporal grounding in the brain, a Telegram edit governor that obeys the real 429s, a two-part whatsapp send-path rewrite, and two security fixes: a2a auth now binds to the gateway, and config files are owner-only with secrets scrubbed from the logs.

### ✨ Features

- `70c94b75` **brain**: temporal grounding via turn ingress dual-time markers and user timezone resolution (#1516)
- `c42b5edb` **skills**: cursor-style globs frontmatter gate for skills (#1515)
- `9d9f5bb6` **compaction**: report the fallback walk on manual compact and every failure to the user (#1521)
- `8a0538e4` **projects**: identify a project by its repository remote, not its name
- `28f7e1a3` **whatsapp**: outbound rate limiter with pacing, rolling daily cap, queue and owner alert (#1407)
- `bd82198a` **tui**: theme catalog, converter + 31-theme curated pack (#1461)
- `c401fbb3` **memory**: structural truncation visibility + offset pagination (#89)
- `c34e9ec6` **memory**: depth-2 impact chains over call_edges (#89)
- `8daad249` **memory**: external hits render repo-relative paths (#89)
- `e0a0af98` **memory**: tag corpus provenance on every scope=all hit (#89)
- `b66a6a66` **brain/tools**: add optional background parameter to bash tool (#152)
- `16aa0b9a` **targets**: oc:// target URL scheme + central target resolver
- `7a31d350` **subagents**: add include_brain parameter and context gap disclosure
- `80aac1d4` **prompt**: exclude interactive-only tools from headless tool roster (leshchenko1979/opencrabs#129)
- `ab8c240f` **prompt**: strip FOLLOW-UP SUGGESTIONS from headless brains (leshchenko1979/opencrabs#129)
- `b11b4d9c` **agent**: headless backstop flag — ToolExecutionContext.headless + hard ToolError in session_notify/suggest_options
- `187c191c` **subagent**: children headless by law — strip notify/options from child surface + preamble on spawn/resume prompts
- `48aef302` **cli,daemon**: wire headless flag + preamble at all four registry call sites
- `4972285c` **tools**: headless gate on register_core_agent_tools + HEADLESS_PREAMBLE const
- `31b97ee4` **cron**: session: deliver_to target — cron results into session notify queues
- `f4aba6fa` **cron**: session: deliver_to target — cron results into session notify queues
- `1382f8bf` **notify**: one shared delivery policy for tool, A2A and CLI surfaces
- `9a1de684` **notify**: persist parked pushes at chokepoints, clear on delivery, boot redelivery (#111)
- `9f8d8145` **db**: notify_queue table + repository — durable parking for undelivered pushes (#111)
- `7a4108dd` **skills**: load_brain_file slug form + seen-skills registry for compaction stamp (issue #131)
- `d7e19ae4` **scheduler**: opt-in forum-thread delivery for telegram deliver_to (#104)
- `d557f5f1` **channels**: persist origin thread id for topic-aware cron delivery
- `2464ed7c` **providers**: rename zhipu to zai with full back-compat
- `3b03ab70` **sessions**: link a session to its project on the first turn
- `32cf143f` **compaction**: advisory skill-inventory stamp on post-compaction continuation prompts
- `e712c193` **telegram**: fold mid-turn session-notify lines into the live flow roll (#61)
- `680af24b` **telegram**: merge table-bearing answers via the rich markdown plane
- `b1b8fe8e` **telegram**: markdown-plane host persistence for merged suggest controls

### 🔧 Fixes

- `201b0d03` **agent**: bound output token reserve by context window (#1518)
- `a9afecc9` **targets**: sync profile override, channel secrets, and fire-time refusal for upstream main base
- `2c864801` **118**: clippy — drop unused SendMessageSetters import, needless &token borrow
- `d7cd1a0d` **plan**: preflight mermaid fences in plan task descriptions at creation (#1524)
- `6ab5b91d` **telegram**: pace G2/G4 governor defaults below group rate limit and route interactive UI edits through EditClass::Interactive (#1523)
- `3c08bb03` **config**: derive write guard and typo warnings from the Config struct (#1508)
- `39a73861` **cron**: per-job cron sessions to prevent concurrent context and provider bleed (#1511)
- `1e9c1b15` **context**: clean persisted ledgers and reasoning from loaded rows for CLI providers too (#1522)
- `14e838f6` **compaction**: ask HTTP providers through the stream path and fold it into one response (#1519)
- `a9f65d9b` **provider**: record non-streaming retries as RetryAttempt notices (#1520)
- `25bd663b` **tui**: keep the tail of long messages in unfocused panes
- `280c5b8f` **phantom**: anchor work-announcement colon marker at end of line (#1513)
- `b44d29e6` **whatsapp**: chunk over-long quote-replies like the send path (#1490-C)
- `0cd1630d` **whatsapp**: account for delivered chunks when a chunked send fails partway (#1490-B)
- `9f292521` **whatsapp**: stop the channel preamble denying whatsapp_send exists (#1489)
- `0606f645` **whatsapp**: unify persisted format across send and reply paths (#1490-E)
- `40a56e9e` **tui**: display surfaces report the session's provider, not the global (#1464)
- `c6a25e4c` **tests**: re-anchor unbacked-facts detection sentinel to fired_branches (#1506)
- `2ffe8535` **agent**: exempt structured reports from phantom-kill regeneration (#1506)
- `147f434b` **agent**: stop reading signature algorithm names as sha claims (#1501)
- `10f07400` **telegram**: thread-scoped tmp photo and voice file pickup (#124)
- `647e1c21` **tui**: keep the background-task manager across a provider-switch rebuild
- `bfe9e032` **plan**: receipt binding resolves repo from plan working_directory (#1452)
- `15727f1a` **memory**: restore MemorySearchTool import beside the Tool-trait schema pin (#89)
- `d0eec7f9` **memory**: use the imported Tool trait path for the schema pin (#89)
- `943462fe` **memory**: drop unused symbol-graph re-export, import Tool trait in parity pins (#89)
- `5774ab38` **memory**: structural graph listings get n-floor and deterministic order
- `5089be5d` **brain/tools**: convert error string slices to String for ToolResult::error in bash tool
- `80857867` **tests**: retry flock reacquire under CI load (#cron_scheduler_lock)
- `a2c0bfe9` **telegram**: tolerant fence pairing in mermaid detection
- `78592938` **tests**: collapse nested match — clippy collapsible_match
- `e4463309` **tests**: count only ToolUse block starts in empty-finish guard test
- `c0a1f350` **tests**: collapse ContentBlockDelta match into outer pattern
- `8713aaed` **tests**: clippy - drop unused port param, collapse nested match
- `7eb1753e` **provider**: ignore empty finish_reason chunks in SSE streaming
- `5a1cedb2` **cron**: remove unharvested validate_delivery_target from target resolver PR
- `8a09fc92` **129**: live_eval example passes headless flag to tool_access_prompt
- `37ab5dbc` **headless**: shared atomic headless flag + stable str deref in roster filter
- `50fbe9e7` **headless**: clippy gate r2 fixes for leshchenko1979/opencrabs#129 — pub ALWAYS_EXCLUDED, &self execute receivers, headless ctx field in tests
- `9d1d21e1` **cron**: session target resolution — async loader, pure testable core
- `a5362f0e` **cron**: session arm — re-exported type paths + return None (E0603/E0308)
- `b146298d` **cron**: session target resolution — async loader, pure testable core
- `337dc335` **cron**: session arm — re-exported type paths + return None (E0603/E0308)
- `31c6ce37` **notify**: resolve delivery policy before the session-existence gate
- `1487da0b` **notify**: turn-end test takes guard after the await, not across it
- `90e5538a` **notify**: drop MutexGuard across await in a2a turn-end test
- `d0f9e036` **notify**: dead cap const dropped, test guard after first await
- `c1d1eb44` **notify**: import CONFIRM_CAP in A2A handler + carrier fmt join
- `d76577c5` **notify**: wire A2A handler imports to shared policy layer
- `e814a5e7` **notify**: compile fixes on the policy extraction
- `1241891f` **subagents**: persist output_full, remove dead output_summary
- `8ea46427` **telegram**: auto-evict deleted forum topics on thread-not-found 400
- `456c0adf` **config**: read_config renders every known section (#86)
- `06e9f647` **channels**: deny-by-default allowlists on Discord, Slack, and WhatsApp (OC-02)
- `ab39bfd8` **tools**: deny read_file on confidential paths at the harness, not by prompt (OC-08)
- `b9816f6b` **tools**: route execute_code and dynamic shell through the bash blocklist, reserve core tool names (OC-06)
- `fb19870d` **tools**: one SSRF guard for every fetch tool, DNS-resolved and redirect-checked (OC-04)
- `f88890a2` **119**: drop fork-only #91 cross-turn glue from the upstream port
- `15888234` **evolve**: verify the release SHA256SUMS before swapping the binary, pin the download host (OC-03)
- `fd914abf` **telegram**: restore fold-tier numbering — numbered Go: body lines + numbered buttons (#119)
- `b784bca8` **119**: owner render corrections — bold Go lines, ? dedup, paragraph break
- `e6ad5f31` **telegram**: single-option suggest rides a full-width button, not the numbered fold (#119)
- `bfd764ab` **channels**: owner-gate every tool-approval and YOLO button on all four channels (OC-01)
- `686b84fd` **telegram**: off-by-one in ensure_blank_line_before_tables
- `1ec29585` **config**: write_key recursive JSON→toml_edit — objects/nested arrays survive, null+mixed arrays error (#87)
- `2ba57e14` **telegram**: resolve harvest clippy — test-only re-export path + collapsible if (#132)
- `5b9807db` **tui**: keep the visible completion when Esc-Esc cancels a turn (#1468)
- `93b82f18` **telegram**: detach glued prose label from collapsed table on reflow (#132)
- `002eec97` **telegram**: make rich::table crate-visible for direct test imports (#132)
- `0aa93a3c` **telegram**: normalize collapsed tables on the rich plane — single canonical entry (#132)
- `b0e36592` **rich**: insert blank line before abutting tables so Telegram rich renders them
- `657b2512` **telegram**: replace labeled block with try_html_style_tag helper
- `479e094c` **telegram**: parse standard HTML style tags in rich inline parser (#106)
- `a040fdf4` **telegram**: topics_for_chat LEFT JOIN keeps nameless threads; test rename row carries topic_edited type
- `7e5c9d12` **#143**: restore FROM clause and dedupe ORDER BY in topics_for_chat
- `ba1439c9` **#143**: topics_for_chat returns one row per thread
- `3ea0db19` **telegram**: stay current on forum topic renames (#143)
- `12ca4509` **telegram**: bg-resume receipt cards re-land on the HTML rich wire (#85)
- `54f56c1d` **db**: stamp past a migration that already ran under an older list order
- `d93d3cfb` **projects**: linking a session to a project no longer restamps recency (#1460)
- `88835016` **tui**: session list shows last activity, not creation date
- `dcc620b1` **agent**: keep the recovery row when shutdown cancels a turn (#1462)
- `858ff3eb` **claude-cli**: surface silent turns instead of faking success (#1441)
- `107b5ccf` **db**: keep the #111 migration in name order and heal databases past it
- `9c190334` **skills**: make the #131 slug form build and match main's miss contract
- `024449f4` **111-port**: restore serde derives on BgTaskMeta (r1 clippy E0277)
- `d0874041` **notify**: gate r1 findings — public import paths, QueuedItem non-Clone, spawn arm type
- `36e166ba` **scheduler**: clippy unnecessary_unwrap in thread-missing error arm — if let Some
- `24e91930` **scheduler**: rebase 104 thread delivery onto current main — Requester import + closure var rename
- `a154b346` **telegram**: normalize mermaid render-cache key across delivery surfaces (#100)
- `2680452b` **118**: send_buttons wire-payload evidence + refuse empty keyboards
- `8424bda3` **telegram**: send_buttons rides raw Bot API JSON — markup no longer dropped (#118)
- `e46ae065` **telegram_send**: same-origin-only thread inheritance + landing echo in success output (#127)
- `d845be3b` **tui**: session picker cursor follows the session, not the slot (#1465)
- `5a8903d9` **config**: rotate same-day backups instead of skipping (#1459)
- `5e27a960` **config**: never wipe keys on unreadable config.toml (#1458)
- `fb5c4d31` **telegram**: stop asking Telegram to replace a rich message with itself
- `26535f4c` **config**: never promote a template placeholder to bot_owner
- `ede71c93` **tui**: persist the theme name itself, not a name wrapped in quotes
- `6c3e4f23` **config**: seed the shipped example from whatever first needs config.toml to exist
- `bd02f800` **config**: stop materialising an enabled xiaomi section in configs that never named it
- `59d755eb` **memory**: load the embedding model on CPU so a Metal assert cannot abort us (#1432)
- `03637828` **telegram**: plan-card restick on interrupt-delivered turns (#69)
- `d2001192` **brain**: catch fabricated facts in post-success wrap-ups (#1423)
- `85a8f907` **port**: adapt #61 live-flow registry to upstream v0.5.0 field set
- `71fc026c` **telegram**: fold-first button ladder — total-row budget, 20-unit fold cap
- `46c076eb` **telegram**: #61 fold-dedupe — one fold per notify per session
- `fd4485a1` **telegram**: #61 roll line strips a leading notify self-echo
- `04b4aee2` **telegram**: restore #38 empty-body guards on receipt-card builders (A3 guard-only)
- `3d44f2b5` **telegram**: #92 demoted-host guard — re-run the #58 promotion guard on the displaced host
- `96fa7704` **session**: persist a live turn before switching away from it (#1421)
- `1828f03f` **tui**: keep a running session marked processing when it regains focus (#1420)
- `ad7ac358` **#108**: blank-line-separate button rows and trailer on the md plane
- `57566b08` **telegram**: fence-safe rich-md edits — mermaid diagrams survive merge/pick/strip redraws (#98)
- `72f745aa` **telegram**: redraw rich-md pick in the markdown plane (#96)
- `a90bad95` carry message_id on rich multipart edits
- `557919bf` **onboarding**: xiaomi model list falls back to binary baseline when /models fails
- `25f2ba35` **brain**: log backup-prune failures instead of printing them (#1413)

### 🔒 Security

- `afec08ff` **a2a**: refuse an unauthenticated non-loopback bind, and compare the token in constant time
- `adfe2a85` **config**: write config files owner-only, move daemon logs off /tmp, scrub all secrets from logs (OC-05)

### 📖 Documentation

- `45a7677a` state the measured ratio rather than a rounded one
- `e2a6da4e` Claude Code CLI refuses its headless mode as root on a VPS
- `ba4e0bc7` place v0.5.0 against v0.3.9 rather than calling it the largest release
- `7cea1d57` reconcile test counts against the tree
- `8dd0d483` refresh test counts for phantom_unbacked_facts_test
- `950d5bea` reconcile test counts against the tree (8,036 across 808 modules)
- `d2b8dbfd` state that OpenCrabs manages the context window, not the CLI (#1450)
- `52cbcbb1` align row counts with the merged pr-1466/1467/1470 stack
- `d7d3a1ec` update test count to 8,055 across 875 modules
- `c8288198` row for config_write_types_test from #1472
- `7fcf430b` test count 8095 from gate4 suite
- `492995f1` add a Security Controls section and correct two now-inaccurate claims
- `617fd7b5` reconcile TESTING.md with tree — 8 drift fixes, 26 new rows
- `5030e23f` reconcile rows missed by the batch pass — 27 rows, counts cfg-aware
- `8b1e1928` rows for the 1500/1502/1503/1505 merge batch test files
- `1cf65d96` cover theme pack generator + import path in the Theme Switching row (#1461)
- `6b25fee9` align schema description with mobile display budgets (#1517)

### 🧹 Miscellaneous

- `d51b485a` test(onboarding): pin the #1419 xiaomi baseline fallback with tests
- `20523b28` refactor(test): move #1422's inline TelegramState tests under src/tests/
- `c214f9f7` work_status: bind sub-agent status to its spawning session (#110)
- `a6436486` spawn: bind sub-agent status file to the spawning session (#110)
- `2580bbad` restart_recovery: route sub-agent reports by parent session, deliver revived outcomes (#110)
- `00a3b06c` ui: route a revived sub-agent's resume outcome to its parent (#110)
- `8eed3ee6` fix compile: legacy migration literal + moved msg in revived delivery
- `8907e485` refactor(compaction): single continuation_prompt() helper for all five compaction sites
- `83d49a52` style: apply rustfmt to the merged sub-agent parent-binding work
- `815016ea` test(compaction): pin the single continuation path the skill stamp rides
- `6da322c2` style(tests): apply rustfmt to rotation test, fix mod registration order
- `b26b16dc` style: rustfmt the cherry-picked PR work against current main
- `4c4b4778` refactor(test): move the #131 seen_skills unit tests under src/tests/
- `614a660d` test(telegram): retarget receipt fence test to post-swap shapes (#85)
- `b893585f` test(telegram): add missing msg helper for #143 rename-precedence pins
- `d0898888` test(telegram): listing rename row carries topic_edited type
- `d8e5b943` style(telegram): type alias StyleNodeFactory for HTML_STYLE_TAGS
- `fff3f888` test(telegram): fix unmatched_openers_stay_literal — empty HTML pair is ONE literal span
- `8d71aa36` style(tests): rustfmt reflows on the merged pr-1466 test files
- `2a4ac541` chore(telegram): trace text extraction and /cowork dispatch in handle_message
- `5ec50acb` style(telegram): rustfmt reflows on the merged pr-1469/pr-1470 files
- `b9005c38` test(config): coverage for recursive write_key — array-of-tables, object, scalar, null + mixed-type hard errors (#87)
- `4997f847` style(telegram): if/else for n=1 tier per clippy obfuscated_if_else
- `e7283951` Session-Id: 1a63f103-b899-4ad2-a5b3-c89f2902bf97 Issue-Ref: #119
- `d4374650` Session-Id: 1a63f103-b899-4ad2-a5b3-c89f2902bf97 Issue-Ref: #119
- `ddd684db` Session-Id: 1a63f103-b899-4ad2-a5b3-c89f2902bf97 Issue-Ref: #119
- `ec371608` test(119): update fold-tier expectations to bold Go render (owner correction)
- `9fee1bde` style(119): rustfmt line wraps in merge test
- `f1051b19` test(config): drop non-empty assert from renders-every-section (#86)
- `bb8dc6ff` test(cron): pin the resolver's real prefix contract (#144)
- `a300af64` test(cron): pin the resolver's real prefix contract (leshchenko1979/opencrabs#144)
- `9040d062` test(headless): pins for leshchenko1979/opencrabs#129 — registry gates, hard-error backstop, preamble, child-strip
- `6e27926b` test(129): fix unsatisfiable interactive-roster pin
- `362f35d2` style(tests): rustfmt mod.rs alphabetical ordering (leshchenko1979/opencrabs#129)
- `d470fd46` style: rustfmt reflows across oc:// resolver, channels and tests
- `0c554fd1` test(brain/tools): assert error on result.error in bash background tests (#152)
- `5b91ea6a` test(memory): pin the #89 P1 parity contract
- `61d6ec09` test(memory): fix definition-ordering pin to the actual sorted contract (#89)
- `6cebb673` test(tui): move presets_test.rs under src/tests/ (#1453)
- `769f0259` test(tui): make auto_title_e2e deterministic via virtual time (#1455)
- `49a9cab0` test(whatsapp): restore the rate-limit test registration dropped in a9f65d9b
- `0936fa02` style: rustfmt the merged PR stack
- `a08a8911` test(brain): isolate time-marker tests from live home recall

### 👥 Contributors

6 contributors this release:

- @adolfousier (Adolfo Usier)
- @leshchenko1979 (Alexey Leshchenko)
- @moneyacademyKE (Moe)
- @adi805 (Syafriadi)
- @loonix (Daniel Carneiro)
- @technologicalsingularity (Andy)

### 📊 Stats


- 213 commits since v0.5.0
- 6 contributors
- 373 files changed, +27,981 / -2,729 lines
- 8,397 tests (8,367 passed, 0 failed, 30 ignored)


[0.5.1]: https://github.com/adolfousier/opencrabs/compare/v0.5.0...v0.5.1

## [0.5.0] - 2026-09-06

595 commits since v0.3.83, 4 contributors. 591 files changed, +57,403 / -11,296 lines.

The biggest release since v0.3.9 in April, and more than six times the largest
one since: 595 commits against a previous best of 97. Only v0.3.9 itself, at
1,022 commits, was larger. The version skips 0.4 deliberately:
pre-1.0 the number is communication rather than arithmetic, and 0.3.84 would
have understated what this contains. The Rust library surface is still
internal and unstable; see Versioning and stability in the README for what the
version number covers.

### ✨ Features

- `0e9d4f1d` **boot**: probe the recovery table at boot and say when turns cannot be recorded (#1401)
- `01986171` **prompt**: the preamble forbids tests from writing the live config or keys (#1399)
- `859bb124` **config**: name the voice flag a reload switched off, and log every wholesale rewrite (#1399)
- `8595c816` **core**: tap-redraw picked suggestion buttons on rich hosts (leshchenko1979/opencrabs#67) (#1394)
- `bb4b11d7` **brain**: endorse single-option suggest_options one-tap confirms
- `8b0a4695` **telegram**: fold bare bg-task completion acks into the settled flow card (#1377)
- `844f57aa` **memory**: advertise external scope and structural code queries in memory_search descriptions and AGENTS template
- `49c399e9` **tui**: interactive theme picker dialog (#1371)
- `4e0d642f` **tui/theme**: user theme presets directory ~/.opencrabs/themes/*.toml (#1365)
- `3523def2` **tui/theme**: truecolor capability check + ANSI-256 fallback tier (#1364 F3)
- `6edd1f3e` **tui**: add AnsiColors field with quantizer-derived values for all 9 presets
- `0f5633ac` **tui/theme**: add rgb_to_ansi256 quantizer with unit tests
- `30c4ec55` **tui**: [tui.theme] config + /theme list|set|reset command (#1364 D+E)
- `66b9de76` **tui**: upstream-verified theme presets registry (#1364)
- `56c6c583` **tui**: theme role core with runtime slot (#1364)
- `0844616b` **provider**: ProviderKey entries for the two [providers.fallback] lists (#1355)
- `8283946d` **prompt**: preamble rule for writing large files in parts and delegating long work (#1352)
- `5141045e` **pdf**: make pdfium a default feature (#1336)
- `88859e8a` **memory**: tree-sitter symbol/call-graph indexing for structural code queries (#1324)
- `f6e9324c` **tools**: carry the session's provider on the execution context (#1318)
- `b220464e` **telegram**: one boundary between a topic key and a wire address (#1319)
- `8b28159a` **rsi**: normaliser for self_improvement_provider spellings (#1314)
- `05f3193e` **tui**: show where a tunnel-pulled drop landed instead of copying it silently (#1311)
- `91f02a86` **agent**: boot summary counts what the recovery pass replays (#1242)
- `0956d359` **compaction**: summarise in the background instead of stopping the turn
- `345ea935` **tui**: attach a dropped file pulled over the tunnel (#1289)
- `de99a641` **cli**: opencrabs drop-agent (#1289)
- `e80182b7` **drop**: pull a dropped file over the SSH connection already open (#1289)
- `12ae20a8` **tui**: choose how to fetch a file dropped from the client machine (#1289)
- `2e85ad7d` **tui**: resolve a dropped path by its extension, not by word boundaries (#1288)
- `a13cf8e9` **notify**: depth-3 injection receipts — notify_id becomes checkable
- `d7712d49` **notify**: confirm=true returns an end-to-end delivery verdict
- `00cdfb88` **notify**: redirect delivery to the session that owns the channel now (#19)
- `0c57dfb0` **nudge**: shared variation directive for loop guards + loud break breadcrumb
- `12b2fcee` **flow**: deferred-notification segment on the flow footer
- `101a43ad` **notify**: quiet delivery mode with starvation cap and deferred verdict
- `e90d6178` **notify**: delivery policy object with interrupt alias and action enum
- `acd7a849` **notify**: machine-readable send verdict on session_notify
- `29b85660` **agent**: in-loop mermaid regen nudge on parse errors (#37)
- `1e2b5249` **agent**: mermaid_regen_nudge text builder (#37)
- `dad3599d` **telegram**: preflight_parse_errors for the regen nudge (#37)
- `ff919559` **telegram**: cache deterministic mermaid render outcomes (#37)
- `bceaade7` **telegram**: classify mermaid parse errors as MermaidResult::ParseError (#37)
- `1e1e44aa` **agent**: in-loop mermaid regen nudge on parse errors (#37)
- `326c0f9c` **agent**: mermaid_regen_nudge text builder (#37)
- `c222d55a` **telegram**: preflight_parse_errors for the regen nudge (#37)
- `7f038632` **telegram**: cache deterministic mermaid render outcomes (#37)
- `93a13aac` **telegram**: classify mermaid parse errors as MermaidResult::ParseError (#37)
- `59eddf42` **tool-loop**: loud break — user-visible breadcrumb when a loop guard ends the turn (#32)
- `3258c892` **tool-loop**: near-match loop-guard nudge composes the shared variation directive (#32)
- `a12a226f` **bash**: Layer-3 retry rejection composes the shared variation directive (#32)
- `bf827bb1` **nudge**: shared variation_directive() — the recurring-call lesson for loop guards (#32)
- `4d8e4251` **telegram**: trailing ❕ on the ctx segment while the pressure hint is active (#29)
- `af117dcf` **telegram**: pin flow header to a dedicated compacting state (#29)
- `29fbf25a` **telegram**: compaction signal — event payloads, first CompactionSummary emit, flow body entries (#29)
- `18df27ed` **cli**: add opencrabs session notify — session notifications for tooling (#23)
- `0657e9f8` **notify**: redirect delivery to the session that owns the channel now (#19)
- `97873452` **tg**: render bg-completion and session-notify pushes as receipt cards (#15)
- `618d469b` **agent**: carry bg-task receipt metadata through the push envelope (#15)
- `285af707` **session_notify**: interrupt failsafe gate for mid-flight targets (#13)
- `624878e1` **tg**: deliver mermaid.ink renders as bytes - Telegram never fetches URLs
- `554bdcfc` **tg**: hybrid mermaid resolve - mermaid.ink primary, local render fallback
- `cfa260a2` **tg**: deliver locally-rendered mermaid PNGs via multipart upload
- `807b3162` **tg**: local mermaid renderer core (feature local-mermaid)
- `cf34c1c2` **tg**: add local-mermaid feature (mermaid-render + resvg)
- `a38ec557` **tg**: render bg-completion and session-notify pushes as receipt cards (#15)
- `87882b8e` **agent**: carry bg-task receipt metadata through the push envelope (#15)
- `dc71bc8c` **cli**: add opencrabs session notify — session notifications for tooling (#23)
- `843579d8` **rsi**: stale-scan ledger + cycle wiring - dedup, daily cadence, prompt block (#1240)
- `241c5f79` **rsi**: stale-claim scanner core - anchors, historical-exempt classifier, deterministic verification (#1240)
- `2dc497a7` **plan**: workers read-only by default (#1173 grant) - drift cannot mutate
- `5cabab3a` **tools**: machine-readable session discovery (#1225)
- `b023d7cf` **telegram**: add receive-only userbot capture
- `4bce4302` **telegram**: add local userbot authentication
- `cba7ffdb` **telegram**: add receive-only userbot config
- `ecad8adf` **telegram**: single-flight session resolution per chat and topic (#1201)
- `5f9a110b` **telegram**: tag queued mid-turn messages with their origin (#1213)
- `d18a987d` **tg**: rich-endpoint governor, plus repairs to make #1216 build (#1211)
- `ef41cba4` **config**: one section registry, shared by reads and writes (#1199)
- `60dbede2` **tg**: proactive per-forum flood governors (#1211)
- `007a1330` **tg**: render untagged fences whose body looks like mermaid
- `55ff59fd` **recovery**: park completions for a revived channel session (#1206)
- `6eacb2de` **tools**: session_notify — cross-session push with mechanical from=<uuid> signature (#1203)
- `1c19d9b9` **tg**: suggestion controls ride table-free rich bubbles natively
- `5c4e85d9` **tg**: calibrated suggestion layout ladder
- `3b2e7447` **telegram**: merge suggest_options keyboard into the final reply bubble
- `1fca3341` **subagent**: settle-card counts sub-agents + distinct AwaitingInput state (#1183)
- `68827238` **phantom**: flag a fenced shell command in a zero-tool turn (#1194)
- `b94c9b36` **tools**: write_file overwrite guard requires full read or explicit confirm (#1168)
- `a2aa7337` **tools**: add tail operation to session_search for last-N history retrieval (#1166)

### 🔧 Fixes

- `7a65494f` **db**: heal a pending_requests table stamped past migration 37 without its origin column (#1401)
- `e074b022` **agent**: wrap tool-text-leak fail-clean in AgentError::Provider
- `b0e1d4cc` **agent**: one corrective retry on tool-text leak, then fail clean
- `d027b2d7` **provider**: strip unrecoverable tool-call JSON from responses, flag leak
- `a09b0abd` **tests**: refuse writes to the live default home from a test build (#1399)
- `c58b3f6f` **config**: migration 2 writes the voice enablement it computes (#1399)
- `c72f7a5c` **onboarding**: every voice writer persists fallback_chain with the enabled flags (#1399)
- `b6ed3dac` **voice**: disabled engines are not dispatch candidates and the chain names the primary (#1399)
- `b4898100` **loop-guard**: rotate the fallback chain on a loop break instead of dropping the turn (#1397)
- `c784964a` **loop-guard**: keep identifiers in string arguments of the near-match signature (#1397)
- `d7e40ceb` **onboarding**: remove orphaned key-field helpers left by teal centralization
- `4c9408a8` **onboarding**: stored API keys render teal in all voice-dialog fields
- `b3911216` **telegram**: resolve EditErr/PlaceErr type mismatch + unclosed delimiter
- `7abe6ad8` **telegram**: tap_retry scope + PlaceErr definitions after pr-1393 merge
- `6c19a715` **tui**: theme picker triad compliance — clippy + clamp no-op (#1395)
- `071b9ae1` **theme-picker**: clamped movement returns no-op, not a re-Preview
- `6974a136` **tests**: reqwest_teloxide client in edit_retry tests — Bot::with_client wants teloxide's reqwest 0.12 type (#1345)
- `6789f236` **telegram**: 429 backstop for ephemeral posts + dedup approval sends
- `d337d312` **telegram**: route the 18 unpaced agent.rs callback legs through edit_retry (#62)
- `30d0b46d` **telegram**: defer one identical retry past Telegram's Retry-After window on best-effort UI edits (#68)
- `b24c703e` **telegram**: gate stale_host_count to test builds (dead code in lib target)
- `861889c9` **telegram**: unify stale-strip arm return types (Message -> ()); test host ids i32
- `0fb1f74d` **telegram**: host-aware stale-shell strip — zombie rich buttons die visibly (#59)
- `eafafabc` **tui**: show the transient notice inside the input row, not on its border (#1369)
- `89e3b730` **telegram-userbot**: log every dropped result in the login path
- `ddd96b72` **config**: keys.toml api_id = 0 no longer shadows the userbot api_id from config.toml
- `40de25ea` **telegram-userbot**: abort the MTProto runner together with the watch loop
- `b3b615ee` **tui**: transient notices render on the input's bottom border, not inside the chat body (#1369)
- `8323f66c` **sessions**: scope-all model writes preserve recency order (#1367)
- `c180e4b1` **onboarding**: the voice step writes the real providers.tts.openai key, not the read-only voice view (#1387)
- `be4f7539` **tui**: /compact sends before it reports, and reports a dropped send (#1375)
- `89a0afe1` **telegram**: drop the dead GluedHost variant, await the glue note properly
- `dd6794da` **telegram**: cover PickRewrite::GluedHost in the tap rewrite match
- `4b4ec62c` **telegram**: gate fixes for #55 glue tier
- `e8e11043` **telegram**: glue tier — lamp retired, keyboard glues onto table answers
- `394731cf` **agent**: an overflow walks the compaction chain widest window first (#1379)
- `886e800e` **provider**: a context-length rejection walks the fallback chain (#1379)
- `fa51dad0` **provider**: a fallback runs its own configured default_model, not the model the failed request carried (#1374)
- `2dd2cb46` **tui**: validate profile name at input step, surface dialog errors inline (#1381)
- `b0fd8462` **config**: sync CONFIG_SECTIONS to schema — 8 missing sections in, phantom voice out (#1385)
- `ae9da344` **config**: accept tui+doctor as known top-level keys, pin loader list to schema
- `8400538e` **brain**: seed templates at every entrypoint, not only on wizard completion
- `7eabab57` **channels**: gate-clean the bg-ack fold surface
- `29352269` **loop-guard**: preserve numeric args in near-match signature (#82)
- `bc7b12e8` **tui/theme**: register /theme in SLASH_COMMANDS so it appears in autocomplete and help (#1364)
- `569f0128` **tui/theme**: mission control panels follow active theme (#1364 G2)
- `1f0be7bf` **telegram**: promote oversized or mermaid-bearing trailers to the answer
- `442807d1` **provider**: normalise [providers.fallback] providers and vision entries like the [agent] keys (#1355)
- `ad9822f9` **rtk**: the no-rewrite debug line no longer reads as a refusal (#1353)
- `b5efdf8a` **agent**: end a reasoning stream on a same-character run, not only on a repeated substring (#1351)
- `1ce7b711` **provider**: honour base_url on [providers.zhipu] and document the real hosts (#1350)
- `c23b6406` **provider**: map reasoning_effort onto the GLM-5.3 ladder instead of letting the endpoint silently pick max (#1349)
- `3e0266fe` **provider**: send Preserved Thinking to z.ai for GLM-5.x (#1348)
- `d56c459b` **provider**: send tool_stream to z.ai so tool-call arguments stream as deltas (#1347)
- `53fdedff` **tui**: clear a cancelled turn's indicator in split view (#1342, #1343)
- `fff66170` **cli**: resolve session IDs by unambiguous prefix in get and notify
- `d8f3626b` **telegram**: retry a rich edit that failed in transport instead of downgrading (#1323)
- `9485c95b` **logging**: redact the Telegram bot token before anything is written (#1322)
- `e669984d` **provider**: stop retrying a WAF block page as a transient error (#1332)
- `ab0e7aa8` **provider**: identify OpenCrabs to every gateway, not just OpenCode (#1331, #1333, #1334)
- `0de41ab2` **provider**: identify OpenCrabs and send a session id to OpenCode Zen (#1329)
- `c8080d1c` **memory**: recurse into nested call expressions and impl items in symbol extractor (#1328)
- `3cd4cc9f` **vision**: resolve candidates per request, not once at startup (#1318)
- `377b9010` **vision**: the configured chain decides the order, not a scan (#1318)
- `de844af1` **telegram_send**: make 'no thread' expressible, and stop re-injecting one (#1319)
- `b3351e91` **telegram**: resolve General to no thread on every delivery path (#1319)
- `802a3431` **telegram**: re-publish scoped command menus when skills change (#1317)
- `9517f52b` **plan-mode**: normalise plan_provider / execute_provider before the swap (#1316)
- `59cba8ba` **subagent**: resolve the child's provider/model through one normalised helper (#1316)
- `d230bee7` **rsi**: resolve the cycle's pair through the normaliser and log the correction (#1314)
- `d17027fd` **tui**: the drop-tunnel hint binds the reverse forward to loopback explicitly (#1313)
- `2e2a29d3` **tui**: the remote-drop fallback also names the drop-agent tunnel (#1311)
- `43edb916` **tui**: probe the default drop-tunnel port over SSH instead of requiring OPENCRABS_DROP_PORT (#1311)
- `01e0d86b` **tui**: a file pulled over the drop tunnel keeps the client's filename (#1311)
- `e61d939d` **db**: bump MIGRATION_COUNT to 37 for 034_pending_followups
- `bbba209e` **tg**: bg-echo bubble rides the markdown dialect so tables stay native (#1234)
- `4e3d80ec` **channels**: wait for a channel's transport instead of dropping the wake (#1242)
- `f7e34f53` **compaction**: bound a summariser attempt so a wedged one hands the work on (#1255)
- `a9673df2` **compaction**: a context that shrank always leaves a marker
- `f308887b` **telegram**: one definition of a heading, not two (#1257)
- `c570c768` **tui**: land a dropped file where every other share lands (#1289)
- `70f4496f` **tui**: the scp hint had the direction backwards (#1289)
- `9ea7eccf` **tui**: always give the scp line, mention a better tier rather than substituting (#1289)
- `698377b4` **tui**: tell the user how to fetch an unreachable dropped file (#1289)
- `4f0dd473` **tui**: attach a dropped file even when you type something after it (#1288)
- `d60133a3` **notify**: E0277 test assert &String vs String; carrier rustfmt import order
- `419ce270` **telegram**: warn! on mermaid.ink render failure — non-200 attempts were log-invisible
- `714bbd36` **harvest**: strip fork-only #12/#31 pollution from transplanted hunks
- `e0272596` **telegram**: drop legacy 90s plan-card restick cooldown; restick per user-turn settle (#62)
- `dc966df8` **suggest**: resolve trailer bubble conflict with retry-after refactor
- `f158e271` **notify**: drop leftover quiet_deferred test args after display revert
- `001ca5c8` **telegram**: DRY the suggestion pick-record rewrite — classic host keeps the choice
- `3bfc5328` **test**: align #19 tests with struct-variant Delivery and ToolResult fields
- `d6f2ab83` **memory**: read chunk_hash as Option — NULL on healed legacy rows
- `a88308c7` **tests**: satisfy clippy bool_assert_comparison in heal tests
- `84d52803` **memory**: heal missing chunk_hash column on pre-#1107 stores (#14)
- `85903898` **notify**: clear clippy dead-code + collapsible-if in quiet_delivery
- `6f4313ef` **notify**: drop the summary argument from the defer_quiet callsite
- `34bd8ff2` **telegram**: compaction progress dedupe, observed-ETA predictor, icon-aware gear strip (#1271)
- `48d3195a` **notify**: refuse delivery into a channel a newer session now owns (#17) (#1267)
- `3fe517e8` **telegram**: boot-time wake of recently-active sessions, log-only (#1270)
- `057673f4` **notify**: clippy + compile fixes on the v2 rail
- `ecbfda33` **telegram**: recalibrate shared-row button cap 8 -> 12 chars (#49)
- `c4a9ef55` **memory**: read chunk_hash as Option — NULL on healed legacy rows
- `a0d91e13` **tests**: satisfy clippy bool_assert_comparison in heal tests
- `8087eb6c` **memory**: heal missing chunk_hash column on pre-#1107 stores (#14)
- `d730e9ca` **telegram**: drop needless borrow in rich-fallback options gate (#46)
- `00c63a0f` **telegram**: rich-fallback arm consults options_pending (#46)
- `96fd2501` **telegram**: drop needless borrow in options_pending call (#45)
- `c7c28754` **telegram**: button-bearing prose rides the rich plane (#45)
- `1fa42e40` **provider**: sanitize journal text tails — embedded newlines split log lines
- `6bc07521` **provider**: two exhaustive TokenUsage literals gain ..Default::default()
- `1895eef5` **agent**: catch mid-sentence truncation at a backtick — fence/backtick parity + usage-gap journal
- `6ce4c22b` **provider**: TEXT_ACCUM/STREAM_RECONCILE journal — reconcile billed usage vs received text per stream
- `b6a4ce49` **telegram**: keep suggest_options stash alive on Retry-After, defer re-placement (#1264)
- `6bbdf1c3` **plan**: align archive writer/reader on one stem so completion cards post again (#1265)
- `d7afa569` **provider**: sanitize journal text tails — embedded newlines split log lines
- `8e19c3d0` **telegram**: DRY the suggestion pick-record rewrite — classic host keeps the choice (#39)
- `89895168` **telegram**: drop redundant top-level Requester import
- `48972be1` **telegram**: port chrome-reclaim test to options_pending tuple API
- `4bcac963` **telegram**: collapse trailer-embed conditionals, allow 8-arg render_suggestions (#31)
- `c15f7891` **telegram**: gate fixes for trailer lane — Requester import + move-before-log (#31)
- `8cecebba` **telegram**: reclaim post-halt sign-off as trailer after option-surface buttons (#31)
- `16568074` **telegram**: drop unused FlowEntry import — REBASE-PORT keep-both leftover
- `02779c38` **provider**: two exhaustive TokenUsage literals gain ..Default::default()
- `c26b9adc` **agent**: catch mid-sentence truncation at a backtick — fence/backtick parity + usage-gap journal
- `cc5f36c7` **provider**: TEXT_ACCUM/STREAM_RECONCILE journal — reconcile billed usage vs received text per stream
- `267089af` **telegram**: union pop_trailing_folded_texts with #1253 chrome-awareness — REBASE-PORT keep-both damage
- `4cd213f3` **telegram**: drop stray closing brace in trailing_matches — REBASE-PORT keep-both damage
- `5e789417` **telegram**: flow chrome — strip the standing gear before another icon (#29)
- `84b3f693` **telegram**: allow 8-arg chrome-pref renderers (#29 clippy too_many_arguments)
- `fa75f22f` **telegram**: compaction signal — footer dedupe + observed-ETA predictor (#29)
- `72806138` **telegram**: boot wake goes log-only — drop the 'beep' bubble, keep the audit line (#34)
- `552653a2` **test**: drainer converge is event-driven — mode-4 flake removed (#28)
- `268f34bc` **telegram**: capture tool-run presence before draining aside (#31)
- `7e317380` **telegram**: collapse trailer-embed conditionals, allow 8-arg render_suggestions (#31)
- `003a9ea5` **telegram**: gate fixes for trailer lane — Requester import + move-before-log (#31)
- `907c7dbd` **telegram**: reclaim post-halt sign-off as trailer after option-surface buttons (#31)
- `b71287ad` **test**: governor drainer mock — serve the full retry budget, counter-asserted exactly-once (#28)
- `cb8f6d96` **telegram**: keep suggest_options stash alive on Retry-After, defer re-placement (#30)
- `704843d6` **test**: compacting field in resume-test StreamingState initializer (#29)
- `97dd5e6d` **test**: governor drainer wire test — timeout-free bot client (#28)
- `774fbbea` **plan**: align archive writer/reader on one stem so completion cards post again (#16 round 2)
- `168bd25f` **plan**: never let tool auto-approve satisfy the plan approval gate (#20)
- `56c2dd4e` **tg**: strip-match arm takes the Message ok-variant (#1226)
- `cc21d894` **tg**: reclaim the final answer across an option-surface halt (#1226 K)
- `c1192b7b` **plan**: re-arm finalize only after completed-card lands (#16)
- `095ef4f2` **tg**: label DM sessions with the bot's username, not the reader's name (#15)
- `a889db12` **test**: allow await_holding_lock on the three session/notify suite tests (#23)
- `368d1b68` **ci**: re-wrap three lines to rustfmt 1.98.0 shape (#25)
- `e86b14bd` **clippy**: clear the four main-lane findings introduced by #19 (#22)
- `826a1c5c` **bg-resume**: park unconditionally, never re-enter the route table (#21)
- `1f2224b4` **test**: align #19 tests with struct-variant Delivery and ToolResult fields
- `0e1bad85` **notify**: refuse delivery into a channel a newer session now owns (#17)
- `5c3703fc` **tg**: count inline <details><summary> openers as rich structure (#15)
- `b4fda5cd` **recovery**: capture push-initiated turns for restart recovery (#12)
- `8a1bdf8c` **test**: align N4 shape expectation with the 45-char preview boundary (#15)
- `da3233eb` **test**: point bg_push_echo_test imports at the receipt-card builders (#15)
- `9c899d9e` **clippy**: clear PR-lane clippy debt on telegram-only tree
- `4c9db2a3` **port**: compile 5 all-features test errors exposed by phase-1 gates job
- `e4871884` **tg**: mermaid.ink dimension ladder — request-fitted renders, remote-only delivery (#1238)
- `5135ea02` **subagent**: cover Delivery::RefusedInFlight in spawn push_result (#13)
- `d9537164` **tg**: type photo-dims cap as f32 - From<u32> for f32 does not exist (#1238)
- `0de3cc56` **tg**: borrow bot for bg-resume rich outbox call (#1234)
- `98fe0fd3` **tg**: adaptive raster scale for oversized mermaid diagrams
- `c6661d1a` **tg**: bg-echo bubble body rides canonical md-to-rich outbox (#1234)
- `72fd32e7` **resume**: bounded SDK-readiness wait parks boot-window wakes instead of dropping them
- `6af2f920` **tg**: wrap mermaid-render SVG fragment in a document root for usvg
- `df844751` **tg**: pin ok_or_else error to String in local mermaid pixmap alloc
- `fad31677` **tg**: pass &mut PixmapMut to resvg::render (resvg >=0.45 signature)
- `c2d0423a` **build**: restore resvg dep eaten by usvg/tiny-skia manifest insert
- `3362c8a8` **tg**: declare usvg + tiny-skia as direct deps of local-mermaid
- `da1decac` **tg**: multipart carrier — swap build arg order, drop non-Clone form.clone
- `28bc255e` **restart**: boot-time wake of recently-active bound sessions (#1227)
- `4776bee2` **phantom**: a bare tool name counts when the model claims to have used it (#1262)
- `a15b7f0b` **phantom**: catch a sequenced plan announcement in a zero-tool turn (#1261)
- `2acac9bc` **channels**: a chat's /cd directory actually sticks to that chat
- `d9d6960c` **tg**: match upstream send_markdown_outbox arg order on the receipt-card route (#15)
- `aabc3bc3` **tg**: label DM sessions with the bot's username, not the reader's name (#15)
- `3d0b42ef` **tg**: count inline <details><summary> openers as rich structure (#15)
- `f8f2538a` **test**: align N4 shape expectation with the 45-char preview boundary (#15)
- `23c00a36` **test**: point bg_push_echo_test imports at the receipt-card builders (#15)
- `0d944a64` **provider**: a balance 429 stops telling the user to retry later (#1254)
- `27f44bdd` **tg**: never reclaim system chrome as the turn's answer (#1253)
- `795f9ec5` **provider**: never remove a provider from the fallback walk (#1251)
- `a0954b63` **compaction**: walk the fallback chain instead of dying on the primary (#1247)
- `34b26778` **provider**: reload [providers.fallback] on config change instead of at startup only (#1249)
- `ce05e09b` **tg**: route inline-button callbacks to the session that serves the chat (#1248)
- `a4870089` **tg**: close the brace my let-chain conversion orphaned in plan_card
- `30f9a7c9` **tg**: token-keyed suggestion stash - each tap resolves its own keyboard (#1217)
- `0709cf19` **voice**: persist STT/TTS enablement in both writer paths (#1233)
- `c57de25c` **plan**: scope archived-plan selection to the completing session (#1239)
- `3e6e1eac` **tg**: kill plan-card wrong-settle - one-shot archived consume gate replaces the 120s polling window (#1231)
- `da6216ac` **tg**: telegram_send reply/edit route rich-first - tables render as real grids not pre blocks (#1230)
- `4a2bcd63` **tg**: render bg-push echo bubbles as native <details> rich cards (#1221 follow-up)
- `d8d54484` **tg**: suggestion picker edges + shared session gate + session bindings (#1226 items 1+2+5+6, #1228, #1224 core) (#1232)
- `4d557542` **tg**: normalize General-topic sessions (#1220); recovery replay claims the turn slot (#1222) (#1223)
- `e445180d` **telegram**: repair the merged suggestion-merge branch (#1204)
- `1f315763` **telegram**: hold the gate across resolve and registration (#1201)
- `b3aad40a` **telegram**: give a detached result a real tool loop, not a single round (#1213)
- `00710210` **telegram**: mark bg and sub-agent results as detached work (#1213)
- `63faa87a` **subagent**: bind the child's tool_search weakly (#1210, #1214)
- `a0d0a139` **subagent**: rebind tool_search to the child registry on spawn (#1210)
- `a62d297d` **tool_search**: bind to the registry whose active set is actually read (#1210)
- `19145c7b` **config**: reject a write to a section that does not exist (#1199)
- `a065eb1f` **telegram**: prefer the session's topic on the startup resume path (#1200)
- `6acbc6bd` **telegram**: route detached-work pushes to the session's own topic (#1200)
- `5d0a5a0e` **mermaid**: classify bare fences by the opening info string (#1208)
- `28bd3391` **session_notify**: a parked delivery is queued, not a missing route (#1207)
- `9c0e4b81` **routing**: report delivery as three outcomes, not a bool (#1207, #1206)
- `c9dc7447` **tests**: assert the channel budget invariants at compile time (#1212)
- `d7f126bf` **browser**: remove the unused EventLog::is_empty helper (#1212)
- `b9b435e1` **tests**: drop imports left stale by the SubAgent::new refactor (#1212)
- `931ff9e1` **subagent**: re-bind tool_search to each child registry (#1210)
- `3660a1e7` **tg**: surface mermaid-bearing intermediates instead of folding them
- `aa653d54` **daemon**: stop handing a headless process the TUI event channel (#1206)
- `41085e51` **routing**: a revived channel session is not a local session (#1206)
- `b9773e25` **session-notify**: ToolResult::error takes String not &str (E0308)
- `0eaa4d8b` **tg**: wildcard rich flag in fallback arm — inner match can't inherit outer arm narrowing
- `5ae79b6e` **tg**: annotate empty keyboard vec element type for inference
- `e707cdb8` **tg**: import EditMessageTextSetters — parse_mode setter needs its payload trait
- `7e6d9823` **tg**: declare final_bubble capture variable left undeclared
- `2368c66a` **tg**: close RetryAfter arm left unclosed by match restructure
- `d3000b2f` **tg**: resume path renders suggestions post-delivery with merge host
- `4f222f6e` **tests**: resolve Into inference ambiguity in SubAgent::new fixtures
- `0084e1ed` **#1197**: deliver results on all completion paths - resume and team agents were silent
- `1a2911c3` **build**: gate discord/slack/whatsapp refs behind their features so non-default subsets compile (#1186)
- `13044a30` **phantom**: accept a connective before the gerund in work_announcement_re (#1193)
- `0e74d6bf` **phantom**: break announcement candidates on em dash, colon and semicolon (#1192)
- `37e928d3` **image**: recover reaction directives from orphan-fence wrapped turns (#1182)
- `ec3cad36` **channels**: wire subagent manager into channel factory so tasks_list works in chat sessions (#1170)
- `ba449f00` **tools**: self-describing non-regular-file rejection in validate_file_path (#1164)
- `9bd2f747` **tools**: expand ~ in bash working_dir before validation (#1165)
- `eec98794` **tools**: autocorrect missing leading / in slash_command (#1167)
- `0942f07d` **cron**: repair legacy dedup scan schedule and warn once (#1163)

### 🔒 Security

- `10c2e657` **deps**: bump dependencies, clearing the yanked chacha20 (#1344)
- `3e40b8ad` **deps**: clear two advisories and make the audit ignore list truthful (#1344)

### 📖 Documentation

- `1796af02` correct the release-binary claim and refresh the test counts
- `e698ccd5` add a vulnerability-disclosure policy (#1406)
- `5cf0588e` drop the task_manager row — it documents a tool that is not registered
- `78b1c3f9` state what the version number covers before 1.0 freezes it (#1403)
- `1b629458` tests: count the restart-recovery work for #1401
- `a293d227` tests: count the preamble live-config rule test
- `15be6b5e` contributing: tests never touch the live config or keys (#1399)
- `068d08fe` tests: count the voice enablement work for #1399
- `6de2fc4f` tests: count the loop-guard work for #1397
- `1fee0e33` contributing: update CI failure policy — comment with fix instructions, don't merge until green
- `e6543a84` tests: count the notice tests added for #1369
- `e9fffc33` tests: count the userbot test modules merged from PR #1209
- `b58e188b` readme: mark the Telegram userbot rows experimental
- `3f880c5e` tests: count the six tests merged from PRs #1372 and #1373
- `c05c3978` voice: README teaches the real providers.stt/providers.tts engine blocks, not the retired [voice] section (#1389)
- `c4d0e17c` theme picker + user themes in README/TESTING, refresh counts
- `844654a3` contributing: state atomic commits as repository-wide, with short-lived branches or stacked PRs
- `9d387543` template: two-step code-analysis rule (memory search → source verify)
- `e75fb0ba` contributing: issue titles follow Conventional Commits like commit messages
- `a80e8019` refresh the test counts and regenerate the TESTING.md inventory from the run (#1362)
- `c4c28922` drop the phantom [image.vision] provider key; the vision pin is [providers.fallback] vision (#1355)
- `ac9fbc42` refresh the test counts for the cancel-indicator suite
- `b0adac26` close the README gaps found auditing 444 commits since v0.3.83 (#1336, #1337, #1338, #1339)
- `5db959fa` refresh the test counts for the redaction and transport-retry suites
- `5811b557` refresh the test counts and complete the TESTING.md inventory (#1335)
- `9d7920a7` eval: Alexey spike vs code-graph head-to-head report (12-query comparison)
- `13644d3e` eval: generic-heavy callers row to 5/5 post-#1328
- `b4c4c8c0` readme: refresh benchmark to post-#1328 graph numbers (15,769 symbols / 95,601 edges, 5/5 caller recall)
- `a6e42b5b` readme: code-graph benchmark in Benchmarks section; session_search described accurately
- `1f0cbae6` readme: document code-graph symbol graph + benchmark, browser_find/browser_act (#1326)
- `9c7f7da2` all four [agent] provider keys correct custom:<name> and provider/model spellings (#1316)
- `c3c1f79d` the custom provider label is the provider's name in every other key, plus a troubleshooting entry (#1315)
- `ab7b2e4b` the RSI provider key is the bare section name, never custom:<name> or provider/model (#1314)
- `2c5d0bfb` drop_transfer: the module examples use the loopback-bound forward too (#1313)
- `186e7812` say what the drop-agent tunnel exposes (#1312)
- `e57bf27b` the VPS drop section leads with the copy, and the ssh -R flow needs nothing on the server (#1311)
- `eee70139` fix README table of contents placement and add measured benchmarks section
- `bcb6b965` refresh test counts in README (7,566 tests / 747 modules / 33 ignored) [skip ci]
- `e64b3fc3` drop: name the floor before the upgrade (#1289)
- `1f3dd95f` drag-drop into a remote TUI, over the SSH connection already open (#1289)
- `38d3d803` dropping files into a TUI running on a VPS (#1289)
- `b71e39fc` document [memory] extra_paths in config example and README (knowledge-base workflow, #1287)
- `354076d8` telegram: define read-only userbot boundary
- `96a7f4d1` #1195: plan_auto_start key - separation default, cascades as opt-in future work

### 🧹 Miscellaneous

224 commits of internal work not enumerated here: 88 refactors, 59 test additions, 17 chores, 14 formatting passes, 7 CI changes, and the remainder small reverts and
housekeeping. Run `git log v0.3.83..v0.5.0 --no-merges` for the full list.

### 👥 Contributors

4 contributors this release:

- @adolfousier (Adolfo Usier)
- @leshchenko1979 (Alexey Leshchenko)
- @moneyacademyKE (Moe)
- @adi805 (Syafriadi)

### 📊 Stats

- 595 commits since v0.3.83
- 591 files changed, +57,403 / -11,296 lines
- 7,886 tests (7,856 passed, 0 failed, 30 ignored)


## [0.3.83] - 2026-08-23

52 commits since v0.3.82. 125 files changed, +8,152 / -834 lines.

### ✨ Features

- `a765bc60` **doctor --fix repair mode**: repairs stuck cron rows, stale markers and permissions instead of only reporting them
- `ed103d2e` **One writer at a time per path**: concurrent writes to the same file serialize instead of racing
- `9fe65235` **Sub-agent worktree isolation**: each child gets its own worktree so a fan-out stops colliding
- `35057f13` **Type-aware acceptance criteria** (#1133): criteria are enforced against the plan's toolchain, not just counted
- `e80730f9` **DeepSeek thinking knobs**: sends the parameters DeepSeek actually reads

### 🔧 Fixes

- `0c86df9e` **profile_list in KNOWN_TOOL_NAMES** (#1161): the custom OpenAI-compat provider stops rejecting the tool
- `2c3fbf58` **Profile-addressed A2A gateway** (#1161): the gateway resolves the profile it serves
- `1261e420` **tasks_list, detached status files, prompt hint** (#1160)
- `3b398955` **A2A sessions resume by context id** (#1159): dead stream action dropped
- `faa1a43c` **Tracked plan card finalized on completion** (#1158): instead of deleted
- `884a6b01` **Scoped bot menus for solo-owner groups** (#1155): auto-registered
- `e09a7f08` **Plan glyph sets converge on status_mark** (#1157)
- `eb44eb41` **Checkbox glyphs replace the thin-dot pending mark** (#1154)
- `8c4e396b` **A provider's config models are its own**: not the active one's
- `3d810d18` **Folded-final reclaim suppressed after a dedup match** (#1152)
- `3f02b37f` **Buried flow blocks restick** (#1150): bot-bubble evidence and a sticky budget
- `56044d3c` **Model picker enumeration deduped** (#1149): between body and buttons
- `9665dd34` **/stop cancels immediately** (#1148): during provider handshake and retry backoffs
- `9e96cc28` **Stealth-model remap WARN healed** (#1147): and the context-window log stops lying
- `918d430f` **Model picker pages and filters**: instead of arriving whole
- `a0d33d86` **Waiting state in the settled card header**: while background tasks run
- `9a6a1065` **Overlong follow-up option labels compacted**: per channel
- `8b4cbfb5` **Plan-card prose pipeline unified** (#1142): soft-break parity, mermaid through the gated converter
- `ad242aa4` **No design scaffold .md for checklist plans** (#1145)
- `d2b77a24` **Plans find the project from the session's folder**
- `03418282` **Plans verify with the project's toolchain**: not always cargo
- `14978039` **A loop-detector kill walks the chain**: instead of dropping the turn
- `ab70149b` **A cron job sends where it was configured to**: or nowhere
- `f3302581` **Vacuous-pass guard and audit trail** (#1134, #1135): for criteria_policy flips
- `8def58e3` **Shared status_mark() helper** (#1136): for the Telegram card
- `29f65bc0` **Epistemic store resolves under the profile-aware home** (#1137)
- `265235d2` **DeepSeek empty content string on tool-call-only turns**
- `f146a08b` **Phantom detector rolls the whole chain**: never delivers work that was not done
- `e6bf6e16` **A shared session answers only for its own chat**
- `df3411de` **A reaction lands on the session that owns it**: or nowhere
- `f523564f` **Restart into the binary the evolve signal named** (#1130)
- `7c11f3a3` **A config reference that resolves to nothing is reported**
- `50e4723f` **A turn that ran elsewhere cannot rewrite the session's model**
- `57f5283c` **Context budgets against the provider's token count**: not our estimate

### 📖 Documentation

- `3ba07297` readme: refresh the test counts
- `05ea47eb` readme: document sub-agent worktree isolation and path locks
- `7626d24d` readme: record what the verification gate now checks
- `93cd4c2d` prompt: state the commit rule once, in the preamble
- `8642fe8b` template: tell agents how diagrams actually reach Telegram
- `d5367ca0` testing: generate the coverage table from the test runner
- `062aabdb` testing: regenerate the coverage table
- `6deb0b45` changelog: correct the pre-release build trigger claim (#1111)

### 🧹 Miscellaneous

- `2d44e291` ignore desktop/ for now
- `f2145df7` build A2aConfig in one initializer
- `225da6cc` apply cargo fmt
- `7424e3de` drop the startup-warning snapshot and its delivery (revert)
- `67019229` clear the lints Rust 1.98 added

### 📊 Stats

- 52 commits since v0.3.82
- 125 files changed, +8,152 / -834 lines
- 6,997 tests (6,965 passed, 0 failed, 32 ignored)


## [0.3.82] - 2026-08-20

79 commits since v0.3.81. 162 files changed, +11,298 / -4,874 lines.

### ✨ Features

- `c43f4c6a` **session_id on every log line** (#1078): tracing spans at turn entry, cron jobs and the RSI engine make a single turn greppable out of a shared daily log
- `c04fe914` **Legacy config section migrated on disk** (#1116): the old `[gateway]` spelling is rewritten to `[a2a]` so one name survives the round trip
- `c8440fae` **Chunk-hash caching for memory** (#1107): unchanged chunks are skipped instead of re-embedded on every write
- `09007693` **Pre-release binary builds** (#1111): five platform targets plus SHA256SUMS, published as a rolling pre-release so contributors can test unreleased code without waiting for a tag. Runs **on demand** via workflow dispatch, not per push: `49c1463c` in this same release made it dispatch-only, since five targets on every commit spends build time continuously producing artifacts nobody asked for
- `a2e55422` **Plan template warnings reach the agent** (#1103): surfaced as a one-shot retry nudge instead of being swallowed
- `9c86b651` **Uncheckable checklist rows are marked**: a row whose acceptance criteria cannot be verified is flagged as such
- `9b966d09` **Done-when convention made visible and derivable**
- `8844a5e5` **send_markdown_outbox** (#1085 P1b R2): one send ladder every proactive Telegram writer goes through
- `86b29356` **Send-correlation telemetry** (#1085 P1a): at the three Telegram send chokepoints
- `d50566c7` **Telemetry on every telegram_send arm** (#1085 P2)
- `9a78f5de` **Final streaming edit logged** (#1085): the edit-in-place that closes a stream is now recorded

### 🔧 Fixes

- `00ca69f0` **Logger stops dropping events and writing to the TUI's terminal** (#1115)
- `e21aebda` **Logger decoupled from the send path** (#1077): `try_lock` so one stalled write cannot silence every other thread
- `c3694980` **An interrupted flock is no longer read as a held lock**: stale-lock false positive on startup
- `e9bfcaff` **Both spellings of one config section fold together** (#1116): and a failed reload reports the real reason
- `7ef35955` **Upstream templates stop syncing into user-owned brain files** (#1119): SOUL.md, USER.md and MEMORY.md are no longer merged into
- `bc0f4dfb` **Directives route to AGENTS.md, MEMORY.md keeps facts** (#1121): an on-demand file no longer carries rules that must bind cold
- `095fb32a` **Long rate-limit windows bail immediately** (#1110): instead of parking the send inline
- `a7ebc98f` **Rich sends stop building a double-slash URL** (#1117)
- `37b9e6da` **Rich API calls route through the Bot's api_url** (#1088): `set_api_url` now covers the rich path
- `f0b926fb` **Stale plan pre-init markers auto-expire after 5 minutes** (#1109)
- `08cbb351` **Task-outcome beliefs scoped to the plan id** (#1083): and the flag count is capped
- `c0f95fc5` **Superseded beliefs archived under their own key namespace** (#1083)
- `b12e19c6` **No-criteria completions inherit Uncertain** (#870): closing the asymmetry
- `c113af1a` **Quota breaker recognises modelscope's monthly wording** (#1084)
- `1ef6859e` **Tool ownership follows the active chain entry**, not the primary
- `d9441a1a` **`session limit` counts as a claude-cli rate limit**
- `6784993e` **112 discarded async Results now handled** (#1098): `let _ = ...await` no longer swallows failures silently
- `ba40448f` **Cron awaits delivery handles before exec** (#1105): and persists the rebuild completion report
- `b6b061b5` **Background detach only when the marker starts a command**: a command that merely mentions one no longer detaches
- `3ae9175e` **A channel-driven turn updates the TUI's counters too** (#1092)
- `e9df9713` **All six telegram arms route through thread resolution**
- `08170ccd` **Char-boundary-safe truncation in the dedup scan rationale** (#1082)
- `5caf1d98` **brain/browser added to the config typo whitelist** (#1090)
- `b41835da` **HOME-mutating tests converted to a task-local override** (#1096): removing a suite-only flake
- `8b8c3937` **h2 patched for RUSTSEC-2026-0258**, with the unreachable-to-fix hit ignored and documented
- `28180bc5` **TTS text cleaning builds without the local-tts feature**
- `d7e94166` **Phantom detector catches promised work whose verb was never enumerated**
- `92adcde9` **Streaming placeholder-edit failures logged in the resume path** (#1085)
- `79b93ee9` **Telemetry review fixes** (#1085): info-level, origin threading, full coverage
- `49c1463c` **Pre-release builds on demand, publish job repaired** (#1111): dispatch-only instead of on every push to main, the publish job now checks out the repo it reads the version from, and the notes name the actual ref instead of asserting main
- `085a500e` **Tests updated for chunk-hash caching and the rate-limit bail**
- `531c0e42` **insert_embedding allows too_many_arguments**: 8 params required by chunk-hash caching

### 📖 Documentation

- `8a8e429d` document plan-scoped belief keys and the archive namespace (#1083)
- `1837129d` state that `[gateway]` and `[a2a]` are one setting (#1116)
- `c82e30a6` add the Trendshift daily and weekly badges
- `2ce31159` replace the star history chart with Trendshift
- `a17a2b04` point the star history chart at a working host
- `564ad136` drop an issue reference that points at unrelated work

### 🧹 Miscellaneous

- `323fca1d` `f56586ea` `2a7c7853` `a8338391` `6b72e01b` `05955d99` telegram handler decomposition, seams 1 to 5 (#1086)
- `ea3b2d98` resume-shape smoke of the shared streaming edit loop (#1086)
- `5c0625db` typed targets and extracted action methods (#1080)
- `55797670` one rate-limit wait helper, send_html_or_plain on the shared ladder (#1085 P1b R1)
- `f07c1bea` best-effort helpers replace the discard culture (#1085 P3)
- `3e6e250e` share the detached-work typing tail, log Discord pings
- `6e2f3ff7` split delivery routing out of the task manager
- `a07aede0` one shell scanner, one classifier, and a start log
- `16dc55b5` remove dead plan schema fields and the unwired validation pass
- `8e93c17e` enforce a checkable acceptance-criteria bar across prompt, schema hint and sample plan
- `f2fb29ce` plan import mirrors #581 auto-approve, fixes the Editing/start contradiction
- `763862b5` run the plan import tests under a temp profile home
- `d9ab48d0` assert the corrected logger contention contract (#1115)
- `c577413c` `4d3b713b` `ea5278ca` `f041f8d1` `266e86d5` CI runner migration to the self-hosted Mac
- `4ac898a2` build both darwin targets on our own Mac
- `5e993486` give coverage a cap a hosted runner can finish under
- `f0847cc8` ignore the desktop build output
- `89507574` remove the stray cerr file from the repo root
- `85b1dfca` use str::repeat for the filler string
- `96ac4276` `f720517a` formatting

### 📊 Stats

- 79 commits since v0.3.81
- 162 files changed, +11,298 / -4,874 lines
- 6,813 tests (6,781 passed, 0 failed, 32 ignored)

## [0.3.81] - 2026-08-17

48 commits since v0.3.80. 157 files changed, +12,219 / -1,906 lines.

### ✨ Features

- `1f07010f` **Mermaid with native tables** (#1044): markdown+media route keeps tables native
- `ddd0cad9` **Mermaid as images** (#1044): render mermaid fences as images in rich Telegram messages
- `f5f4739d` **Early loop detection**: notice a repeating tool round before the provider rejects it
- `41ac9db2` **Subagent result reporting** (#1036): finished agents report back to the spawning session
- `f7696521` **Qwen effort tier default** (#1034): tiered family defaults to the recommended effort tier
- `5559c793` **OSC 8 clickable paths** (#1031): emit OSC 8 so paths and wrapped URLs are clickable (dropped again by `76c03118` in this same release, see Fixes)
- `9263422b` **External index paths** (#1051, #1055): memory search reads external index paths

### 🔧 Fixes

- `e23d112f` **Token-field rejection wording** (#1059): recognise every wording of a token-field rejection
- `60ca68ac` **Reap dropped tool children** (#1046): reap the child process when a tool future is dropped
- `8f43beff` **MemoryConfig test literals**: keep them valid after the external-paths merge
- `597919e6` **Profile claim first** (#1072): claim the profile before any startup work
- `6308707b` **Embedding health** (#1069, #1067): sweep for unembedded documents, show embedding health in doctor
- `9507ea24` **RSI failure split** (#1068): environmental bash failures split out of the tool-defect count
- `51d66a2b` **Stored-key marker ownership**: one owner, and heal keys already poisoned by it
- `99a177c3` **Onboarding wizard**: reopened wizard no longer writes STT/TTS back to disabled
- `65840ed1` **keys.toml key loss** (#1066): stop dropping four provider API keys at runtime
- `f8a6c07f` **Dotted commands** (#1074): sentence split no longer tears them in half
- `988fb636` **Wider command allowlist**: inspection claims become checkable
- `0d3a3895` **Fact-based self-heal** (#1073): detectors fire the self-heal, not just gate it
- `6801fcb6` **Reasoning-tag stripping**: stop eating message text
- `6b35bcba` **No double answers** (#1070): stop delivering the same answer twice in one turn
- `5cdc54d0` **Model picker order** (#1057): newest first
- `6f91fe97` **Telegram rate-limit cap** (#1064): inline waits capped at 30s
- `c952d867` **Embedding call hardening** (#1062): timeouts, a vector gate, non-blocking writes
- `4211a0a8` **RSI engine gate** (#1063): autonomous engine behind rsi_enabled, off for headless daemons
- `59602c14` **Rich blocks as typed AST** (#1058): decode rich message blocks as typed AST
- `83201e76` **Unauthorised chats** (#1043): stop recording chats that were never authorised
- `60232da5` **Owner-only group adds** (#1042): only the owner may add the bot to a group
- `8684d0e9` **Bot-added detection** (#1041): tell being added apart from another bot arriving
- `337dcd0a` **Hosted Qwen reachable** (#1040): outside Alibaba and vendor-prefixed model ids
- `9b526673` **Voice dialog keys** (#1039): stop hiding and overwriting stored keys
- `152d8ad4` **Session-routed recovery** (#1037): recovery reports route by session, not by whoever booted
- `ba65e9de` **Orphaned status files** (#1038): reconcile at startup
- `865b8c7f` **Family-gated thinking knobs** (#1034): each model gets the one it reads
- `c420dd84` **preserve_thinking** (#1033): reasoning carries across turns
- `76c03118` **OSC 8 dropped** (#1031): linkify corrupted the screen
- `9547c362` **Telegram flow footer + section cap** (#1052, #1053, #1054, #1056)

### 📖 Documentation

- `ea429e1b` telegram: ground the owner-gate claim in the fix that made it true (#975)
- `58863be9` follow the qmd drop and refresh counts after the merge
- `cd2dcf48` refresh test counts and document memory_search scope

### 🧹 Miscellaneous

- `bd09c413` move the last inline test blocks into src/tests (#1076)
- `82d48230` move three more inline test blocks into src/tests (#1076)
- `33c368e8` move the memory module tests into src/tests (#1076)
- `9eda6d1b` move the external-paths tests into src/tests (#1076)
- `b3fb852f` cargo fmt the external-paths merge (#1051)
- `df58616b` format test files left unformatted by 921cb9cf and b1d1994f
- `07980fc3` cargo fmt the memory-store refactor (#1032)
- `02f5ca7e` drop qmd dependency, own the memory store (#1032)

### 📊 Stats

- 48 commits since v0.3.80
- 157 files changed, +12,219 / -1,906 lines
- 6,688 tests (6,659 passed, 0 failed, 29 ignored)

## [0.3.80] - 2026-08-12

96 commits since v0.3.79. 191 files changed, +12,486 / -1,472 lines.

### ⚠️ Upgrade Notes

- **Brain-file templates changed**: the AGENTS.md, BOOT.md, and SOUL.md templates were updated in this release (#1003, #992). Seeding and template sync NEVER overwrite existing files, so nothing propagates automatically: diff your files against `src/docs/reference/templates/` and merge by hand if you want the new defaults.
- **Qwen 3.8 Max users**: this model can sit in long internal reasoning before acting. If turns stall in thinking loops, set `agent.thinking_loop_timeout_secs = 120` in config.toml (tune up toward the 600s default if you prefer more patience). The loop guard kills the stalled stream and retries with tool-call enforcement, which makes Qwen behave noticeably better. 0 disables the guard entirely.

### ✨ Features

- `98831de2` **Deployment isolation rule**: the preamble now says where a deployment goes — a VPS or a container, never installed into the OpenCrabs environment without explicit owner approval (#1026)

- `c6bdeaa8` **Browser inventory mode for browser_find** (#1022)
- `ac8113ac` **memory_search gets a scope**: the agent can search its own rules, daily logs, or both (#1020)
- `d07668c4` **Slack renders tables and headings in its own shape** (#1016)
- `15c4c881` **Proactive one-shot fallback chain setup suggestion** (#1008)
- `2d1d9b9c` **Template SOUL.md gets an operating posture, not just a voice** (#992)
- `e0f0db99` **Telegram records each configured group's name in its config section** (#984)
- `3fc4f6a6` **Per-call fallback provenance in streaming logs** (#969)
- `6fc05944` **cron_manage update action**: patch jobs in place (#966)
- `ca72b428` **Spoken stop honoured**, not just the bare word (channels, tui)
- `513d3f8d` **VBA macro source extraction** from macro-enabled workbooks (doc_parser)
- `7f9906d2` **Repo-sharing guidance**: prompt tells agents how to share a repo with other sessions
- `c4de2488` **Cross-turn announcement loop guard** + Luna fixture (#957)
- `26156b48` **Bash near-match loop guard** in the tool loop (#957)
- `a744daf3` **xlsm/xlsb/ods via calamine, legacy .doc via rwml** (doc_parser, #955)

### 🔧 Fixes

- `dd0cb602` **Loop-detector kill reaches the fallback chain**: raised as a provider-attributable error so a turn the announcement-loop detector ends can try another provider instead of surfacing (#1023)

- `a03da3fb` **Owner-gate user commands and skills on the channel catch-all arm** (#975)
- `ea0f33b6` **tool_search results are activated**: a found tool is callable (#1025)
- `bf0850a8` **Loop-detector kill no longer reported as a provider fault** (#1023)
- `bca2d93c` **Spent thinking-loop budget routed into the fallback chain** (#1021)
- `05d546a5` **Memory freshness churn stopped, embedding kept off the search path** (#1021)
- `8c0eabeb` **Brain files indexed into the brain collection, not memory** (#1018)
- `5463c3ef` **Three channel delivery failures that were dropped entirely are now logged** (#1019)
- `a33e2188` **Memory indexes on write**: memory_search stops being a boot-time snapshot (#1018)
- `1ab6ae91` **Provider response IDs persisted in logs for request correlation** (#1013)
- `7817a09b` **Slack folded narration matched on the character stream**, not paragraphs
- `252adfc5` **Slack final message no longer repeats step-group narration** (#1010, corrected reapply)
- `cc475248` **Plan receipt binding for commit claims in completions** (#1011)
- `566cd1f3` **Codex CLI failures route into retry and fallback** (#1004, #1005)
- `169ee0d9` **HTTP 400 failover guidance and chain-exhaustion ledger** (#1006, #1007)
- `782901c2` **Telegram no longer deletes a completion because the turn ran no tools** (#1009)
- `b7d23c71` **Memory chunker owns qmd's panics on multi-byte characters** (#1002)
- `9877fc95` **API backfill chunked too, placeholders swept on that path** (#1001)
- `6136cdb8` **Lexical hits narrowed to the matching chunk** (#1000)
- `d3f91e24` **Memory store resolved per profile instead of cached globally** (#999)
- `faa6f4fe` **Documents chunked before embedding, later chunks searchable** (#998)
- `1cd1b228` **Telegram wrap_p threaded through lists, quotes, headings in rich HTML** (#997)
- `6e9adf3f` **Brain recall folds Latin diacritics**, multilingual eval added
- `2a9c82f7` **MEMORY.md recall ranked with BM25**, not shared-word counts (#996)
- `58e83faf` **multi-agent skill registered as a built-in** (#990)
- `18742529` **HEARTBEAT.md seeded into new profiles** (#989)
- `465fb053` **read_file per-line clamp and output byte budget** (#986)
- `85eb7ca3` **read_file truncation warning carries the exact resume offset** (#988)
- `061e45c6` **read_file announces empty files instead of returning silence** (#987)
- `4bfcfd49` **list_chats prints each chat id once** (#985)
- `fd4006f8` **Fallback providers get the same nudge budget as the primary**
- `faaff5ce` **-p/--profile applied before logging resolves the home** (#983)
- `a5bb3fed` **RSI cycle provider resolved from user config, not registry order** (#977)
- `68eacd3b` **RSI cycle interval backs off on zero-improvement streaks** (#977)
- `32af2135` **Config example name corrected from a2a to gateway**
- `b8d6f08a` **RSI agent runs paused on convergence self-reports** (#977)
- `62ed0892` **RSI findings hashed by stable identity**, no description churn (#977)
- `a8b7197b` **Telegram nudges and fallbacks counted in place**, no stacked lines
- `52402fc6` **RSI cycles gated on actionable feedback deltas** (#977)
- `bbba22fe` **Session history sealed with a compaction marker each RSI cycle** (#977)
- `ab93f598` **xlsb/xlsm/ods support for channel ingestion** (file_extract, #959, #962)
- `f02cf14e` **Collapsed tables reflowed in the intermediate rich-report gate** (#980)
- `1d14137c` **Nudge escalation no longer poisons the context it hands on**
- `cdada3fe` **Empty answer nudged even when the model reasoned nothing**
- `5c71da30` **anyhow cause chain preserved in AgentError::Database** (#974)
- `e28fbc5c` **context_window honoured on the CLI providers**
- `1f9b9935` **provider_registry opt-in instead of on by default**
- `5ff573c1` **Turn duration kept after the turn settles**
- `6374b1d4` **Homebrew installs upgrade via Homebrew** (evolve)
- `c38cc66e` **Reworded announcement loops caught mid-turn** (#961)
- `fd0f8d72` **Near-match loop guard generalized to all tools** (#961)
- `5aa3eef3` **Truncated answer no longer delivered as if it were finished**

### 📖 Documentation

- `679db659` eval: record the memory-retrieval before/after, and what was not measured
- `eb5fea12` changelog: reconstruct the v0.3.79 release notes
- `2bf08f7d` templates: move memory-save triggers to the always-loaded file (#1003)
- `54a2ff99` eval: record what chunked embeddings changed, and what was not measured
- `93bde4df` brain: dedup_scan documents exact matching, not paraphrase (#993)
- `84d67bb5` brain: drop stale IDENTITY.md references, SOUL.md owns identity (#991)
- `33314a74` readme: document the seven config options that had no entry
- `dcb69445` readme: scope self_improvement_provider/model to the built-in RSI engine (#968)
- `72c073f0` readme: clarify provider enabled=false does not block by-name usage (#967)
- `c5acf86d` near-match loop guards for all tools, mid-turn announcement checks (#961)
- `9aff80b9` near-match loop guards in README and changelog (#957)
- `cd412efa` readme: document legacy .doc and xlsm/xlsb/ods parsing (#955)
- `eec57371` readme: document parse_document stack and out-of-scope formats

### 🧹 Miscellaneous

- `7d49a59c` ci(release): add workflow_dispatch so a release can be re-triggered from the API
- `6006e01d` fix(cron): Unix day-of-week 0 acceptance, reverted below before release (not shipped)
- `a78f1a26` revert: cron day-of-week 0 fix pulled before release
- `75e6228e` fix(slack): first narration fix, superseded by the reapply above (#1010)
- `dd381010` revert: undo the first slack narration fix before the corrected reapply (#1010)
- `2fc589e6` feat(agent): reasoning token budget, reverted below before release (not shipped)
- `b5caa4d4` revert: reasoning token budget feat pulled before release
- `6793510c` style: cargo fmt the response-id log line (#1013)
- `19bce549` style: cargo fmt the self-healing additions (#1004, #1006, #1007, #1008)
- `1b7a83ac` perf(brain): stop re-reading MEMORY.md every turn, make both paths agree (#995)
- `c368de88` style: rustfmt the restored channel-ingestion commit [skip ci]
- `9c290b5c` test(errors): drop unused anyhow::Context import
- `56015b2b` test: drop reporter and group names from loop-guard fixtures
- `37405faf` test(fixture): strip reporter handle from Luna fixture (#957)
- `161d7d9a` chore(release): retire the Homebrew tap, homebrew-core autobumps now (#958)
- `b2ffc621` test(doc_parser): regression tests for legacy .doc and spreadsheet routing (#955)

### 📊 Stats

- 96 commits since v0.3.79
- 191 files changed, +12,486 / -1,472 lines
- 6,399 tests (6,373 passed, 0 failed, 25 ignored)

> This section accumulates changes between releases.

## [0.3.79] - 2026-08-06

67 commits since v0.3.78. 118 files changed, +9,334 / -1,228 lines.

> Reconstructed retroactively on 2026-08-12: the original release run skipped the changelog entry, which left the v0.3.79 GitHub release notes empty.

### ✨ Features

- `86910a11` **Enforce provider quota and circuit breaker** (#952): the quota gate is checked before dispatch and before every fallback hop
- `b22d18fc` **Plan state persists across session boundaries** (#949)
- `a4d3ad46` **Plan tasks can run in isolated workers** (#947)
- `0f44a49a` **Sub-agent sessions included in compaction and session list** (#936)
- `d24d1be1` **Homebrew tap**: brew install adolfousier/tap/opencrabs, formula from a template, bottles on every release
- `87b2e8b1` **/restart and /exit slash commands in the TUI** (#954)
- `3ce89954` **/restart and /exit accepted from the owner on channels**: Telegram, Discord, Slack, WhatsApp
- `970277a7` **Daemon restarts instead of exiting on unexpected crashes** (#953)
- `f0d96bc6` **Tool call IDs exposed in tracing**

### 🔧 Fixes

- `31de8079` **/exit stays clean even when /restart fires first** (#953)
- `e2303e04` **Telegram plan card rendered from the same plan state the text sees** (#935)
- `087f4b63` **Plan tasks stay coherent across session boundaries** (#951)
- `518793c5` **Isolated tasks read the same plan file the parent uses** (#950)
- `e016b914` **Isolated plan tasks no longer edit the parent's plan file** (#948)
- `75961585` **Compaction summaries stay with the session that owns them** (#946)
- `e9ab32c0` **Plan isolation defaults ON, plan_dir actually resolves** (#947)
- `8382415b` **Task isolation opt-in per start, default off** (#947)
- `42136955` **Plan path resolved once and handed to the worker** (#947)
- `59959614` **Spinner stays alive after the first token streams** (#945)
- `5196801e` **Long markdown tables reflowed in a rich Telegram follow-up** (#943)
- `72f46875` **Eval recall measured against the exact needle phrasing** (#940)
- `0e17e643` **Chunking sweep deterministic across runs** (#942)
- `45e2e26f` **Chunk sweep report's numbers made honest** (#942)
- `7f705748` **Placeholder chunking leftovers cleaned on startup** (#941)
- `39b30301` **Honest retrieval numbers reported for the chunker** (#940)
- `110c5b89` **No more "1 tool calls ran" in plan card status** (#934)
- `5847e526` **Plan card's "Task: title" line allowed to truncate** (#934)
- `b13a218d` **Plan card's task and status kept on one line** (#934)
- `99c59b84` **Eval retrieval measured on the real store** (#939)
- `9d2d1e83` **Eval harness stops eating the corpus it scores** (#939)
- `6a9d1f91` **Metadata stripped before chunking** (#939)
- `1f5f850a` **recall@3 measured like production searches** (#938)
- `3464d244` **Corpus chunked once, then searched** (#938)
- `77a5d42a` **Migration 33 healed before the first turn of the session** (#897)
- `7a4829a7` **Migration 33 healed before the session's first turn** (#897)
- `6747a57b` **Migration 33 healed at startup and before the first turn** (#937)
- `e0687210` **No duplicate Telegram plan cards on re-edit** (#935)
- `2a32c65a` **CI publishes the core candidate formula with tap sync**
- `a370b036` **Systemd restarts the daemon when it exits, even cleanly** (#953)
- `2767291c` **Onboarding saves each step's config immediately** (#926)
- `1a9d3b8c` **Ralph verification runs in the session's directory** (#921)
- `2c17d385` **TUI stops double-rendering on the same frame** (#927)
- `c8c00619` **TUI stops redrawing the frame when nothing changed** (#928)
- `07e93e8d` **No duplicate frame draws on keypress and resize** (#929)
- `9f25e11f` **/usage shows cost without a provider call** (#930)
- `028f5055` **Unused usage tool dropped from the /usage command** (#931)
- `95a2b1f6` **Compaction warns before the context wall, not after it** (#909)
- `141d076d` **Compaction pre-warns as a visible nudge** (#910)
- `d891b724` **secrets.toml written atomically, corrupt configs repaired** (#911)
- `238a32c9` **Eval retrieval measured on the real store, one process** (#912)
- `6e00048b` **Chunker's gains made measurable** (#914)
- `62a1c73e` **Chunked sweep on the full corpus, real recall numbers** (#917)
- `b582e772` **Memory eval stops corrupting the agent's own database** (#919)
- `291b5842` **macOS arm64 cross-compile fix restored**
- `e589d827` **Option 3 cross-compile setup corrected**

### 📖 Documentation

- `33c1f26a` document the plan-isolation config, correct stale claims (#947)
- `78129114` readme: document the provider-config escape hatches and rtk
- `15438c76` readme: correct stale /approve and provider-config claims

### 🧹 Miscellaneous

- `ca071ef0` style: cargo fmt Homebrew publish job
- `39c0493e` ci: bump Homebrew tap with formula + SHA256SUMS
- `916520f0` ci: install rust in Homebrew publish job
- `78681a83` ci: use GitHub runners for Homebrew tap sync
- `003e9826` ci: skip homebrew tap sync on failure
- `c4d0923d` refactor(config): move plan_dir under [plan] and fix config examples (#947)
- `c9424735` test: isolate the config-repair tests from the real secrets.toml (#911)
- `2b75d5e2` test(plan): cover isolation defaults and task-level overrides (#947)
- `e20c9978` test: verify /usage skips provider calls and works offline (#931)

### 📊 Stats

- 67 commits since v0.3.78
- 118 files changed, +9,334 / -1,228 lines
- Tests at tag time not recorded (the release run skipped this entry). Last pre-tag gauntlet (2026-08-05): 6,030 tests (6,001 passed, 0 failed, 29 ignored incl. doctests)

## [0.3.78] - 2026-08-01

68 commits since v0.3.77. 122 files changed, +9960 / -566 lines.

### ✨ Features

- `e48f36ca` **Mission Control analytics filter + card grid**: global D/W/M analytics filter, responsive 3-across card grid, and model status icons (#900)
- `7fe07b0d` **Analytics panel tabs + scroll**: phantom/model/D-W-M tabs with scroll in the analytics panel (#900)
- `45c6f384` **Analytics queries + report sections**: add analytics queries and report sections (#899)
- `dade356f` **Analytics emitters**: wire analytics event emitters across the agent (#901)
- `4846c774` **Analytics events DB layer**: add the analytics events database layer (#898)
- `b588324b` **OpenAI TTS onboarding**: OpenAI TTS voice selector and API key field (#874, #875)
- `197f1151` **Thinking-loop timeout**: thinking-loop timeout with phantom enforcement retry
- `b4ecaa24` **Epistemic Orient gate**: gate plan start behind the epistemic Orient phase (#886)
- `199cc545` **Criteria-aware verification gate**: Ralph criteria-aware verification gate (#870)
- `10270544` **Belief tracking wiring**: wire belief tracking into brain-write and plan paths
- `5ebe5009` **Epistemic engine**: belief tracking with confidence levels
- `d06b6836` **TOML brain verification**: TOML-driven post-write brain file verification
- `50cf22c2` **Ralph verification gate + iteration cap**: mechanical verification gate with iteration cap
- `5687ebb0` **TOML bash blocklist**: TOML-driven bash blocklist, runtime-configurable safety gates

### 🔧 Fixes

- `3ef84597` **session_context concurrency**: pin the concurrency fix and clean up temp files (#906)
- `e4823d12` **Reasoning-block redaction**: remove reasoning blocks on delivery, not just their markers (#903)
- `6325d01c` **Phantom shipped-and-tracked claim**: catch the shipped-and-tracked completion claim (#903)
- `a56364cd` **Analytics model recording**: record the active model on the primary provider path (#905)
- `c8f9e55e` **Deliverable report visibility**: keep a deliverable report visible when its turn folds (#904)
- `d2840333` **Background running indicator**: clear the running indicator before delivering (#896)
- `3e8bb4b5` **slash_command resolution**: resolve skills and the /onboard:<topic> form (#889)
- `b538730f` **config_manager nested paths**: resolve nested paths and child names to their section (#889)
- `63c78a34` **generate_image extension**: always give a generated image an extension (#889)
- `83852ff8` **Telegram rich delivery**: unify rich delivery on markdown, drop the dead blocks path (#871)
- `65734067` **Phantom colloquial claims**: catch colloquial delivery claims, drop a false positive (#894)
- `be87b8c2` **Telegram follow-up attribution**: name the member who tapped a follow-up (#895)
- `d89ba482` **Phantom media-delivery claims**: catch media-delivery claims, and the oven idiom (#891)
- `c1cfb4d6` **Onboarding model filter hint**: always render the model filter hint above the list (#873)
- `2874d69d` **RSI digest totals**: surface raw failure totals vs surfaced opportunities in the digest (#888)
- `006f9571` **clean_auto_title UTF-8 panic**: fix the UTF-8 panic on multi-byte characters (#885)
- `733c8f5c` **RSI brain_verify gate**: route self_improve through the brain_verify Orient gate (#881)
- `e1f7aa07` **Sanitize quoted secrets**: redact quoted secrets, and catch the colon-token shape (#879)
- `f121faa5` **Telegram callback routing**: route callbacks to the originating session, not the chat-bound session (#878)
- `d4f5bce7` **Channel chunk boundaries**: prefer chunk boundaries that leave no markup open (#876)
- `eb60c998` **Onboarding exact model match**: an exactly-typed model id beats a substring match (#873)
- `8b7f8bfe` **session_context atomic write**: atomic write to eliminate trailing-characters corruption (#853)
- `9509cdae` **Voice OGG/Opus conversion**: convert OGG/Opus to WAV before sending to voicebox STT (#866)
- `a3b29a7d` **Telegram rich verdict logging**: log the rich verdict and both send outcomes (#860)
- `17775521` **Truncation continuation join**: join a truncation continuation onto its partial (#859)
- `19556176` **RSI rule size cap**: cap rule size and allow a narrow consolidation path (#857, #858)
- `7247c7ca` **brain_verify contradiction scope**: scope contradiction matching per-entry, not whole-file (#855)
- `10d39848` **Safety TOML reload**: reload the safety TOMLs on change, and pin the behaviour
- `9eab9ed7` **Telegram open-group registration**: gate open-group registration on the persisted open flag (#848)
- `edd4123b` **Telegram background streaming**: one streaming turn per session for background results (#845)
- `0065a8a2` **Telegram follow-up recording**: record a tapped follow-up on its own block (#844)
- `2682b06c` **RSI capability-gap disposition**: keep the disposition of capability-gap opportunities (#842)
- `5562d6b5` **Mission-control journal blocks**: stop malformed journal blocks corrupting neighbours (#841)
- `861b9ade` **Telegram admin logging**: stop logging absent admins as menu failures (#839)
- `ef088c5f` **Telegram cowork members**: record cowork members on first message (#840)

### 📖 Documentation

- `5cc8b643` refresh Mission Control, TTS and redaction sections in the README (#907)
- `7d1f034c` document the Ralph loop, and correct an inert config key (#907)
- `284e7d6d` document the epistemic engine and define OODA/BDI (#907)
- `9227d25d` document the safety gates (#907)
- `644a16cb` move the anti-fabrication rules into the preamble
- `e89c88bf` clean build artifacts before a release build (preamble)
- `9d07382f` clean build artifacts before a release build
- `458ea84f` scope the CODE.md release-build exception to OpenCrabs itself
- `d5ccef59` scope /rebuild to OpenCrabs' own source, on explicit request only

### 🧹 Miscellaneous

- `15cf8ded` local cross-compilation setup for unreleased dev binaries
- `181b5cb1` analytics panel render + D/W/M switching tests (#900)
- `0a13c127` add analytics query + report tests (#899)
- `ae5aef93` add emitter integration tests (#901)
- `55b7ba4b` import criteria types and dedup helper field (#870)
- `a463e8cc` cover the criteria-aware policy matrix (#870)
- `56bf6a71` use resume_session for followup tap and callback routing (#869)
- `91b28929` collapse nested epistemic blocks into let-chains
- `1786f29e` add dev profile build optimizations
- `7ba99e3a` add a runner seam to the verification gate and pin it

### 📊 Stats

- 68 commits since v0.3.77
- 122 files changed, +9960 / -566 lines
- 5925 tests (5896 passed, 0 failed, 29 ignored)

## [0.3.77] - 2026-07-27

4 commits since v0.3.76. 4 files changed, +68 / -26 lines.

### 🔧 Fixes

- `51d3f6af` **Plan-card lock deadlock**: stop the plan-card lock deadlocking every chat (#831)
- `31544ba4` **Plan prose duplication**: stop plan prose duplicating across card and flow block (#824)
- `e1d24cf8` **Telegram send HTML conversion**: apply HTML conversion to all telegram_send actions (#835)
- `b35c2710` **TUI bg-task position**: move bg-task feedback from left to right on input border (#836)

### 📊 Stats

- 4 commits since v0.3.76
- 4 files changed, +68 / -26 lines
- 5649 tests (5624 passed, 0 failed, 25 ignored)

## [0.3.76] - 2026-07-27

53 commits since v0.3.75. 96 files changed, +6937 / -278 lines.

### ✨ Features

- `7dc9f376` **Telegram /usage cache breakdown**: per-model cache breakdown in /usage (#815)
- `1eda8e1e` **RSI /evolve proposal**: surface an available release as an /evolve proposal (#821)
- `d1521ce2` **RSI config-example tracking**: track the config examples so pricing fixes reach users (#819)
- `78ddf975` **Brain template-sync merge**: additive key-level TOML merge for template sync (#819)
- `785b6bed` **TUI finished checklist**: keep the finished checklist on screen after a plan completes (#810)
- `0dc08c2e` **Models natural naming**: accept a model named the way people say it (#801)
- `8683d7bd` **Brain proactive memory**: surface relevant memory without waiting to be asked (#799)
- `70f6a14d` **Brain query loading**: let load_brain_file return only the sections that match (#800)
- `c852daae` **/execute provider routing**: route /execute onto its own provider and model (#793)
- `d37f1308` **/plan provider routing**: route /plan onto its own provider and model (#792)
- `34558b5d` **Telegram /new gating**: gate /new to the owner and scope the menu to its audience
- `d82629c7` **Brain clarify-before-build**: instruct the agent to clarify before implementing

### 🔧 Fixes

- `59b40556` **Memory UTC timestamps**: store timestamps in UTC with the zone stated (#826)
- `3632f741` **Phantom file-delivery detection**: detect a claimed file delivery when nothing sent one (#825)
- `58a28b81` **Telegram /start closed-group**: /start recognises an existing member of a closed group (#776)
- `9ad09c13` **RSI finding hash**: hash the finding, not the events that illustrate it (#804)
- `3ecc8c2a` **Telegram substance check**: stop defining substance as "contains a pipe table" (#824)
- `c3e238ae` **Phantom null-effect calls**: null-effect tool calls no longer buy immunity (#825)
- `a1cf081c` **Brain no-announce rule**: restore the rule against announcing a call instead of making one
- `700fcc49` **RSI content-gated sync**: gate template sync on content, not on the version number (#820)
- `c6a817dd` **Telegram plan-card serialisation**: serialise plan-card writes so concurrent refreshes stop duplicating (#822)
- `439ed829` **Usage Opus 5 pricing**: price Claude Opus 5 on /usage (#817)
- `1ff869de` **Usage qwen3.8 pricing**: price qwen3.8-max-preview on /usage (#816)
- `8c93bc71` **Telegram plan-card flood**: stop the plan card flooding itself into duplicates (#814)
- `3b99c835` **Telegram plan-card persistence**: persist plan-card tracking so a restart stops duplicating it (#809 defect 1)
- `dc072018` **Discord typing indicator**: add the sustained typing indicator it never had (#812)
- `f26ac0ee` **Channels typing persistence**: keep the typing indicator alive through background tasks (#812)
- `5d1a4e9f` **Plan completed-card render**: let the completed card render its final state (#809 defect 2)
- `3a390bed` **RSI capability-gap routing**: route capability gaps to a proposal instead of discarding them (#811)
- `b314020f` **RSI dead-cycle guard**: stop cycles that can never execute a tool (#805)
- `f0bfe3b4` **Usage cost attribution**: attribute cost to the provider that served it (#807)
- `a9898159` **TUI duplicate-submit guard**: drop a re-submitted message the running turn is already answering (#798)
- `83704dd9` **Brain unrun-command quote**: quote the command a turn never ran back at it (#797)
- `04ebc6e6` **Brain phantom-belief correction**: correct the belief behind a phantom turn, not the format (#796)
- `424e4f98` **Brain reasoning-cannot-tools**: state that reasoning cannot execute tools (#795)
- `822d2092` **TUI command labelling**: label commands by what they run, not the directory (#790, #791)
- `14bb031f` **Phantom command verification**: verify claimed commands against what the turn actually ran
- `a56f6bed` **Repetition fenced-code exclusion**: exclude fenced code from streaming loop detection
- `65255c9e` **Telegram follow-up tools turn**: run a real tools turn when a follow-up is tapped
- `8b29df55` **TUI harness-status handling**: stop counting harness status as work, and stop burying it
- `a57c1241` **Phantom evidence check**: check quoted evidence against what the tools returned
- `9eea2a86` **Phantom investigation claim**: count claimed investigation as a completion claim
- `198c6999` **Phantom both-ends check**: examine both ends of a turn, not just the lead
- `a2c285f6` **TUI steps-and-tools display**: show steps AND tool calls, drop the either/or fallback
- `7f645896` **Phantom self-update claim**: count self-update claims as completion claims
- `6fa56e77` **Brain preamble iteration fix**: stop the preamble telling the agent not to iterate, and tighten it
- `b3c09301` **TUI fold row count**: count only the rows the fold hides in the step header

### 📖 Documentation

- `0b44e266` docs(readme): document the spaced /models form (#803)
- `09ec1783` docs(readme): document plan/execute provider and model routing (#794)

### 🧹 Miscellaneous

- `b49419b3` chore(provider): log the reasoning_content decision (#830 step 1)
- `cead5014` test(rsi): assert what the sync tracks and how each entry routes (#823)
- `7df4d617` revert(plan): stop resurrecting completed plans everywhere (#813, #809)
- `7ecadacf` refactor(brain): cut the domain sections to reference size

### 📊 Stats

- 53 commits since v0.3.75
- 96 files changed, +6937 / -278 lines
- 552 tests (549 passed, 0 failed, 3 ignored)

## [0.3.75] - 2026-07-25

62 commits since v0.3.74. 113 files changed, +6757 / -1188 lines.

### ✨ Features

- `9636584d` **Background-task indicator on the input border**: move the background-task indicator onto the input border
- `3a6c0d94` **Brain hints on approval-branch errors**: inject brain-file hints into approval-branch tool errors too (#767)
- `f1d7f1ff` **Brain-file hints on tool misses**: surface relevant brain notes when a tool call misses or errors (#767)
- `7b47e034` **Wider TOOLS.md load trigger**: widen the TOOLS.md proactive load trigger so routing, skill, and cron questions load it
- `14cbbb31` **Inline-keyboard dedup approval**: approve brain dedup proposals from a Telegram inline keyboard (#765)
- `507c539e` **Weekly dedup safety net**: a weekly cron scan catches cross-file brain duplication (#765)
- `ecf979ca` **Post-write dedup scan**: fire a cross-file dedup scan after brain writes (#765)
- `53063409` **/dedup command**: on-demand cross-file brain dedup scan (#765)
- `385c8185` **Reasoning summary room**: give the live reasoning summary room to finish a thought
- `bf10972e` **Detached command status**: show what a detached background command is doing
- `f9baad56` **Probe rich-message scoping**: ask the Telegram server whether rich messages can be scoped
- `95b934bc` **Scope owner-command replies**: scope owner-gated command replies to the invoker
- `1fd7d0fd` **Fold working-out into the header**: fold a turn's working-out into its header
- `f6164ccf` **Per-turn header**: a per-turn header summarising the work a turn did
- `fe6dca92` **Turn-boundary inference**: infer turn boundaries so a turn can be grouped
- `c2d9b776` **claude-cli model discovery**: discover the model list from the CLI instead of hardcoding
- `2244878a` **CLI background-task resume**: background-task resume in the interactive agent
- `6e5708ad` **WhatsApp background-task resume**: activate background-task resume on WhatsApp
- `b4cad46c` **Slack background-task resume**: activate background-task resume on Slack
- `5b4ad2ce` **Discord background-task resume**: activate background-task resume on Discord
- `0025b09f` **Shared bg_resume helper**: a shared helper powering background-task resume across channels

### 🔧 Fixes

- `ace0e502` **Phantom: issue-tracker completion**: count issue-tracker actions as completion claims
- `842c00d7` **Fold intermediate text live**: fold intermediate text while the turn is still running
- `b27fb39c` **approval_policy on every surface**: let approval_policy reach the tool gate on every surface
- `622f51b1` **Drop crabrace**: drop the crabrace dependency, keep the two GETs it existed for
- `723021c1` **Clear quick-xml DoS advisories**: clear the quick-xml DoS advisories from the dependency tree
- `fc8b90bf` **Hide bg-task scaffolding**: stop a background task's system scaffolding reaching the chat
- `ee4f0d40` **Keep header after expand**: keep a turn's header after it is expanded
- `daecf786` **No expand scroll-jump**: stop an expand from scrolling the view up by its own size
- `78c930d8` **Collapse stale narration**: collapse narration the model kept thinking past
- `67b919ef` **Split reloaded turn iterations**: split a reloaded turn back into its iterations
- `761cef24` **Report restart-killed tasks**: report background tasks a restart killed instead of waiting forever
- `b585b77e` **Readable folded turn**: make a folded turn readable and keep it in place when toggled
- `9dea8585` **Reconcile /models picker**: reconcile the /models picker against the live inventory
- `a4c509b5` **Tool summary stays visible**: keep the tool-call summary at full visibility when a turn is folded
- `0206e58b` **qwen blank reasoning chain**: stop sending a blank reasoning chain that made qwen leak and repeat
- `9b97c398` **Fold every turn by default**: fold every turn by default, running or settled
- `f6d412f7` **Fold on settle**: fold a turn as soon as it settles, not only older turns
- `4411d605` **Model picker newest-first**: order the model picker newest-first instead of by merge order
- `144ae43a` **Consistent claude-cli rows**: format every claude-cli model row consistently in the picker
- `a0ee110f` **Surface Opus 5**: surface Opus 5 in the model picker via a third parallel list and state discovery
- `6b518b8a` **Phantom verdict on recovery paths**: turn-end phantom verdict catches narration delivered via recovery paths
- `baaa5614` **No image hallucination on give-up**: don't deliver an image hallucination when self-heal gives up
- `054229f4` **Multilingual hallucination detection**: make image-generation hallucination detection multilingual
- `895e00b8` **Recover poisoned session**: auto-recover from a repetitive-tool-call poisoned session
- `307ed939` **Patient mid-stream retry**: make mid-stream retry more patient (5 attempts, exponential backoff)
- `55baa517` **Retry flaky 404**: retry a flaky-provider 404 instead of treating it as permanent
- `a7569327` **Catch image hallucination**: catch image-generation hallucination (media claim, no marker, 0 tools)
- `a7c0557c` **Bound the self-heal loop**: bound the phantom self-heal loop instead of re-nudging forever
- `c4795ffd` **Strip discarded self-heal narration**: strip discarded phantom self-heal narration from the live buffer
- `632d1686` **Drop ctx from spinner**: drop ctx from the live spinner, it already shows under the input
- `63d99437` **Short live-thinking excerpt**: live thinking shows a short excerpt, not a scrolling wall
- `ccd14660` **Label live token counter**: label the live token counter as a turn total and show ctx budget
- `ede0763b` **Auto-complete delivery task**: auto-complete the trailing delivery task at turn settle
- `eec518c7` **Truthful tab-close outcome**: validate tab close with a truthful outcome
- `83b6016d` **Larger browser viewport**: larger viewport and maximized headed window
- `2b7d8340` **Dedup scanner ignores code and tables**: dedup scanner ignores fenced code and table rows

### 📖 Documentation

- `e8e43aaf` say approval_policy governs headless runs too

### 🧹 Miscellaneous

- `de3f8058` style(tui): make inline code the footer's slate blue, not a brown
- `bf097170` style(tui): quiet inline code so it stops out-shouting the prose
- `122ac2c9` refactor(config): rename the provider-registry surface off the vendor name
- `95be6345` refactor(tui): extract the tool-group scan from render_chat behind a tested seam

### 📊 Stats

- 62 commits since v0.3.74
- 113 files changed, +6757 / -1188 lines
- 5399 tests (5399 passed, 0 failed, 25 ignored)

## [0.3.74] - 2026-07-23

40 commits since v0.3.73. 70 files changed, +2948 / -212 lines.

### ✨ Features

- `d9511856` **Background-task resume plumbing**: symmetric message-enqueue plumbing so a long task can hand its session back when it finishes (#722)
- `5a347a25` **Background-task manager**: background-task manager plus bash auto-promotion of known-long commands (#722)
- `5aea1b3f` **Background tasks on the TUI**: a long command runs in the background and resumes the TUI session on completion (#722)
- `0dfbd4c0` **Background-task preamble**: tell the agent long tasks run in the background and resume the session when done (#722)
- `30262ae7` **Background tasks on Telegram**: activate background-task resume on Telegram (#722)
- `bcfd23a3` **3-state reasoning expand**: click/ctrl+o cycles reasoning collapsed to capped to full, capped so it never floods the view (#727, #726)
- `1204c783` **/cowork opens the group**: /cowork sets the target group open=true and persists it (#718)
- `37d0adce` **/cowork admin deep link**: the /cowork deep link requests admin rights so the bot joins already-promoted (#709)
- `604eb2e4` **Group onboarding greeting**: greet the group with an onboarding nudge when the bot joins (#707)
- `41952b09` **follow_up_question degrades gracefully**: returns the question as plain text on non-interactive surfaces (#716)
- `f0748daf` **Config edits via config_manager only**: the agent edits config/keys only through config_manager, never raw file edits (#715)
- `f886bba7` **Verify motion-graphics scenes**: verify every scene before declaring motion-graphics work done (#711)
- `e3e89d76` **Version in the UI**: show the running version on the TUI header and in channel /help + /usage (#696)

### 🔧 Fixes

- `645e10c5` **Anchor block on expand**: pin the clicked block's header to its screen row so expanding/collapsing grows it in place instead of jumping the viewport (#728)
- `2ff69c75` **Outbound media dedup**: collapse an identical file+caption re-uploaded to the same chat within a short window so the agent can't send it twice (#721)
- `9101ff54` **No lingering cancelled query**: drop a cancelled-before-reply query and its empty assistant placeholder as a pair so it doesn't linger in context and duplicate on resend (#730)
- `d04e60ff` **No perpetual resume**: a completed session no longer resumes on every restart — resume turns are not re-tracked as pending requests (#729)
- `e694cc00` **Click vs drag on mouse-up**: decide click-vs-drag on mouse-up so a click-drag selects text instead of expanding (#726)
- `a594ecbe` **qwen reasoning_content safety**: apply the reasoning_content missing-safety to qwen, not just Moonshot (#725)
- `d96f991d` **Telegram follow-up taps**: render suggest_followups last and fix taps doing nothing (#723, #724)
- `3e4da206` **Vision tilde paths**: expand ~ in provider_vision and analyze_video paths (#720)
- `edceeece` **Secure member auto-register**: gate member auto-registration to open=true groups, secure by default (#717)
- `4819b99a` **Register joining members**: auto-register any member who joins a bot group (#710)
- `86b8a5a7` **Per-group /start registration**: group /start registers the member per-group instead of the stale config.toml lecture (#708)
- `8ee8de82` **Group onboarding UX**: refine the group onboarding UX (#707, #708)
- `fc9c9911` **Re-validate before write**: re-validate config before fs::write in the config_manager write paths (#714)
- `e5300d23` **Guard config-breaking writes**: deny edit_file/write_file writes that would break config.toml/keys.toml (#713)
- `1387b97d` **Validate keys before snapshot**: validate keys.toml before saving the last-good snapshot (#712)
- `b3f7d4d8` **Accurate follow-up description**: make suggest_followups description channel-accurate so the model invokes it instead of writing plain-text suggestions (#706)
- `66b23a27` **Don't persist involuntary remap**: don't persist an involuntary provider/model remap over a session's saved pair (#705)
- `5350050b` **Restore session provider**: restore a session's saved provider before its turn so it never runs on the global default (#704)
- `68f52039` **Per-session working directory**: isolate the working directory per session so it can't leak across concurrent sessions (#703)
- `8ba3b107` **Catch bare "Done."**: catch bare completion with zero tools on build tasks (#702)
- `cef2a4f0` **Double-escape returns query**: double-escape before any reply returns the query to input and removes it (#698)
- `cb60e0c3` **Merge /usage duplicates**: strip -@botname suffix from model names so /usage merges duplicates (#697)

### 📖 Documentation

- `4e957075` document the click/ctrl+o 3-state expand cycle and drag-to-copy (#727, #726)
- `1b504692` document the per-group open field and the /cowork opens-group model (#719)
- `defb769b` update the /cowork flow for admin-on-add and /start registration (#707, #708, #709)
- `5968fae5` document the debug_logs config flag (#701)
- `ed61adf4` document the force_default provider flag (#699)

### 📊 Stats

- 40 commits since v0.3.73
- 70 files changed, +2948 / -212 lines
- 5244 tests (5244 passed, 0 failed, 29 ignored)

## [0.3.73] - 2026-07-22

15 commits since v0.3.72. 33 files changed, +1427 / -706 lines.

### ✨ Features

- `699acb0d` **Unified parallel web search**: web_search, exa_search, and brave_search now fan out in parallel under a single web_search tool

### 🔧 Fixes

- `a415a560` **Stable stream stop_reason**: default stop_reason to EndTurn on the stream terminal chunk so text turns stop instead of retrying forever (#694)
- `b333be54` **printpdf security bump**: bump printpdf to 0.11.3 to resolve lopdf RUSTSEC-2026-0187
- `c6691bf4` **Tool-aware reasoning nudge**: empty-reasoning nudge no longer suppresses tool calls (#692)
- `b98b1cd1` **Disable cache split**: reverted the #658 2-part array cache split that broke tool-calling (#693)
- `406deecc` **Preserve reasoning on nudge**: preserve_thinking models stop re-reasoning on the empty-reasoning nudge (#692)
- `80774491` **reasoning_effort for custom providers**: send reasoning_effort for non-Kimi custom providers (qwen/modelstudio) (#691)
- `45545e84` **Re-expand collapsed tables**: inline tables re-expanded so they render on Telegram (#690)
- `0318a4c0` **Table-rendering preamble**: spell out table-rendering mechanics so Telegram renders them (#689)
- `e0ff7234` **Cleaner auto-title input**: strip channel preamble from the title prompt input (#688)
- `dd7eab97` **Skip pre-execution misses**: don't record tool_executions for pre-execution misses (#687)
- `6707a5b1` **Decode peer-bot messages**: decode rich_message content from peer bots into readable text (#686)
- `e90ea38e` **Full group history capture**: persist every group message to history, even from non-allowlisted senders and bots (#685)
- `cdb79b10` **DDG captcha detection**: detect DuckDuckGo captcha via HTTP 202 + structural form check
- `0b07b97b` **Correct group speaker**: stop the agent addressing a group-history sender as the current speaker (#682)

### 📊 Stats

- 15 commits since v0.3.72
- 33 files changed, +1427 / -706 lines
- 5200 tests (5200 passed, 0 failed, 29 ignored)

## [0.3.72] - 2026-07-21

3 commits since v0.3.71. 14 files changed, +426 / -138 lines.

### 🔧 Fixes

- `e98962b0` **Telegram table rendering**: restore rich table rendering by skipping only the doomed native-blocks attempt instead of the whole rich branch, so tables no longer render as bare markup (#679)
- `e4553003` **Self-heal phantom detection**: catch zero-tool turns that claim high-stakes side-effects (ship/push/tag/release), so a fabricated completion report with no tool calls is flagged instead of shipped (#680)
- `a29cdd8f` **Preamble audit**: profile-resolve the channel_attachments path via opencrabs_home(), restore the full date+time in Runtime Info (uncached suffix), and lock the runtime-suffix cache-split boundary (#681)

### 📊 Stats

- 3 commits since v0.3.71
- 14 files changed, +426 / -138 lines
- 5162 tests (5162 passed, 0 failed, 29 ignored)

## [0.3.71] - 2026-07-19

73 commits since v0.3.70. 124 files changed, +8424 / -2108 lines.

### ✨ Features

- `1f5a272e` **Tmux watchdog**: pane-only kill, 30s warning, auto-reattach
- `d7d1bb0a` **Eval self-awareness scenarios**: tool-set + environment self-awareness, lazy and non-lazy (#672)
- `3692cf8c` **Built-in /github skill**: full gh CLI control for issues, PRs, code reviews, repo management
- `d862bb8c` **Config-driven debug_logs**: toggle with hot-reload on top of --debug (#678)
- `93d1e856` **Plan-gate three-state**: GateDecision with bash going to approval in post-init Editing (#649)
- `c53e2d1e` **Plan-gate read-only filter**: registry filter for subagents spawned in Editing (#649)
- `72eec599` **Configure-not-reimplement**: preamble directive to configure compiled-but-unconfigured capabilities (#635)
- `f66409e9` **Secret redaction scope**: scope to global/group/dm + /redact command (#677)
- `3c45e41a` **Telegram plan card re-stick**: re-stick plan card and fold prose into it (#621)
- `9c7668cb` **Telegram plan card goal**: render goal section on the plan card (#621)
- `d3b2ce78` **MCP hints overrides**: add overrides for sends and browser mutators
- `e52c7c1d` **MCP-style ToolHints**: introduce risk model for tool classification
- `041c49cf` **Usage price fallback**: fall back to last-known price for unknown models (#655)

### 🔧 Fixes

- `2f1acf08` **Browser screenshot loop**: hard auto-break (#664)
- `96ee9309` **Bash grep-no-match**: stop counting as failure (#663)
- `a42a4df3` **Headless brain build**: build_system_brain now surfaces compiled features + paths (#671)
- `d6c38230` **Github skill templates**: populate empty templates directory with bug/PR templates
- `ab3fe5d4` **Kimi reasoning leak**: stop coding endpoint leaking inline reasoning as chat messages (#616)
- `2ac2cc7a` **Onboarding custom fetch**: send the real saved key on custom model fetch (#656)
- `bc303675` **Onboarding model ID**: let users type a model ID not in the fetched list (#676)
- `b76eb1b4` **Phantom Indonesian detection**: expand with full regex coverage
- `9ec202af` **Channel attachments**: know they persist; don't fake a blocker (#659)
- `f459a05c` **MiniMax reasoning leak**: send reasoning_split=true to prevent reasoning leak
- `03b6c3d2` **Qwen reasoning**: round-trip as reasoning_content, not content (#654)
- `e4bca047` **Redact command**: advertise /redact in the Telegram command menu and command reference
- `d6eccd0c` **Github skill templates**: remove orphaned templates/ dir
- `2d91beaf` **Telegram md_to_html**: harden for **double** bold and fenced code (#650)
- `3f206ffc` **Telegram bot defer**: defer when a reply tags a different bot (#648)
- `438a3c6d` **Telegram rapid resend**: stop dropping in-flight turns (#652)
- `cedce669` **Telegram card prose**: render as per-heading expandables (#621)
- `fe5f8d27` **Telegram tables**: retire native rich-blocks (#651)
- `807d5e32` **CORE_TOOLS drift**: correct name drift dropping core tools in lazy mode (#669)
- `d67cfca0` **Tools inventory**: adds team tools + brave_search, drops stale provider_vision (#670)
- `9ebfccd9` **TUI /help dialog**: document channel commands and /stop

### 📖 Documentation

- `4c4d3620` Refresh test counts + fully regenerate TESTING.md table (#634)
- `5e23ecbd` Refresh outdated project structure in README and CONTRIBUTING
- `73448d5e` Check current context (issue/PR + comments, git, code) before changing
- `f060ede9` Amend umbrella ADR for #649 exploration gate
- `fe402865` Explain provider fallback + /models syntax so the agent stops guessing (#674, #675)
- `2425284e` Document the context/memory evaluation harness (#633)
- `b2f89835` Document /redact, scoped redaction, and debug_logs config in README

### 🧹 Miscellaneous

- `231243a6` Add retry logic and timeout guards to cron list queries (#665)
- `a4f9519e` chore: stop gitignoring examples/ (#641)
- `827f7e3e` ci: drop the Windows build from the PR gate (#627)
- `645e5291` eval: capability self-awareness dimension (configure-not-reimplement) (#636)
- `7cf4e41e` eval: self-awareness measures tool-awareness, drop the completion probe (#644)
- `8f02653a` eval: register the eval feature in compiled_features (#618)
- `c1850e35` eval: drop the false-positive whisper keyword from the self-awareness probe (#638)
- `cda8303c` eval: give the self-awareness producer its tools; judge the action, not prose (#643)
- `6d82a26b` eval(live-L0,L1): eval_providers config + resolver + majority-vote judge panel (#629, #630)
- `340b8943` eval(live-L2,L3): live artifact producers + on-demand runner with variance & baseline drift (#631, #632)
- `7596835e` eval(live): report median + failure-rate and save outlier artifacts (#642)
- `b2d8d2f4` eval(live): actually track the on-demand runner + K=5 variance (#637, #640)
- `cc46a340` eval(live): per-probe + artifact-head diagnostics for run 0 (#643)
- `f7fd002b` eval(live): on-demand runner under the real system brain (#637)
- `98d53674` eval(phase0): context-manifest trace hook (#620)
- `b5df93be` eval(phase0): offline fixture-driven replay provider (#619)
- `fe2684dc` eval(phase1): compaction-fidelity harness + BinEval scorer (#621, #622)
- `7a3e29ab` eval(phase2-4): recall metrics, regression baseline, RSI before/after (#623, #624, #625)
- `a8f18655` perf(brain): date-granular runtime timestamp so the prompt cache can hit (#657)
- `8d726fda` perf(cache): cache the stable brain independently of runtime info (#658)
- `77142794` plan_gate: simplify to single is_destructive check per state
- `7d4c597d` plan-gate: unify pre-init and post-init destructive treatment
- `a936f69a` plan-gate: allow full agent-tool family during Editing
- `8a3d9c1c` preamble: require stating the concrete configuration action, not just that the built-in exists (#639)
- `d1978800` refactor(plan-gate): remove spawn_agent from EDITING_DENIED_NAMES (#649)
- `3cd46541` refactor(telegram): move TelegramState out of mod.rs into state.rs
- `2a50bcd3` refactor(tools): drive plan gate off MCP hints, delete both name lists
- `ba538b68` refactor(tools): extract shared is_destructive/is_read_only classifier (#649)
- `122b7ac6` style: keep push_home_anchor doc with its fn; drop redundant ref in live_eval (#671, #672)
- `726dd684` style(tools): sort classify module declaration into alphabetical order
- `a873b2a6` test: fix stale tests left behind by #652 and drop dead import
- `9263b6fc` test(cache): guard lazy-tools content stays in cached prefix, runtime in suffix (#662)

### 📊 Stats

- 73 commits since v0.3.70
- 124 files changed, +8424 / -2108 lines
- 5155 tests (5155 passed, 0 failed, 29 ignored)

## [0.3.70] - 2026-07-19

18 commits since v0.3.69. 57 files changed, +1310 / -128 lines.

### ✨ Features

- `822d7ef8` **Kimi reasoning-effort config**: per-model reasoning-effort field, validated against the model's allowed set
- `6295f8e8` **Kimi plan-tier context window**: API plan vs Coding plan drives the context window automatically
- `fb89940e` **Skills review_gate**: frontmatter declaration for high-stakes skills that require user approval before side effects
- `17cb7dae` **Plan discard autonomy gate**: agent-initiated plan discard now requires plan autonomy, user-initiated discard always allowed
- `3eb6ab7d` **Plan yolo scope**: design-track refusal scoped to /plan slash origin, not every yolo session

### 🔧 Fixes

- `90add3ca` **MiniMax live model fetch**: fetch models from /v1/models endpoint instead of hardcoded list
- `3d805011` **Kimi context_window persist**: derived context_window saved to config on plan pick
- `584f14ca` **Moonshot config_for()**: session slot resolves the moonshot provider correctly
- `6d640783` **Moonshot keys.toml merge**: provider key merged from keys.toml into the active config
- `9d85ca5f` **Compaction context window**: honor the provider's configured window without a manual swap
- `cd77d138` **Phantom detector language-agnostic**: accents and non-ASCII no longer disable strict detection
- `457f646a` **RSI proposals prune**: already-handled entries removed from pending proposal files
- `4ef3af52` **Mission-control Flakiest panel**: windowed so fixed tools age out of the view
- `d1abefc7` **Phantom leading-imminence**: catch work announcements like '... Now downloading X'

### 📖 Documentation

- `604f4226` Document skill review_gate and the yolo design-gate caveat
- `6c34134f` Amend yolo Editing rule, document review gates

### 🧹 Miscellaneous

- `268140c9` perf(lazy-tools): tool_search activates on use, not all 8 matches (#604)
- `50004329` perf(lazy-tools): LRU-bound the per-session active tool set (#603)

### 📊 Stats

- 18 commits since v0.3.69
- 57 files changed, +1310 / -128 lines
- 5017 tests (5012 passed, 0 failed, 29 ignored)

## [0.3.69] - 2026-07-18

19 commits since v0.3.68. 74 files changed, +2646 / -88 lines.

### ✨ Features

- `a38a8eb7` **Built-in Moonshot AI provider**: Moonshot (Kimi) as a first-class provider with API plan (api.moonshot.ai) and Coding plan (api.kimi.com) endpoints, alias-resolved from kimi/moonshotai
- `abeff7cf` **Moonshot onboarding wizard**: API plan / Coding plan picker before the API key field, with live model fetch per plan
- `15a57720` **Command Code CLI provider**: add Command Code CLI (cmd) subprocess provider
- `1617a5aa` **MiniMax-M3**: wire the current flagship into the config example, README, crate docs, and usage normalizer; default_model bumped to MiniMax-M3
- `d47cbf4e` **Plan discard/show**: agent can discard and show the plan on the user's request
- `775a2f60` **Click-to-expand**: left-click a tool-call or reasoning block to expand just that block, complementing Ctrl+O
- `de89f73b` **Follow-up suggestions tool**: suggest_followups tool plus event plumbing
- `0857d066` **Follow-up suggestions in the TUI**: rendering and keys, fill-not-submit
- `1b91ce47` **Telegram follow-up suggestions**: tap-to-send optional follow-ups
- `b3e849b8` **Discord follow-up suggestions**: tap-to-send optional follow-ups
- `ae64c1a7` **Slack follow-up suggestions**: tap-to-send optional follow-ups
- `661506a7` **WhatsApp follow-up suggestions**: numbered tap-to-send optional follow-ups

### 🔧 Fixes

- `5e416d0c` **Kimi K3 pricing**: price Kimi K3 correctly on /usage instead of billing $0.00
- `36228a36` **Phantom plan detection**: catch narrated plans hidden behind a structured preamble
- `9132a620` **Telegram react marker**: recover a leaked react marker on reaction turns
- `745ad200` **Command Code CLI build**: make the provider build and actually work

### 📖 Documentation

- `e4d540ef` Moonshot config example section and README provider docs

### 🧹 Miscellaneous

- `95f8fd53` refactor(provider): rename Moonshot display name "Moonshot Kimi" to "Moonshot AI"
- `ac17513b` refactor(provider): rename onboarding display name "Anthropic Claude" to "Anthropic"

### 📊 Stats

- 19 commits since v0.3.68
- 74 files changed, +2646 / -88 lines
- 5018 tests (4989 passed, 0 failed, 29 ignored)

## [0.3.68] - 2026-07-14

21 commits since v0.3.67. 42 files changed, +1898 / -488 lines.

### ✨ Features

- `6a26b66d` **Agent self-approval**: agent self-approval when the user grants autonomy
- `376000df` **Persistent plan card**: single persistent plan card instead of per-turn checklists
- `0fe21498` **/plan <query>**: /plan <query> enters plan mode with the query as the intent

### 🔧 Fixes

- `3cc1d31c` **Rich report intermediate**: deliver a rich report intermediate instead of folding it
- `6c94a2e6` **Hide unfilled fields**: hide unfilled plan-scaffold fields in the flow block
- `9afd1e0f` **Retry on 429**: retry rich flow edit on 429 instead of splitting to HTML
- `e5464868` **Stream plan execution**: stream plan Approve-button execution to Telegram
- `edf0fb84` **No duplicate presentation**: stop duplicating plan presentation and approval acknowledgement
- `402f22c6` **Run tool loop on Approve**: run the tool loop on plan Approve button, so it actually starts
- `53d529fa` **Approve on tasks**: approve a checklist-mode plan on its tasks, not the design .md
- `e8848378` **Guide plan commands**: guide plan/session commands instead of erroring
- `2e99c020` **Successful invocation**: a completed response is a successful invocation
- `b129ab74` **Always show Approve/Discard**: always show plan Approve/Discard at settle, don't gate on readiness
- `10ab4ab6` **Widen hash**: widen hash to 4 chars and strip the real read prefix
- `36be030c` **Inject plan reminder**: inject plan reminder in tool loop and gate Approve on readiness
- `5f45cf5f` **Keyboard at turn end**: show plan Approve/Discard keyboard only at turn end
- `0cc1a226` **Resolve channel commands**: resolve channel commands despite auto-injected media markers
- `e0c4aacf` **Require approval**: require approval for checklist mode, drop description param from init
- `986f8b17` **Remove preview**: remove user message preview from flow status

### 🧹 Miscellaneous

- `ba8f3b98` Back pre-init with a marker file, not a stub plan JSON
- `937f8b6f` Simplify prompts for LLM clarity (#568)

### 📊 Stats

- 21 commits since v0.3.67
- 42 files changed, +1898 / -488 lines
- 4964 tests (4964 passed, 0 failed, 29 ignored)


## [0.3.67] - 2026-07-14

52 commits since v0.3.66. 104 files changed, +11090 / -3413 lines.

### ✨ Features

- `1df3c044` **Multilanguage prompt analyzer**: shared PromptAnalyzer with soft-nudge on TUI and Telegram, 6 language packs (EN/ES/FR/ID/PT/RU) (#518, #538)
- `7b7ea4d6` **Plan-state copy lock**: F3 plan-state copy lock and /show-plan demotion (ADR 0005)
- `ad3e04ae` **Goal chrome**: F3 goal chrome with turn-local retention (ADR 0005)
- `1ab25268` **Per-heading plan prose**: F3 per-heading plan prose in flow chrome (ADR 0005)
- `84b2ed5e` **Archive at turn settle**: archive completed plan at turn settle, not mid-complete (ADR 0005)
- `9ac48392` **Always-visible title**: F2 always-visible title and full checklist chrome (ADR 0005)
- `356e21ba` **Uncollapsed flow shell**: F1 uncollapsed flow shell with merged footer (ADR 0005)
- `797a6d6f` **Web search retry**: rotate User-Agent and retry DuckDuckGo 403s (#525)
- `6c9b5084` **Telegram rate-limit retry**: retry telegram_send on 429 rate limits (#524)
- `14a1303d` **Config reload feedback**: surface silent config.toml hot-reload recovery to the user (#534)
- `6ab209ce` **Channel context header**: include chat_id/thread_id in the [Channel: ...] prompt header (#533)
- `96648fb1` **Provider-aware narration cap**: make folded-narration cap provider-aware (#532)
- `9eab3285` **Approve/Discard flow**: D3 Telegram Approve/Discard flow chrome and TUI plan overlay (#521)
- `f25278c1` **Dual-track prompts**: D2 dual-track prompts, state-branched compaction recovery (#521)
- `93414269` **Design-track commands**: D1 commands, Approve validator, seed turn, design-track soft-nudge (#521)
- `756bdd05` **Plan tool contract**: C3 plan tool contract, design/checklist tracks, goal wiring (#520)
- `f457593d` **Session markdown mirror**: C2 session markdown mirror and plan-file-changed reloads (#520)
- `99bc0f8d` **Tool-loop write gate**: C1b tool-loop write/bash gate and Active .md freeze (#520)
- `39641597` **Lifecycle engine core**: C1a lifecycle engine core, Editing/Active status model with durable pre-init flag (#520)
- `491c4d24` **Merged flow message**: merge pre-flow into the flow message, flow chrome sections (#519)

### 🔧 Fixes

- `297bcb64` **task_type enum**: document the full task_type enum and log the Other fallback
- `18971bea` **Non-empty description**: require a non-empty description on every task entry point
- `675e4cf2` **Checklist validation**: enforce required root title and description on checklist import
- `ed7476aa` **React-only marker**: keep react-only marker so claude-cli turn ends cleanly
- `3baf00af` **/models provider**: /models command shows session provider instead of global
- `d8bd3de7` **React-only turn**: surface a long no-tool react-only turn instead of silently dropping it (#546)
- `a1f62fe7` **Goal cleanup**: clear the session goal on plan discard
- `19b11029` **Checklist defaults**: assign order and default complexity on checklist import
- `7e1021bc` **Header-only cleanup**: clean up the header-only flow block on a react-only turn (#544)
- `b3c3eab7` **Design prose display**: surface design prose in /show-plan and point chrome at it
- `b606e7d6` **Plan keyboard re-attach**: re-attach plan keyboard after flow restick
- `cd178879` **Bot menu commands**: register plan mode commands in bot menu (#537)
- `ce3c008b` **Shell escaping**: context-aware shell escaping so double-quoted params are safe (#523)
- `a327e1d9` **@bot mention strip**: strip @bot only as a command suffix, preserve standalone mentions (#528)
- `024e0cc2` **Reaction markers**: accept keyword-less <<EMOJI>> reaction markers
- `35c5891f` **Working header**: show the 'Working on: <query>' header for claude-cli turns (#527)
- `6926ba2b` **Reaction terminators**: accept </react> and bare > as reaction-marker terminators
- `08993632` **Session model/provider**: resolve Runtime Info model/provider per session at prompt injection
- `cd0c4782` **Attachment handling**: preserve attachment names, store directed files durably, organize channel_attachments by platform (#513)
- `09ee09f0` **Double gear dedup**: dedup the double gear when the live header status is the bare-tool fallback

### 📖 Documentation

- `e295b307` Seed plan-mode umbrella SSOT, cluster ADRs and specs
- `11b708f3` Correct stale status and approved_at semantics in the live spec
- `150a6572` Fix self-contradiction on when the analyzer sets pre_init_editing
- `1465fa39` Drop phantom execution_history and reflection from the spec
- `82c8e8e3` Describe async project/profile-aware session dir resolution

### 🧹 Miscellaneous

- `12b6386d` Drop deprecated task_manager from the tool catalog
- `81496afd` Drop orphaned plans/plan_tasks tables
- `54611a75` rustfmt the /execute BotCommand registration
- `31ff3990` Remove unused opencli skill pointing at unbundled binary (#536)
- `87180a31` Retire orphaned Plan/PlanTask row models, CHANGELOG for the lifecycle engine (#520)
- `89ead77f` Resolve session dir async, project/profile-aware

### 📊 Stats

- 52 commits since v0.3.66
- 104 files changed, +11090 / -3413 lines
- 4938 tests (4938 passed, 0 failed, 29 ignored)

### 🗑️ Removed

- **`opencli` built-in skill** (#536): dropped the skill whose `SKILL.md` advertised 25+ `opencli-rs` dynamic tools (`hn_top`, `twitter_trending`, `google_news`, and others) that require a separate `opencli-rs` binary and a daemon on port 19825. Nothing shipped or provisioned that binary, so on a stock build the skill loaded a doc describing tools that were never registered. Usage history confirms zero successful executions of any opencli-rs tool. Removed the `BUILTIN_SKILLS` registration, the template directory, and the README and BRAIN_CONSTITUTION references.

### ✨ Features

- **Plan mode UX** (#521): the user-visible product on top of the lifecycle engine. `/plan`, `/show-plan`, `/execute` and `/discard` land on TUI and Telegram; the Approve validator scans the session `.md` (Context labels filled, at least one numbered step) and a passing `/execute` sets Active, stamps `approved_at`, and dispatches one visible seed turn that converts the numbered steps into the checklist and starts task 1. Approve and `/execute` are refused while a turn runs (never queued); `/discard` cancels the turn first. The shared soft-nudge swaps the plan hint to the design track and plan keywords durably enter Plan mode. Prompts now teach SESSION PLAN vs CHECKLIST with the locked track-selection table, every Editing turn pins the plan-mode rules, compaction recovery branches by plan state and the summary carries a harness-written PLAN STATE block, and the deprecated `task_manager` tool left the preamble and catalog. On Telegram, Approve/Discard inline buttons (owner-only in groups, `plan:` callback prefix) plus Editing/Building-checklist chrome ride the latest flow message; on the TUI, `/show-plan` opens a scrollable plan overlay with approve/discard keys, and the checklist strip shows Building checklist… or seed-error while the seed window is open.
- **Plan lifecycle engine** (#520): plans live as NoPlan / Editing / Active. The seven-value status enum collapsed to Editing and Active with a legacy map on load (Completed archives silently under `agents/session/archive/`, Cancelled deletes, old draft checklists stay executable), all plan IO goes through one shared store (`src/utils/plan_files.rs`), and a durable `pre_init_editing` sidecar survives restarts. A tool-loop gate enforces the Editing write policy: pre-init denies project writes but allows exploratory bash, post-init Editing denies all bash and allows writes only to the session plan `.md` (whose body mirrors into the JSON `description` on save), and Active freezes the design `.md`. The plan tool gained `mode` (design or checklist), `add_tasks` (with `add_task` kept as an alias), Active-only `start`/`complete` with deterministic Editing refusals, import rules, archive-on-last-complete, and acceptance criteria that set the session goal while a task runs; auto-approve on first `start` is gone, and the dormant SQLite plan path (`PlanService`, `PlanRepository`, row models) is retired.
- **Telegram flow chrome merge** (#519): one live flow message per turn. The pre-flow status bubble is gone; thinking and Working-on previews ride the flow header from the first activity tick, all three renderers support header-only output (empty entries still emit the header), and no-tool long turns open and settle a header-only Processing log. The ctx footer moved off final answers onto the settled flow message, plan title, checklist progress (from the live session plan JSON), and the active goal one-liner render as always-visible flow sections via one shared section builder, and crash-recovery resume now shares the same open-early, live-duration, and settled-header path as live turns.
- **Shared PromptAnalyzer soft-nudge on TUI and Telegram** (#518): the keyword analyzer moved from `src/tui/prompt_analyzer.rs` to the shared `src/utils/prompt_analyzer.rs` and now runs on both surfaces. Plan hints teach the live tool contract (`init`, `add_task`, `start`) and explicitly reject the dead `create`/`finalize` operations. Hints are LLM-only (appended to the agent input, never to Telegram `display_text` or TUI chat bubbles), and slash commands plus skill/user-command expansions are never analyzed on either surface.

## [0.3.66] - 2026-07-10

53 commits since v0.3.65. 84 files changed, +7943 / -3582 lines.

### ✨ Features

- `824d955c` **Direct model switching everywhere**: `/models <provider/model>` switches directly on every channel (#467)
- `8c9daf66` **Apply-to-scope selector**: choose session or global scope for direct model switches (#468)
- `549e1f6c` **Headless model switching**: `opencrabs session set-model` for non-interactive use (#465)
- `91c0fce0` **force_default on reload**: pushes the default provider/model pair to all sessions (#466)
- `f9ed6748` **Config drift warnings**: startup warnings when a config value silently drifts (#477)
- `c9d315e6` **Native rich blocks on Telegram**: final responses render as native rich blocks, fixing fence mangling (#476)
- `03a37312` **Flow header wall-clock duration**: finished/failed/timeout states carry the total turn time (#486)
- `cb080298` **Bash comments as flow status**: line-start comments in a bash command surface as live flow status (#488)
- `e8d5e701` **Full status preview**: the status preview uses the whole last human-readable line, no truncation (#487)
- `0c419e31` **Per-group open mode**: opt-in `open = true` so a trusted group serves all members (#495)
- `c32a24b9` **Skill candidates from tool sequences**: RSI detects recurring tool sequences as skill candidates (#504)
- `333e540f` **Slash commands from repeated asks**: RSI proposes slash commands from repeated user requests (#504)
- `816da9a4` **RSI staleness indicator**: mission-control staleness indicator plus a provider-creation fallback (#469)

### 🔧 Fixes

- `f830b50c` **Mid-turn slash command runs**: a slash command landing mid-turn injects as its own directive, not a foldable follow-up
- `addc9d40` **Status-first live header**: the live flow header leads with the status message, bold, count and duration italic (#509)
- `0449c27a` **Atomic reaction turn claim**: the reaction handler claims the turn via `try_begin_turn` (#508)
- `8520724b` **Atomic turn claim**: follow-ups can no longer fork a parallel turn (#501)
- `8447e8cf` **Text answers a pending question**: a mid-turn text message resolves a blocked `follow_up_question` (#500)
- `4753f659` **Smarter loop-break**: nudge before breaking identical tool-call loops, catch interleaved repeats (#507)
- `1d52cfcc` **Retry transient model errors**: temporary model-unavailability 4xx now classified as retryable (#505)
- `97b4a266` **No re-proposing settled skills**: RSI stops re-proposing already applied or rejected skills and commands (#502)
- `aa06a08d` **Unified separator**: single `•` bullet everywhere, the `·` middle-dot dropped (#499)
- `14cedfd4` **Live-only preview**: the activity preview no longer sticks to the settled Finished header (#498)
- `00bb3eb3` **/start in the menu**: onboarding is discoverable from the Telegram command menu (#497)
- `5c37fee1` **Headless tools execute**: `run`/`agent --auto-approve` route through the real tool loop (#492)
- `223761cf` **Mid-turn narration folds**: narration joins the flow block instead of a standalone bubble (#490)
- `22f2a780` **Resume narration folds**: the resume streaming placeholder is gated on an open flow block (#490)
- `d266f72b` **Folded narration capped**: a length cap keeps the flow block compact (#489)
- `de8bd62e` **Status from raw command**: bash status comments read from the raw command, not the decorated hint (#488)
- `86434a5b` **Follow-ups keep the block**: mid-turn follow-ups no longer shred the flow block (#475)
- `1e343780` **Reaction sees the answer**: the folded answer is reclaimed before the reaction decision (#478)
- `4ed8ea28` **Fences disqualify rich**: code fences disqualify the native rich markdown path (#476)
- `6655e161` **Fence disqualification reverted**: tables render rich again (#476 revert)
- `f66806ed` **Collapsed summary shows progress**: latest-activity preview in the collapsed rich flow summary
- `0d0d90b1` **Phantom completion hardening**: closed anchor evasions and verb gaps behind live fake completions (#463, #464)
- `4b35f880` **Rust 1.97 clippy clean**: cleared the new stable clippy lints

### 📖 Documentation

- `d016c06b` fix the stale `detach_flow_for_followup` comment to match #475 behavior (#496)
- `f483b268` drop the stale `working_directory` config key, note per-session cwd (#479)
- `dfde946f` document the #461 model-switching family in the README
- `7c842b45` add direct file links to the README documentation section
- `0db045d2` add Context7 config and documentation references
- `858f5761` add the morning-recap cron template script

### 🧹 Miscellaneous

- `1498a6d6` refactor(telegram): move the unified delivery path to delivery.rs (#471 phase 4)
- `0552e728` refactor(telegram): resume rides the shared deliver_final_response (#471 phase 3)
- `4a511c4e` refactor(telegram): extract deliver_final_response from handle_message (#471 phase 2)
- `b5e619d8` refactor(telegram): move crash-recovery resume to resume.rs (#471 phase 1)
- `3f1ef23d` refactor(telegram): move keyboard builders to keyboards.rs (#471 phase 1)
- `392c5ada` refactor(telegram): move send helpers to intermediates.rs (#471 phase 1)
- `6c68ac86` refactor(telegram): move incoming-media helpers to media.rs (#471 phase 1)
- `2e030adf` refactor(telegram): move markdown/HTML transforms to markdown.rs (#471 phase 1)
- `fa6ffd24` refactor(telegram): move flow-block machinery to flow.rs (#471 phase 1)
- `3d5c7378` refactor(telegram): one shared post-loop display drain for both handlers (#470)

### 📊 Stats

- 53 commits since v0.3.65
- 84 files changed, +7943 / -3582 lines
- Tests/clippy not run this pass (skipped per request)

## [0.3.65] - 2026-07-08

27 commits since v0.3.64. 51 files changed, +2013 / -279 lines.

### ✨ Features

- `833eae42` **Slack Block Kit delivery**: rich completion formatting via Block Kit (#455)
- `934fb06b` **Slack ctx footer as grey context block**: small grey block showing context (#457)
- `f9ac0cda` **Telegram flow block re-sticks**: open flow block re-sticks to chat bottom when buried (#451)
- `2e7516d6` **Telegram channel_search message_type filter**: add message_type filter and attachments operation (#445)
- `4f44533a` **Telegram group attachments persist**: persist all group attachments to disk and channel_messages (#445)
- `5e56be0d` **PDF landscape/page-size**: add landscape and custom page-size support to PDF backend (#438)
- `c89b8e59` **DOCX orientation/page-size**: add orientation and page-size support to DOCX backend (#440)
- `a594b3fe` **PPTX slide size**: add slide size/aspect ratio support to PPTX backend (#441)
- `6ab11945` **Flat tool inventory in prompt**: system prompt lists all tools so models stop claiming they're absent (#448, #449)

### 🔧 Fixes

- `51e9dd8a` **TUI session switch syncs working directory**: agent working directory always synced on session switch (#460)
- `0aa07577` **Slack ctx footer edits in-place**: ctx footer edits into the completion message, never posts below (#459)
- `f17fb83e` **Agent persists closing iteration**: persist the closing iteration even when the phantom skip fired (#458)
- `c69a4451` **Slack ctx footer on intermediate turns**: ctx footer lands on intermediate-as-answer turns too (#456)
- `3aab35f4` **Health check noise reduction**: demote keyless provider warnings to debug, filter /status display
- `a1c69e35` **Cron one scheduler per profile**: one scheduler per profile via lock, stop duplicate job execution (#453)
- `cb14d8ae` **Telegram_send inherits session origin**: telegram_send inherits session origin for chat_id and thread_id (#450)
- `fd20443c` **Telegram mention-only mode**: other bots can't trigger us by tagging in mention-only mode (#447)
- `daf7c9ce` **Telegram flow-header timer**: coarse flow-header timer so Desktop expansion survives (#452)
- `6406ccf0` **Tests .recent() call sites**: update all .recent() call sites for new message_type parameter
- `02f7db86` **Session load error**: session load error never forks a new session (#442)
- `9cdf2249` **Telegram reaction completion**: never let a reaction swallow the completion of a work turn (#439)
- `e2ae4b12` **DB token_count overflow**: token_count i32 overflow causes negative display for high-usage sessions (#437)
- `780da835` **Telegram flow log dedup**: dedup sub-header in rich HTML details flow log (#436)
- `636d2626` **Cron next_run_after anchor**: use now as anchor for next_run_after to prevent test-trigger catch-up loop (#432)
- `e3cde0d2` **Preamble owner-only gate**: owner-only gate for /evolve and /rebuild with accurate descriptions (#431)
- `a873767a` **Preamble owner-only gate**: add owner-only gate for /rebuild, /evolve, /compact and clarify what each does (#431)

### 🧹 Miscellaneous

- `f3fe105c` style: rustfmt reflow leftovers in slack handler and channel search test

### 📊 Stats

- 27 commits since v0.3.64
- 51 files changed, +2013 / -279 lines
- 4716 tests (4716 passed, 0 failed, 29 ignored)

## [0.3.64] - 2026-07-08

11 commits since v0.3.63. 33 files changed, +1179 / -388 lines.

### ✨ Features

- `82064aca` **Rich messages on by default**: Telegram rich_messages defaults to true (#425)
- `c225238f` **Onboard rich-text toggle**: checkbox in the Telegram channel dialog (#418)

### 🔧 Fixes

- `a132bed5` **Vision candidate roll-through**: per-provider endpoints, Gemini last (#430)
- `12a2eebc` **DSML-corrupted closers**: tolerate mangled closing tags in element-style leaks (#419)
- `cc01797e` **Vision gate relaxed**: vision_model alone is sufficient, enabled no longer required (#401)
- `af337979` **Flow-block collapse restored**: rich HTML details path for native collapse (#421, #423)
- `b1459add` **Rich details paragraph wrap**: flow entries wrapped in block-level elements (#424)
- `7d9b4206` **PDF image decoders enabled**: printpdf images feature turned on (#426)
- `77583eed` **Status bubble shows the task**: previews request text, not sender name (#427)

### 📖 Documentation

- `caf9a36d` rich_messages defaults to true since v0.3.64 (#428)
- `ff05d88f` vision needs vision_model + key, never enabled (#401)

### 📊 Stats

- 11 commits since v0.3.63
- 33 files changed, +1179 / -388 lines
- 4694 tests (4694 passed, 0 failed, 29 ignored)


## [0.3.63] - 2026-07-06

35 commits since v0.3.62. 53 files changed, +3082 / -750 lines.

### ✨ Features

- `2336e4f2` **/usage breakdown by provider and model**: per-provider and per-model cost breakdowns with period filters (day/week/month/all)
- `6c7a80d0` **image.vision.provider override**: bypasses the enabled gate for vision-only providers
- `354e482c` **Telegram flow logs via rich API**: 32K-character processing-log blocks with HTML fallback for short messages
- `b77fdc23` **doc_gen image blocks**: embed local PNG/JPEG inline in PDF and DOCX reports
- `6cc83797` **Discord interactive components**: select menus, modal forms, component TTL, role access, forum threads
- `d1faf9df` **Discord media gallery**: batch generated files into one multi-attachment message
- `e086900d` **Discord collapsible grouped tool calls**: Expand/Collapse toggle on grouped blocks
- `9bd7fda8` **Discord reaction turns and react-back**: emoji reactions trigger agent turns, Slack/Telegram parity
- `dfd90ed4` **Concurrent auto-approved tool batches**: parallel execution of independent tool calls
- `d4da1bbb` **PPTX style layer**: brand templates, accent titles
- `1206761b` **DOCX style layer**: accent headings, page furniture, shaded tables
- `b2b48645` **XLSX style layer**: header fill, zebra rows, freeze panes
- `b3320c85` **default_provider and default_model config**: new [agent] config keys for session fallback

### 🔧 Fixes

- `0a0ddf3d` **Telegram mid-turn guard**: prevents double-processing on concurrent messages
- `39dfe2e1` **Provider tool-call leak attribution**: fixes parameter name leaks in attributed providers
- `6c1982cb` **Channel attachments before final answer**: send media attachments before the text response
- `0b95b62b` **Telegram collapsed block previews**: correct preview rendering in collapsed state
- `695b8141` **Telegram flow block freezes**: prevent freezing during long tool chains
- `13415b9e` **Telegram leftover status bubble**: delete status after flow completes
- `d3c77bf4` **Provider element-style tool-call leaks**: parse and recover malformed tool-call elements
- `1f3fcb64` **Discord unread channel_id**: drop stale channel references
- `400f5a3d` **Telegram stale quip pool**: delete leftover quips after session reset
- `9c9432ed` **Telegram status-draft loop**: stop infinite edit loop on status messages
- `c5a5d6c7` **Telegram strip_html_tags**: generic HTML stripping for all channel contexts
- `0a1236e2` **Telegram RetryAfter handling**: respect Telegram rate-limit backoff
- `ddfde517` **Telegram split_message char count**: correct character counting for message splits

### 📖 Documentation

- `42d279aa` clarify default_provider/default_model fallback-only behavior
- `20b53933` document security deny-by-default access model
- `e8ec8a05` document doc_gen image blocks inline in PDF and DOCX
- `7e75f7f2` document telegram flow logs via rich API (32K)
- `68f3ab38` document image.vision.provider config key
- `03e43ead` document /usage breakdown by provider and model
- `fa15ee84` document discord feature parity with Slack (interactive components, media gallery, grouped tools, reactions)
- `dcab09ed` clarify atomic issues in contributing guide

### 🧹 Miscellaneous

- `f3ff299c` chore(logs): stop debug spam from flow-log channel

### 📊 Stats

- 35 commits since v0.3.62
- 53 files changed, +3082 / -750 lines
- 4680 tests (4680 passed, 0 failed, 29 ignored)

## [0.3.62] - 2026-07-06

49 commits since v0.3.61. 64 files changed, +7500 / -404 lines.

### ✨ Features

- `537f5c9f` **Collapsible Slack tool groups**: Expand/Collapse toggle on grouped tool-call blocks
- `13c950ce` **Slack reaction turns and react-back**: emoji reactions trigger agent turns, Telegram parity (#372)
- `d324f427` **Slack grouped tool-call messages**: tool calls collapse into one edited-in-place message (#371)
- `dfd90ed4` **Concurrent auto-approved tool batches**: parallel execution of independent tool calls (#361)
- `d2c8a749` **Document tools in lazy-tools discovery**: generate_document variants surfaced in tool_search (#368)
- `d4da1bbb` **PPTX style layer**: brand templates, accent titles (#366)
- `1206761b` **DOCX style layer**: accent headings, page furniture, shaded tables (#365)
- `b2b48645` **XLSX style layer**: header fill, zebra rows, freeze panes, autofilter (#364)
- `272d60dc` **DejaVu Sans bundled for PDF**: real Unicode text rendering (#363)
- `a06a71a7` **PDF page-header logos**: brand logos rendered on every styled PDF page (#362)
- `d937690c` **PDF style layer**: brand colors, page furniture, zebra rows (#362)
- `8c5dee93` **PDF content-sized columns**: auto-width columns, header separator, row rules
- `7ab3cb7c` **PPTX fallback backend**: generate_document PPTX via fallback when native backend unavailable (#357)
- `7f4c64a9` **Native PDF backend**: generate_document PDF generation (#357)
- `a96ee480` **Native DOCX backend**: generate_document Word document generation (#357)
- `f57cd0b3` **generate_document tool**: new tool with native XLSX backend for spreadsheet generation (#357)
- `d1ff6fdb` **Native Telegram rich content decoding**: raw JSON stays as safety net (#359)
- `a42cef1c` **Raw-aware Telegram update intake**: forwards of undecodable content reach the agent (#354)
- `b3320c85` **default_provider and default_model config**: new [agent] config keys (#314)
- `5cdf4b61` **Self-extract media frames**: preamble guidance for vision-absent models (#308)

### 🔧 Fixes

- `0844954d` **Slack image and TTS external uploads**: migrated to the external upload flow (#370 follow-up)
- `3b406f6c` **TUI inactive pane rendering**: live tool rounds now show on inactive panes (#369)
- `1e845ed3` **Slack send_file external upload**: migrated to the external upload flow (#370)
- `06ae1620` **Synthetic prompt compactness**: system tags persist instead of scaffolding
- `e1a35543` **PDF mojibake and table overlap**: follow-up fix for document generation (#357)
- `e8a02397` **Telegram mid-turn message queuing**: queue instead of cancelling in-flight tools
- `7b1e11f0` **Telegram standalone status removal**: removed entirely (#360 follow-up)
- `cb8deb41` **Telegram payload truncation limit**: increased to 4096
- `beb77eab` **Telegram single progress surface**: status folds into block header (#360)
- `ca10019e` **Telegram edit-failure resilience**: never delete the processing-log block (#356)
- `6794b215` **Telegram forward preservation**: never drop forwarded messages, tag with provenance (#354)
- `188b34c0` **Telegram reaction mapping**: map to allowed set, never go silent (#353)
- `7b89fe4d` **Telegram folded-finals dedup**: exact-match at any length (#316)
- `1c46ec7b` **Telegram status parse_mode**: add HTML to sends/edits (#312)
- `5433f704` **Telegram initial status HTML**: parse HTML in initial status messages
- `b826d97b` **Lazy tools CORE_TOOLS**: add analyze_image/analyze_video
- `fc5f575b` **Telegram markdown-to-HTML**: apply conversion for formatted output (#315)
- `e900903f` **Telegram bold status**: visual prominence (#312, #313)
- `2758f7e0` **Telegram folded-block dedup**: match by prefix, not exact (#311)
- `ac6397b0` **Reaction-triggered turns**: default to react-only, not text (#309)

### 📖 Documentation

- `3054db7e` update test counts to 4,647 across 424 modules (11 new test files)
- `06e60222` document Slack grouped tool calls, reactions, external uploads
- `1d3d8578` document [agent] max_concurrent now that it is enforced (#361)
- `7598f419` document the generate_document tool and its styling surface (#367)
- `a0c74bfa` tool description tells the agent to deliver files on channels
- `1ebe8598` clarify rebuild is a cron-scheduled background job
- `af362f40` document default_provider and default_model in [agent] section (#314)

### 🧹 Miscellaneous

- `b717c0d8` test(agent): cover parallel tool-batch eligibility, ordering, cancellation (#361)
- `54a7d69f` test(doc_gen): prove every wrapped table line gets its own baseline

### 📊 Stats

- 49 commits since v0.3.61
- 64 files changed, +7500 / -404 lines
- 4648 tests (4648 passed, 0 failed, 29 ignored)



## [0.3.61] - 2026-07-04

27 commits since v0.3.60. 38 files changed, +2296 / -474 lines.

### ✨ Features

- `5f2633a2` **Group tool calls**: consecutive tool calls collapse into one expandable block (#291)
- `8a66620b` **Fold intermediate text**: intermediates fold into the same in-place processing log as tool calls (#300)
- `6b82665e` **Deliver build outcomes**: rebuild results reach whoever asked (#304, #305)
- `ab71d45c` **Frame reactions**: inbound reactions read by sentiment and address the user by first name (#302)
- `2b3943b9` **Mid-turn reactions**: a reaction during a running turn injects into that loop instead of firing a second turn (#302)
- `88f8d5e9` **Self-goaling**: goal_manage tool lets the agent set and drive its own multi-turn goals (#307)

### 🔧 Fixes

- `5f5e8225` **Expandable blockquote**: grouped tool calls render as a native blockquote (#295)
- `64bc05fd` **Rich API for tool groups**: route collapsible blocks through the rich API
- `a2b25927` **Edit tool group in place**: consecutive calls stay one block (#296)
- `7a586fe3` **Command replies vs 429**: wait out rate limits instead of dropping (#297)
- `41eb0976` **Streaming placeholder**: re-post only when a message lands below it (#299)
- `a448c233` **Reaction decision tree**: clearer react-vs-respond directive (#298)
- `9aeb33b9` **Completion out of the block**: keep the final answer from being folded away (#300)
- `23e220b2` **Reclaim only when needed**: pull the folded final only when no separate answer exists (#300)
- `6644d5c6` **Formatted grouped content**: render block content inline, not raw markdown (#306)
- `493c1358` **analyze_image tilde**: expand `~/` paths via resolve_tool_path
- `3830f29e` **Tool allowlist**: add web_scrape, restore whatsapp_send to KNOWN_TOOL_NAMES
- `84f44b4e` **Separator recovery**: recover tool calls with non-ASCII corrupted separators
- `75a7b5f3` **Separator recovery**: recover a corrupted non-ASCII separator
- `0d86583e` **WhatsApp owner DMs**: stop responding to the owner's DMs with other people
- `cf4f3e19` **Reaction marker**: tolerate an escaped marker prefix so the reaction fires
- `8171e302` **WhatsApp LID JIDs**: convert LID chat JIDs to PN for non-owner DM responses

### 📖 Documentation

- `aaca6be1` make the rebuild-vs-evolve distinction explicit for the agent
- `6d7511e5` add bug fix tracking workflow to CODE.md template
- `ec792613` add bug fix tracking workflow to AGENTS.md template

### 🧹 Miscellaneous

- `c85dd572` add Excel/spreadsheet parsing support (XLSX, XLS, CSV)
- `374eb94d` cargo fmt (analyze_image, doc_parser, file_extract)

### 📊 Stats

- 27 commits since v0.3.60
- 38 files changed, +2296 / -474 lines
- 4563 tests (4563 passed, 0 failed, 29 ignored)

## [0.3.60] - 2026-07-03

17 commits since v0.3.59. 32 files changed, +2360 / -59 lines.

### ✨ Features

- `50328e90` **SSRF guard**: URL validation rejects private IPs (10.x, 172.16-31.x, 192.168.x), localhost, loopback IPv6, link-local, cloud metadata (169.254.169.254), and non-http(s) schemes. Uses `url::Host` enum to properly classify IPv6 literals like `[::1]`.
- `3fc75911` **Structural HTML cleaner**: Language-agnostic cleaning strips scripts, styles, inline handlers, HTML comments. Decodes entities, collapses blank lines. Genericized from insight_forge (removed Portuguese-specific filters).
- `2d5467fc` **Main-content extraction**: CSS selector cascade (article, main, .content, etc.) isolates primary content from HTML, falling back to body with junk selectors (header, nav, sidebar, ads) removed.
- `5633132f` **HTML-to-markdown**: htmd converts cleaned HTML to markdown. `absolutize_urls` resolves relative src/href against page base URL. Images preserved as `![alt](url)` tags for selective agent vision.
- `7fedb1ee` **HTTP fetch + JS detection**: reqwest with browser UA and timeout. `is_js_shell` heuristic detects JS-heavy pages (React/Vue/Angular/Svelte shells, `<div id="app">`, no `<article>`). Escalates to browser manager when available.
- `149a4b66` **Sitemap discovery**: Discovers /sitemap.xml, /sitemap_index.xml, and common variations. Recursively crawls sitemap indexes (iterative worklist, 1000-URL cap, 3 levels deep). Returns URL list for agent to pick from.
- `ce2bf212` **Profile/project-aware export**: Sitemap mode exports markdown to directory. Output path resolves to project files dir if session is assigned, else profile-scoped opencrabs home. Never writes outside managed workspace.
- `c8d497d1` **web_scrape orchestrator**: Thin Tool trait impl wiring validate → fetch → extract → clean → markdown → export. Registered in tool_setup.rs, categorized as `web`, kept out of CORE_TOOLS for deferred loading via tool_search.

### 🔧 Fixes

- `6afb4cc0` **Vision video/GIF gate**: Accepts provider vision_model, not just Gemini. Fixes routing when active provider has vision capabilities.
- `70dbc4f6` **Vision registration**: Register analyze_video whenever any vision backend exists, not just when Gemini is configured.
- `8490aed3` **Vision fallback routing**: Route video frame fallback through the active provider instead of hardcoded model.
- `540e2ddd` **Personalize greeting**: /evolve wake-up greeting now evolves with context instead of static message.

### 📖 Documentation

- `a5e4459a` README.md: Added web_scrape to Search & Web tools table.
- `e53d7af1` tools.toml.example: Added `web` category (http_request, web_scrape) to built-in tools comment. Documented core vs deferred distinction for lazy-tools users.

### 🧹 Miscellaneous

- `a710c7d5` style: cargo fmt line-wrap in web_scrape markdown test.
- `50508c1d` test: Added benchmark suite comparing old approach (http_request + agent loop) vs new web_scrape tool.
- `305989ef` test: Added real-world comparison table: Claude CLI + Opus 4.8 (62s, 20k tokens) vs OpenCrabs + Qwen 3.7 max (15s, 1 tool call) on meetneura.ai.

### 📊 Stats

- 17 commits since v0.3.59
- 32 files changed, +2360 / -59 lines
- 4,507 tests (4,507 passed, 0 failed, 29 ignored)

## [0.3.59] - 2026-07-02

25 commits since v0.3.58. 36 files changed, +2374 / -237 lines.

### ✨ Features

- `36373e98` **Three-tier project directive-file discovery**: CLAUDE.md, GEMINI.md, .cursorrules, and other directive files auto-loaded from working directory at session start
- `4e759c44` **Auto-discover project directive files**: working directory scanned for directive files on every session create

### 🔧 Fixes

- `33b95290` **Plus prefix normalization**: is_owner comparisons now strip leading + from phone numbers (#277)
- `8fc0044c` **WhatsApp LID DM allow list**: match against sender's PN twin for LID-based DMs (#276)
- `d6787884` **Directive scan follows /cd**: project directive files re-scanned when working directory changes
- `b0199001` **Per-group /respond_to override**: creates group-specific override even when value matches global default
- `5395fd43` **/cowork group ACL**: registers users to group allowlist, not global allowlist
- `eeeec2dc` **/onboard:channels deep-link fix**: channel argument no longer dropped from deep-link URL
- `df8d2830` **Custom provider ignore-enabled**: by-name creation now honours ignore-enabled contract (#270)
- `95eb1978` **CLI turn streaming persistence**: turns persisted to DB as they stream, not at turn end (#269)
- `bf4bc21e` **In-flight turn resume**: never age out in-flight turns, resume every surviving row (#268)
- `a632f782` **Live model persistence**: fetched models for custom providers now appended to config, real default shown (#267)
- `69a816ed` **Channel command interrupt fix**: /models, /help, /usage no longer abort in-flight tasks (#266)
- `758f1881` **/new working directory scope**: inherits from same chat session, not global-latest (#263)
- `f60f26e7` **@botname strip fix**: strips @botname from command token only, keeps arguments intact (#265)
- `eaaa07b3` **Intermediate reactions**: fire reactions mid-thinking, strip directive from streaming placeholder
- `c39fdc5f` **Fallback error clarity**: stop scaring operators before fallback, name exhausted providers
- `3073bf84` **Fallback chain summary**: one-line log summarising fallback chain build result
- `ff442e1e` **Custom provider resolution**: resolve custom.<name> fallback entries with actionable error messages

### 📖 Documentation

- `38bfb593` document project directive-file auto-discovery in README
- `e3e6f823` clarify /cowork registers to group allowlist, not global

### 🧹 Miscellaneous

- `36778b59` style(telegram): collapse handle_message ACL guard to one line
- `351a66bc` test: serialize real-home tests on shared HOME env lock, kill flakes
- `30676628` test(telegram): prove per-group /respond_to persists and survives reload (#264)
- `0545b875` style(telegram): add trailing newline to agent.rs

### 📊 Stats

- 25 commits since v0.3.58
- 36 files changed, +2374 / -237 lines
- 4453 tests (4453 passed, 0 failed, 24 ignored)




## [0.3.58] - 2026-07-02

29 commits since v0.3.57. 31 files changed, +1743 / -99 lines.

### ✨ Features

- `42be5cf4` **Inbound reaction handler**: Telegram bot now processes user reactions with dispatcher wiring and bot message content lookup
- `696f3bf5` **Bot message lookup DB**: added bot_content_by_platform_message_id for inbound reaction context retrieval
- `cefd4f4c` **Reaction directive instructions**: channel prompts now include rules for when to react vs reply
- `fdfb6b7f` **Reaction-only delivery**: Telegram responses can now deliver as reactions only when appropriate
- `871c62d0` **Extract react marker utility**: added utility to parse and validate emoji reactions from text
- `2674f333` **Telegram photo captions and replies**: send_photo/send_document now support caption and reply_parameters
- `9b347f70` **Write opencrabs file trace logging**: replace operations now log hex bytes on miss for debugging
- `70879f81` **Goal judge retry**: retry once on parse failure in judge_goal for resilience

### 🔧 Fixes

- `79ea3768` **Group-chat self-silencing**: templates no longer instruct bot to silence itself in group chats
- `f580a35d` **React directive stripping**: intermediates no longer leak react directives to users
- `faa8e8c8` **Emoji validation**: extract_react_marker only accepts real emoji, skips code spans
- `a1d79b5b` **Whitespace normalization**: Telegram dedup comparison now normalizes whitespace
- `1855a8e5` **Claude-cli model selector**: TUI selector now fetches full SUPPORTED_MODELS instead of stale hardcoded list
- `870c47c0` **WhatsApp greeting**: greeting now only sends on fresh pairing, not every restart
- `36ef8117` **Channel search message IDs**: channel_search now returns platform_message_id for rich fallback
- `a597098f` **Session working_directory inheritance**: new sessions inherit working_directory from most recent session
- `63bbda12` **Unicode NFC normalization**: write_opencrabs_file now NFC-normalizes both sides before replace
- `9846de4e` **Follow-up question options**: removed 40-char hard cap from follow_up_question options
- `29ae4e03` **Goal judge max_tokens**: increased from 512 to 4096 for complex reasoning

### 📖 Documentation

- `3fec2b95` add Zero Telemetry section to README

### 🧹 Miscellaneous

- `88606987` test(reactions): add edge-case tests for extract_react_marker
- `1d902695` test(session): add tests for working_directory inheritance on /new
- `23340557` refactor(session): add working_directory param to create_session_with_provider
- `be94f65f` test(telegram): caption and reply_parameters for send_photo/send_document
- `0dd609f1` test(write_opencrabs_file): add Unicode NFC normalization tests
- `8d259aa1` docs(tools): update follow_up_question schema with 40-char recommendation
- `7e6794f5` test(tools): add regression test for long options passing through
- `679cfd00` test(goal): add tests for judge retry and max_tokens
- `eaa108ee` style: cargo fmt

### 📊 Stats

- 29 commits since v0.3.57
- 31 files changed, +1743 / -99 lines
- 4398 tests (4398 passed, 0 failed, 24 ignored)


## [0.3.57] - 2026-06-30

29 commits since v0.3.56. 37 files changed, +2616 / -474 lines.

### ✨ Features

- `7fbd37c1` **Vision fallback chain**: wire vision fallback chain into active_provider_vision()
- `6b6c431a` **Vision fallback config**: add vision fallback chain field to FallbackProviderConfig
- `290f9063` **WhatsApp response_policy**: owner_only/allowlist/open modes with operator-aware filter
- `adb15e52` **WhatsApp connection test**: greet the paired account's own self-chat on connect
- `41e8e5b7` **WhatsApp action-based send tool**: refactor WhatsAppSendTool with 14 actions
- `7693a3eb` **TUI /onboard:channels shortcut**: jump straight to a channel's onboarding dialog

### 🔧 Fixes

- `c978987e` **Telegram progress events**: surface RetryAttempt and ProviderSwitched in Telegram
- `10f1e747` **Channel progress events**: surface RetryAttempt and ProviderSwitched in Discord, Slack, WhatsApp
- `79f7f540` **Telegram send persistence**: persist telegram_send posts so replies to them are recoverable
- `6942f5a3` **Telegram bot dedup**: only match THIS bot in replied_to_bot, silently drop bot senders
- `999c3265` **Telegram exact-match dedup**: exact-match only, not substring replace (#252)
- `d3ee1ccc` **Per-group /respond_to**: persist /respond_to per-group when called from a group
- `85eca3fa` **WhatsApp self-chat routing**: route owner self-chat reply to the PAIRED account number
- `19fbe85f` **WhatsApp connection gate**: gate connection test on is_connected + retry on transient drop
- `895b10ed` **Vision provider scan**: scan all enabled providers for vision_model, not just active (#253)
- `ed7476c7` **TUI markdown colours**: keep markdown colours in user messages, don't flatten to white
- `544d526c` **Brain plan ops**: correct plan op names + proactive planning and complete-as-you-go
- `06d4bf8f` **WhatsApp LID/PN registry**: migrate to upstream HEAD for device registry

### 📖 Documentation

- `18b411b1` document WhatsApp pairing any owned number + response_policy
- `764f8e3d` correct WhatsApp QR pairing: TUI only, not channel-scannable
- `c6be08ae` add vision fallback chain to config.toml.example
- `b1ac5eb4` update test counts in README and TESTING.md

### 🧹 Miscellaneous

- `536c48f7` style: collapse nested if blocks in vision fallback resolution (clippy)
- `17957b26` style: apply rustfmt to telegram and discord handlers
- `511db80b` test(vision): 14 tests for vision fallback chain resolution
- `2677be3c` test(whatsapp): 19 tests for whatsapp_send helpers and schema

### 🔨 Build

- `d1495539` cfg(crates_publish) for crates.io compat without breaking git-HEAD build
- `3ef4c8d9` add crates_publish cfg for crates.io compatibility
- `79a65de9` add version = "0.6.0" to whatsapp-rust git deps for cargo publish

### 📊 Stats

- 29 commits since v0.3.56
- 37 files changed, +2616 / -474 lines
- 4,350 tests (4,350 passed, 0 failed, 24 ignored)


## [0.3.56] - 2026-06-29

51 commits since v0.3.55. 256 files changed, +14841 / -17680 lines.

### ✨ Features

- `f57eb274` **Per-group /respond-to command**: override response mode per Telegram/Discord group via slash command
- `7b3ab774` **Auto mention-only mode**: automatically switch to mention-only when a group grows beyond 2 members
- `6de61f78` **Uniform bot-owner identity**: explicit, consistent bot-owner detection across all channels
- `d628f2df` **Bot-owner migration**: seed channel bot_owner from the allow list on config migration
- `db5d6913` **RSI BinEval triage**: failure triage modeled on BinEval in the RSI prompt
- `117ed40a` **RSI tool-ban guard**: refuse self_improve rules that would ban built-in tools
- `9df35301` **RSI recoverable failures**: keep recoverable tool failures out of the success-rate denominator
- `25803109` **Per-chat ACL**: group allow lists, DM gate, and per-group respond_to config
- `eacec7d7` **Forum callback routing**: route follow_up_question and approval callbacks to the correct forum topic

### 🔧 Fixes

- `7ec02d6c` **WhatsApp Sender receipts**: accept Sender receipts as delivery confirmation for self-chat
- `3cdd9278` **WhatsApp encrypt race**: serialize the session store to stop the encrypt race that drops replies
- `a70c46ff` **WhatsApp streamed replies**: route streamed reply text to the PN too, not just the final reply
- `9f19d2b8` **Telegram /cd state**: isolate /cd directory browser state per forum topic
- `c6758279` **Telegram voice indicator**: voice typing indicator now targets correct forum topic
- `f9992f68` **WhatsApp onboarding**: onboarding test confirms real delivery, not just transmission
- `158bfef4` **WhatsApp owner replies**: send owner replies to the PN, not the LID self-chat
- `771799ec` **WhatsApp self-chat**: collapse the owner self-chat to a single session
- `8014828d` **Disabled provider fallback**: exclude disabled providers from the fallback chain
- `28182fdf` **Session provider inheritance**: never inherit a disabled or absent provider into new sessions
- `f3414c55` **WhatsApp idempotent resend**: resilient resend so a skipped device can't drop replies
- `5fefeb4e` **Tokio stack overflow**: enlarge tokio worker stack to stop agent-turn stack overflow
- `94fdccfb` **WhatsApp restart cleanup**: disconnect the live client before aborting on restart/stop
- `3e197c44` **WhatsApp greet dedup**: greet once per connection, not on every reconnect
- `1362bd69` **WhatsApp pairing**: authorize the paired owner, confirm via agent, lock QR after connect
- `ea0f00cb` **WhatsApp QR flow**: show pairing QR on connect and force fresh re-pair on reset
- `f8d33803` **Owner-only /cd**: restrict /cd directory browser to the bot owner
- `5e57d4ce` **Channel agent restart**: restart dead channel agents on reconcile

### 📖 Documentation

- `30fcc070` document bot_owner and owner-only commands
- `3c4e1d35` document per-group ACL, DM gate, and respond_to auto/override

### 🧹 Miscellaneous

- `c5e2d52c` refactor(whatsapp): split store.rs into store/ submodules
- `a696bf67` chore(deps): migrate WhatsApp to wacore/whatsapp-rust 0.6
- `5a536074` refactor(tests): migrate all inline #[cfg(test)] modules to src/tests/
- `e5b66868` through `c5371a0a` refactor(tests): move tests by module into registered src/tests/ files (14 commits)
- `dbaa22b7` chore: remove dead vendored wacore-binary patch crate
- `14474933` test: recover 12 tests dropped by #248 inline-to-src/tests migration
- `d2d92183` chore(evals): move security-eval under src/ and fix path references
- `6d5a1f61` through `8401f779` revert 4 commits (runtime stack size, disabled-provider fallback chain)

### 📊 Stats

- 51 commits since v0.3.55
- 256 files changed, +14841 / -17680 lines
- 4,308 tests (4,308 passed, 0 failed, 24 ignored)


## [0.3.55] - 2026-06-27

20 commits since v0.3.54. 41 files changed, +2569 / -200 lines.

### ✨ Features

- `cfde361e` **xiaomi endpoint_type selector**: API vs Token Plan mode with per-provider switching
- `567476d8` **wire /profiles in slash_command handler**: slash_command tool routes to profile manager
- `a71bcf30` **profiles rendering for Discord**: Discord handler shows profile browser/switch/delete/create
- `2b4d4b8f` **profiles rendering for WhatsApp**: WhatsApp handler shows profile browser/switch/delete/create
- `5d59192b` **profiles rendering for Slack**: Slack handler shows profile browser/switch/delete/create
- `5c443514` **TUI profiles dialog module**: state, actions, and input handling for native /profiles dialog
- `052abf32` **TUI profiles dialog renderer**: full browse/create/delete/migrate UI rendering
- `23d28397` **wire AppMode::Profiles into TUI**: events, state, slash commands, and render dispatch
- `ebe10ff3` **profiles dialog tests**: 49 tests for matching, decide, navigation, create/delete/migrate flows
- `15cb19c7` **tools.toml backup recovery**: last_good .bak snapshot on every write, fallback on parse error

### 🔧 Fixes

- `883d31e9` **preserve /v1 in custom provider base_url**: onboarding no longer strips /v1 from endpoints
- `879a69c0` **reply context includes replied-to author identity**: telegram reply messages now show who was replied to
- `1b2414c1` **recover exact replied-to message by id**: telegram replies fetch the precise message, not the latest
- `1b4a2645` **stop fabricating reply context when content is unretrievable**: telegram no longer fakes reply context
- `73de1cd5` **persist delivered Telegram message id for reply recovery**: cron scheduler saves message IDs for later reply lookups

### 📖 Documentation

- `b9fa1a53` add /profiles entry to commands.toml.example
- `2a26ff18` update test counts in README (4,257 tests, 319 modules, 24 ignored)
- `429d042f` update TESTING.md with accurate test counts (146 missing entries added, stale counts corrected)

### 🧹 Miscellaneous

- `4ee0747b` style: cargo fmt cleanup for profiles dialog
- `fb60c323` test: pin exact replied-to message lookup by platform id

### 📊 Stats

- 20 commits since v0.3.54
- 41 files changed, +2569 / -200 lines
- 4,257 tests (4,257 passed, 0 failed, 24 ignored)

[0.3.55]: https://github.com/adolfousier/opencrabs/compare/v0.3.54...v0.3.55

## [0.3.54] - 2026-06-27

21 commits since v0.3.53. 47 files changed, +1728 / -380 lines.

### ✨ Features

- `94558f16` **TUI markdown rendering**: render emphasis, lists, links, and task items in the terminal UI
- `10ed2726` **Wire /goal to all channels**: the autonomous /goal command is now available across every channel with bare-command denial (#232)
- `d49f67d4` **Xiaomi MiMo as a normal keyed provider**: the collab-specific keyless proxy is gone, MiMo now behaves like any other keyed provider

### 🔧 Fixes

- `0cb0a9bf` **Restore custom-provider live model fetch**: onboarding model fetch from a custom base_url was broken, now pulls the live model list again
- `e3755e44` **Surface tools.toml parse errors**: a malformed tools.toml no longer silently drops every tool, the parse error is now reported (#235)
- `2885b225` **Recover Telegram reply context for bot rich messages in DMs**: reply context for bot rich messages was lost in DMs, now recovered (#234)
- `a63f8440` **Remove duplicate get_last_assistant_message**: deduplicated the helper and its duplicate tests (#234)
- `4501226b` **Remove hallucination fuel from post-evolve prompts**: post-evolve prompts no longer carry text that triggered hallucinations

### 📖 Documentation

- `422a725c` docs(providers): scrub Xiaomi collab references for the keyed provider
- `6f83f8a1` docs(cron): add copy-paste cron job templates

### 🧹 Miscellaneous

- `c316aebb` style(provider): apply rustfmt to factory key resolution
- `f6aecad2` test(providers): pin Xiaomi keyed-provider behaviour and shadow agreement
- `53310f4d` test(onboarding): pin custom-provider live model fetch regression
- `9dfc35b9` test(tools): cover tools.toml parse-error handling and data-loss guard (#235)
- `f81b5b19` test(telegram): pin DM reply-context recovery for bot rich messages (#234)
- `c012a30a` test(goal): pin /goal dispatch, bare-denial, and directive shape (#232)
- `27941dd7` test(tui): cover markdown emphasis, lists, links, and task rendering
- `f5fd1427` test(skills): pin profile-scoped skill discovery and isolation (#231)

### 📊 Stats

- 21 commits since v0.3.53
- 47 files changed, +1728 / -380 lines
- 4193 tests (4193 passed, 0 failed, 24 ignored)

[0.3.54]: https://github.com/adolfousier/opencrabs/compare/v0.3.53...v0.3.54

## [0.3.53] - 2026-06-26

1 commit since v0.3.52. 3 files changed, +32 / -0 lines.

### 🔧 Fixes

- `12bf340c` **Wire /goal command into TUI dispatch, autocomplete, and commands.toml**: the autonomous /goal feature shipped in v0.3.52 with backend code but zero entry points connected (TUI showed 'Unknown command', no autocomplete, no commands.toml entry)

### 📊 Stats
- 1 commit since v0.3.52
- 3 files changed, +32 / -0 lines

## [0.3.52] - 2026-06-26

10 commits since v0.3.51. 23 files changed, +1324 / -529 lines.

### ✨ Features

- `173208b0` **Autonomous /goal command**: set a goal via `/goal <text>` and the agent loops autonomously until an LLM judge decides it's satisfied or the turn budget (default 20) runs out

### 🔧 Fixes

- `c5152e6d` **Share runtime tool registration across all agent entry points**: tools registered at startup are now available everywhere, not just the main session
- `a7ec82a9` **Clarify render() doc**: thinking never leaks to output
- `36421e8f` **Strip thinking/reasoning tags from output**: `<thought>`, `<thinking>`, and `<𝑎𝑛𝑡𝑚𝑙:thinking>` tags no longer leak into user-facing messages
- `bbb7e0b6` **Wire tool registry into multi-profile daemon factory**: tool definitions are now properly available in the daemon's agent factory
- `69ce7192` **Add draft_streaming config flag**: new `[telegram] draft_streaming = true/false` to disable draft message streaming
- `ce78b943` **Check bot admin status before creating invite link**: /cowork no longer errors when the bot lacks admin permissions

### 🧹 Miscellaneous

- `88da8c9d` chore(deps): bump pdf-extract 0.10 -> 0.12 to pull lopdf 0.42 (RUSTSEC-2026-0187)
- `be0d7787` test(cron): pin that register_core_agent_tools populates the registry
- `a9eb03e0` docs(readme): document /goal autonomous command

### 📊 Stats

- 10 commits since v0.3.51
- 23 files changed, +1324 / -529 lines
- 4147 tests (4147 passed, 0 failed, 24 ignored)

## [0.3.51] - 2026-06-26

Patch release: restores the Xiaomi keyless collab proxy for the final day of the collab. Users on 0.3.50 should update — that build shuts the keyless proxy off a day early (at 2026-06-26 00:00 UTC).

### 🔧 Fixes

- `60309763` **Xiaomi keyless collab window closed a day early**: the free keyless window flipped closed at 2026-06-26 00:00 UTC instead of the collab's actual end of 2026-06-27 00:00 UTC, so the default Xiaomi provider stopped working mid-collab — RSI cycles failed with "Xiaomi not configured (missing API key)" and session swaps to Xiaomi silently didn't take. The cutoff now uses the literal end date (`2026-06-27`) with a strict `today < end` comparison, so the window closes exactly at 2026-06-27 00:00 UTC. A boundary test pins the 26th-open / 27th-closed behavior.

### 📊 Stats

- 1 commit since v0.3.50
- 2 files changed, +40 / -8 lines

## [0.3.50] - 2026-06-25

8 commits since v0.3.49. 14 files changed, +689 / -211 lines.

### ✨ Features

- `28e803df` **Forum-topic session labels**: sessions in forum topics now show the topic name instead of the generic group label

### 🔧 Fixes

- `8cc9bf83` **Scope group-history injection to the forum topic (#226)**: recent message injection no longer bleeds messages from all topics into the current one
- `73731912` **Standardize bot menu on underscore form**: all bot commands now use the consistent underscore form in the menu
- `73ec2dda` **Keep mission-control in the bot menu**: `/mission-control` stays in the menu as a concatenated command
- `89b9764b` **Drop hyphenated commands from bot menu**: removed ambiguous hyphenated command variants
- `e2c2bf1e` **Route command dialogs through native rich rendering**: command responses now go through the rich AST renderer
- `4b706c61` **Render /help and /usage as tables**: help and usage output now renders as clean markdown tables; fix /mission-control routing

### 🧹 Miscellaneous

- `a2076128` test(channel_commands): fix format_help parser for md_table output

### 📊 Stats

- 8 commits since v0.3.49
- 14 files changed, +689 / -211 lines
- 4140 tests (4140 passed, 0 failed, 28 ignored)

## [0.3.49] - 2026-06-25

22 commits since v0.3.48. 44 files changed, +2114 / -177 lines.

### ✨ Features

- `2a1337de` **Block::Details AST variant**: new AST node for `<details>/<summary>` collapsible blocks with HTML fallback
- `730aafbc` **Parse `<details>/<summary>` collapsible blocks**: markdown parser now recognizes `<details>` and `<summary>` HTML tags
- `c76f51a2` **Draft message streaming for DMs**: ephemeral "typing..." messages that update in-place as tokens stream
- `47257dc5` **Wire draft streaming for Telegram DMs**: connects the rich AST renderer's draft streaming to Telegram's edit API
- `51938f39` **Rich structure detection tests**: `<details>` parsing tests + `has_rich_structure()` utility
- `720c2b4e` **Promote most-used commands to top of /help**: /new, /cd, /sessions, /stop now appear first in command listings
- `92380f72` **Auto-assign project on /cd**: `/cd` now auto-assigns the session to the matching project + project-scoped skill resolution
- `fd8c1edd` **Expanded TOOL LIFECYCLE directive**: proactive tool_search, skills check, fallback-on-failure, and refusal interception

### 🔧 Fixes

- `be8a29e4` **Cron double-fire on first tick**: fixed race condition where jobs near the cron boundary would fire twice on startup
- `971c6e4f` **Cron shared session with compaction isolation**: cron jobs now share a single session per job with proper compaction boundaries
- `359cfc6c` **Forum topic isolation in channel_search**: `recent()` now filters by thread_id so forum topic messages don't leak across topics
- `65f8ab4f` **Rich AST renderer for slash command responses**: slash commands in Telegram now render through the rich AST pipeline
- `10a1b510` **Re-inject skill bodies after compaction**: active skill definitions are re-injected into the system brain after context compaction
- `c48299d4` **Accept dotted keys in [brain.caps]**: TOML config now accepts dotted keys like `brain.caps.model` with full error logging
- `4fd1f50f` **Bot replies stored after rich fallback**: reply context is now recovered when rich rendering falls back to plain text
- `bbe6cc12` **Strip MiniMax/mimo namespaced `<mm:think>` tags**: reasoning tags with `mm:` prefix are now properly stripped from output

### 📖 Documentation

- `da130901` Brain Constitution added to `src/docs/reference/BRAIN_CONSTITUTION.md`
- `eb6378d0` Brain Constitution hyperlink in README + project structure updated

### 🧹 Miscellaneous

- `a25065ec` test(skills): inline closure returns to satisfy clippy
- `b4fcfafc` refactor: move `security-eval/` to `evals/security-eval/`
- `ab8a3b75` evals/security-eval/README: update deterministic CI section
- `b381621e` style: cargo fmt on 10 files

### 📊 Stats

- 22 commits since v0.3.48
- 44 files changed, +2114 / -177 lines
- 4130 tests (4130 passed, 0 failed, 28 ignored)

## [0.3.48] - 2026-06-24

2 commits since v0.3.47. 5 files changed, +200 / -186 lines.

### 🔧 Fixes

- `f146af47` **Race-free photo storage and multi-image pickup**: Track pending file-save JoinHandles to eliminate fire-and-forget race, pick up ALL recent photos from tmp (Vec<PathBuf>), archive photos to project dir on arrival regardless of mention, ephemeral "Processing your photos..." feedback message
- `dcdad728` **Simplify cowork flow**: Remove misleading workspace name step, /cowork now immediately shows Add to Group button, auto-register all group members when bot joins, cowork state auto-expires after 120s so users never get locked

### 📊 Stats

- 2 commits since v0.3.47
- 5 files changed, +200 / -186 lines
- 4102 tests (4102 passed, 0 failed, 24 ignored)

[0.3.48]: https://github.com/adolfousier/opencrabs/compare/v0.3.47...v0.3.48

## [0.3.47] - 2026-06-22

37 commits since v0.3.46. 52 files changed, +2665 / -289 lines.

### ✨ Features

- `e3f20a64` **Proactive tool discovery**: agent searches for tools before claiming inability, plus project-directive loading
- `c8ddb521` **Confidential file protection**: SSH keys, .env, credentials protected. Owner verification required in group chats
- `87621927` **Per-project brain overlay**: project-specific brain files layer on top of profile brain files
- `1adc8b17` **Owner impersonation detection**: detect non-owners trying to act as the owner in Telegram group chats
- `0ddd8479` **bot_owner config field**: TelegramConfig with is_owner() helper for identity checks
- `67b55edf` **/profiles command**: manage AI profiles from any channel
- `a63b042c` **/cd hidden dirs toggle**: show/hide hidden directories in the directory picker

### 🔧 Fixes

- `217a431e` **Forum topic session isolation**: each forum topic gets its own session (#215)
- `b24f13a9` **Follow-up question topic routing**: questions go to the correct topic thread, not #general
- `ee367d2d` **System brain rebuild on change**: system brain rebuilt from disk when brain files change (#213)
- `56b8b057` **JIT tool activation**: extended tools activated on-demand when called by name (#214)
- `56df0352` **tool_search guidance**: extended tool guidance moved to preamble + RSI (#214)
- `5d0b4b47` **RSI tool success rate**: pre-execution misses no longer penalize success rate (#214)
- `53620509` **Internal-state query routing**: queries route to their tools, not the raw DB
- `c00b4a83` **State query routing refined**: further cleanup of internal-state query routing
- `cc689374` **Profile/project directives**: default profile stated, profile and project directives added
- `6889668d` **Session recovery hint**: corrected to use plan operation="start"
- `a37820cb` **Compaction reinjection fix**: TOOLS.md and CODE.md no longer re-injected after compaction
- `c6f900ce` **Plan auto-approve**: plan auto-approved on first start
- `238ded51` **rm-blocklist bypasses closed**: reversed flags, quoted $HOME, long flags, chained rm
- `3dbd430e` **Blocklist recursion**: recurse blocklist through interpreter indirection
- `1328f8fa` **@botname stripping defense**: defense-in-depth stripping + tracing for group command bugs
- `e4aa995b` **TOOLS.md template cap**: trimmed below 100-line regression cap

### 📖 Documentation

- `8036b7e3` Require a tool_search reminder when naming extended tools
- `f225cc5c` Add YAML frontmatter requirement for SKILL.md files

### 🧹 Miscellaneous

- `6f762022` Move AGENTS.md to last injection position
- `16c53b56` Drop routing-map duplication from AGENTS.md Owns header
- `17075a23` Drop redundant contextual-files pointer; AGENTS.md owns it
- `2d77b301` Move operational how-tos out of AGENTS into their owners
- `0070d118` Gate build/test on cargo-audit so advisories fail fast
- `09a83340` Live system-brain rebuild test
- `0d522443` Accept STT-chain umbrella error so CI isn't network-fragile
- `0ef60dd3` Per-project brain overlay integration test
- `64b349d2` Docker e2e adversarial eval harness
- `425f1713` cargo fmt commands.rs
- `bbd4247d` cargo fmt for commands and telegram
- `8ced3bc7` Bump quinn-proto 0.11.14 -> 0.11.15 (RUSTSEC-2026-0185)

### 📊 Stats

- 37 commits since v0.3.46
- 52 files changed, +2665 / -289 lines
- 4,103 tests (4,103 passed, 0 failed, 24 ignored)

## [0.3.46] - 2026-06-21

36 commits since v0.3.45. 81 files changed, +3799 / -1972 lines.

> **⚠️ EXISTING USERS NOTE:** The brain-file ownership model has changed. **SOUL.md** is now personality/voice ONLY. **AGENTS.md** is always-loaded and holds all hard rules and governance. Ask your OpenCrabs to fetch the latest templates (`src/docs/reference/templates/`) and update your brain files on top of them. If you have hard rules in SOUL.md, move them to AGENTS.md.

### ✨ Features

- `bc40d32a` **Cron Discord and Slack delivery**: wire result delivery to Discord and Slack channels
- `8e0e36d5` **Cron timezone and validation**: honor timezone, validate schedule with next-run feedback, document format
- `d0c4779f` **Runtime commands index**: inject a live commands and skills index so the agent sees runtime ones
- `eb75c3d3` **Project file shares**: symlink local shares, copy ephemeral ones into project files
- `887ef8b4` **AGENTS always-loaded**: preamble + RSI route hard rules to always-loaded AGENTS.md
- `7ffdf2d5` **Brain hard rules**: promote AGENTS.md to always-loaded so hard rules are enforced
- `1b9616ea` **Telegram /cd routing**: add /cd callback routing for directory navigation
- `56d446cd` **Telegram /cd browser**: render /cd directory browser with inline keyboard
- `53ad0b67` **Telegram /cd state**: add directory browser state to TelegramState
- `ee108af6` **/cd directory browser**: add /cd directory browser core
- `98ca35d3` **Brain-file ownership**: teach brain-file ownership in preamble and RSI

### 🔧 Fixes

- `9206c346` **Test preemption safety**: never run instance preemption against real workspace from tests
- `e32fd4b1` **Phantom detection**: catch work announcements ending with colon after 'now'
- `aa6ebaab` **TUI channel priority**: TUI takes priority over running daemon for channel locks
- `571ab10a` **Evolve cleanup**: sweep stale transient restart units before scheduling
- `187d689e` **Voicebox TTS timeout**: prevent timeout on long text with dynamic scaling and chunking
- `ecce87b0` **Compaction language**: make post-compaction CODE summary language-agnostic
- `f67750a8` **Language agnostic preamble**: don't impose Rust, language pref is CODE.md's
- `a97df0c7` **Template cleanup**: drop dead BOOTSTRAP.md, seed BOOT.md, document service setup
- `a5eac082` **Template cleanup**: remove unused VOICE.md template and personal-file references
- `4f4c00bd` **Glob safety**: bound the walk with symlink-safe, off-executor, capped, with timeout

### 📖 Documentation

- `c084c690` Explain TUI vs daemon run modes and the TUI-priority rule
- `9ca6182b` SOUL.md: personality/voice only
- `c4006ed5` Brain structure: SOUL=personality, AGENTS=always-loaded hard rules
- `29da49fb` Slim AGENTS.md to lean governance + gates
- `18a87392` Document the brain-file ownership model
- `0bb76c9b` Add ownership-map headers to each brain file
- `dd64fa12` Single-source-of-truth the seed brain files
- `ca5f42bb` Add RSI Engine section + Brain System TOC entry

### 🧹 Miscellaneous

- `b5ffb799` cargo fmt for cron + plan-tool changes
- `8d6d3f7c` Update plan test suite for 4-command plan tool
- `623debb9` Redesign plan tool to 4 commands, kill dead pipelines
- `12e23bfe` Governance guards for brain-file structure
- `979afb76` cargo fmt (import order + line wrapping)
- `32c3df00` Move slash commands to contextual on-demand loading
- `0d5fdae1` Verify SOUL.md ordering + fix stale comments
- `04a4581b` Move SOUL.md to end of system prompt

### 📊 Stats

- 36 commits since v0.3.45
- 81 files changed, +3799 / -1972 lines
- 4,073 tests (4,073 passed, 0 failed, 24 ignored)

## [0.3.45] - 2026-06-20

6 commits since v0.3.44. 9 files changed, +394 / -41 lines.

### ✨ Features

- `92687c73` **Cron job deletion safeguard**: Two-step delete (shows details first, requires `confirm=true`), `disable` now requires approval, auto-backup to `~/.opencrabs/backups/cron/` with rotation (last 10 snapshots). Updated AGENTS.md template with Cron Job Protection section.
- `dbbfcaaa` **Rich schedule detail popup**: Mission Control schedule detail now shows prompt, delivery target, next/last run, execution history (status, cost, duration), profile, and created date. Schedule service fetches recent runs to avoid N+1 queries.

### 🔧 Fixes

- `fc0bd4d8` **Mission Control popup sizing**: Detail popup now measures actual content height (including soft-wrap estimation) instead of fixed 70% of screen. Short entries get a small popup, long prompts get up to 70%.
- `3236c913` **Evolve health-check spawn retry**: Retry health-check spawn up to 5 times with backoff on transient ETXTBSY (kernel still writing) or ENOENT (write not settled) instead of failing and rolling back.
- `7fa01ec9` **Evolve concurrent-run guard**: Single-flight guard prevents two evolves from racing on the same temp file. Process-unique temp names (`evolve_tmp.<pid>`) eliminate file collisions.
- `f9194af2` **SSE stream log demotion**: Per-SSE-chunk `[STREAM_RAW]` logs moved from debug to trace. Prevents hundreds of raw-chunk lines flooding log files in debug mode.

### 📊 Stats

- 6 commits since v0.3.44
- 9 files changed, +394 / -41 lines
- 4,019 tests (4,019 passed, 0 failed, 27 ignored)


## [0.3.44] - 2026-06-19

15 commits since v0.3.43. 31 files changed, +3,373 / -2,205 lines.

### ✨ Features

- `ca266e98` **Evolve migration guard**: Check migration count compatibility before swapping binary. Prevents DB schema mismatch after evolve.
- `6c2c1a27` **Manual /compact continuation**: Manual /compact now adds a brief continuation to context, matching auto-compaction behavior.

### 🔧 Fixes

- `4b51303e` **web_search DDG Lite**: DuckDuckGo Instant Answer API is dead. Switched to DDG Lite HTML parsing.
- `2e6e4f77` **Plan tool double bug**: Completed plans were getting deleted from disk by TUI reload_plan. start_task now auto-completes stale InProgress tasks so plans no longer get stuck.
- `d6a9ae1a` **/compact brief confirmation**: Return a clean confirmation instead of dumping the raw compaction summary (#208).
- `777ccba3` **Config auto-repair**: Auto-repair broken config.toml and never poison the last-good config.
- `960f67cb` **Plan TaskType round-trip**: TaskType was serializing as a map instead of a string, breaking plan file round-trip.
- `1e488d5e` **Routing fix**: "check the website content" now routes to http_request instead of browser.
- `9db55ff7` **Clippy fix**: too_many_arguments on evolve_via_binary_download.

### 📖 Documentation

- `5c4e3e3c` Add Voice and Audio reference section to tools docs
- `b5f1292a` Refresh test counts and document config resilience tests

### 🧹 Miscellaneous

- `cf55d016` Extract config/keys file IO into types/io.rs
- `00711b82` Split impl Config out of types.rs into types/loader.rs
- `5e81cac0` cargo fmt, collapse nested match in cmd_evolve
- `42e26831` fmt after web_search DDG Lite merge

### 📊 Stats

- 15 commits since v0.3.43
- 31 files changed, +3,373 / -2,205 lines
- 4,018 tests (4,018 passed, 0 failed, 23 ignored)

## [0.3.43] - 2026-06-18

8 commits since v0.3.42. 15 files changed, +559 / -29 lines.

### ✨ Features

- `9b21cca9` **Plan pinning**: Pin the active plan at the end of the prompt each turn so the agent never loses track.

### 🔧 Fixes

- `b3dd4398` **Telegram bot-command registration**: Fix bot-command registration failing entirely due to 'mission-control' hyphen in command name.
- `52f4097b` **Telegram /help and text commands**: Fix /help and all text commands silently failing — escaped loop on bot commands.
- `5ef56c12` **Stuck-loop detection precision**: Only flag a stuck loop when the SAME intent line repeats, not for distinct lines.
- `2a509fed` **Project file archiving**: Archive all artifacts, exclude only repository code.
- `4d1c607d` **Project file archiving scope**: Only archive ephemeral tmp files, not persistent work.

### 📖 Documentation

- `3ad5c243` Update multi-agent skill

### 📊 Stats

- 8 commits since v0.3.42
- 15 files changed, +559 / -29 lines
- 3,992 tests (3,992 passed, 0 failed, 27 ignored)

## [0.3.42] - 2026-06-18

31 commits since v0.3.41. 51 files changed, +2,284 / -142 lines.

### ✨ Features

- `b534340b` **Project file archiving**: Archive shared images and session files into the assigned project's files directory.
- `8c86a807` **Project session file archiving**: Archive a project-assigned session's files under projects/<name>/files/.
- `31f0223c` **Reasoning repetition loop detection**: Detect and break reasoning loops that cause infinite thinking cycles.
- `fc93dc26` **silence_group_start config param**: Add Telegram config param to silence /start responses in groups.
- `19e3a18e` **TUI /cowork launch**: Launch /cowork from the TUI via a cowork_connect agent tool.
- `5a7823f5` **MiMo 200k context window**: Default MiMo context window to 200k tokens.
- `ca282fba` **/cowork workspace creation**: Create cowork workspaces with auto-registration for Telegram groups.
- `2df99270` **Fast-cancel on stop**: Kill active tasks immediately on 'stop' or '/stop' command.
- `efe20161` **Numeric ID in /onboard:channels**: Accept owner numeric ID in Telegram channel onboarding.
- `ca205403` **Group file tracking**: Track incoming files in groups and pick them up when the bot is tagged.
- `903382d6` **WhatsApp pairing QR**: Render pairing QR as a scannable PNG for WhatsApp channels.
- `0609e766` **Channel-capable onboarding**: Channel-capable /onboard:image|voice|channels with OpenAI-compatible vision.
- `c81dfd59` **/rename command**: /rename command for Telegram, Discord, Slack, and WhatsApp.
- `f202e65c` **Browser automation rules**: Add browser automation rules to the system prompt.

### 🔧 Fixes

- `dc41bfd6` **Remove hardcoded paths**: Remove hardcoded ~/.opencrabs/ from agent-facing strings.
- `ac79a7b8` **Drop hardcoded paths from prompt**: Drop hardcoded ~/.opencrabs/ from system prompt and vision hint.
- `3c68f14f` **Profile-aware RSI and tmp**: Route RSI state, tmp purge, and Telegram tmp through the profile home.
- `2e58db90` **Profile-aware projects_dir**: Make projects_dir profile-aware under the active profile's home.
- `849159b2` **Profile-aware known-paths**: Make known-paths profile-aware so config edits hit the right profile.
- `53486685` **Store untagged group photos**: Store untagged group photos to tmp and pick them up when tagged.
- `693a3b7d` **Cowork UX copy**: Update UX copy for the cowork group creation flow.
- `f4df8561` **Silence non-allowed users in groups**: Only reply to non-allowed users in groups when mentioned.
- `e2308d39` **Rich completion scroll fix**: Scroll to bottom on rich completion by deleting placeholder before fresh send.
- `91d57b75` **Onboard Telegram field label**: Label Telegram field as 'User ID' and require it before confirming.
- `c4ff0be6` **React-safe browser_type**: Replace value and dispatch input/change events for React forms.
- `18e2e621` **LABELED_CRED_RE prefix fix**: Skip key-prefix fragments shorter than 8 chars in credential redaction.
- `5ffe893b` **Labeled credential redaction**: Redact labeled credentials with colon separator (Password: xxx, Token: xxx).

### 📖 Documentation

- `d9293ea5` Document project file archiving under projects/<name>/files/
- `b106abb2` Note Xiaomi vision routing during collaboration window
- `f76d0d48` Document Telegram group security model + silence_group_start
- `2b367c18` Document /rename, fast-cancel, file pickup, TUI cowork, onboard channel improvements

### 📊 Stats

- 31 commits since v0.3.41
- 51 files changed, +2,284 / -142 lines
- 3,992 tests (3,992 passed, 0 failed, 27 ignored)

## [0.3.41] - 2026-06-16

19 commits since v0.3.40. 30 files changed, +681 / -282 lines.

### ✨ Features

- `4b77b54a` **Session search in TUI**: search filter + viewport scroll
- `dcc91cf2` **Session search in channels**: `/sessions:<query>` filter for channel commands
- `cba0daab` **Agent-driven onboarding welcome**: new welcome flow
- `f9097070` **Assigned session highlight**: green highlight for assigned sessions in assign mode

### 🔧 Fixes

- `38bfbff0` Telegram rich fallback when final reply deduped to zero
- `56691fb7` Plan summary uses markdown task lists for rich Telegram rendering
- `517edeae` Preserve rich tables when appending context footer
- `b332062c` Gracefully handle Kitty keyboard enhancement on Windows (#203)
- `b39769d8` Extend tmp file cleanup from 3 to 30 days
- `61e5074c` Add /search to SESSIONS section in help center

### 📖 Documentation

- `009454cb` README: add /sessions search and /sessions:<query>
- `1b55f05c` Template workflow: /tmp rule, start-with-template recommendation
- `3c5b23ac` README: add split panes screenshot to images section
- `c1728a01` Templates/boot: add environment awareness and self-knowledge

### 🧹 Miscellaneous

- `30ce3c07` Update welcome message tests for agent-driven onboarding
- `e16ed912` Edge-case tests for PR #202 + cleanup
- `8300936c` Bump JS actions to Node 24 versions
- `fdf4ab8e` Only publish binaries when crates.io upload also succeeds
- `0f718230` Cargo formatting normalization

### 📊 Stats

- 19 commits since v0.3.40
- 30 files changed, +681 / -282 lines
- 3,270 tests (3,952 passed, 0 failed, 23 ignored)

## [0.3.40] - 2026-06-15

72 commits since v0.3.39. 105 files changed, +6252 / -4121 lines.

### ✨ Features

- `88334c71` `c9fb8c47` `a3ad07ee` `6e476fb2` `9d5ab236` `ad02aac6` `52f1ed6f` **Projects system**: full CRUD UI with ProjectRepository (SQLite), ProjectService, and TUI mode. Assign sessions with A key. Project name badges with per-project colours. Help section with feature checkmark. Session file artifacts for per-project file management.
- `69e8d21c` `a15d8a5b` `3eb9b694` `d364d662` `76eb93cd` `0e70a8ae` `60947481` `2dd79d3e` **Telegram native rich messages**: full markdown-to-rich rendering gated behind `channels.telegram.rich_messages` (default off). AST parser handles headings, tables with alignment, nested/ordered/task lists, fenced code, blockquotes, and inline+block math. Responsive table rendering with card layout for wide tables. Native `sendRichMessage` for proactive sends via `telegram_send`. Structured intermediate messages render rich during streaming. Final streamed reply converts to rich on completion. Resume session delivery uses rich. Smart gating: only promotes on real block structure, plain prose untouched. Fresh rich message instead of editing placeholder. Any-length replies attempt rich. Silent HTML fallback on any failure.
- `344176c6` **Prompt formatting instruction**: model instructed to format responses with headings, tables, and fenced code blocks by default.
- `7657670f` **OpenRouter cache by default**: caching enabled on OpenRouter by default. Prompt Caching docs section with TTL behavior and cost-safety notes.
- `663dae17` **Context window override**: native and CLI providers honor `context_window` config, letting you cap or expand context for any model. Edit-or-ask config workflow documented.
- `abf69192` **Per-project badge colours**: stable colour from on-brand palette mapped by project id.

### 🔧 Fixes

- `509bf6d7` **TUI input preservation**: keep in-progress input when a queued message enters mid-turn.
- `003b8a53` `5f53faf7` `9dcc5c02` `f0be4af3` `a072a979` `2ed18f78` **Telegram rich renderer**: wire into resume_session, send fresh instead of editing placeholder, attempt for any-length replies, restore blank-line spacing, don't italicize intra-word underscores, never collapse allowed_users.
- `9b43517c` `d0b37547` **Provider orphan tags**: exclude bare `-->` by value, handle orphan think close tags in streaming + non-streaming.
- `c3daa12f` `183e2550` **Service scope**: resolve invoking user's home under sudo, probe unit file scope (#200).
- `22f49906` **Config snapshot**: snapshot last_good when config changes and parses.
- `03c9e1e8` `d9de9d96` `eb5ec1fd` `fbd21804` **Brain dedup hardening**: incident-line stripping, cap evidence entries, extend looks_like_failure_log, normalize timestamps.
- `3bab6228` `afee75c2` **Onboarding**: Image Handling layout, Quick onboard dot count.
- `f4a6ea34` `3fab3e20` **Zhipu**: restore newest-first order, merge config.toml models into GLM picker.
- `b287efac` `f767f4ef` **Vision**: register setup-hint analyze_image, Xiaomi vision_model + Gemini fallback.
- `0c1ccc0c` **Daemon**: systemd Restart=always.
- `ff6fbe3f` **Hot-reload**: detect atomic-save config edits.
- `42446c06` **Help**: keep built-in commands alphabetical.
- `20fa17cb` **Providers**: clean up custom-provider section on rename.
- `3cdf160c` **Voice**: surface Voicebox generation failures.
- `2f3486fe` `0853dbbc` `3b314b7c` **TUI**: remove duplicate /mission-control, badge brand orange, badge real-time update.

### ⚡ Performance

- `b3a77e87` In-memory current-config mirror, refreshed by the watcher.
- `73588534` Demote per-load config-load logging from info/debug to trace.

### 🧹 Miscellaneous

- `a2236d67` `a11116b1` Remove all item-level `allow(dead_code)` + dead OpenRouter AnthropicOR path.
- `fdb5fa6f` Delete dead ModelSelector mode.
- `158532a4` `/models` reuses onboarding provider picker.
- `f1177669` Replace `/analytics` with `/mission-control`.
- `54ee96fe` `d652f5b6` `d82b240f` Relocate unit-scope tests, incident log dedup tests, 25 project integration tests.
- `999957c7` `b12ba129` One-liner install per platform, auto-start service recommendation.
- `21c29adc` `2d9943ec` `498cffa4` `48392fe4` `ff80a6d1` `8d551dcf` Prompt Caching docs, context window guidance, cost-safety notes.
- `15b4426a` `b5da3dac` `7b5b3b04` README: Telegram rich formatting, /mission-control, daemon troubleshooting.
- `935c6bfb` `1804d9e2` Cargo formatting normalization.

### 📊 Stats

- 72 commits since v0.3.39
- 105 files changed, +6252 / -4121 lines
- 3,249 tests

## [0.3.39] - 2026-06-14

30 commits since v0.3.38. 52 files changed, +3015 / -581 lines.

### ✨ Features

- **Mission Control Analytics** — native analytics panel replacing the external opencrabs-analytics tool. Shows brain file sizes, tool usage with proportional bars, flakiest tools by failure rate, and RSI applied by dimension. Full-height right column with 2x2 grid layout. Enter for detail popup. No external calls, no telemetry, nothing leaves the machine.
- **`/analytics` command** — TUI opens the Mission Control Analytics panel; channels (Telegram, Slack, Discord, WhatsApp) return the report as a message. Also exposed as `analytics_report` agent tool so you can ask in plain language.
- **Migrate CLI** — `opencrabs migrate openclaw` and `opencrabs migrate hermes` to migrate config, brain files, memory logs, and skills from other AI agent tools. Scans the system for source instances, shows interactive picker if multiple found, then spawns an agent to handle the migration. Post-migration verification confirms which files were updated.
- **RTK auto-download** — when RTK is not bundled or on PATH, OpenCrabs downloads the pinned release for your platform on first use. Covers source builds and installs where /evolve stops at "Already on the latest version." Runs in background at startup so bash commands never block.
- **Clipboard image paste** — paste images copied from the browser or any app directly into TUI input. Raw image bytes from the clipboard (macOS: osascript, Linux: wl-paste/xclip) are saved to a temp file and attached through the existing image pipeline.

### 🔧 Fixes

- **Keyless/local provider vision** — `analyze_image` and `generate_image` tools now register for keyless providers (Xiaomi free window) and local providers (Ollama, llama.cpp, LM Studio). Previously gated on API key, now gates on model field.
- **Hot-reload tools** — config/key-gated tools (browser, local STT, local TTS, knowledge base, Trello, WhatsApp, X) now register/deregister at runtime when you edit config or add/remove API keys, no daemon restart needed.
- **Channel /usage cache efficiency** — `/usage` in channels now shows cache efficiency and savings percentage, matching the TUI `/usage` output.
- **Slash command matching** — channel slash commands now match by dash/underscore-insensitive key, so `/analy-tics` and `/analy_tics` both resolve to `/analytics`.
- **Onboarding dark theme** — unselected provider list rows are now visible on dark terminal themes. Switched from DarkGray to Gray.
- **Onboarding vibe check** — health check passes for keyless providers and local endpoints instead of failing "API Key Present."
- **RTK rewrite normalization** — `find_rtk_binary()` now always returns bare `"rtk"` regardless of discovery path (bundled, PATH, or auto-download). Fixes CI test failures where the full absolute path was prepended instead of just `rtk`.
- **RTK graceful handler** — `/rtk` shows a friendly message when RTK is not installed instead of crashing.
- **RTK bundled in evolve** — `/evolve` now extracts the bundled RTK binary from the release archive alongside the opencrabs binary.
- **Evolve exe path** — strip all stacked "(deleted)" markers from `/proc/self/exe` path, not just the first one. Fixes restart failures when evolve runs multiple times.
- **Service honest errors** — systemctl/launchctl failures in onboarding now report the actual error. Systemd install as root uses a system unit. macOS launchctl uses `-w` flag.
- **Analytics layout** — responsive width, 2x2 grid, full-height right column with detail popup.

### 📖 Documentation

- **Migration section** — dedicated section covering both CLI migration (`opencrabs migrate`) and agent-chat migration (ask the agent to research and migrate from any tool).
- **Analytics** — documented `/analytics` command and `analytics_report` tool in TOOLS.md, README, and commands.toml.example.
- **Dynamic tools** — documented `$OPENCRABS_PARAMS`, added missing 'omit' coercion rule, expanded executor format examples.
- **SOUL.md template** — more personality and swagger.

### 🧹 Miscellaneous

- Cargo formatting normalization for telegram agent, xiaomi test, test module ordering.

## [0.3.38] - 2026-06-12

10 commits since v0.3.37. 25 files changed, +973 / -63 lines.

### 🔧 Fixes

- **Xiaomi MiMo keyless without config block** — `config_defaults` now seeds a default Xiaomi section so keyless onboarding works from a blank slate (no pre-existing `config.toml` entry needed).
- **Xiaomi MiMo tool-call parsing** — parse tool calls wrapped in `<tool_call_list>` XML that MiMo models emit, so `tool_use` succeeds instead of falling through as prose.
- **Xiaomi MiMo structured tool calls** — added a reminder to system prompts when the active model is Xiaomi MiMo so tool calls are structured JSON, not prose instructions to the user.
- **Evolve restart on Linux** — `running_binary_path()` strips the " (deleted)" marker that Linux appends to `/proc/self/exe` after unlink+rename. Restart now exec's the real binary. Evolve also hands `RestartReady` the exact new-binary path captured pre-swap.
- **Telegram peer-bot settle window** — wait ~2s of edit silence (down from 4s) before processing a peer bot's message in groups. Clears the ~1.5s edit cadence so we never act on a partial.
- **Telegram group bot handling** — hold a bot's text message in a group until its edit stream settles, then dispatch the final text. Each edit resets the settle timer so the latest frame wins. Humans, DMs, and non-text messages are unaffected.
- **Multilingual phantom self-heal** — intent-phrase matching now scans all languages at once instead of gating on `detect_language()`. Single-word verbs stay language-gated. Added missing forward-commitment shapes and filled use/write/run verb gaps in es/fr/pt/ru.
- **Brief work announcements** — short announcements like "Running checks now." or "Building now." are now caught as phantom intents.
- **Multi-sentence turn announcements** — the agent can announce work ("Checking CI status.") then do real work in the same turn. The check now returns early on announcements instead of flagging the turn as a phantom.

### 🧪 Tests

- Cover `ChannelMessageRepository::update_content` reconcile path
- Phantom intent detection across all supported languages
- Self-update path resolution with "(deleted)" marker
- Config defaults seeds Xiaomi section when none exists

### 📊 Stats

- 10 commits since v0.3.37
- 25 files changed, +973 / -63 lines

## [0.3.37] - 2026-06-12

57 commits since v0.3.36. 121 files changed, +4,676 / -10,787 lines. Closes #179, #180, #181, #182, #183, #185, #187, #189, #190, #191, #192.

**🎉 OpenCrabs × Xiaomi MiMo Collaboration:** Xiaomi MiMo is now the default provider for new users. 2 weeks completely free with keyless mode via proxy (no API key needed). Models: mimo-v2.5-pro (1M context), mimo-v2-pro, mimo-v2.5, mimo-v2-omni, mimo-v2-flash. Thinking enabled by default. After the free window (2026-06-25), users can add their own key or switch providers. Full integration with keyless proxy mode, live model fetch, automatic caching.

**Lazy tool-schema loading (3 commits):** Tool discovery via `tool_search`, core tools only by default. Reduces startup time and context usage. Flag-gated (`[agent] lazy_tools`), now enabled by default. `lazy_tools` is the default mode.

**Multi-profile cron daemon (1 commit):** One process now covers all profiles' cron jobs. No need to run separate daemons per profile.

**Background /rebuild (1 commit):** `/rebuild` now runs in the background via a one-shot cron job instead of blocking the TUI.

**Per-model cache efficiency (1 commit):** The Cache card now shows a per-model breakdown with hit rates, sorted highest-first. See at a glance which models cache well.

**Restart failure fix (closes #179, 1 commit):** Pre-built binaries now use `std::env::current_exe()` as `binary_path` instead of pointing at a never-built `target/release/opencrabs`. Source only gets cloned when `/rebuild` is invoked, via the new lazy `ensure_source_tree()`. All three restart paths (/evolve, /rebuild, auto-restart) share the same approach.

**Inactive pane markdown rendering (closes #180, 1 commit):** Inactive pane now routes content through the same `parse_markdown` + `wrap_line_with_padding` pipeline as the focused pane. No more raw `**asterisks**`, no more truncated words, no more broken list indentation.

**Local file paths in telegram_send (closes #181, 1 commit):** `telegram_send` now accepts local file paths in `send_photo`/`send_document`, not just HTTPS URLs. New `resolve_input_file()` helper: http(s) → `InputFile::url()`, anything else → tilde-expanded local path read into memory.

**Cron job profile isolation (closes #182, 2 commits):** Cron jobs are now isolated to their origin profile. Each job is stamped with its profile at creation. The scheduler skips any job whose stamp doesn't match the running process. Legacy NULL-stamped jobs still run. New migration, `job_runs_in_active_profile` predicate with unit tests. Task-local profile home override persists for the entire agent execution so all tool calls resolve to the correct profile.

**Version in /doctor output (closes #183, 1 commit):** `/doctor` now shows `Health Check (v0.3.x)` in the header on Telegram, Discord, Slack, WhatsApp. Both code paths (direct channel command and LLM-routed) covered.

**RSI efficiency gate (closes #185, 1 commit):** RSI tool proposals now require the rationale to explicitly state which efficiency gate applies: TOKEN SAVINGS, ERROR REDUCTION, or CAPABILITY ADDITION. Commands and skills exempt. 3 tests added.

**Deprecate execute_code/task_manager (closes #187, 1 commit):** `execute_code` and `task_manager` marked DEPRECATED in their tool descriptions. `rsi_proposals` removed from main tool registry (Mission Control still uses it directly). Removed from catalog.rs system category.

**CDP endpoint config (closes #189, 1 commit):** `browser.cdp_endpoint` allows connecting to a shared Chromium instance instead of spawning per-profile. Reduces memory usage in multi-profile setups (~260MB vs ~750MB for 3 profiles).

**Daemon file logs (closes #190, 2 commits):** Reliable daemon file logs + self-healing daily file writer. Recovers from lost fd mid-run.

**Disable sensitive data redaction (closes #191, 1 commit):** `agent.redact_sensitive_data = false` disables redaction for sysadmin/devops work where IPs, tokens, etc. need to be visible.

**Lock file corruption fix (closes #192, 1 commit):** `is_pid_alive(0)` returns false; corrupted lock files taken over. Fixes Telegram startup wedge.

**Bash guard fix (1 commit):** Allow `python -m` module invocations (no false-positive REPL rejection). Fixes the main reason agents fell back to `execute_code`.

**Mid-turn model switch fix (1 commit):** Mid-turn manual switch applies next turn, current request completes.

**Onboarding keyless fix (1 commit):** Keyless providers skip API key field, reach model select.

**RSI dedup fix (1 commit):** Dedup before appending to brain files — stop append→dedup→append loop.

**Retry backoff (1 commit):** Patient backoff for DNS/connection errors — stop fast-failing flaky providers.

**Context usage fix (1 commit):** Reject over-reported provider usage to prevent inflated counters.

**Telegram model picker fix (1 commit):** No longer hung on 'loading' for long model names.

**Telegram media fix (1 commit):** Pass user's caption alongside media to the agent.

**Logging self-healing (1 commit):** Self-healing daily file writer — recover from lost fd mid-run.

**Cache efficiency metric fix (1 commit):** Measures caching-capable requests only (metric B), excludes non-caching providers.

**Docs & refactor (6 commits):** Audit + fix ADDING_NEW_PROVIDERS against real pipeline. Document `[agent] lazy_tools` flag in config.toml.example. Added onboard video with captions. Centralize keyless check into ProviderSelectorState. Remove dead underscore-prefixed bindings. Drop accidental .modum-baseline.json, gitignore it.

## [0.3.35] - 2026-06-04
## [0.3.36] - 2026-06-07

76 commits since v0.3.35. The largest release to date — 130 files changed, +20,719/-2,341 lines. Closes #163, #164, #165, #167, #169, #170, #171, #174, #175, #176.

**Multi-pane live updates (closes #163, #165, 5 commits):** Background-session live-state cache for non-focused panes, inactive panes update live via background-session routing, IntermediateText/QueuedUserMessage routed into per-session delta with cleanup, Ctrl+N binds focused pane + live-refreshes footer title, prevent provider/model contamination when closing/switching panes.

**Channel UX overhaul (closes #174, #175, 9 commits):** Inline ctx budget footer into last response message across all 4 channels (Telegram, Discord, Slack, WhatsApp) instead of separate message. Telegram rolling status edit-in-place instead of delete+recreate. Telegram bot command hot-reload on config/skills change. Guard tok/s footer against burst-delivery artifacts. Warn on unknown slash commands + reject at input level (TUI). Append ctx footer to last intermediate (vanishing fix). Don't send footer-only message when text empty after dedup.

**analyze_video frame-extraction fallback (1 commit):** When Gemini video API fails (network error, upload failure), auto-extract frames via ffmpeg at 1 fps, cap at 30 frames, analyze each with Gemini vision. Combines results into chronological description.

**Cache efficiency dashboard (3 commits):** New cache efficiency card on /usage showing hit rate percentage. Persist cache_creation/cache_read tokens to messages table (migration #25). Layout fix + graceful degradation.

**Plan tool improvements (closes #169, 4 commits):** Bundle reference plan JSON files + minimal import format. `insert_after` for mid-plan insertion with renumbering. Forward-only re-test pattern documentation. Fix bundled plans compilation, serde defaults, and comprehensive tests.

**Provider/retry overhaul (10 commits):** Retry rate limits in-place (3 retries) before fallback chain. Patient backoff defaults (1s/2s/4s/8s vs 100ms hammering). Consolidate onto utils::retry, delete duplicate. Walk full fallback chain on any HTTP error. Block cross-provider model leaks at request time. Fail fast on hard-down endpoints (DNS failure, connection refused), patient on transient. Retry transient 4xx HTML infra pages + DNS-fail fast-bail. Surface retries as `RetryAttempt N/M - reason` events to user. Sync model on swap_provider_for_session to prevent contamination. Remove orphan keys.toml section on custom-provider rename + break circular cleanup.

**Near-miss tool name self-healing (closes #176, 1 commit):** When model guesses wrong tool name (e.g., `tg_send_message`), registry tries normalized match, abbreviation expansion (tg->telegram), and typo fallback before returning NotFound. Conservative: only heals on unique high-confidence match.

**Secret redaction (3 commits):** Bearer tokens, API keys, URL passwords redacted in one-line tool-call summary (TUI + DB). Query-param keys + URL passwords covered in redact_secrets. RSI TUI notifications redacted.

**TUI polish (closes #167, #170, 9 commits):** Cap thinking display to 12 lines rolling window. Git branch in footer + /sessions dialog (cyan). Active profile chip in footer. Fix dedup for cancelled/repeated requests. Show dedup content in Mission Control detail popup. Preserve intermediate text when strip_llm_artifacts zeros it out. Render each reasoning-only iteration as its own Thinking row. Dedup repeated user messages during network failures. Atomic provider+model swap (27 call sites updated, prevents footer desync).

**CI overhaul (1 commit):** Gate on PR + main push, not tags. Kills double-build on tag events (ci.yml + release.yml both compiled). Routine main pushes: lint + Linux test only (~5-8 min). Cross-platform build gated to PRs + manual dispatch.

**RSI + brain hardening (closes #164, #166, 9 commits):** Respect [providers.fallback] chain. Pruned-sidecar tracker prevents sync from re-adding deleted sections. Per-file line cap in sync_templates with bail and warn. Purpose-order canonical + N-1 dedup proposals + stub warnings. Strip empty header stubs at read time. Close silent error drops and _-hides. Exclude sentinel dimensions from opportunity generation. Add awareness of custom .md reference files to prompt. Stop directing tool failure suggestions to TOOLS.md.

**Refactors + tests + docs (closes #171, 8 commits):** Slim TOOLS.md template from 660 to 56 lines + regression tests. Expose params via OPENCRABS_PARAMS env var for structured data. Contributing docs: cargo run vs release, test placement, atomic commits. Bundled plans docs, plan re-test pattern. Bring test count + TESTING.md current for v0.3.36. Tests: telegram helpers, RSI fallback chain, evolve systemd, BRAIN_PREAMBLE cross-ref, proxy detection.

**Provider fixes (1 commit):** Stop /models per-field save from corrupting last-active custom section.

**Fixes:** Message dedup for cancelled/repeated requests. Dedup repeated user messages during network failures. apply_brain_dedup removes ALL occurrences instead of just the first. Cache card layout and graceful degradation. Evolve: check user-level systemd units when scheduling restart.

**Config docs (1 commit):** Document self_improvement_provider/model config keys.


22 commits since v0.3.34. Quality-of-life release closing #152, #153, #155, #156, #157, #159, #161. Alexey Leshchenko (`leshchenko1979`) contributed dynamic shell tool single-quote escaping (#153) and plan import from JSON files (#160); the remaining 20 commits added `opencrabs evolve` CLI, per-call provider/model overrides for subagents, profile-aware paths, phantom detector hardening, IDENTITY.md consolidation, and teloxide 0.13 → 0.17 upgrade.

**Plan import (closes #160, 1 commit):** New `import` operation loads pre-defined plans from JSON files. Security: target-only symlink check (rejects `/var` ancestor false positive on macOS), explicit orphan dependency validation.

**Per-call subagent overrides (closes #152, 1 commit):** `spawn_agent`, `resume_agent`, and `team_create` now accept optional `provider` and `model` fields that override config defaults for a single call. Enables mixed-model teams (e.g., plan with GLM, code with Deepseek, review with Kimi).

**`opencrabs evolve` CLI (1 commit):** Terminal command to check for and install updates, matching the existing `/evolve` TUI slash command. Supports `--check-only` flag.

**IDENTITY.md removal (closes #159, 2 commits):** Dropped redundant IDENTITY.md template and all brain file references. SOUL.md already owns identity (name, vibe, boundaries), so the separate file was duplication. PR #159 closed.

**Profile-aware paths (closes #155, #156, #157, 3 commits):** Replaced hardcoded `~/.opencrabs/` paths with `opencrabs_home()` throughout. Subagent status dir and tools.toml fallback now profile-aware. `write_opencrabs_file` confirmations show actual resolved paths.

**Phantom detector hardening (2 commits):** Re-engaged self-heal on forward intent after successful tool calls. Cleaned up destructive verb intent phrases across all five languages (EN, ES, FR, PT, RU).

**RSI cycle_number persistence (1 commit):** `cycle_number` now persists to `~/.opencrabs/rsi/cycle_number` so the dedup scan (every 24 cycles) fires correctly across TUI restarts.

**Telegram polish + teloxide upgrade (3 commits):** Upgraded teloxide 0.13 → 0.17 with member join detection before allowlist. Marathon-bucket rolling status now rotates through project-author quip pool. Join detection notification tests added.

**tok/s footer parity (1 commit):** Channel context budget footers now match TUI with `| N tok/s` using provider-reported tokens divided by active streaming time.

**Qwen tool-call leak strip (1 commit):** Stripped bare `{"name":...,"arguments":...}` JSON tool-call leaks from Qwen content text via new `bare_tool_call_extractor`.

**Shell tool single-quote escape (closes #153, PR #153):** Dynamic shell tool params now properly escape single quotes.

**CI release-only trigger (1 commit):** CI now runs only on release tags (`v*`), coverage workflow on main pushes.

**Team agent spawn fix (closes #161, 1 commit):** Added missing `mark_awaiting_input` in team agent spawn loop so the TUI correctly reflects waiting state.

Closes #152, #153, #155, #156, #157, #159, #161.

82 files changed, +3,358/-776.

PLAN IMPORT (1 commit, closes #160)
- 188d188c feat(plan): add import operation — load pre-defined plan from JSON file
- 0c10caff fix(tools): narrow symlink check to target-only, add orphan dep validation

PER-CALL SUBAGENT OVERRIDES (1 commit, closes #152)
- 9018b595 feat(subagent): per-call provider/model override on spawn/resume/team_create

EVOLVE CLI (1 commit)
- 2d1871e7 fix(cli): add evolve subcommand to check for and install updates

IDENTITY.MD REMOVAL (2 commits, closes #159)
- e816206e refactor(brain): drop IDENTITY.md — SOUL.md already owns identity
- ce857664 refactor(brain): remove IDENTITY.md template and fix compile errors

PROFILE-AWARE PATHS (3 commits, closes #155, #156, #157)
- c21e2b2e fix: replace hardcoded ~/.opencrabs/ paths with profile-aware opencrabs_home()
- bb91f4da fix(profiles): profile-aware subagent status dir + tools.toml fallback
- 0367a906 fix: show actual resolved path in write_opencrabs_file confirmation messages

PHANTOM DETECTOR (2 commits)
- 471e2663 fix(phantom): re-engage self-heal on forward intent after a successful tool call
- d1f9b39b fix(phantom): cleanup / destructive verb intent phrases across all five languages

RSI PERSISTENCE (1 commit)
- 5729232f fix(rsi): persist cycle_number across restarts so dedup scan actually fires

TELEGRAM + TELOXIDE (3 commits)
- 214200ba upgrade teloxide 0.13 → 0.17 and add member join detection before allowlist
- 82780614 fix(telegram): rotate marathon-bucket rolling status through project-author quip pool
- 8628a716 test: add join detection notification tests

TOK/S FOOTER (1 commit)
- 3c398a77 fix(tok/s): provider-reported tokens / active streaming time, shared by TUI + channels

QWEN TOOL-CALL STRIP (1 commit)
- 201e85d8 fix(qwen): strip bare `{"name":...,"arguments":...}` tool-call leaks from content text

SHELL SINGLE-QUOTE ESCAPE (1 commit, closes #153)
- 8f98862d fix: escape single quotes in dynamic shell tool params

CI RELEASE-ONLY (1 commit)
- 109c53af fix(ci): CI triggers only on release tags, coverage on main pushes

TEAM SPAWN FIX (1 commit, closes #161)
- 17264333 fix(team): add missing mark_awaiting_input in team agent spawn loop

HOUSEKEEPING (3 commits)
- 11953732 chore: remove unrelated .codegraph dir from PR #156 merge
- 7d7b3a9b style: apply cargo fmt formatting
- (fmt fixup staged with release)
## [0.3.34] - 2026-06-02

39 commits since v0.3.33. Stability and UX release closing #141, #142, #147, #148, #149, #151, #152. Alexey Leshchenko (`leshchenko1979`) contributed the file_extract double-extension fix (#146) and RSI counter cleanup (#150); the remaining 37 commits layered brain dedup automation, follow-up question polish across all channels, provider registry hardening, skill description injection, RTK sysadmin expansion, phantom detector hardening, channel footer tok/s parity, Claude CLI auto-learning, fallback provider cascade coverage, and subagent config documentation on top.

**Brain file dedup scan (closes #147, 6 commits):** New RSI proposal kind that scans all 11 brain files daily, clusters duplicate lines (minimum 10 chars, skips structural markdown like headings and separators), and files dedup proposals into Mission Control. Soft purple badge in the inbox, runs every 24 RSI cycles (about once per day at 1-hour intervals), never auto-applies. Human approval required through the existing `rsi_proposals` apply/reject flow. Core scan logic in `dedup_scan.rs` (393 lines), hooked into the RSI cycle with periodicity gating, 14 regression tests covering empty files, short-line filtering, cross-file detection, proposal format, and canonical selection.

**follow_up_question race fix (closes #142, 5 commits):** All four channels (Telegram, Discord, Slack, WhatsApp) now flush intermediate text handles before presenting the follow-up keyboard. Prevents the race where the bot's in-progress message gets orphaned or duplicated when the user taps a button mid-stream. Each channel got its own atomic commit with per-channel regression tests pinning the flush-before-keyboard sequence.

**follow_up_question display polish (closes #148, 3 commits):** Telegram keyboard is now single-column with a 40-character label cap (rejects options longer than 40 chars in the tool validator with a clear error). Rolling "Running follow_up_question (16s)" status is suppressed while the keyboard is pending, and the LLM is now instructed to call the tool silently without echoing the question text in surrounding prose. Discord left alone due to its 5-ActionRow-per-message hard limit.

**Provider registry cleanup (closes #141, 1 commit):** Dropped hardcoded if-else provider ladders in favor of a single registry source of truth. The registry correctly requires `api_key` for API providers (anthropic, openai, github, gemini, openrouter, minimax), so resolution skips them when keys are missing instead of silently falling back. Follow-up test fix added dummy API keys to 6 test helpers across `provider_sync_test.rs` and `github_provider_test.rs` that were creating `ProviderConfig` without keys.

**RSI decorative counters (closes #149, 2 commits):** PR #150 (leshchenko1979) removed the counter-bumping logic that incremented inline counters in SOUL.md like `phantom_tool_call: 219`. These counters were decorative only, nothing read them, and the real canonical source is the SQLite feedback ledger at `~/.opencrabs/feedback.db`. Counters went stale (ledger showed 302, SOUL.md showed 219) and got wiped by upstream template sync. Replaced with evidence appends (date/session). DB stays the single source of truth. Follow-up commit escaped unescaped double quotes the PR introduced in the prompt string literal and added text regression tests verifying the prompt no longer contains counter-bumping instructions.

**File extract fix (closes #146, 2 commits):** PR #146 (leshchenko1979) collapsed double extensions from forwarded messages (e.g., `document.pdf.pdf` → `document.pdf`). Follow-up commit corrected the `collapse_double_extension` logic to properly rebuild the filename by checking the stem, not just the extension.

**Error handling and persistence (2 commits):** Agent failures now persist as permanent chat bubbles with actionable wording on TUI and channels instead of vanishing. UTF-8 panic after redact-prefix scan fixed by snapping to char boundary.

**TUI and prompt fixes (4 commits):** Unescape literal `\n` sequences in tool-arg display so newlines render correctly. Gated `unescape_display_string` re-export with `#[cfg(test)]` to stop clippy unused-import warnings in non-test builds. Rewrote FINISHING A TURN prompt to split side-effect vs analysis responses and nudge on empty data-fetch closes. Typed model name overrides in onboard flow plus added MiniMax-M3 to the suggestion list.

**Phantom tool call fix (1 commit):** Stopped `phantom_tool_call` from firing on completion acks after successful tool runs.

**Baseline additive merge (1 commit):** Additive merge so new models reach users without manual config edits.

**Skill description injection (closes #151, 1 commit):** Skill descriptions were documented in TOOLS.md as LLM auto-invoking triggers but were never actually injected into the system prompt, so the LLM could not auto-invoke from description. Added `push_skills_section()` to `prompt_builder.rs` that loads all skills via `crate::brain::skills::load_all_skills()` and formats each as `- skill_name: description`, appending an `## Available Skills` block to both `build_core_brain()` and `build_system_brain()`. 2 regression tests.

**RTK sysadmin expansion (1 commit):** Closed the gap vs fast-rlm's 69.9% token reduction. Added 11 sysadmin commands (`ps`, `top`, `lsof`, `netstat`, `ss`, `journalctl`, `dmesg`, `dig`, `nslookup`, `host`, `traceroute`) that were bypassing RTK entirely. Bundled `rtk_filters.toml.example` with 8 conservative starter rules (ps-aux-compact, lsof-i-compact, netstat-compact, journalctl-recent, dmesg-recent, git-log-oneline-cap, git-diff-cap, gh-pr-list-cap, dig-answer-only). 6 sentinel tests pin the list shape.

**Phantom detector hardening (2 commits):** Two narration shapes leaked past the phantom detector: pronounless deferment (`Need to read the X`) and bare gerunds (`Reading the current state of the affected files`). Added 28 pronounless EN variants, 15 telegraphic FR `besoin de` variants, and gerund+determiner bigrams. A new regression file pins both leaked sentences verbatim. Follow-up fixed French accent detection: `detect_language` missed `é/è/ë/ü`, so French narration fell through to English and the new `besoin de` phrases never matched. Added the 4 markers.

**tok/s in channel footers (2 commits):** Channel context budget footers showed only `ctx: XK/YK Z%` while the TUI also showed `| N tok/s`. Added `tokens_per_second: Option<f64>` to `AgentResponse`, extended `format_ctx_footer` to accept a third `tps` parameter, computed tok/s from `total_output_tokens / turn_duration` across the whole turn in `tool_loop.rs`, and wired it through all four channels (Telegram, Discord, Slack, WhatsApp). Second commit is `cargo fmt` wrapping the long call sites.

**Claude CLI model auto-learn (1 commit):** Footer showed Opus 4.7 after Anthropic shipped 4.8 because `default_for_alias` hardcoded `opus -> opus-4-7`. Now the provider learns the CLI-resolved version from `message_start` events, persists to `~/.opencrabs/claude_cli_models.json` (rewriting only when the value changes), and the TUI refreshes the session model live so the footer self-corrects to the actual version without code changes. `default_for_alias` prefers the learned cache and falls back to a build-time seed only on a fresh install. The SSE model is normalized to the short form so display, writeback, and pricing agree.

**Fallback provider cascade (closes #152, 1 commit):** `/models` swaps and session restores were storing a raw provider instead of wrapping it in `FallbackProvider`, so the fallback cascade could not fire on 5xx/429 errors after a model switch. Every active provider now gets wrapped unless it is already a chain or no fallbacks are configured. 174-line integration test simulating 5xx cascades across swapped providers.

**Subagent config docs (1 commit):** `spawn_agent`, `resume_agent`, and `team_create` tool descriptions now mention `subagent_provider` and `subagent_model` config keys so the LLM knows these exist instead of suggesting users set them per-call. README rewritten with a dedicated section. 91-line test verifies the descriptions contain the config key names.

Closes #141, #142, #146, #147, #148, #149, #151, #152.

80 files changed, +5,010/-398.

BRAIN FILE DEDUP SCAN (6 commits, closes #147)

New RSI proposal kind that scans all 11 brain files daily, clusters duplicate lines, and files dedup proposals into Mission Control. Soft purple badge, runs every 24 RSI cycles, never auto-applies. Human approval required. 14 regression tests.

- 49a25af5 feat(rsi): add BrainDedup proposal kind for mission control inbox
- 0555a514 feat(rsi): add brain file dedup scan core logic
- 09a848f9 feat(rsi): add file_dedup_proposals to write scan results into ProposalsStore
- 404246b4 feat(rsi): hook brain dedup scan into RSI cycle with daily periodicity
- 6a916428 test(rsi): add regression tests for brain dedup scan and proposal generation
- 3af08075 fix(rsi): register dedup_scan module, fix compilation and test imports

FOLLOW_UP_QUESTION RACE FIX (5 commits, closes #142)

All four channels flush intermediate text handles before presenting the follow-up keyboard. Prevents orphaned or duplicated messages when the user taps a button mid-stream. Per-channel regression tests.

- 52c3702a fix(channels/telegram): flush intermediate text before follow_up_question to prevent race
- 6c7221be fix(channels/discord): add intermediate text handling and flush before follow_up_question
- 59e55ce1 fix(channels/slack): flush intermediate handles before follow_up_question to prevent race
- 281505d2 fix(channels/whatsapp): add intermediate handle tracking and flush before follow_up_question
- 3dfc5dc3 test(channels): add regression tests for follow_up_question intermediate flush

FOLLOW_UP_QUESTION DISPLAY POLISH (3 commits, closes #148)

Telegram keyboard is now single-column with a 40-char label cap. Rolling status suppressed while keyboard pending. LLM instructed to call tool silently without echoing question text.

- dc050ad7 fix(follow_up_question): single-column Telegram layout, 40-char label cap
- 05580314 fix(telegram): suppress status messages while follow_up_question is pending
- a08bd7ef docs(follow_up_question): instruct LLM to call tool silently

PROVIDER REGISTRY CLEANUP (1 commit, closes #141)

Dropped hardcoded if-else provider ladders in favor of a single registry source of truth. Registry correctly requires api_key for API providers.

- d5e96c35 fix(config): drop hardcoded provider ladders, single registry source of truth

RSI DECORATIVE COUNTERS (2 commits, closes #149)

PR #150 (leshchenko1979) removed counter-bumping logic. SQLite feedback ledger is the canonical source. Follow-up escaped unescaped quotes and added text regression tests.

- 4d34e60c fix(brain): stop RSI agent from bumping decorative SOUL.md counters
- 77a4d57d fix(rsi): escape prompt quotes and add text regression tests

FILE EXTRACT FIX (2 commits, closes #146)

PR #146 (leshchenko1979) collapsed double extensions from forwarded messages. Follow-up corrected filename rebuild logic.

- 2ecf7385 fix(file_extract): collapse double extension from forwarded messages
- fb5ad506 fix(file_extract): correct collapse_double_extension logic to properly rebuild filename

ERROR HANDLING AND PERSISTENCE (2 commits)

Agent failures now persist as permanent chat bubbles with actionable wording. UTF-8 panic after redact-prefix scan fixed.

- 7256f666 fix(errors): persist agent failures as permanent chat bubbles + actionable wording on TUI and channels
- 1341212e fix(sanitize): snap to char boundary after redact-prefix scan to stop UTF-8 panic

TUI AND PROMPT FIXES (4 commits)

Unescape literal \n in tool-arg display. Gated unescape_display_string re-export. Rewrote FINISHING A TURN prompt. Typed model name overrides plus MiniMax-M3.

- bc99cb82 fix(tui): unescape literal \n sequences in tool-arg display
- 1008a06e fix(tui): gate unescape_display_string re-export with cfg(test)
- 1a2b9c90 fix(agent): split FINISHING A TURN into side-effect + analysis, nudge on empty data-fetch close
- 54d4d44c fix(prompt): rewrite FINISHING A TURN to require acknowledgement, never silent close
- 8912500c fix(onboard): typed model name overrides suggestion list + add MiniMax-M3

PHANTOM TOOL CALL FIX (1 commit)

Stopped phantom_tool_call from firing on completion acks.

- e843f405 fix(phantom): stop firing on completion acks after successful tool runs

BASELINE ADDITIVE MERGE (1 commit)

Additive merge so new models reach users without manual config edits.

- addd1956 fix(baseline): additive merge so new models reach users without manual config edits

TEST INFRASTRUCTURE (1 commit)

Added dummy API keys to ProviderConfig test helpers after registry cleanup.

- ce963cac fix(tests): add dummy api_key to ProviderConfig helpers after registry cleanup

SKILL DESCRIPTION INJECTION (1 commit, closes #151)

Skill descriptions were documented in TOOLS.md as LLM auto-invoking triggers but were never actually injected into the system prompt. Added `push_skills_section()` to `prompt_builder.rs` that loads all skills and formats each as `- skill_name: description`, appending an `## Available Skills` block to both `build_core_brain()` and `build_system_brain()`. 2 regression tests.

- f8ae7ac5 fix(brain): inject skill descriptions into system prompt

RTK SYSADMIN EXPANSION (1 commit)

Closed the gap vs fast-rlm's 69.9% token reduction. Added 11 sysadmin commands (`ps`, `top`, `lsof`, `netstat`, `ss`, `journalctl`, `dmesg`, `dig`, `nslookup`, `host`, `traceroute`) that were bypassing RTK entirely. Bundled `rtk_filters.toml.example` with 8 conservative starter rules. 6 sentinel tests.

- fa821ed0 fix(rtk): expand RTK_SUPPORTED_COMMANDS with sysadmin family + bundle filter template

PHANTOM DETECTOR HARDENING (2 commits)

Two narration shapes leaked past the phantom detector: pronounless deferment (`Need to read the X`) and bare gerunds (`Reading the current state...`). Added 28 pronounless EN variants, 15 telegraphic FR `besoin de` variants, and gerund+determiner bigrams. Follow-up fixed French accent detection: `detect_language` missed `é/è/ë/ü`, so French narration fell through to English and the new `besoin de` phrases never matched.

- 49a2ba3b fix(phantom): catch pronounless Need to X + bare-gerund openers
- 8b972771 fix(phantom): add é/è/ë/ü to French marker set so besoin de phrases match

TOK/S IN CHANNEL FOOTERS (2 commits)

Channel context budget footers showed only `ctx: XK/YK Z%` while the TUI also showed `| N tok/s`. Added `tokens_per_second` to `AgentResponse`, extended `format_ctx_footer`, computed tok/s from `total_output_tokens / turn_duration` across the whole turn, and wired it through all four channels. Second commit is `cargo fmt` wrapping the long call sites.

- 17aa6abb fix(channels): add tok/s to context budget footer to match TUI
- 01222300 style(channels): wrap long format_ctx_footer call sites

CLAUDE CLI MODEL AUTO-LEARN (1 commit)

Footer showed Opus 4.7 after Anthropic shipped 4.8 because `default_for_alias` hardcoded the version. Now the provider learns the CLI-resolved version from `message_start`, persists to `~/.opencrabs/claude_cli_models.json`, and the TUI refreshes the session model live so the footer self-corrects without code changes.

- 60127ee0 fix(claude-cli): learn resolved model version from the CLI, stop hardcoding it

FALLBACK PROVIDER CASCADE (1 commit, closes #152)

`/models` swaps and session restores stored a raw provider instead of wrapping in FallbackProvider, so fallback cascade could not fire on 5xx/429 after a model switch. Every active provider now gets wrapped unless already a chain or no fallbacks configured.

- e39cd7c3 fix(fallback): wrap per-session providers in FallbackProvider so /models swaps keep cascade coverage

SUBAGENT CONFIG DOCS (1 commit)

spawn_agent, resume_agent, and team_create tool descriptions now mention subagent_provider and subagent_model config keys. README rewritten with dedicated section.

- bf62682b docs(subagent): surface subagent_provider/subagent_model in tool descriptions (closes #152)

## [0.3.33] - 2026-05-31

2 commits since v0.3.32. Patch release closing #138. The v0.3.32 tag
was published to crates.io before PR #140 and its sentinel test could
land, so this version bumps to ship them properly. crates.io 0.3.32
remains as-is (immutable, not yanked) — anyone on it gets the older
user_correction metadata; upgrade to 0.3.33 to pick up the fix.

**User-correction feedback (closes #138):** PR #140 (leshchenko1979)
fixed the user_correction feedback-ledger metadata path. The previous
code captured the first 200 chars of the channel-prefixed agent input.
On Telegram the wrapper prefix alone is 236 chars (`[Channel: Telegram
— ...]\n`), so 26+ ledger entries stored pure channel boilerplate and
RSI analysis could not see what users were actually correcting. The
fix prefers `display_text_override` (the clean message channels
already pass for DB persistence) and falls back to `user_message` for
TUI / CLI sessions. Follow-up sentinel tests pin the chain shape
(whitespace-normalised so rustfmt drift doesn't false-fail), include
a scoped negative assertion against the regression pattern, and a
documentation anchor on the Telegram prefix size.

Closes #138.

4 files changed, +185/-32.

USER-CORRECTION FEEDBACK (2 commits, closes #138)

- a3c3e663 fix(user-correction): capture actual user message in metadata, not channel prefix
- 8b0835b4 test(user-correction): sentinel for PR #140 fix + rustfmt normalisation

## [0.3.32] - 2026-05-31

8 commits since v0.3.31. Hardening release closing #135, #136. Alexey
Leshchenko (`leshchenko1979`) contributed the foundation via 2 PRs
(#135, #137); the other 6 commits layered observability, pre-flight
safety checks, sentinel tests, and changelog work on top.

**Evolve / self-update (closes #136):** PR #137 (leshchenko1979)
replaced `std::fs::rename` with a remove-then-rename pair so Linux
no longer blocks the swap on busy binaries, and scheduled a delayed
`systemd-run` restart after the swap so the running daemon picks up
the new inode. Follow-up commits added: structured tracing on every
failure branch (GitHub API, download, filesystem, rollback) with
status codes, rate-limit headers, target paths, and body excerpts,
replacing the generic "rate limited or unavailable" suffix that
masked 404s, 5xxs, and genuine rate limits alike; logged outcomes
on the two `let _` discard sites (`remove_file` + `systemd-run`
spawn) so the most user-visible regression mode — agent says
"Evolved!" while the daemon never restarts — leaves a forensic
trail; a pre-flight `count_matching_systemd_units` check that
skips the spawn entirely when zero units match the glob and tells
the user to restart manually instead of lying "Restarting..." when
nothing was actually scheduled; and 5 sentinel tests pinning the
systemd-run arg list (flag drift would silently break the restart;
`--collect` / `--quiet` must stay out for RHEL 7 compat).

**Stream / sanitize:** caught a Qwen 3 regression where the model
echoed its own SentencePiece `<|tool▁...|>` markers (U+2581
word-boundary chars that render as `_` in most fonts) into
`content` alongside the proper `tool_calls` field. Four new opener
patterns plus a regex safety net in `strip_llm_artifacts` catch
the whole marker family and any future variants.

**CI / release workflow (closes #135):** PR #135 (leshchenko1979)
made CI skip on docs-only changes and the release workflow walk
ancestors to find the last green `CI Required Gate` check, so a
docs-only commit no longer blocks a release.

**Docs / changelog:** restructured v0.3.31 with categorized
sections, fixed em-dashes in v0.3.30 prose, normalized URL refs,
and broke both wall-of-text entries into themed paragraphs for
readability.

Closes #135, #136.

12 files changed, +1,557/-268.

EVOLVE / SELF-UPDATE (4 commits, closes #136)

Hardening pass after a user hit a silent no-op restart on systemd.
`std::fs::rename` replaced with remove+rename so busy binaries on Linux
no longer block the swap. Delayed `systemd-run` restart scheduled after
the swap. Pre-flight unit-count check skips the spawn when zero units
match the glob. Every failure branch emits structured tracing.

- 1f817de1 fix(evolve): replace rename with remove+rename and add systemd delayed restart
- 824d454b fix(evolve): honest error branching + structured tracing on every failure path
- 923bf51c fix(evolve): log failures of remove_file + systemd-run instead of swallowing them
- 732c97c6 fix(evolve): pre-flight unit-count check + honest message + restart-arg tests

STREAM / SANITIZE (1 commit)

Qwen 3 SentencePiece `<|tool▁...|>` marker family (U+2581 boundary
chars) stopped from leaking to channels via four new opener patterns
plus a regex safety net in `strip_llm_artifacts`.

- cdb92580 fix(stream/sanitize): stop Qwen `<|tool▁...|>` marker family from leaking to channels

CI / RELEASE WORKFLOW (1 commit, closes #135)

CI skips on docs-only changes. Release workflow walks ancestors for the
last green `CI Required Gate` check so docs-only commits no longer block
a release.

- 7b077928 ci: skip CI for docs-only changes; release: walk ancestors for last green (#135)

DOCS / CHANGELOG (2 commits)

v0.3.31 restructured with categorized sections, v0.3.30 em-dashes
fixed, URL refs normalized, and both wall-of-text entries broken into
themed paragraphs.

- 9fc537fc docs(changelog): restructure v0.3.31 with categorized sections, fix v0.3.30 em-dashes, normalize URL refs
- 9581d4c2 docs(changelog): break v0.3.30 + v0.3.31 wall-of-text into themed paragraphs

## [0.3.31] - 2026-05-30

48 commits since v0.3.30. Minor release closing issues #130, #131, #132.

**Telegram routing (closes #130, #131):** forum topic routing was broken —
proactive sends and startup resumes landed in general chat instead of the
originating topic, and replies to messages within a topic also routed to
general chat. Both reactive and proactive paths now carry `thread_id`
through the full send pipeline, and a new `list_topics` action surfaces
the (thread_id, topic_name) pairs so the agent can translate
`#announcements` into the numeric ID. Reply context now extracts the
quoted span from `reply_to_message` entities so the agent sees which
specific paragraph the user highlighted.

**RSI feedback ledger (closes #132):** had no visibility into which bash
commands the agent actually ran. Command text is now appended to event
metadata with a subsystem classifier tagging each command by category,
and successful patterns surface as tool / command / skill proposals in
Mission Control.

**PDF / document parsing:** full overhaul. New `page_range` parameter
accepts `"1-30"`, `"5,7,10-15"` spans so the agent can target specific
pages without burning tokens on the whole document. Text-first routing
skips Gemini vision entirely for text-native PDFs (fixes the 10 MB /
HTTP 413 incident). Inline cap raised from ~20 to ~60 pages with the PDF
saved to disk for remainder fetching. Partial vision renders are
preserved when `pdftoppm` fails mid-loop, and the `pdftoppm` loop is
bounded to the actual page count.

**Agent self-awareness:** compiled features surface in the system prompt
with a check-first directive so the agent stops reimplementing built-ins.
A `Known paths` section ensures "check the logs" always lands at
`~/.opencrabs/logs/opencrabs.YYYY-MM-DD`.

**Telegram UX:** pre-tool status line is fully context-aware — names the
running tool with elapsed time, surfaces a live reasoning excerpt while
the model is thinking, or rolls a phrase anchored on the user's own
message with a leading verb that escalates across elapsed buckets when
no other signal is available yet. Plan-tool summaries render as
monospace `<pre>` panels.

**Prompt safety:** hallucinated `CODE_EDIT_BLOCK` fences are stripped
before channel delivery. IDE-style inline edit formats (Cursor
`search_and_replace`, Aider conflict markers, unified-diff dumps) are
explicitly forbidden in `BRAIN_PREAMBLE`. The Qwen 3 / DeepSeek
SentencePiece-style `<|tool▁...|>` marker family is captured by the
streaming filter and post-stream sanitizer, fixing a leak that could
send hundreds of `<|tool▁calls_section_end|>` tokens to channels when
the model degraded into a repeat loop.

**Usage dashboard:** consolidated all Qwen 3.7 Max variants (including
`qwen-latest-series` family) into a single parent row with indented
breakdown showing genuinely distinct variants. All model names
normalized to kebab-case lowercase.

**TUI / onboarding:** provider list no longer re-indents on Up/Down.
`/onboard:provider` and `/models` stopped truncating on small terminals.
Plan checklist grew to ~10 visible tasks with scrolling. The tok/s
footer now counts reasoning chunks toward the live rate.

**RSI:** gained `skill` as a third proposal kind (alongside tool and
command) with an apply path that writes a `SKILL.md` brain file.

**Provider stability:** fallback only sticks after 4 consecutive rescues
(not on first incident). Missing-[DONE] markers are accepted when the
text-only response looks complete.

**CI / build:** `[profile.ci]` tuned for wall-clock instead of runtime
perf. `Cargo.lock` committed for reproducible builds. Stage-gated
workflow with Linux-only tests and main-only coverage. Release workflow
`wait-for-ci` gates on the stable `CI Required Gate` check (was
`Test (ubuntu-latest)`, which broke silently after the test job was
renamed to `Test (Linux)`).

**Compaction:** defaults to the fun POST-COMPACTION PROTOCOL prompts
(users have called out the in-character one-liners after recovery as a
delight feature) with a new `[agent] silent_compaction = true` opt-out
in `config.toml` for formal / customer-facing deployments where
mid-session profanity would be inappropriate. All four compaction sites
(regular async, mid-loop, emergency, post-tool) route through a single
`compaction_prompts` module so the fun and silent variants stay
byte-for-byte aligned.

**Tool routing:** explicit pick-me-when guidance added across the search
and browser tool descriptions — `web_search` announces itself as the
DEFAULT research tool, `exa_search` / `brave_search` announce
"PREFERRED over web_search" with their respective strengths,
`browser_navigate` is framed as last-resort and forbidden for research
or GitHub, and `bash` calls out the `gh` CLI as the GitHub surface with
`--json` / `--jq` examples. A new WEB / GITHUB / BROWSER ROUTING block
in `BRAIN_PREAMBLE` puts the rule in the system prompt every turn.

Closes #130, #131, #132.

94 files changed, +18,690/-592.

PDF / DOCUMENT PARSER OVERHAUL (6 commits)

New `page_range` parameter accepts `"1-30"`, `"5,7,10-15"` spans so
the agent can target specific pages without burning tokens on the
whole document. Text-first routing skips Gemini vision entirely for
text-native PDFs, fixing the 10 MB / HTTP 413 incident. Inline cap
raised from ~20 to ~60 pages with the PDF saved to disk. Partial
vision renders preserved when `pdftoppm` fails mid-loop. The
`pdftoppm` loop bounded to actual page count from the PDF trailer.

- f3a4095e feat(doc_parser): page_range param + agent-facing guidance for paginated PDFs
- 0eaee829 fix(pdf): text-first routing + lazy per-page vision (closes 10 MB / 413 incident)
- 1e46cd26 fix(pdf): raise inline cap to ~60 pages + save PDF so agent can fetch the rest
- c40f03f5 fix(pdf): keep partial vision renders + bound pdftoppm loop to actual page count
- a9b0d399 docs(tools): document page_range + fix wrong pages-as-string hint
- 62baba8d test(pdf): 25 direct cases for parse_page_range + normalise spaces around dash

TELEGRAM FORUM TOPICS + UX (10 commits, closes #130, #131)

Both proactive and reactive forum topic routing fixed: sends, startup
resumes, and replies carry `thread_id` through the full pipeline so
they land in the originating topic. A new `list_topics` action
surfaces (thread_id, topic_name) pairs. Reply context extracts the
quoted span from `reply_to_message` entities. Pre-tool status line is
context-aware: tool name + elapsed, live reasoning excerpt, or rolling
phrase anchored on user input. Plan-tool summaries render as monospace
`<pre>` panels. The invented `THINKING_QUIPS` array was deleted.

- 82362383 fix(telegram): route proactive sends and startup resumes into forum topic (closes #130)
- ce0d2597 fix(telegram): route forum-topic replies back to the originating topic (closes #130, reactive path)
- 066a54de feat(telegram): capture forum topic names + list_topics action (#130 follow-up)
- 5120d79c feat(telegram_send): expose optional thread_id override for proactive sends
- 93ee4004 fix(telegram): surface user-highlighted quote in reply context (closes #131)
- e035de24 fix(telegram): make pre-tool status line dynamic instead of hardcoded "Thinking through this..."
- f6d915e3 fix(telegram): render plan-tool summary as monospace panel via <pre>
- 60a0fef1 fix(telegram): remove the invented THINKING_QUIPS fallback, silence > filler
- 8399d84e fix(telegram): restore pre-tool rolling status, anchored on user input
- 02dd7fb0 chore(telegram): log quote-reply context construction for #131 diagnosis

PROMPT SAFETY (3 commits)

Hallucinated `CODE_EDIT_BLOCK` fences stripped from streamed output
before channel delivery. IDE-style inline edit formats (Cursor-style
`search_and_replace`, Aider conflict markers, unified-diff dumps with
file headers) explicitly forbidden in `BRAIN_PREAMBLE`. Qwen
`<|tool...|>` marker family stripped before channel delivery.

- b339a8c7 fix(sanitize): strip hallucinated CODE_EDIT_BLOCK fences before channel delivery
- 42ada9ed feat(prompt): forbid IDE-style inline edit formats in BRAIN_PREAMBLE
- 0f909959 fix(stream/sanitize): stop Qwen `<|tool...|>` marker family from leaking to channels

AGENT SELF-AWARENESS (2 commits)

Compiled features surfaced in the system prompt with a check-first
directive so the agent stops reimplementing built-ins like local
STT/TTS. A `Known paths` section ensures "check the logs" lands at
`~/.opencrabs/logs/opencrabs.YYYY-MM-DD` instead of guessing.

- b17f06c1 feat(prompt): surface compiled features + check-first directive so agent stops reimplementing built-ins
- b19f1c7d feat(prompt): add Known paths section so "check the logs" lands at the right file

USAGE DASHBOARD (3 commits)

Consolidated all Qwen 3.7 Max variants (including `qwen-latest-series`
family) into a single parent row with indented breakdown showing
genuinely distinct variants (35B-A3B style). All model names normalized
to kebab-case lowercase. Cosmetic-alias variants suppressed from the
breakdown tree.

- 9362c385 fix(usage): consolidate qwen-3.7 family and normalize all model names to kebab-case
- a6196b6c fix(usage): group qwen-latest-series variants under qwen-3.7-max with indented breakdown
- a3b722b3 fix(usage): suppress cosmetic-alias variants from /usage breakdown tree

RSI / SELF-HEAL (5 commits, closes #132)

Bash command text appended to feedback ledger event metadata. A
subsystem classifier tags each command by category (git, cargo, docker,
ssh, fs, net, build, test, etc.). Successful patterns surface as
tool / command / skill proposals in Mission Control. Skill proposals
are a new third kind alongside tool and command, with an apply path
writing a `SKILL.md` brain file.

- 2b4d7c86 feat(rsi): append bash command text to feedback ledger metadata (closes #132)
- d2805778 feat(rsi): bash command subsystem classifier for pattern aggregation
- 5d523530 feat(rsi): detect successful bash subsystem patterns + propose tool/skill extraction
- 8c9d959b feat(rsi): add skill as a third proposal kind alongside tool / command
- 3ba6a8ae fix(rsi): surface skill proposals in Mission Control + add apply path (SKILL.md writer)

TUI / ONBOARDING (4 commits)

Provider list stopped re-indenting on every Up/Down keypress.
`/onboard:provider` and `/models` stopped truncating last fields on
small terminals. Plan checklist grew to ~10 visible tasks with
scrolling. The tok/s footer counts reasoning chunks toward the live
rate.

- 029832e6 fix(onboarding): stop the provider list re-indenting on every Up/Down keypress
- 55a68039 fix(tui): stop truncating last fields of /onboard:provider and /models on small terminals
- 27e0a642 fix(tui): grow plan checklist to ~10 tasks + add scrolling for longer plans
- 0abf41ee fix(tui): count reasoning chunks toward live tok/s footer

CI / BUILD (5 commits)

`[profile.ci]` tuned for wall-clock instead of runtime perf.
`Cargo.lock` committed for reproducible builds. Stage-gated workflow
with Linux-only tests and main-only coverage. Deterministic pacing
tests via `force_grant_now` helper.

- 7c8968e7 build: add [profile.ci] tuned for CI wall-clock instead of runtime perf
- f6a0e77e build: commit Cargo.lock + switch CI to --locked for reproducible builds
- 201d7ec3 ci: stage-gated workflow, Linux-only tests, [profile.ci], main-only coverage
- ab29d48f test(rate_limiter): deterministic pacing tests via force_grant_now helper
- 0a393987 chore(tests): collapse nested if-let in prompt_compiled_features sentinel

PROVIDER STABILITY (3 commits)

Fallback only sticks after 4 consecutive rescues, not on first
incident. Missing-[DONE] accepted when text-only response looks
complete. Evolve tool got honest error branching and structured tracing
on every failure path.

- 05fcf43c fix(fallback): only stick the fallback after 4 consecutive rescues, not on first incident
- 97683fb0 fix(stream): accept missing-[DONE] when text-only response looks complete
- 457cff3d fix(evolve): honest error branching + structured tracing on every failure path

TOOL ROUTING (1 commit)

Agent was reaching for `browser_navigate` on "check the GitHub PR" or
"look up the docs for X" when the right surfaces were the `gh` CLI
and search tools. Tool descriptions now carry explicit pick-me-when
guidance and a WEB / GITHUB / BROWSER ROUTING block in `BRAIN_PREAMBLE`
puts the rule in the system prompt every turn.

- b25c1cc8 fix: route research to search, GitHub to gh CLI, browser to last resort

COMPACTION (2 commits)

Defaults to silent continuation with fun POST-COMPACTION PROTOCOL
fallback only when the post-compact state is genuinely ambiguous. New
`[agent] silent_compaction = true` opt-out for formal deployments.
All four compaction sites route through a single module.

- fb325fb5 fix(compaction): silent continuation by default, fun fallback only when unclear
- d3c239c1 feat(agent): restore fun post-compaction narration, add silent_compaction opt-out

PLAN TOOL (1 commit)

- 79c05b56 feat(plan): add concrete trigger criteria to plan tool description

TEST (1 commit)

- e65eb82f test(prompt): gate local-stt/tts test + parse source for feature list under tarpaulin

DOCS (1 commit)

- 882361cf docs(changelog): align v0.3.31 entry with shipped compaction + rolling-status behaviour

## [0.3.30] - 2026-05-29

36 commits since v0.3.29. Minor release closing issues #125, #126, #127,
#128, #129.

**Issues closed:** #125 (RTK daemon hang from sync `Command::new`
blocking the tokio runtime on single-worker profiles), #126 (/models
picker hid unconfigured providers entirely and showed Claude model
names for OpenCode CLI), #127 (channel_search had no way to target a
specific Telegram forum topic, so searches across topic-enabled
supergroups returned a flat undifferentiated list), #128
(rename_session silently accepted empty titles and wiped the session
label so it showed as "Untitled" in /sessions), and #129 (Telegram
/sessions response printed every session in the message body AND as
inline-keyboard buttons, pure duplication).

**RTK async refactor (closes #125):** RTK binary detection swapped
`std::sync::OnceLock` plus `std::process::Command` for
`tokio::sync::OnceCell` plus `tokio::process::Command` across
`find_rtk_binary`, `is_rtk_available`, `rewrite_command`, and
`rewrite_command_string`. The tracker mutex was lifted to
`tokio::sync::Mutex`, with a dedicated no-block regression suite
proving single-worker runtimes stay responsive.

**Qwen cache auto-enable:** Qwen custom providers get zero-config cost
savings. A `looks_like_qwen_target` detector scans base_url and model
name for Alibaba-shaped endpoints (dashscope, aliyun, aliyuncs,
dialagram) or `qwen-*` model prefixes and auto-enables ephemeral
`cache_control` markers on the system prompt, last streaming message,
and last tool. Logged once per (base_url, model) pair so mixed-model
providers only mark Qwen requests.

**Qwen tool-call extraction:** now handles bare-args bash calls AND
Anthropic-shaped `<invoke name="X">` XML nested inside
`<qwen:tool_call>` wrappers (verbatim user-screenshot malformations
preserved as regression tests). Orphan close tags like
`</tool_result>` are stripped from streamed output before reaching the
TUI.

**/models picker (closes #126):** surfaces every known provider
including unconfigured ones with a 🔒 lock + setup help text (never
asks users to paste API keys inline, Telegram bots can't delete DM
history). The displayed CLI model list comes from a single
`cli_supported_models` source of truth pinned per provider so channel
footers match what the TUI actually offers. Custom providers with no
configured models show a helpful empty-state instead of a single inert
button. The Telegram /sessions body was trimmed to header +
current-session indicator so it no longer duplicates the inline
keyboard (closes #129).

**Telegram onboarding:** full pass. Auto-detect the user's numeric ID
from getUpdates when the chat ID field is empty. Persist partial
config on Cancel so users don't lose typed tokens when they back out.
The quick-jump wizard no longer commits on intermediate Enter presses
(Tab on Model/CustomContextWindow used to silently rewrite ~30 config
keys; now only explicit Enter on the last step commits, and rebuild
swaps the per-session provider so the footer reflects the new
selection immediately).

**Channel handlers (Telegram, Discord, Slack, WhatsApp):** a follow-up
message during an active agent run is now treated as ESC x2 (cancel
and start fresh). Telegram status messages are dynamic and
context-aware (actual tool being called, tokens streamed, elapsed
time) instead of hardcoded quips. ZIP attachments from users are
extracted and processed inline (text files inlined, images get vision
markers, PDFs get text extraction, capped at 50 files / 10 MB per
entry). channel_search gained a `topic_id` filter for Telegram forum
supergroups (closes #127).

**Custom providers:** base URLs are normalized on Enter / Tab / paste
(strips trailing `/v1/chat/completions`, `/chat/completions`, `/v1`,
`/`).

**Image generation:** `generate_image` gained an optional `image`
parameter (local path or HTTPS URL) that feeds Gemini `inlineData`
parts for img2img editing. OpenAI-shaped backends reject it with a
clear error pointing users at Gemini.

**Self-heal + RSI:** the self-heal detector now catches "I need to X" /
"I have to X" / "I must X" / "I should X" deferment stalls in all five
supported languages (en/es/pt/fr/ru). The RSI `self_improve` tool
rejects trivial test content before it can pollute brain files.
`rename_session` rejects empty / whitespace-only titles so sessions
can't become unidentifiable (closes #128).

**TUI tok/s meter:** real-time throughput meter in the footer (between
model info and approval policy pill). Counts ONLY active streaming
time; tool execution, approval waits, between-stream network
round-trips, and compaction delays are excluded. Persists the last
finalized rate so the footer keeps showing the previous turn's tok/s
during idle until the next turn produces its first token.

**Plan widget:** dynamically hides tasks that don't fit the terminal
height instead of overflowing.

**Help screen:** Esc x2 reads "Cancel / abort immediately" instead of
the vague "Clear input". /onboard:channels lists every channel by
name. The footer carries a `docs.opencrabs.com` link.

**Style:** `assert_eq!(x, false)` collapsed to `assert!(!x)` across the
suite.

**CI / test infra:** in-memory test pool fix plus Unix-gated tests that
depended on a temp `$HOME` (Windows uses `SHGetKnownFolderPath`
instead of env vars).

Closes #125, #126, #127, #128, #129.

64 files changed, +3996/-501.

RTK ASYNC REFACTOR (3 commits, closes #125)

- 5d88479a fix(rtk): eliminate sync blocking in async RTK binary detection
- 95bbaa60 refactor(rtk): extract inline tests + switch tracker to `tokio::sync::Mutex`
- 66df474c test(rtk): no-block regression tests for single-worker tokio runtime

QWEN TOOL-CALL EXTRACTION (3 commits)

- 61ba04a0 fix(custom_openai_compatible): extract bare-args bash tool calls (qwen-3.7-max-thinking)
- b609e2d2 fix(custom_openai_compatible): extract Anthropic `<invoke name="X">` XML inside `<qwen:tool_call>`
- 93f53c15 fix(sanitize): strip orphan close tags (`</tool_result>` etc.) leaking to TUI

QWEN CACHE AUTO-ENABLE (5 commits)

- 3263c44b feat(qwen): add `looks_like_qwen_target` detector for auto-cache enablement
- e3bdf591 feat(qwen): auto-enable ephemeral `cache_control` for qwen-shaped custom providers
- c5630977 feat(qwen): log once per (base_url, model) when auto-cache engages
- 14ec9bce test(qwen): integration test for custom-provider cache auto-enable
- aafa6899 docs(qwen): document zero-config Qwen cache auto-enable for custom providers

CHANNEL UI / MODELS + SESSIONS PICKER (5 commits, closes #126, #129)

- 9d10d939 fix(channels): show OpenCode CLI models in /models, not Claude names
- b2f2033c refactor(providers): single-source CLI model list, pin via `cli_supported_models`
- 06be28b4 feat(channels): surface unconfigured providers in /models picker (closes #126)
- 4b2cc8a3 fix(channels): custom-provider /models picker returns empty list + help when no models configured
- 15206f86 fix(channels): /sessions body no longer duplicates inline-keyboard labels (closes #129)

IMG2IMG FOR GENERATE_IMAGE (1 commit)

- c71ddd25 feat(generate_image): add img2img support for user-uploaded images

HELP SCREEN IMPROVEMENTS (2 commits)

- c77aaaa6 improve(help): clarify Esc x2 abort and list channels in /onboard:channels
- cc536cbb feat(help): add docs.opencrabs.com link to help screen footer

ONBOARDING FIXES (4 commits)

- d3e90f0e fix(onboarding): only Enter on the last step commits in quick-jump mode
- 6a1bf3ab fix(tui): swap per-session provider on quick-jump rebuild so footer updates
- d2dcfbd9 fix(onboarding): normalize custom provider base URL on entry and paste
- 60150336 feat(onboarding): auto-detect Telegram user ID, persist on cancel, better errors

TELEGRAM / CHANNEL FEATURES (4 commits, closes #127)

- 95cacc03 feat(channels): follow-up message cancels running agent (ESC x2 behavior)
- 512bf002 feat(telegram): replace hardcoded status quips with dynamic context-aware messages
- 94419a7e feat(channels): topic-aware channel search for Telegram forums (closes #127)
- 10094519 feat(channels): handle ZIP file attachments from users

RENAME SESSION (1 commit, closes #128)

- 4e7ad6a8 fix(rename_session): reject empty / whitespace-only titles

TUI FIXES (3 commits)

- 4bfc20b4 fix(tui): dynamic plan widget task visibility based on terminal height
- f04684ef feat(tui): real-time tok/s in footer between model info and approval policy
- cb3d35f3 fix(tui): tok/s footer counts only active streaming time + persists last rate

SELF-HEAL HARDENING (2 commits)

- 899683a4 feat(self-heal): detect "I need to X" deferment stalls in all 5 languages
- aea1ef25 fix(rsi): reject trivial content in self_improve apply action

TEST / CI INFRA (3 commits)

- 6a747ec5 fix(tests): use `assert!(!x)` instead of `assert_eq!(x, false)`
- 388065e6 fix(test): set USERPROFILE alongside HOME so Windows CI sees the temp config
- 6dc273fd fix(test): gate `custom_provider_no_models_test` to Unix (Windows uses Win32 API)

## [0.3.29] - 2026-05-27

10 commits since v0.3.28. Patch release closing issue #121 (the
reporter's session stuck on the default channel-generated title) on
two fronts: reasoning models that return ONLY a Thinking block for
the title call now fall back to extracting a candidate from that
block instead of dropping the title silently, and the Telegram
handler stops clobbering non-default session titles on every
subsequent message via a new should_refresh_label policy plus
explicit chat→session binding on /sessions switch (PR #123 by
@leshchenko1979). Audit of Discord, Slack, and WhatsApp handlers
found the same root-cause bug with a different symptom — exact-title
find_session_by_title lookup orphaned auto-renamed sessions and
created a duplicate row on every subsequent message — fixed by
lifting Telegram's stable [chat:<id>] suffix pattern into a shared
channels::session_resolve module that all three channels now use,
with suffix-first lookup, legacy exact-title fallback, and one-shot
forward-migration of pre-suffix rows. Test infra got two real bug
fixes that were masking CI failures: the in-memory test pool
switched from WAL journal mode (a no-op for :memory: but a source of
spurious SQLITE_LOCKED 262 on shared-cache under concurrent writers
in release mode) to MEMORY journal with a single serialized
connection, eliminating cross-connection lock contention; a
release-mode rfc3339 nanosecond collision flake in the profile
registry test got a 1 ms guard so back-to-back Utc::now() calls
produce strictly-ordered timestamps. Closes #121.

AUTO-TITLE + TELEGRAM SESSION RESOLUTION (4 commits, closes #121)

The reporter's reproduction of #121 had two failure modes that
compounded each other. Reasoning models like qwen-3.7-max-preview-
thinking on a short prompt returned ONLY a Thinking block for the
title call — no Text block — so extract_text_from_response returned
empty and the session never renamed. PR #123 (community contribution
by @leshchenko1979) found a second bug: on every subsequent message
the Telegram handler did `if session.title != computed_default
{ overwrite }`, so any auto-titled name was reverted to the default
template (`Telegram: DM <name> (<id>) [chat:<id>]`) the next time
the user sent a message. Combined fix: extract_title_candidate
falls back to pluck_title_from_thinking when no Text block is
present (last quoted phrase, then last short sentence), and
should_refresh_label only refreshes default→default-different or
group label changes — never auto-titled or custom titles. Telegram
also now binds chat_id→session_id in an in-memory map on /sessions
switch so explicit user choices win over suffix lookup.

- 1aeaca2a fix(auto-title): handle thinking-only responses from reasoning models + add real e2e test (closes #121)
- 13f4930c fix(telegram): preserve auto-titled sessions and bind chat on switch
- 0f0c4174 Merge PR #123: fix(telegram) preserve auto-titled sessions and bind chat on switch
- a63c1826 test(auto-title): add second-message regression + cold-start doc note (PR #123 follow-up)

CROSS-CHANNEL SESSION SUFFIX PORT (1 commit)

Audit of Discord/Slack/WhatsApp handlers found the same root cause
as the Telegram bug PR #123 fixed but with a different user-visible
symptom: all three used exact-title
`find_session_by_title(&template)` lookup. After the agent
auto-renamed a session, the next message's lookup missed and the
handler created a brand-new duplicate session on every turn. New
`channels::session_resolve` module gives all three channels the
same stable `[chat:<id>]` suffix Telegram already had:
`[chat:discord-dm-<user_id>]`, `[chat:discord-<channel_id>]`,
`[chat:slack-dm-<user_id>]`, `[chat:slack-<channel_id>]`,
`[chat:wa-<phone>]`. `resolve_or_create_channel_session` runs
suffix-first lookup, falls back to legacy exact-title for
pre-suffix rows, and one-shot migrates the legacy row to the
suffix-bearing title format so subsequent lookups take the fast
path. Slack `/new` also runs the suffix-first lookup so
auto-titled rows still get archived. 7 unit tests cover suffix
resolve, auto-rename survival, legacy forward-migration, fresh
create, and idle archive + recreate.

- faa8b755 fix(channels): port telegram suffix-stable session lookup to discord, slack, whatsapp

IN-MEMORY TEST POOL HARDENING (3 commits)

CI release-mode runs of auto_title_e2e_test were panicking with
`Database("Failed to create message")`. Surfacing the error chain
revealed the underlying cause: SQLITE_LOCKED 262 on the `sessions`
table. Two compounding pool misconfigurations: WAL journal mode is
a no-op for `:memory:` (no file to journal to) but with
`cache=shared` triggers spurious table-lock failures under
concurrent writers in release mode, and `max_size = 5` let
deadpool hand out distinct connections that fought for
shared-cache table locks. The auto-title flow spawns a background
title task which races the main turn's session touch — reliable
failure in release, flaky in debug. Fix splits `apply_pragmas` so
in-memory connections get `journal_mode=MEMORY`,
`synchronous=OFF`, `foreign_keys=ON` (file pools keep WAL/NORMAL),
and caps the in-memory pool to a single serialized connection. Two
tokio test functions in auto_title_e2e_test.rs violating the file's
own "must be folded into a single tokio test" invariant got folded
back into one. The channel resolver idle test was holding a
deadpool connection across a call that needed another — fine on a
5-connection pool, deadlock on the new single-connection pool —
scoped the connection handle tight.

- 2456b91d test(auto-title): fold second-message regression into single tokio test
- d49c5e04 fix(db): use MEMORY journal + max_size=1 for in-memory test pool
- db43aeef fix(test): scope deadpool connection in idle test to avoid single-conn deadlock

CI HYGIENE (2 commits)

`tests::profile_test::registry_register_overwrites_duplicate` was a
pre-existing release-mode flake: `register` stamps
`Utc::now().to_rfc3339()`, and two back-to-back calls in release
sometimes land in the same nanosecond, producing identical
timestamps and failing the `assert_ne!` on `created_at`. A 1 ms
sleep between calls guarantees the second timestamp is strictly
later. Tarpaulin's cobertura.xml output is now gitignored so
running coverage locally doesn't dirty the tree.

- c46e500e fix(test): release-mode rfc3339 collision in profile registry test + fmt
- 5963f47f chore: gitignore tarpaulin's cobertura.xml output

## [0.3.28] - 2026-05-25

25 commits since v0.3.27. Patch release covering STT/TTS fallback chains
for voice notes, browser multi-step navigation hardening, qwen-3.7
tool-call shape recovery, real-time ctx counter (calibration removed),
auto-title fires on the first turn, and brain-template seeding for new
profiles. Closes #117, #118, #119, #120.

VOICEBOX + STT/TTS FALLBACK CHAINS (4 commits)

Voicebox is a self-hosted Python STT/TTS service. When it crashes (the
2026-05-23 librosa lazy_loader stub error from a PyInstaller bundle
issue) or is offline, voice notes were lost. Three layers now keep them
flowing:

- 2-second liveness probe before every transcribe so a dead voicebox
  fails in seconds instead of blocking on a 30-second multipart upload.
- Known-error translator turns the librosa stub Python traceback into
  an actionable rebuild instruction (collect-data + lazy_loader hook).
- Per-provider STT and TTS fallback chains under [providers.stt] and
  [providers.tts] in config.toml. User sets fallback_chain = ["groq",
  "openai_compatible", "local"] once and outages auto-route through
  the chain. First success wins, composite error on full-chain
  failure. Labels case-insensitive with aliases.

- 3bbeef29 feat(voicebox_stt): 2s liveness probe before multipart POST so dead voicebox fails fast
- a4c1afe4 feat(voicebox_stt): translate librosa/lazy_loader stub error into actionable message
- b0bc756e feat(voice): STT fallback chain — user-configured provider order with auto-failover
- febfce99 feat(voice): TTS fallback chain (mirror of STT) + README docs for both

BROWSER MULTI-STEP NAVIGATION HARDENING (6 commits)

Browser turns were getting stuck at 32+ iterations rotating
navigate-wait-screenshot without making progress. Six surgical fixes:

- browser_click accepts text= and xpath= prefixes in addition to CSS,
  so the agent does not have to reverse-engineer a selector when it
  knows the visible label.
- CSS-miss errors include an inline recovery hint pointing at
  browser_find with mode=text/aria/role.
- Empty-input warning no longer fires for tools whose schema has no
  required fields (browser_screenshot accepts no args).
- Semantic loop detection trips after 4+ screenshots in 8 iterations
  with zero clicks or types, injects a nudge telling the agent to
  interact instead of screenshot.
- browser_screenshot hashes bytes and rejects identical repeats with
  an actionable error pointing at click/type/navigate/find.
- browser_navigate short-circuits same-URL re-navigation so the agent
  does not waste 3s waiting for network-idle on a no-op.

- 236eb7ca feat(browser_click): accept text= and xpath= selectors in addition to CSS
- 38013094 feat(browser_click): inline recovery hint when CSS selector misses
- 6601b101 fix(tool_loop): only warn on empty input when the tool actually requires fields
- 6fae7296 feat(self-heal): semantic loop detection for browser screenshot-spam
- f48dd66b feat(browser_screenshot): reject no-op repeats when the page hasn't changed
- a3caff57 feat(browser_navigate): short-circuit same-URL re-navigation

TOOL-CALL SHAPE RECOVERY (2 commits)

Qwen-3.7-max-preview started emitting tool calls as
`{"call_<hex>": {"name":"...", "arguments":{...}}}` (top-level object
keyed by call ID). The existing extractor handled five other shapes
but not this one, so calls leaked into delta.content as JSON and
Telegram rendered them as code blocks. Two fixes:

- The schema trim that probably triggered the regression in the model
  (removed `operation` param + shortened param descriptions on
  edit_file) was reverted.
- The extractor got a new pass for dict-by-call-id, including the
  markdown ```json fence wrapping the model often adds.

- 13dae03b Revert "refactor(tools): trim file-editing schemas from 32 params to ~15"
- b6288726 fix(custom_openai_compatible): extract dict-by-call-id tool call shape

EDIT TOOL IMPROVEMENTS (2 commits, closes #117)

- af0edd1f feat(edit): fuzzy line-sequence fallback for str_replace (H2 from agent-eval-matrix)
- e2ef58f1 docs(hashline): fix tool descriptions to match hash-only ref format (issue #117, @leshchenko1979)

BRAIN BACKUP ROTATION (1 commit)

Backups for protected brain files now cap at 5 per file and 7 days
maximum age. Without this they grew unbounded and consumed disk on
RSI-heavy installs.

- 705a13bb fix(brain): add backup rotation — max 5 per file, max 7 days old

AUTO-TITLE FIXES (3 commits, closes #118, closes #120)

Two bugs combined to leave Telegram sessions stuck with their default
channel-generated title (`Telegram: DM <name> (<id>) [chat:<id>]`)
forever, making the /sessions list look like 10 duplicate rows:

- Auto-title only fired starting from the second user message because
  the trigger required `db_message_count >= 1` (count taken before the
  current message gets stored). Users who send one message per /new
  session never had auto-title run. Now fires on the first turn.
- mark_auto_title_attempted was set BEFORE the LLM call. If the LLM
  failed, the flag stayed true forever and the session was stuck on
  the default name. Error path now resets the flag so the next message
  retries.

- b65b23ba fix: prevent auto-title from re-triggering on subsequent messages
- ea2943a6 fix: prevent auto-title from firing on every Telegram message
- a12293d0 fix(auto-title): fire on the FIRST turn + retry when the LLM call fails (closes #118, closes #120)

CTX COUNTER REAL-TIME DATA ONLY (1 commit, closes #119)

The 2026-05-24 per-provider token-calibration system shipped with a
brain-token double-count bug that learned a wrong ratio, making the
footer jump from one value at /new to another value after the first
real reply. The whole prediction system was the wrong approach since
opencrabs already has real data from response.usage.input_tokens on
every turn. Calibration module removed. /new shows 0K/200K 0%. Every
reply afterward shows the exact provider-reported input tokens
verbatim. No estimates, no learned ratios, no drift.

- 54f19ffa fix(ctx-counter): real-time data only, rip out the calibration system (closes #119)

PROFILE BRAIN-TEMPLATE SEEDING (3 commits)

Audit triggered by @leshchenko1979's brain-file breakdown on #119 found
that `opencrabs profile create <name>` only made the profile dir,
`memory/`, and `logs/` — never seeded the brain templates. New profiles
started with an empty brain dir and RSI sync did not fix it (it
explicitly skips files that don't exist locally). Three commits:

- create_profile now seeds all 8 brain templates (SOUL, IDENTITY, USER,
  AGENTS, TOOLS, MEMORY, CODE, SECURITY) on profile creation.
- sync_templates recovery path: when an existing profile is missing
  more than half the core templates, seed them on the next RSI cycle.
  Rescues older profiles created before the fix without user action.
- New profile_test coverage locking in the seeding contract.

- 8d87559b fix(profile): seed brain-file templates on `profile create` so new profiles aren't blank
- 52c1baa9 fix(rsi_sync): re-seed brain templates when a profile dir is mostly empty
- 7d1dec35 test(profile): cover brain-template seeding on create_profile + the no-overwrite contract

STYLE (2 commits)

- 6714caaa style: rustfmt pass [skip ci]
- 7b7222e8 style: cargo fmt pass over voicebox + profile commits [skip ci]

## [0.3.27] - 2026-05-22

3 commits since v0.3.26. Patch release fixing session duplication caused
by auto-title stripping [chat:ID] suffix from channel sessions, adding
ctx budget baseline display on channel /new, and improving /sessions
current session indicator across all platforms.

CTX BUDGET BASELINE ON CHANNEL /NEW (1 commit)

All channel platforms (Telegram, Discord, Slack, WhatsApp) now compute
and send the ctx budget footer immediately after /new, showing the
calibrated baseline so users can audit their starting context.

- be1e4e8d feat: show ctx budget baseline on channel /new sessions

AUTO-TITLE SESSION FIX (1 commit, closes #114)

Auto-title was stripping the [chat:ID] suffix from channel session
titles. Next message, find_session_by_title_suffix could not find the
session, creating a duplicate. Every message produced a new session.
Fix: extract_chat_id_suffix() preserves [chat:ID] alongside the
channel prefix during auto-title generation.

- 6525e9b4 fix: preserve [chat:ID] suffix during auto-title to prevent session duplication

SESSIONS DISPLAY IMPROVEMENT (1 commit, closes #115)

Current session now uses prominent indicator (arrow prefix and current
label) instead of subtle checkmark. Button labels updated across
Telegram, Discord, and Slack handlers to match text display.

- b9f2c863 fix: update /sessions button labels across channel handlers to match text display

## [0.3.26] - 2026-05-22

42 commits since v0.3.25. Issue-fix and polish release addressing 8 issues
(#105, #106, #107, #108, #109, #111, #112, #113) and merging 2 contributor
PRs (#4, #113). Hashline collision detection prevents line-shift avalanche.
Channel command parity (/evolve, /rtk, auto-title) matches TUI behavior.
RSI brain file hygiene rejects raw failure-event logs. Tool error output
now includes stdout/stderr with ANSI stripping and 8000 char cap. CI
caching optimizations with rust-cache and mold. Dynamic help screen
auto-generates from slash commands. Context counter calibration prevents
jumpy footer on first turn.

Contributions from @leshchenko1979 (issues #105, #111, #112, PRs #4, #113).

HASHLINE COLLISION DETECTION (4 commits, closes #105)

Pure content hashing prevents line-shift avalanche when lines are inserted.
Collision detection in validate_hash() and read_file hashline mode marks
affected lines with COLLISION prefix, directing LLM to use edit_file
instead. HashRef format changed from 12#VK|content to VK|content.

- ed711a2e fix(hashline): use pure content hash to prevent line-shift avalanche
- 3ded0b4b fix(hashline): detect hash collisions and escalate to edit_file
- 86ab2136 feat(hashline): detect collisions in read_file hashline mode and mark affected lines
- 75532db9 fix(hashline): remove line numbers from HashRef to prevent avalanche problem

CHANNEL COMMAND PARITY (3 commits, closes #106, #107, #108)

/evolve, /rtk, and auto-title now work on all channel platforms (Telegram,
Discord, Slack, WhatsApp), matching TUI behavior. Auto-title fires on
sessions with default channel titles and preserves channel prefix
(Telegram:, Slack:, etc.) when generating new titles.

- a6a86f57 fix(channels): /evolve now restarts daemon on all platforms
- fcb0784a feat(channels): add /rtk command to all channel platforms
- f6658655 fix(auto-title): fire on channel sessions with default titles

AUTO-TITLE TUI FIX (2 commits, closes #109)

TUI sessions now create with None title so auto-rename fires immediately.
Auto-title preserves channel prefix for channel sessions but generates
clean titles for TUI. New Chat recognized as default title.

- 0fb158b7 fix(tui): use None title for new sessions so auto-rename fires
- 152c06f4 fix(auto-title): preserve channel prefix and handle New Chat default title

RSI BRAIN FILE HYGIENE (1 commit, closes #111)

RSI agent prompt updated to reject raw failure-event logs in brain files.
Defense-in-depth: prompt teaches rule vs. log distinction, code guard in
self_improve.rs rejects content with failure-event patterns.

- a8703b13 fix(rsi): reject raw failure-event logs in brain files

WRITE_OPENCRABS_FILE DOCUMENTATION (3 commits, closes #112)

Clarified path rules for brain files vs. other files. Added profiles/
prefix guard to prevent path doubling. Simplified documentation to remove
profile-specific language that confused agents.

- 2ac57c19 docs: clarify write_opencrabs_file path rules for brain files
- 3395ce1a fix(write_opencrabs_file): add profiles/ prefix guard and profile-aware docs
- 59a62af4 refactor: simplify write_opencrabs_file docs to remove profile-specific language

TOOL ERROR OUTPUT HANDLING (2 commits, closes #113)

Tool errors now include stdout/stderr in content sent to LLM, not just
the error message. Extracted build_tool_result_content helper to eliminate
duplication. ANSI escape sequences stripped, output capped at 8000 chars
to prevent context bloat. 7 new tests added.

- 33a93062 fix: include stdout/stderr in tool error content sent to LLM
- 4729f14f improve(tool-loop): extract helper, strip ANSI, cap output size

CI CACHING OPTIMIZATIONS (10 commits, closes #4)

Merged CI caching improvements from @leshchenko1979, then refined with
ZeroClaw-inspired optimizations. Uses rust-cache@v2 (no sccache), mold
linker for Linux, CARGO_INCREMENTAL: 0, concurrency control with
cancel-in-progress, Windows tests use dev profile (no --release).

- 64319193 ci: add rust-cache, sccache, chocolatey cache from upstream/main base
- 8544b459 ci: add rust-cache and chocolatey cache (no sccache)
- 03216e02 ci: add rust-cache, sccache (with GHA cache), chocolatey cache
- a3a30235 ci: use mozilla-actions/sccache-action (proper GHA cache setup)
- 283d0b66 ci: add mold for Linux, fix Windows OOM with sccache cache limits
- 232eb641 fix ci: correct mold RUSTFLAGS syntax
- 9ae3bc81 ci: remove --release from Windows test (dev profile for fast builds)
- 75208f42 merge: CI caching improvements from PR #4 + ZeroClaw refinements
- 2e0e0ce7 ci: refine caching with ZeroClaw-inspired optimizations
- 4e75da81 merge: CI caching improvements from PR #4

DYNAMIC HELP SCREEN (1 commit)

Help screen now auto-generates from SLASH_COMMANDS constant instead of
hardcoding. New commands automatically appear without manual updates.

- 23b963ef feat(tui): dynamically generate help screen from SLASH_COMMANDS constant

CONTEXT COUNTER CALIBRATION (2 commits)

Per-provider tokenizer calibration so the context budget footer doesn't
jump on first turn. Uncalibrated providers show 0/max instead of
misleading raw cl100k estimate.

- b7f3f7e5 feat(ctx-counter): per-provider tokenizer calibration
- 1ad45ba2 feat(ctx-counter): uncalibrated providers show 0/max instead of misleading estimate

DOCUMENTATION UPDATES (3 commits)

Added missing slash commands (/rtk, /mission-control, /skills, /new,
/evolve) to README keyboard shortcuts and channel commands sections.
Updated test count to 2,883.

- fedbfd9c docs: add missing slash commands to README keyboard shortcuts and channel commands sections
- 1f8afcff docs: update test count to 2,883 across README.md and TESTING.md



## [0.3.25] - 2026-05-21

36 commits since v0.3.24. Feature release. Bundles RTK (Rust Token
Killer) as a default feature with zero-config, saving 40%+ tokens on
common dev commands. New tool call stacking collapses consecutive tool
groups into a single summary line. Sensitive data redaction applied to
tool output in TUI and all channels. Context budget footer for channels
(closes #104). hashline_edit tool for hash-anchored file editing (closes
#60). Brain file cleanup_intent for user-driven maintenance (issue #103).
Scroll fixes, auto-title improvements, and various stability fixes.

Contributions from @leshchenko1979 (PRs #100, #101).

RTK TOKEN SAVINGS INTEGRATION (3 commits, PR #102)

Bundled RTK as a default feature with zero-config. Works as a direct
proxy: when the agent runs `git status`, RTK intercepts the output
through Rust, filters it, and returns a token-optimized version. Supports
100+ commands (git, cargo, npm, pnpm, docker, kubectl, grep, find, ls,
tree, curl, etc.) with a blocklist for interactive/REPL commands (vim,
ssh, python, mysql, etc.). Binary discovery checks bundled location
first (same dir or bin/ subdir), falls back to PATH. Locked to RTK
v0.40.0. `/rtk` slash command shows savings stats.

Real-world results: 105 commands, 129.1K tokens saved (41.2%). Top
saver: `cargo test` at 105.4K tokens (100% savings).

- 157b807e feat: add RTK token savings integration
- c91ec81b fix(rtk): use prepend approach instead of non-existent rtk rewrite subcommand
- 65a5ed8f feat(rtk): make rtk default feature, bundle binary in releases, lock to v0.40.0

TOOL CALL STACKING (5 commits)

3+ consecutive tool call groups collapse into a single summary line in
the TUI. Spans across thinking-only assistant messages (empty content +
reasoning). Ctrl+O expands/collapses. Shows "N tool calls" when each
group has 1 call, or "N tool calls (M groups)" when some groups have
multiple. 6 tests added.

- 716ca621 feat(tui): stack consecutive tool call groups into single summary
- a4d6477e feat(tui): stack tool groups across thinking-only assistant messages
- 4b0e74f6 test(tui): add 6 tests for tool call group stacking logic
- 058b52db docs: update test counts to 2,833 (6 new tool stacking tests)
- d2d502a7 fix: remove redundant group count when each tool call is its own group

SENSITIVE DATA REDACTION (1 commit)

Applied redact_secrets() to tool output display in TUI and all channels.
New patterns: env var suffixes (_pass=, _password=, _secret=, _token=,
_key=, _apikey=, _api_key=, _credential=, _auth=), piped secrets
(echo "secret" | command), plus existing patterns (sk-*, ghp_*, xoxb-*,
AWS keys, Bearer tokens, Basic auth).

- c068a35f redact sensitive data in TUI and channel output

CONTEXT BUDGET FOOTER FOR CHANNELS (3 commits, closes #104)

Every channel (Telegram, Discord, Slack, WhatsApp) now appends a context
budget footer (e.g., "ctx: 8K/200K 4%") to the final message, matching
the TUI footer. Footer always delivered even when body is fully consumed
by intermediates.

- 871edb20 add ctx budget footer to channel final messages (closes #104)
- 7fc5d673 debug(telegram): add tracing logs for ctx footer append and delivery
- 7d008235 fix(telegram): always deliver ctx footer, even when body is fully consumed by intermediates

HASHLINE EDIT TOOL (1 commit, closes #60)

New hashline_edit tool for hash-anchored file editing. Each line gets a
2-char content hash from read_file(hashline=true). Reference lines as
LINE#ID instead of reproducing text. Stale hashes rejected before any
changes applied. Supports batch edits (multiple operations in one call).

- cf7b05b3 feat(tools): add hashline_edit tool for hash-anchored file editing (#60)

BRAIN FILE CLEANUP (1 commit, issue #103)

Added cleanup_intent flag to write_opencrabs_file. User-driven brain
file cleanup allowed, RSI agent blocked from shrinking brain files.
Prevents autonomous self-improvement from accidentally wiping brain files.

- 7f2479d7 Add cleanup_intent flag for user-driven brain file cleanup (issue #103)

SCROLL FIXES (4 commits)

Removed load_more_history() from scroll handler (was causing scroll-up
to overshoot hundreds of pages). Preserved scroll position during
streaming and system messages. Skip scroll compensation on first render.
All scroll events routed through coalesced handler.

- 1e7e85ae fix(tui): route all scroll events through coalesced handler to prevent overshoot
- 1644bed2 fix(tui): remove load_more_history from scroll handler to fix scroll-up overshoot
- 03c056bd fix(tui): preserve scroll position during streaming and system messages
- 3fc2ae58 fix(tui): skip scroll compensation on first render to prevent massive jump

AUTO-TITLE & SESSION MANAGEMENT (3 commits, PR #100)

Auto-title fires at end of first turn across all channels. Sessions sort
by last interaction time (/sessions). Context max tokens updates in
footer after model switch.

- d645089b feat(tui): auto-title fires at end of first turn, works on all channels
- 6e7bbb5a fix(sessions): touch updated_at on message create so /sessions sorts by last interaction
- 6fb0ac84 fix(tui): update context_max_tokens in footer after model switch

COMPACTION & SELF-HEAL FIXES (3 commits)

Compaction: dropped 55% kept-tail so summary IS the conversation.
Self-heal: 5-nudge budget for reasoning-only turns with sticky fallback
so empty replies never silently drop. Completion-escape clause for
phantom enforcement messages.

- ee38d710 fix(compaction): drop the 55%-of-window kept-tail — summary is the conversation
- ccf747fa fix(self-heal): 5-nudge budget for reasoning-only turns + sticky fallback
- 1e0cf750 fix(self-heal): add completion-escape clause to phantom enforcement messages

CHANNEL IMPROVEMENTS (3 commits)

WhatsApp photo batching for multi-image uploads. Telegram media_group_id-
based photo batching. Gemini schema: strip default/example from tool
schemas (PR #101 by @leshchenko1979).

- a26a0c33 feat(whatsapp): add photo batching for multi-image uploads
- 0e8cc010 fix(telegram): media_group_id-based photo batching
- fbbc39ec fix(gemini): strip default and example from tool schemas (#101)

OTHER FIXES (5 commits)

Custom provider model selection persistence. CI: gate voice tests behind
feature flags. Compaction prompt dominance fix + plan tool descriptions.
Tool loop borrow-after-move fix from PR #100 merge. Test refactoring.

- 62bb0612 fix(tui): properly save and display custom provider model selection
- 6668d029 fix(ci): gate voice tests behind feature flags, ignore Windows-incompatible doctest
- 24d1c55b fix: compaction prompt dominance + plan tool descriptions + scroll sensitivity
- d457b484 fix(tool_loop): capture db_message_count before move to fix borrow-after-move from PR #100
- 96faf3d5 refactor(tests): move channel handler inline tests to src/tests/

DOCS & SOUL (2 commits)

README updated with RTK token savings feature. SOUL.md: hard rule to
never ignore user images during interruptions.

- fe66e9fb docs(readme): add RTK token savings feature
- c8854889 soul: add hard rule to never ignore user images during interruptions


## [0.3.24] - 2026-05-20

13 commits since v0.3.23. Feature release. Adds two new built-in tools
(`rename_session` and `follow_up_question`), per-parameter value coercion
for dynamic tools, a polished custom-provider onboarding flow with
paste-by-default and typed-not-in-list model acceptance, automatic session
title generation from the first user message, a Gemini schema sanitizer
that strips `additionalProperties` before send, per-session model override
so the TUI surfaces match the wire, and CLI config/keys path corrections.

NEW TOOLS (3 commits)

`rename_session` lets the agent proactively rename the current session
with a short, descriptive title. Useful for long-running conversations
where the default title (e.g., "Telegram: <chat>") becomes unhelpful
after 20 messages. The tool takes a single `title` parameter (3–8 words)
and updates the session row in the database. Channels see the new title
on the next message.

`follow_up_question` lets the agent ask the user a discrete-choice
question with up to 8 button options. Implemented across all four
channels: Telegram (inline keyboard), Discord (button components),
Slack (Block Kit actions), and WhatsApp (quick replies). Returns the
chosen option string so the agent can branch on the answer. Closes #94.

The Tool System catalog documentation was updated to reflect both new
tools plus other long-missing built-ins.

- c3af2f0d feat(tools): add rename_session for agent-driven session titles
- 73c4f4f3 feat(tools): follow_up_question — agent asks user a multi-choice question via channel buttons (#94)
- 8beef289 docs: update Tool System catalog for rename_session, follow_up_question, and other long-missing built-ins

DYNAMIC TOOL ENHANCEMENTS (1 commit)

Issue #95: dynamic tools defined in `tools.toml` had no way to handle
empty-string or null parameters gracefully. A shell tool with an optional
`--verbose` flag would either get an empty string (breaking the command)
or require the agent to always pass a value.

Add per-parameter `coerce_empty_to` and `coerce_null_to` fields. When a
parameter arrives as `""` or `null`, the dynamic tool engine substitutes
the configured value before rendering the command template. Example:
`coerce_empty_to = "false"` turns an omitted boolean flag into a safe
default. Closes #95.

- ca12036a feat(tools/dynamic): per-param coerce_empty_to / coerce_null_to for tools.toml (closes #95)

CUSTOM PROVIDER UX (3 commits)

The `/models` dialog for adding custom OpenAI-compatible providers got
three quality-of-life improvements:

1. **Paste-by-default**: when the API key input is focused, `Ctrl+V` (or
   `Cmd+V` on macOS) pastes from the clipboard immediately. No need to
   tab into the field first.

2. **Enter-to-load**: typing a model name that isn't in the fetched list
   and pressing Enter now adds it to the list and selects it. Previously
   the dialog rejected unknown models and forced the user to pick from
   the (sometimes incomplete) auto-fetched list.

3. **Field refresh**: after saving a new custom provider, the per-provider
   fields (base URL, API key, model list) now refresh immediately so the
   user sees their saved values without restarting the dialog.

- 13abf59e feat(tui/models): paste-by-default + Enter-to-load for custom providers
- fab13d28 feat(tui/models): accept typed-not-in-list custom model and merge with fetched list
- 2838bb7c fix(tui/models): refresh per-provider fields after saving a new custom provider

SESSION MANAGEMENT (1 commit)

New sessions created via `/new` or `Ctrl+N` now get an auto-generated
title based on the first user message. A lightweight non-streaming LLM
call fires in the background (separate from the main context) and
updates the session title in the database. The title never enters
conversation context, so it doesn't pollute the message history. If the
LLM call fails, the session keeps the default "New Chat" title.

- 9b82cb6c feat(tui): auto-generate session title from first user message

PROVIDER FIXES (2 commits)

Issue #99: Gemini rejected tool definitions that included
`additionalProperties` in their JSON schemas (a common pattern for
open-ended object parameters). The Gemini provider now strips
`additionalProperties` from all tool schemas before sending them to the
API. Closes #99.

Issue #97: the TUI's model display showed the global default model even
when a per-session override was active. The override was being sent on
the wire but not reflected in the status bar. The agent service now
propagates the session-level override to all display surfaces so the
user sees what's actually being used.

- 006267cd fix(gemini): strip additionalProperties from tool schemas before send (closes #99)
- 44fdd9b3 fix(agent): per-session model override so display surfaces match the wire (#97 fix)

CLI FIXES (2 commits)

Issue #96: `open crabs doctor`, `open crabs init`, and other CLI
subcommands were using hardcoded paths (`~/.opencrabs/config.toml`,
`~/.opencrabs/keys.toml`) instead of respecting the `--config` and
`--keys` flags. This broke workflows where users kept their config in a
non-default location.

Correct all CLI entry points to resolve paths from the parsed args
first, falling back to the default only when no flag is provided. Also
fixed a type mismatch where `keys_path` was a `PathBuf` but the config
struct expected `Option<PathBuf>`, and added the missing `Config` import
in `cli/commands.rs`.

- 9d0b34c2 fix(cli): use correct config/keys paths (doctor, init, args) (#96)
- 9c440e8d fix(cli): type-correct fixup for #96 (keys_path PathBuf, system_config_path Option, missing Config import)

STYLE (1 commit)

Rustfmt auto-formatted the `version_sort_key` chain in `model_fetch.rs`
for consistency with the rest of the codebase.

- 7fdf5ae1 style: rustfmt version_sort_key chain in model_fetch.rs


## [0.3.23] - 2026-05-19

7 commits since v0.3.22. Hotfix release. Restores phantom detection,
fixes `/new` session switching across channels, adds a defense-in-depth
guardrail so generic write/edit tools cannot clobber protected brain
files, wires the A2A gateway through the config-level approval policy,
removes every "[self-heal] Aborted" exit path so recovery always
retries or falls back, and falls back to numeric version sort when an
OpenAI-compatible model server returns unreliable `created` timestamps.

PHANTOM DETECTION RESTORED (1 commit)

v0.3.21's turn-level `tools_executed_this_turn` gate was too aggressive:
once any tool ran in a turn, the phantom detector went silent for the rest
of the turn. That let fabricated wrap-up text (e.g. "Clippy clean, fmt
clean, 2626 tests pass. Committing: Done. Commit `c4f7898b`") reach the
TUI verbatim even when zero clippy/fmt/cargo/git tool calls had fired.
Drop the gate from all three phantom branches. The favicon-style false
positive comes back as a UI annoyance, but fabricated commit hashes
reaching the user is the worse failure mode.

- ebb0c9d5 fix(self-heal): restore phantom detection (drop turn-level tools_executed gate)

CHANNEL SESSION SWITCHING (1 commit)

Issue #89: `/new` and `/sessions` button clicks did not actually switch
sessions on Telegram. Root cause: `/new` created sessions with the
LEGACY title format (no `[chat:<id>]` suffix), but the per-message
resolver looked up by suffix and never found the freshly-created row.
Same shape on Discord DMs (`"Discord: <name>"` vs
`"Discord: DM <name> (<id>)"`) and Slack DMs
(`"Slack: <user_id>"` vs `"Slack: DM <user_id>"`).

Align `/new` with the resolver's expected format everywhere. DB stays
the single source of truth — `find_session_by_title*` returns the most
recently touched non-archived row, so the freshly-created session
naturally wins the next lookup. WhatsApp / Telegram groups / Discord
channels / Slack channels were already aligned and unchanged.

- caad22a2 fix(channels): /new uses the per-message resolver's title format (#89)

BRAIN FILE GUARDRAIL (1 commit)

Issue #91: `brain_file_safety` enforced append-only + dedup-aware shrink
+ `.bak` snapshots only inside `write_opencrabs_file` and the RSI loop.
Generic `write_file` / `edit_file` could still clobber the 9 protected
brain files (SOUL.md, USER.md, AGENTS.md, TOOLS.md, CODE.md, SECURITY.md,
MEMORY.md, BOOT.md, IDENTITY.md). Add a single `is_protected_path`
check at the top of both tools' `execute()` so they refuse and route
the caller at `write_opencrabs_file`. The guard is name-based, not
directory-based, so legitimate writes under `~/.opencrabs/` (memory
logs, commands.toml, RSI proposals) keep working.

- 5e3f0e6f fix(tools): generic write/edit refuse protected brain files (#91)

A2A APPROVAL POLICY (1 commit)

Issue #92: A2A `message/send` tasks failed every tool that required
approval with "Tool requires approval but no approval mechanism
configured", even with `[agent] approval_policy = "auto-always"`. The
A2A handler called `send_message_with_tools_and_mode` with no approval
callback, so the tool-loop's approval gate had no policy resolver and
fell through to the default-deny branch.

`process_task()` now builds an approval callback that resolves through
`check_approval_policy()` (the same path Telegram/Discord/Slack/WhatsApp
use) and dispatches via `send_message_with_tools_and_callback`. With
`auto-always` or `auto-session` set, tools are auto-approved; with any
other policy A2A returns `(false, false)` and logs a warning, since
A2A has no interactive UI to prompt.

- 1dee77c3 fix(a2a): wire approval policy callback so auto-always works (closes #92)

SELF-HEAL NEVER ABORTS (1 commit)

The stuck-intent-loop branch added previously hard-aborted on the
first detection of 3+ "Let me X" line-starts in a single iteration,
with zero retry or fallback attempts. The cap-exhaustion branch also
ended in an abort once the retry budget ran out. Both wrote
"[self-heal] Aborted" into the assistant message and stranded the
user with no result, even when the turn had already completed real
work earlier.

Rework so the self-heal pipeline never aborts:

- Stuck-intent-loop is treated as a strong phantom signal, not a death
  sentence. Fast-escalates to a sticky fallback provider when the
  retry budget is at least half-burned; otherwise falls through to
  the regular nudge-and-retry path.
- Cap-exhaustion resets the retry counter to 0 and injects a hard
  nudge ("STOP narrating, call the tool now"), then continues the
  loop. The user presses Stop if they want out.
- `phantom_retries_used` resets to 0 on every successful tool
  execution. The counter is now "consecutive phantoms since the last
  real tool", so a single late phantom burst after good work no
  longer pushes us straight over the cap.

- 894fc2f3 fix(self-heal): never abort, always retry or fallback

VERSION-AWARE MODEL SORT (2 commits)

OpenAI-compatible servers like vLLM and llama.cpp return all models
with the same (or zero) `created` timestamp, so the existing sort by
recency collapsed every list into stable but meaningless order.

Add `version_sort_key()` that extracts numeric segments from model
names and compares them numerically in descending order (newest
version first). Apply it as a fallback in both the OpenAI and Ollama
fetch paths whenever every `created` value is zero or identical, so
the model list surfaces the newest version at the top regardless of
how the server reports timestamps.

- c1f51d6c fix: version-aware model sort when server timestamps are unreliable
- fba263ee fix(model-fetch): wire version_sort_key into both OpenAI and Ollama sort paths

## [0.3.22] - 2026-05-19

3 commits since v0.3.21. Hotfix for a v0.3.21 regression in compaction
plus a `/new` archive-behavior unification across channels.

COMPACTION TYPING WITHOUT BANNER (2 commits)

v0.3.21's `66533b1a` wired `ProgressEvent::Compacting` end-to-end and surfaced a visible
"🗜️ Compacting context — this may take 30-60s on long sessions" banner to every channel
and the TUI. Compaction has always been transparent — the banner was unrequested. Reverted
and replaced with a typing-only refresh that keeps the native "is typing" indicator alive
during the silent 10-60s window without emitting any text:

- Telegram: one immediate `send_chat_action(Typing)` on Compacting; the existing 4s pinger
  loop covers the rest of the window.
- Discord: spawn a bounded loop that calls `broadcast_typing` every 8s for up to 90s
  (Discord has no continuous pinger like Telegram). Self-terminates; if compaction
  finishes early, real streaming chunks resume the indicator naturally.
- TUI: unchanged `return` no-op — the spinner already shows "is responding/thinking".
- Slack / WhatsApp: no typing API, no handler — silent as they have always been during
  compaction.

- 6a9de7bb Revert "fix(compaction): wire ProgressEvent::Compacting so channels show activity"
- bdf47bfc fix(compaction): refresh typing indicator on channels without banner text

CHANNEL `/new` ARCHIVE CONSISTENCY (1 commit)

Discussion #87 (thanks @leshchenko1979): the four channel handlers disagreed on `/new`.
Telegram and WhatsApp archived the previous session only for non-owners; Discord and
Slack archived for everyone unconditionally. Owner sessions on Discord and Slack were
disappearing from `/sessions` even though the Telegram/WhatsApp design intent was to
preserve them for history review. Now all four channels behave the same: non-owner
sessions get archived on `/new` so the next title lookup resolves cleanly, owner
sessions stay non-archived and remain visible in `/sessions`.

- 19ec8193 fix(channels): unify /new archive behavior — keep owner sessions, archive guests

## [0.3.21] - 2026-05-19

21 commits since v0.3.20.

PHANTOM TOOL-CALL SELF-HEAL OVERHAUL (5 commits)

Multi-language phantom tool-call detection via compile-time TOML character sets.
Instead of regex patterns per language, each language defines char ranges in a TOML file
that gets compiled into match arms at build time. New languages are added by editing the
TOML — no Rust changes needed. Discussion #86, thanks @leshchenki1979.

The self-heal pipeline was hardened: phantom detection is now gated on turn-level tool
execution (not just text patterns), phantom iterations are no longer persisted to DB,
phantom text is stripped from context before the next turn, and sticky fallback is
applied on exhaust to prevent cascading failures.

- 53fe53cc feat(phantom): multi-language detection via compile-time TOML loading
- 77e4a8fa fix(self-heal): gate phantom on turn-level tool execution + sticky fallback on exhaust
- 517e33ca fix(self-heal): skip persisting phantom iterations to DB
- c7814618 fix(self-heal): stop injecting phantom text into context
- c965e65b test(phantom): cross-language regression for char-set-based detection

OPENAI-COMPATIBLE IMAGE GENERATION (4 commits)

New image generation backend that calls any OpenAI-compatible /v1/images/generations
endpoint. Providers can override the generation model independently of the chat model
via a `generation_model` field in config, with an onboarding prompt in ImageSetup.

- 30e8bdc6 feat(generate_image): add OpenAI-compatible images backend
- e3581da1 feat(config): add generation_model override field on ProviderConfig
- f7f72e11 feat(onboarding): prompt for generation_model override in ImageSetup
- 35e4d8f6 feat(provider): resolve generation_model override via factory helpers

SELF-HEAL & CONTEXT FIXES (4 commits)

- d88fed89 fix(self-heal): make working directory visible across tools in same iteration
- 684d5b6a fix(context): strip compaction banner from LLM context so models don't echo it
- 66533b1a fix(compaction): wire ProgressEvent::Compacting so channels show activity
- 82ea18ed fix(rsi): dedup cycle output by hashing assembled opportunities

TELEGRAM & UX FIXES (4 commits)

- 0c638138 fix(telegram): pipe-separate model callback so custom-provider colons survive parse
- e42af3b4 fix(telegram): touch session updated_at on switch (#85)
- 95e45e25 fix(dialogs): custom-provider model selection now persists + syncs live list
- 1b3ae187 fix(usage): correct one_shot_pct display, remove spurious ×100 (#84)

DOCS & STYLE (4 commits)

- 51e32e64 docs: bump test count to 2,626 + document per-provider generation_model
- cc8673a3 style: rustfmt line-wrap in tools/trait.rs
- d8e94260 docs: remove typo
- 39fffb0a fix(telegram): touch session updated_at on switch (duplicate of e42af3b4)

## [0.3.20] - 2026-05-18

20 commits since v0.3.19.

BY-MODEL QUANTIZATION TREE VIEW (7 commits)

The /usage dashboard now groups models with quantization variants (e.g. qwen3.6-35b-a3b-gguf,
-oq2, -oq4, -iq4_xs) under a single parent row showing aggregated tokens/cost/calls.
Tree-style prefixes (├─ / └─), column widths account for both parent and variant data.

- 17eb336f refactor(usage): tighten By-Model tree prefix, drop 4-space lead
- 1026f9d5 fix(usage): align child row columns with parent, deeper indent
- 950b9a4b fix(usage): strip provider-namespace prefix from model names
- 7d389404 fix(usage): strip .gguf extension before quant-pattern matching
- ad027c3c fix(usage): skip blank tool_name entries
- aaad2188 fix(usage): only draw tree connectors for models with variants
- ee160d65 fix(usage): clear clippy lints

FEATURE (1 commit)

- 92d3f4f6 feat(tui): per-pane error and notification banners

BUGFIXES (7 commits)

- 3463676e fix(heal): abort on stuck intent loops instead of retrying
- 10c8e99c fix(heal): replace phantom-exhaustion text with abort notice
- 9557b6f5 fix(provider): catch bare top-level tool-call arrays leaking to TUI
- 7955d5c1 fix(mc): restore cron jobs — tolerate BLOB-typed prompt rows
- 7955d5c1 fix(tui): clamp slash/emoji popup height to fit short terminals
- 7955d5c1 fix(rsi): resolve home directory instead of CWD-relative path
- 92d3f4f6 feat(usage): tree view for 'By Model' card with quantization grouping

DOCS (2 commits)

- 7955d5c1 docs: fix LICENSE reference path (PR #82, @kriptoburak)
- 7955d5c1 docs: refresh total to 2,614 passing tests

EXTERNAL CONTRIBUTIONS (1 commit)

- 7955d5c1 fix(cli): load dynamic tools from tools.toml in run and agent modes (issue #79, @leshchenko)

TESTS (1 commit)

- 7955d5c1 style(tests): rustfmt line-wraps in rsi_test

## [0.3.19] - 2026-05-15

### Added

- **Codex OAuth provider** — native OpenAI Codex subscription auth via device-code PKCE flow. No CLI dependency, no API key. User authenticates through browser once; tokens stored in `~/.opencrabs/auth/codex.json` with automatic refresh and background rotation. Appears as "Codex" in `/models` and `/onboard:provider` alongside existing "Codex CLI" option. Two-step PKCE exchange matching Codex CLI's exact protocol: deviceauth poll → authorization code → token exchange. Full TUI integration with verification URL + code display, waiting state, success/failure feedback.
- **OpenAI-compatible embedding API** (discussion #78, 2/2) — users can now configure external embedding providers instead of downloading the 300MB GGUF model. Supports OpenAI (`text-embedding-3-small`), Ollama (`nomic-embed-text`), Jina, LM Studio, and any `/v1/embeddings` endpoint. Config via `[memory.embedding]` section with `url`, `model`, `api_key`, `dimensions`. Dynamic vector dimensions detected from API response.
- **FTS5-only memory mode for VPS** (discussion #78, 1/2) — new `[memory]` config section with `vector_enabled` flag. When `false`, skips GGUF model download, llama.cpp engine init, embedding backfill, and vector search entirely. Pure FTS5 keyword search with zero RAM overhead. Auto-detects VPS environments and appends config automatically.

### Fixed

- **Codex OAuth device flow field names** — OpenAI's device auth API uses non-standard field names (`device_auth_id` instead of `device_code`, string `interval` instead of number, `expires_at` instead of `expires_in`). Fixed response struct with serde aliases and custom deserializer.
- **Codex OAuth verification URL** — was hardcoded to non-existent `auth.openai.com/verify`, changed to `auth.openai.com/codex/device` matching Codex CLI.
- **Codex OAuth model list** — `/models` dialog showed non-OpenAI models (Phi-4, Llama, Mistral) because the `codex` provider ID wasn't mapped to the curated GPT-5 model list.
- **`/models` and `/onboard:provider` UX mismatch** — `/models` showed static "use /onboard:provider" hint for OAuth providers instead of triggering the device flow interactively. Now both dialogs have identical Codex OAuth device flow UX with shared state.
- **Tool loop reasoning markers** — persisted reasoning content in non-CLI content column so thinking state survives across tool loop iterations.
- **Windows CI test failures** — `tool_loop_helpers_test.rs` used hardcoded Unix `/tmp/` paths and `/etc/hosts` assertions that aren't valid on Windows. Added platform-specific test variants with `#[cfg(unix)]` / `#[cfg(windows)]`.
- **CI Node 24 forced upgrade** — removed `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24: true` env var that broke `actions/cache@v4` with `punycode` deprecation on Node 21+.
- **Cron provider/model cross-contamination** — cron's `execute_job` called global `swap_provider()` instead of session-scoped `swap_provider_for_session()`, so concurrent cron jobs on the shared `Cron` session overwrote each other's provider. Now each job swaps on its own session ID, preventing cross-job pollution.
- **Cron silently dispatching mismatched provider/model pairs** — reversed cron config (e.g. `default_model = "zhipu"` where `zhipu` is a provider name) produced impossible pairs like `dialagram/zhipu` that timed out with no diagnostics. Added validation: if `effective_model` is not in the provider's `supported_models()`, the job is skipped with a loud error logging the job name and the bad pair.
- **RSI feedback recorded pre-remap provider/model pairs** — when `helpers.rs` remaps a mismatched model to the provider's `default_model()`, RSI still recorded the original impossible pair (e.g. `dialagram/zhipu`). All 3 recording sites in `tool_loop.rs` now resolve the actual model that will be sent before constructing the feedback dimension.
- **`@` file picker swallowed results in large repos** — recursive walk with `.hidden(false)` traversed `.git/` before sibling source dirs, exhausting the 5000-result cap on pack/ref files alone. Added filter skipping `.git`/`.hg`/`.svn` and raised cap to 20k.

### Changed

- **README updated** with three embedding modes (Local GGUF, OpenAI-compatible API, FTS5-only) and auto VPS detection documentation.

### Removed

- **`examples/browser_test.rs`** — manual browser debug script replaced by `browser_e2e_test.rs` regression suite.

## [0.3.18] - 2026-05-10

### Added

- **Codex CLI built-in provider** — full subprocess-based integration with OpenAI's `@openai/codex` CLI. User authenticates once via `codex` CLI; OpenCrabs piggybacks on cached credentials (zero API key handling). Non-interactive mode via `codex exec --json` with JSONL streaming. Models: GPT-5.5, GPT-5.4, GPT-5.3-Codex. Wired into `/models` picker, `/onboard` wizard, factory registry, and config (`[providers.codex_cli]`).
- **Repo-audit skill** — language-agnostic repository health checks. 5-phase pipeline: language detection → native tool execution → git metrics → language-specific AST analysis → scoring + recommendations. Covers Rust, JS/TS, Python, Go with per-language metrics for error handling, dependencies, naming conventions, module structure, and god-file detection.
- **Generic `deliver_api_key` for cron jobs** — HTTP webhook Bearer token auth configurable per-job via `cron_manage` tool. Replaces hardcoded provider-specific auth in `deliver_http`. New `deliver_api_key` column on `cron_jobs` table (migration 21).
- **`browser_close` tool** — close browser tabs and free CDP sessions. Prevents stale page reuse across browser actions. Includes opt-in e2e regression suite.

### Fixed

#### Providers & Security
- **Gemini API key leaked in URL query string** — `analyze_video` passed the API key as `?key=...` in two call sites (resumable upload init + file-state polling), triggering CodeQL #64 (HIGH). Moved to `x-goog-api-key` header, matching `analyze_image` and `generate_image`.
- **Cloud handshake timeout killing slow-but-healthy providers** — bumped from 30s to 60s. Routing proxies like dialagram legitimately take 20-45s; 30s was murdering mid-request.

#### Browser
- **Network idle wait after navigate** — was only waiting for CDP `load` event, missing async fetches. Now waits for `networkIdle`.
- **CDP manager lock held across await** — lock was held during screenshot await, blocking concurrent browser operations. Dropped before await.
- **Missing CDP pre-flight health check** — screenshot could fail on stale CDP connection. Added health check before capture.
- **Silently dropped browser errors** — navigate errors were swallowed with `let _ =`. Now logged at WARN.

#### Stream & TUI
- **File paths starting with `/` treated as slash command typos** — `/Users/.../file.pdf yo crabs check this` triggered "Unknown command". Added `looks_like_file_path()` helper gating both TUI and channel slash-command handlers.
- **Truncation continuations triggered provider fallback** — mid-sentence continuations should stay on the same provider. Fallback now skipped for truncation paths.
- **Fallback error reason hidden from TUI** — when fallback fired, the underlying error was swallowed. Now surfaced as a system message.
- **Pipe-delimited rows not hard-broken** — when not recognized as a table, pipe rows ran together. Added hard-break between rows.

### Refactored

- **Extracted `truncation.rs`** — truncation-mid-sentence continuation path pulled out of the main service module.
- **Extracted `feedback.rs`** — feedback ledger writes isolated into their own module.
- **Extracted `compaction.rs`** — `enforce_context_budget` logic separated from service.
- **Replaced magic provider indices** — hardcoded index lookups replaced with `index_of_provider()` helper across TUI and onboarding.

## [0.3.17] - 2026-05-06

### Added

- **Video vision support (Phase 1, Gemini-native)** — new `analyze_video` tool routes video attachments through Gemini's multimodal API. Inline-bytes path for files ≤18 MB; resumable Files API upload + ACTIVE-state polling for larger uploads. `FileContent::Video(PathBuf)` variant + `<<VID:path>>` marker (analogous to `<<IMG:>>`). MIME table covers mp4/m4v/mov/webm/mkv/avi/3gp/flv. Frame-extraction fallback for non-Gemini providers deferred.
- **Video uploads supported across all channels** — Slack, Telegram, Discord, WhatsApp, and Trello automatically route video attachments to `analyze_video` when `image.vision.enabled` with a non-empty API key.
- **TUI video attachments** — pasting a video path emits `<<VID:path>>` on send; top-right indicator labels each as `Video #N`; chat display rewrites `<<VID:...>>` → `[VID: clip.mp4]`.
- **Partial JSON repair** — new `json_repair` module closes unterminated strings, balances brackets, strips trailing commas, and drops trailing keys-without-value. Wired into 5 drop sites across OpenAI-compatible providers and the ContentBlockStop finalizer. Unrecoverable input returns a `{"_partial": ..., "_repair_failed": true}` envelope instead of crashing the turn.
- **TCP keepalive on all HTTP clients** — 15s keepalive on Anthropic, Gemini, and custom OpenAI-compatible providers. Detects silent TCP drops at the OS level in ~15-45s instead of waiting for the 300s idle timeout.
- **Health-aware sticky fallback persistence** — `FallbackProvider::new_with_health()` checks `provider_health.json` on creation and advances the active index if the primary has 2+ consecutive failures and a fallback has more recent success. Sticky fallbacks now survive restarts.
- **RetryAttempt progress event** — new `ProgressEvent::RetryAttempt { attempt, max, reason }` emitted on stream-drop retries, translated to TUI system messages ("⏳ Retry 2/3 — stream dropped") so the user sees transient recovery in progress.
- **Self-heal phantom detection expanded** — catches "Now <file-op gerund>" phantoms (creating/writing/editing/...) and build/deploy intent + past-tense completion claims. Gaslighting and phantom detectors extracted into their own module.
- **RSI escalation for repeat violations** — RSI now bumps a violation counter on existing rules instead of deduping repeat violations away. Rules that keep getting broken get louder, not silenced.
- **OpenRouter response caching** — zero cost for identical requests.
- **4 safe built-in skills** — opencli, browser-cdp, a2a-gateway, dynamic-tools. SKILLS section added to help screen and splash integration.
- **Thinking content persisted to DB** — captured on both ResponseComplete and IntermediateText events.
- **Approval policy read at runtime** — loaded from config on every tool request instead of cached at startup.

### Fixed

#### Providers & Context
- **Claude CLI context leak** — `cli_manages_context()` defaulted to `cli_handles_tools()`, causing Claude CLI to silently opt out of compaction. Context grew unbounded (484k/200k = 242%). Override returns `false` so OpenCrabs owns compaction for Claude CLI sessions.
- **Sticky fallback not sticking** — stream error path dropped the restore guard without nulling `original`, so Drop always reverted to primary. Both stream error and 5xx paths were missing `ProviderSwitched` event emission. Fixed guard pattern and added events.
- **ProviderSwitched handler incomplete** — handler updated DB and `current_session` but not `session_providers` Arc, causing stale footer on pane switch. Now creates new provider via `create_provider_by_name` and calls `swap_provider_for_session` + updates `provider_cache`.
- **Per-session provider isolation** — `save_provider` invalidation loop nuked ALL sessions matching provider name, breaking other panes' pins. Now only invalidates the current session's stale entry. Footer reads live `session_providers` instead of stale DB data.
- **Per-session swap persistence** — per-session swap now sticks in both DB and memory; live-fetched models are honored; `/models` save sources model from session provider, not global; locked {provider, model} pair enforced on every response write.
- **Sticky-fallback pair persisted independently** — fallback pair now written to DB without depending on the progress callback firing.
- **Per-session context window isolation** — prevents cross-session contamination of context budgets.
- **Oversized tool_result bodies capped at 50 KB** before entering context.
- **Compaction restored to synchronous** — reverted async spawn-then-swap (was causing race conditions); restored pre-regression `enforce_context_budget` logic.

#### Channels
- **Slack intermediate-vs-final dedup race closed** — IntermediateText spawns are fire-and-forget `tokio::spawn` tasks; they post to Slack (~hundreds of ms) and then push `(ts, hash)` into the dedup list. Stream-end fires the final-response handler nearly instantly, which used to read the list before the in-flight push completed and posted the same body a second time. Now every IntermediateText `JoinHandle` is captured and awaited before the dedup check. A post-completion sweep runs after the final post and deletes any late entry whose hash matches `final_hash`, as belt-and-suspenders against future progress sources that race the same way. `<<VID:>>` markers also stripped symmetrically in both intermediate and final paths so video flows preserve hash-match dedup.
- **Slack `chat_update` / `chat_delete` failures now logged** — the `let _ = session.chat_{update,delete}(...).await;` pattern silently dropped API errors, masking dedup-related issues (when a non-matching intermediate fails to delete, it sticks around alongside the final post and looks like a duplicate). Five sites converted to `if let Err` with site-context warnings.
- **Telegram 20 MB Bot API download cap now surfaces to user** — Telegram chats accept uploads up to 2 GB but the Bot API hard-caps `getFile` downloads at 20 MB. When a user sent a larger video / animation / video_note / document the `?` operator propagated the error up and logged at ERROR — user heard nothing back. New `fetch_file_or_notify` helper detects "file is too big" and replies "compress to under 20 MB and resend"; applied to all six download sites (voice, photo, video, animation, video_note, document).
- **Telegram dropped video / animation / video_note silently** — handler only branched on text/voice/photo/document, so every other media type returned `Ok(())` with no response. iPhone `.mov` uploads (auto-converted by Telegram to MP4-backed Animation) were the most visible casualty. Now downloads + routes through `process_file_with_vision`, including handling Telegram's occasional `image/gif` MIME on MP4 animations.
- **Clean display text in TUI** — Telegram/Discord/Slack/WhatsApp/Trello all persist clean text to DB and TUI instead of LLM metadata brackets. Each handler builds appropriate display: bare text for owner DMs, `Sender: text` for groups/non-owner.
- **Slack duplicate-completion gap closed** — dedup map now shared across the handler to prevent double responses.
- **Slack intermediate text cleaned** — `<<IMG:path>>` and `<antThinking>` markers stripped from intermediate text.
- **Slack dedup scoped to one turn** — global window was dropping legitimate responses.
- **Slack empty-final guard** — keeps intermediates as visible answer when final is empty.

#### UI & Other
- **Scroll offset capped** and first-render inflation explosion prevented.
- **Streaming scroll compensation removed** — was inflating offset by 1000+ lines during thinking model streaming.
- **Exit/start splash updated** — skills section added, `/models` command corrected (was `/model`).
- **Brain directory path exposed** in core prompt context index.
- **Session working directory survives crash recovery.**
- **TOOLS.md and CODE.md moved** from core to contextual brain files.
- **Session IDs no longer dumped in assert messages** (CodeQL #62, CWE-312).

## [0.3.16] - 2026-05-02

### Added

#### Mission Control
- **`/mission-control` full-screen dialog** — three panels in one place: pending RSI proposals (Inbox cards), recent RSI activity (improvements log feed), and the schedule queue (cron jobs + paused/active state).
- **Apply / reject inbox proposals inline** with `a` / `r` — same machinery as the agent's `rsi_proposals` tool, byte-identical install. Notification surfaces the result; inbox refreshes immediately.
- **Keyboard navigation** — Tab/Shift-Tab cycle panels, j/k or ↑/↓ within, g/Home and G/End to jump, Enter for the detail popup, Esc to close.
- **Modular layout** — three parallel module trees (`brain/mission_control/` data services, `tui/render/mission_control/` panel renderers, `tui/app/mission_control/` state + input + actions). Pure layout fn + pure decide fn keep the geometry and keystroke contracts unit-testable without spinning up an `App`.

#### Skills (cross-harness `SKILL.md`)
- **Cross-harness skill format** — `~/.opencrabs/skills/<name>/SKILL.md` files with YAML frontmatter (`name`, `description`) plus markdown body. Same format works on Claude Code, Anthropic managed agents, and OpenClaw.
- **Embedded built-in skills** with user-directory overlay — built-ins ship with the binary via `include_str!`; user files at `~/.opencrabs/skills/<name>/SKILL.md` override by file presence.
- **Auto-registration as `/<name>` slash commands** across the TUI input bar **and** every connected channel (Telegram, Discord, Slack, WhatsApp). No `commands.toml` entry needed.
- **`/security-audit` skill** — language-agnostic security & CVE audit. Detects project type from manifests (Cargo / npm / Go / Python / Dart / Ruby / PHP / Java / Swift / .NET), runs the appropriate scanner, reviews the diff for injection / auth / crypto / deserialization / path-traversal patterns, and scores 0-100.
- **`/cost-estimate` skill** — codebase cost-to-build estimate, AI-assisted ROI breakdown, and fair-market valuation.
- **`/skills` picker dialog** — full-screen filterable browser for every loaded skill. Type-to-narrow (case-insensitive on name + description), Tab/Shift-Tab cycle wraps at edges, Enter runs the selected skill, Esc closes. When the filter narrows to a single match, Enter just fires it.

#### RSI
- **Autonomous tool & command proposals via inbox** — the RSI background loop now proposes new dynamic tools and slash commands. Lands in `~/.opencrabs/rsi/proposed_*.toml`; user reviews via the `rsi_proposals` tool or Mission Control's Inbox panel. Banner on session start shows pending count.

#### Compaction
- **Async proactive context compaction** — the 65% soft trigger now spawns the LLM summarization task in the background. The agent keeps processing turns; subsequent visits to `enforce_context_budget` atomically swap the summary in once the spawned task finishes. The 90% hard-truncate path cancels any in-flight compaction so a stale snapshot can't land on top of fresh truncation.

#### CI
- **`cargo audit` job** for dependency CVE scanning on every push/PR via the prebuilt `taiki-e/install-action`. One advisory ignored: `RUSTSEC-2024-0437` (DOS-class only, transitive via `crabrace → prometheus`, no upstream fix).

#### Security hardening (Bash)
- **`setsid` `pre_exec` hook** — every bash-tool child detaches from the controlling TTY before exec. Programs that bypass `stdin` and open `/dev/tty` directly (ssh password prompt, sudo getpass fallback, gnu readline) can no longer steal the user's TTY and leak escape sequences into the chat.
- **SSH password askpass flow** — `ssh` / `scp` / `sftp` / `rsync` go through a probe-then-prompt sequence: probe with `BatchMode=yes -o ConnectTimeout=15`; on a recognisable auth-failure stderr, prompt the user via the existing TUI password dialog and retry with `SSH_ASKPASS` pointing at a 0600-tempfile-backed shell script.
- **POSIX single-quote askpass tempfile path** — askpass shell script now wraps the tempfile path in POSIX single quotes (with `'\''` escape for embedded quotes), defeating any `$TMPDIR` containing `$` / `` ` `` / `"`.

### Changed

- **Slash autocomplete dropdown is now responsive** — width grows to fit the longest visible row (capped at terminal width − 1). Long skill descriptions no longer panic ratatui's buffer write or get hard-clipped without ellipsis. Description column lines up vertically across rows via a precomputed name-column width.
- **Skills layer doc + format work** — adopted the de-facto `<name>/SKILL.md` directory layout with YAML frontmatter; documented format + auto-registration + cross-harness portability across the README, TOOLS.md template, and the user brain file.
- **Paragraph-level dedup for brain-file appends** — RSI sync now dedups at paragraph granularity rather than section, preserving more user content during upstream template merges.
- **Brand palette lifted into `tui/render/palette.rs`** — orange / teal / white / text shades centralised; mission_control/theme.rs now re-exports from the shared palette and only carries panel-specific aliases. Future dialogs reuse the palette without dipping into MC's namespace.

### Fixed

#### Providers
- **OpenCode `/models` selection now persists** — `merge_provider_keys` was missing an explicit branch for the `opencode` provider, so the api_key written to `keys.toml` never made it into the runtime config. The factory then reported "OpenCode enabled but API key missing", the rebuilt agent fell back to the previous provider, and the session footer never updated to reflect the user's selection. Added the merge branch (mirroring qwen's auto-enable-on-key pattern) plus 4 regression tests in `merge_provider_keys_test.rs` covering the persistence, sentinel-placeholder rejection, and the disabled-state-preservation invariant.

#### TUI
- **Custom provider dialog clipping** across three sub-fixes — separators (5029035), `↑↓ more` indicators (ba7fe8d), and base form-line accounting (ed8e2f1). Custom-provider dialogs no longer clip the bottom fields when models are fetched.
- **Custom providers fetch `/v1/models`** on paste, key entry, and Enter (c5b5b65, e753bde) — model list picker shows fetched models for custom providers when the endpoint responds (196514b).
- **Scope `ProviderSwitched` and `SystemMessage`** to originating session (3495a8a) — a 429 fallback in session A no longer pushes alerts into session B.
- **Match provider IDs with hyphens, not underscores** in `save_provider_selection` (480785d).
- **Prevent scroll drift during streaming** when the user has scrolled up (05e753c) — viewport stays anchored to the same content while streaming continues.

#### Compaction
- **Demote routine async-compaction logs to debug** (74cb9de) — only the 90% hard-truncate floor / stuck compaction (>10 min) / panics / cancellations remain at warn or error.

#### Session search
- **Direct SQL `LIKE` query** against the messages table (bd450d6) — replaces the stale QMD-indexed flow that lagged behind for active sessions and large conversations.

#### Pending requests
- **Purge stale rows on startup** (6624411) — rows older than 10 minutes are deleted before fetching interrupted requests, stopping the "found 23 interrupted request(s) — resuming" replay loop after `cargo run` restarts.

### Tests

- 13 new test files added: `bash_ssh_detection_test` (10), `bash_posix_quote_test` (9), `rsi_proposals_test` (13), `skills_test` (14), `skill_slash_dispatch_test` (7), `slash_autocomplete_dimensions_test` (12), `mission_control_layout_test` (7), `mission_control_inbox_service_test` (6), `mission_control_activity_service_test` (8), `mission_control_schedule_service_test` (5), `mission_control_input_test` (23), `skills_dialog_test` (18), `merge_provider_keys_test` (4).
- **2,522 tests passing** (+176 since v0.3.15)

## [0.3.15] - 2026-04-28

### Added

#### Providers
- **OpenCode as native built-in provider** — Go + Zen plans supported. No API key needed, free local completions via OpenCode binary.
- **Ollama as native built-in provider** — local models via Ollama API. Run any Ollama model natively without custom provider setup.

#### Agent
- **Persist recent file paths** across sessions to anchor the agent — remembers which files it was working on after restart.
- **Centralized temp file cleanup** on startup — stale temp files from crashed sessions cleaned automatically.
- **Personalized welcome message** with first-time detection — new users get a tailored greeting on first launch.

#### RSI
- **Upstream template sync** with version gate and append-only diff — brain templates auto-sync from repo without duplicating existing content.

#### Docs
- **README updated** with Ollama native provider, path normalization, recent file memory, bash loop prevention.
- **Onboard video** added to README — visual setup guide for new users.

#### Tests
- **Provider factory regression suite** — 26 tests covering all provider creation paths.
- **RSI sync and model fetching regression tests** — template sync and custom provider model fetching covered.

### Changed

- **Provider factory refactored to registry pattern** — all providers registered centrally. Cleaner architecture, easier to add new providers.

### Fixed

#### Providers
- **Generic model fetching for custom providers** (solves #63) — custom providers can now fetch their model list via `/models`.
- **Show models for custom and CLI providers** in `/models` dialog — all provider types now support live model listing.
- **Remove opencode alias collision** — custom providers with built-in names no longer spawn wrong subprocess.
- **HTTP 402 fallback chain** — quota/payment exhausted walks to next provider. No more terminal errors when a provider's quota runs out.
- **Stream-handshake timeout tightened to 30s** for cloud providers — faster failure detection on unresponsive upstreams.

#### TUI
- **Processing spinner clears immediately** on response complete — DB write and plan reload spawned in background. Spinner no longer stuck for 5+ seconds after agent finishes.
- **Tool call group flushes immediately** when all calls complete — no longer waits for next text chunk. "Processing:" label disappears as soon as tools finish.
- **Path normalization across the board** — `$HOME` collapsed to `~` in system prompt, tool display, and brain files. Cleaner display, less token waste on long paths.
- **Viewport freeze during streaming** — scroll up while streaming without being yanked back. Read earlier messages while the agent is still responding.
- **Usage "By Activity" columns sized by max(header, data)** — dashboard columns no longer clip on narrow terminals.
- **Simplified model change message** to provider/model format — cleaner status messages when switching models.

#### RSI
- **Brain files are now append-only** with backup-before-write — prevents accidental overwrites and corruption of brain files.
- **Suppress RSI alerts** whose dimension already has a fix commit — no more noise from already-resolved failure patterns.
- **Window tool failure stats** so stale alerts age out — old failures stop triggering alerts after they're no longer relevant.

#### Bash
- **Short-circuit same-command retries** — agent stops looping on identical failing commands. Prevents infinite retry loops that waste tokens and time.
- **Reject interactive commands up-front** instead of looping — interactive commands (vim, git add -p) fail immediately with helpful hint.

#### Channels
- **Slack dedup overhaul** — content-level dedup, composite-key catches retries, VecDeque FIFO eviction. Eliminates duplicate responses from Slack retry storms.
- **Key channel sessions by stable chat_id** instead of title — sessions survive group name changes.
- **Per-session message queue** with isolated display — queued messages no longer cross between sessions.
- **Prevent crossed provider/model pairs** in channel model switching — each channel session keeps its own provider correctly.

#### Voice
- **5-minute timeout + two-stage fallback** for TTS opus conversion — TTS no longer hangs indefinitely on conversion failures.
- **ffmpeg timeout + fallback** for Telegram TTS — Telegram voice replies more resilient.

#### Browser
- **Name actual browser in launch errors** and bump timeout — clearer error messages when Chrome/Chromium fails to start.

#### CI
- **Pricing fallback for token tracking tests** in CI — tests pass even without live pricing data.

**2,479 tests passing** (+133 since v0.3.14)

## [0.3.14] - 2026-04-24

### Added

#### Voice
- **Voicebox TTS engine selection** support — Voicebox can now be selected as the TTS engine alongside OpenAI and Local.
- **Voicebox engine field** added to onboarding flow — onboarding wizard now includes Voicebox as a TTS option.

#### Database
- **Thinking column** for non-CLI provider reasoning content storage — new `thinking` column in messages table stores reasoning content separately.

#### Usage Dashboard
- **Kimi K2.6 pricing** added — Kimi model costs now tracked correctly.
- **GLM pricing and display labels** — z.ai GLM models now have correct pricing and human-readable labels.
- **Request count** shown in By Model card — usage dashboard now displays request count per model.

### Changed

- **Removed hardcoded pricing**, moved to usage/pricing.rs — pricing is now centralized in a single module instead of scattered across files.

### Fixed

#### Provider & API Resilience
- **Kimi/Moonshot reasoning_content support** — assistant messages with tool calls now include `reasoning_content` field when thinking mode is enabled. Kimi rejected requests with 400 when thinking was enabled but reasoning_content was missing from assistant tool-call messages.
- **Proxy error envelope unwrapping** with automatic retry — proxies sometimes wrap errors in nested JSON envelopes; now unwrapped and retried instead of failing immediately.
- **HTTP 401 distinction** — model-unsupported vs actual auth failure. 401 responses from "model not supported" no longer trigger auth-error fallback chains.
- **5xx retry** with exponential backoff and automatic fallback chain — transient server errors now retry in-place before walking the fallback chain.
- **Claude CLI opus alias** bumped to 4-7 — keeps the Claude CLI provider pointing at the latest Opus model.
- **Gemini analyze_image error logging** for debugging vision failures — vision errors now log the actual response body instead of generic connection errors.
- **Custom provider api_key status** displayed in startup logs — startup logs now show whether custom providers have real API keys loaded.

#### Channel & Session Isolation
- **Session isolation from TUI** — Telegram sessions never inherit TUI's provider on restart. Removed fallback to global provider when `create_provider_by_name` fails; sessions keep their stored provider.
- **Custom provider key merging** — keys.toml entries merged even when config.toml lacks `[providers.custom]` section. `merge_provider_keys` now creates minimal custom provider entries from keys.toml data instead of silently dropping them.
- **Per-session provider swap** with per-chat isolation — each session carries its own provider instance; switching models in one session never affects others.
- **Pin provider per-session** on /models command — provider selection persists to the session, not global config.
- **/models shows actual current provider** instead of stale global — model switch dialog now reads from the session's own provider.
- **Restore missing sticky_swap** functionality — provider swaps now persist across session reloads.
- **Duplicate message elimination** on Telegram — pre-send dedup prevents the same intermediate from being sent twice during streaming.
- **Duplicate response fix + multi-image support** on Slack — Slack no longer sends duplicate responses; multiple images handled correctly.

#### TUI & Display
- **New sessions use global provider/model pair** — never crossed. `create_new_session` now reads from `agent_service.provider_model()` instead of stale `default_model_name`.
- **Tool-call tag bleed prevention** in TUI rendering — removed false-positive markers and `is_local_stream` gate so filtering runs for all providers.
- **Stop persisting literal "default"** string as default_model — empty model names no longer get written as the string "default".
- **Balanced JSON scanning** for Claude CLI tool-call markers — JSON scanning now handles balanced braces correctly without false positives.
- **DisplayMessage loads thinking** from dedicated DB column — thinking content now loads from the `thinking` column instead of being reconstructed from markers.
- **Strip orphan thinking tags** on session reload — orphan `<think>` tags from previous sessions are stripped on load.

#### Self-Healing Engine
- **49 "let's" contraction variants** added to phantom detection — models frequently write "let's check the logs" instead of "let me check"; these were slipping through.

#### RSI Feedback
- **Provider name in feedback records** — dimension now includes provider/model pair. RSI logs previously showed only model name (e.g. `qwen-3.6-plus`), making it impossible to identify which provider failed.

#### Database
- **Migration idempotency** for thinking column — migration now handles the case where the column already exists.

#### Usage Dashboard & Pricing
- **Normalized model names** and recalculated all costs — model name variants now merge correctly for accurate cost tracking.
- **Total cost sums** now use recalculated per-model costs — dashboard totals are accurate after per-model cost recalculation.
- **GLM 5.1 and GLM 5 Turbo** tracked separately — two GLM variants no longer merge into one cost entry.

#### Input Handling
- **Unicode whitespace normalization** on paste — non-breaking spaces and other Unicode whitespace now normalize to regular spaces.
- **Tab expansion** on paste for consistent input — tabs in pasted content are expanded to spaces.

#### Tools
- **Detach subprocess stdin** from TUI's TTY to prevent hangs — subprocesses no longer inherit the TUI's terminal stdin.

#### Tests
- **Migration count** bumped to 19 — thinking column migration added but count wasn't updated.
- **Token test model names** corrected to match pricing file — `opus-4-6` changed to `claude-opus-4` to match pricing entries.

**2,145 tests passing** (+14 since v0.3.13).

[0.3.14]: https://github.com/adolfousier/opencrabs/compare/v0.3.13...v0.3.14

## [0.3.13] - 2026-04-20

### Added

#### Self-Healing
- **14 investigative intent phrases** (`let me hunt/trace/track/look into/check into/find out/dig into` + `i'll` variants) — catches "Let me hunt down where..." drops where agent announces investigation but never calls tools.
- **14 "Now + gerund" patterns** (`now cherry-picking/updating/fixing/committing/pushing/merging/rebasing/deploying/building/testing/checking/applying/restarting`) — catches status-report-then-action drops like "Now cherry-picking to main..." followed by silence.
- **Regex-based gerund detection** with sentence boundary requirement — prevents false positives like "Are you now checking the logs?".

#### Browser
- **`browser_find` tool** — enumerate page elements with stable selectors (CSS/XPath/text/aria modes). Returns selectors passable directly to `browser_click` / `browser_type` without ambiguity.

### Changed

- **Extracted `has_investigative_intent()`** as public helper + re-exported in service mod — enables test coverage for phantom detection logic.

### Fixed

#### Self-Healing
- **Pre-send intermediate dedup** on Slack — checks `sent_intermediates` before sending. Same intermediate emitted twice during streaming no longer sends twice.
- **Sanitized intermediate storage** on Slack — `strip_llm_artifacts` + `redact_secrets` applied before storing. Final response `.replace()` dedup now matches correctly (raw vs sanitized mismatch).
- **Image download fallback** on Slack — tries `url_private` when `url_private_download` returns HTML. Slack sometimes returns HTML preview page instead of raw bytes; now falls back instead of saving HTML as `.png`.
- **Pre-send intermediate dedup** on WhatsApp with `sent_intermediates` tracking + sanitized storage — same pattern as Slack/Telegram, prevents duplicate intermediates during streaming.

#### Browser
- **`browser_click` waits for network-idle** instead of fixed 500ms sleep — pages with async loads stabilize before screenshot.
- **Screenshot failure surfaced** in tool result instead of swallowed — users see actual error instead of silent failure.
- **`browser_eval` caps output at 50 KB** — prevents massive DOM dumps from blowing up context.
- **Waits up to 10s for user's Chrome profile** to unlock before fallback — reduces SingletonLock failures when user's Chrome is running.
- **Per-session tab isolation** — no more cross-session DOM stomping. Multiple concurrent sessions no longer interfere.
- **Drop impl aborts CDP handler** + releases Browser properly — prevents zombie Chrome processes on session end.
- **Stealth JS registered** via `addScriptToEvaluateOnNewDocument` — anti-bot detection runs before any page script.
- **Detects dead CDP handler** and relaunches instead of zombie — recovers from crashed Chrome automatically.
- **Sweeps stale SingletonLock** before launch — fixes launch failures from crashed Chrome leaving lock files.
- **Detects user's default browser** correctly on macOS — uses `LSHandlers` from `com.apple.LaunchServices` plist.

#### Tools
- **`wait_agent` resolves by prefix or label**, lists actives on miss — no more "No sub-agent found" when using short IDs or labels.
- **`exa_search` supports stateless MCP servers** (missing session header) — fixes "MCP server did not return session ID" errors.
- **`http_request` sets default User-Agent** — stops GitHub API 403 Forbidden errors.

### Tests

- **5 investigative intent tests** in `src/tests/self_healing_test.rs` — coverage for 14 new phrases + false positive prevention.
- **2 gerund pattern tests** with 17 assertions — coverage for "Now + gerund" detection + false positive prevention.
- **12 tests** for `wait_agent` id resolver and miss-message.
- **8 tests** for macOS LSHandlers default-browser parser.
- **3 tests** for `http_request` User-Agent default fix.
- **4 tests** for `exa_search` stateless-MCP fallback.
- **Linux xdg + Windows reg** default-browser tests.

**2,131 tests passing** (+75 since v0.3.12).

[0.3.13]: https://github.com/adolfousier/opencrabs/compare/v0.3.12...v0.3.13

## [0.3.12] - 2026-04-18

### Added

#### Voice
- **OpenAI-compatible STT** via `stt_base_url` + `stt_model` + `providers.stt.openai_compatible.api_key` — any Whisper-compatible `/v1/audio/transcriptions` endpoint (self-hosted Whisper, Deepgram-compatible proxies).
- **OpenAI-compatible TTS** via `tts_base_url` + `tts_model` + `tts_voice` + `providers.tts.openai_compatible.api_key` — any `/v1/audio/speech` endpoint (self-hosted Coqui/Bark, ElevenLabs-compatible proxies).
- **Voicebox STT** via `voicebox_stt_enabled=true` + `voicebox_stt_base_url` — self-hosted open-source voice stack, no API key.
- **Voicebox TTS** via `voicebox_tts_enabled=true` + `voicebox_tts_base_url` + `voicebox_tts_profile_id` — async `POST /generate` → poll `/generate/{id}/status` → fetch audio from returned path (HTTP(S) URL / server-relative path / local filesystem).
- **Unified provider dispatch** in `voice::transcribe` / `voice::synthesize` — priority chain: Voicebox → OpenAI-compatible → (Groq STT / OpenAI TTS) → Local. First match wins. Every TTS output normalised to OGG/Opus via `ensure_opus` before return.
- **`SttProvider` / `TtsProvider` named enums** replace string discriminators — typed variants (`Off` / `Groq` / `OpenAICompatible` / `Voicebox` / `Local`) prevent mismatched-string bugs at compile time.
- **TTS voice reply** on Slack via `files.upload` (OGG/Opus, inline waveform UI) — was missing entirely; Slack voice-input users got text-only replies.
- **Full STT/TTS parity** across Telegram / WhatsApp / Discord / Slack — single code path via `crate::channels::voice::{transcribe, synthesize}`; no channel reinvents voice handling.
- **TTS dispatch logs** (`tracing::info!`) name the provider + its params — failures now point at the right config; previously silent on the happy path.

#### Onboarding
- **Voice screen rewrite** with 5-provider radio selectors per mode — Off / Groq / OpenAI-compatible / Voicebox / Local for STT; Off / OpenAI / OpenAI-compatible / Voicebox / Local for TTS. Fields shown/hidden based on selected provider.
- **Voice API keys wired to `keys.toml`** — adds `providers.stt.openai_compatible.api_key` + `providers.tts.openai_compatible.api_key`; previously only `config.toml` received the field.

#### Brain
- **`TOOLS.md` always injected** into core brain — on-demand loading caused the model to guess CLI syntax from training data (duplicate `gdrive` uploads on 2026-04-18). Actual tool syntax now always available.

### Fixed

#### Voice
- **Audio fetch supports HTTP(S) + server-relative + filesystem paths** — fixes silent "audio never arrived" when server returns a URL instead of a local path.
- **Async polling** — `POST /generate` → poll `/generate/{id}/status` every 500 ms up to 120 s. Fixes hang-or-fail on servers that don't synthesise synchronously.
- **ffmpeg uses `pipe:0`/`pipe:1`** instead of `/dev/stdin`/`/dev/stdout` — the named device paths don't exist on macOS; the pipe syntax works on Linux and macOS.
- **ffmpeg output uses `-f ogg`** explicitly — raw Opus without Ogg container was being emitted; Telegram rejects it. Every channel now gets a proper Ogg/Opus file.
- **`voice_msg_ids: Vec<MessageId>`** tracked in `StreamingState` — doc-comment-enforced invariant: cleanup paths must never iterate this list. TTS voice notes are the most expensive artefact the bot produces; losing one to a well-intentioned sweep is a regression we've made hard to introduce.
- **Diagnosable voice drop on mid-stream cancellation** — when a new user message cancels a voice-input turn before the TTS block runs, `tracing::warn!` names the session. Previously silent.
- **`send_voice` error no longer swallowed** — logs real error + Debug repr instead of a generic "TTS failed" placeholder.

#### Onboarding
- **Paste routes to the focused voice field** — old handler keyed on field indices that shifted when providers were added; pasting into TTS base URL could land in STT model name.

#### Telegram
- **Kill duplicate responses at the source** — `ends_with_url` helper in `looks_truncated_mid_sentence` + pre-send dedup. URL-terminated replies were flagged truncated → model re-stated the whole answer → duplicate intermediates.
- **Keep prior intermediates + tool-call history on follow-up cancel** — cancel path used to delete every prior intermediate + tool bubble. Now only deletes the typing placeholder.
- **`redact_secrets` applied to intermediates** — consistency with final-response redaction; otherwise a redacted final wouldn't match an un-redacted intermediate and dedup failed.
- **URL path segments no longer redacted as secrets** — paths like `/api/v1/users/123` were treated as tokens and stripped. Redaction only fires on actual-secret patterns now.

#### Provider / Session Resilience
- **Construct session provider by name on custom pick** — `/models` save calls `create_provider_by_name(&cfg, chosen)` instead of cloning `agent_service.provider()`. Fixes "session says opencode2 but routes to opencode" regression.
- **Verify `enabled=true` actually lands on disk**, retry via `try_write` on drift — no more silent divergence between dialog state and config.toml.
- **Sticky fallback on 429 / 401 / 403** — session provider swap holds, `ProviderSwitched` persists to DB, alerts read `'provider/model'`. Users with opencode / opencode2 / opencode3 can now tell which subscription got limited.
- **Cancellation-safe swap** via `FallbackProviderGuard` Drop — restores on error + future-drop. Fixes "400 Unknown Model" from a mid-fallback cancel.
- **Hard invariant against mismatched `{provider, model}` pair** in `stream_complete` — remaps to provider default + warns if model not in `supported_models()`. Catches every stale-pin / upstream-bug / cancelled-fallback path.
- **Custom provider wins name collisions** against built-in ids (`opencode`, `anthropic`, …) — prevents custom entry with a built-in name from silently spawning a CLI subprocess.
- **Custom edit = update in place; rename = table-key move** with api_key preserved — fixes rename-duplicate that left `opencodeiolo` keyless on 2026-04-18.
- **401 / 403 trigger fallback chain** instead of terminal error — retry-in-place is pointless when the key is bad; next provider has its own key.
- **`session.provider_name` not silently overwritten on swap failure** — old code overwrote with fallback's name in DB; next restart loaded the wrong provider.
- **Bot replies recorded in `channel_messages`** table (Telegram/Discord/WhatsApp/Slack) — `channel_msg_repo.recent()` builds group/channel conversation context on every turn. Previously stored only user messages; bot saw a one-sided transcript.

#### TUI
- **Reasoning renderer double-wrap + double-padding eliminated** (3 sites in `chat.rs`) — each paragraph was wrapped twice → inconsistent 2-vs-4 space continuation indent + awkward word breaks. Renderer reserves outer indent in wrap budget, skips second pass.
- **Flush-left reasoning wraps** — continuation padding switched from `"  "` to `""`. Continuations now align with paragraph starts at a consistent 2-space column.
- **Faster `/models` navigation** — mid-field navigation skips `rebuild_agent_service`. Old flow froze 5–10 s per field move. Full rebuild runs once on final Enter.
- **Full `/usage` dashboard on Telegram/Discord/Slack/WhatsApp** — was showing only current-session line + top 5 models; now renders 5 period-grouped cards per period, within the 4096-char Telegram cap.

## [0.3.11] - 2026-04-17

Major refactor: Qwen OAuth rotation replaced with DashScope API-key provider,
per-session provider isolation, local model tool-call extraction, and 40+ TUI
and self-heal fixes.

The headline change is the DashScope migration: Qwen OAuth rotation (10+ files,
device flow, credential persistence, rotation logic) was replaced with a simple
API-key provider (`qwen3.6-plus` default). The CLI provider was also removed.
This deletes ~2,500 lines of complexity.

Local models (Qwen, Unsloth) now have their tool-call hallucinations
auto-extracted from text content: bare JSON `{"tool_calls":[...]}` envelopes,
Claude-style XML `<TOOLNAME><PARAM>value</PARAM></TOOLNAME>`, and
Qwen-specific formats are all recovered into real executable tool calls.

Per-session provider isolation means switching models in one session no longer
affects Telegram, Discord, or other sessions. Each session carries its own
provider and context budget.

### Added

#### Qwen/DashScope Migration
- **DashScope API-key provider** — replaces OAuth rotation with a single
  API key. `qwen3.6-plus` as default model.
- **API-key onboarding with DashScope defaults** — `/models` and `/onboard`
  walk through key entry; no more device flow or multi-account management.
- **`qwen3.6-plus` default model** — added as the DashScope default.
- **`chat_template_kwargs` injection** — local thinking models receive proper
  `enable_thinking` flags via DashScope-compatible body transforms.
- **Qwen-style tool call recovery from text content** — detects Qwen's
  `<!-- tool_calls -->` or raw JSON in `delta.content` and converts into
  real tool calls before the tool loop sees them.

#### TUI
- **Stack queued messages instead of replacing** — multiple messages queued
  while the agent processes now stack visibly instead of overwriting each
  other.
- **Shell-mode visual cue** — input starting with `!` shows a visual `shell`
  indicator matching the original design.
- **Collapse `$HOME` → `~` and middle-truncate tool summaries** — channel
  delivery paths and long tool descriptions are shortened for readability.

#### Brain
- **Anti-code-block nudge for local models** — brain instructions explicitly
  tell the model to use `tool_calls`, not markdown code blocks.

[Unreleased]: https://github.com/opencrabs/opencrabs/compare/v0.5.5...HEAD
[0.3.32]: https://github.com/adolfousier/opencrabs/compare/v0.3.31...v0.3.32
[0.3.31]: https://github.com/adolfousier/opencrabs/compare/v0.3.30...v0.3.31
[0.3.30]: https://github.com/adolfousier/opencrabs/compare/v0.3.29...v0.3.30
[0.3.29]: https://github.com/adolfousier/opencrabs/compare/v0.3.28...v0.3.29
[0.3.28]: https://github.com/adolfousier/opencrabs/compare/v0.3.27...v0.3.28
[0.3.27]: https://github.com/adolfousier/opencrabs/compare/v0.3.26...v0.3.27
[0.3.26]: https://github.com/adolfousier/opencrabs/compare/v0.3.25...v0.3.26
[0.3.25]: https://github.com/adolfousier/opencrabs/compare/v0.3.24...v0.3.25
### Changed

#### Qwen/DashScope Migration
- **Rewrote `qwen.rs` as thin DashScope header/body helper** — the module is
  now ~200 lines (was ~1,500 with rotation, device flow, and credential
  management).
- **Removed Qwen CLI provider entirely** — `qwen-code-cli` no longer appears
  in the provider list; all routing goes through DashScope API.
- **Dropped `qwen_accounts` rotation field** — config.toml no longer has
  `[[providers.qwen_accounts]]`; replaced with single `api_key` in keys.toml.
- **Stripped Qwen OAuth device flow and rotation wizard** — onboarding no
  longer shows the multi-account setup screen.
- **Route provider-specific branches by id, not index** — TUI dialogs match
  on provider identifier strings instead of hardcoded numeric indices, so
  removing the CLI provider doesn't shift everything.

#### Agent
- **Per-session provider isolation** — each session carries its own provider
  instance; no more global swap that affected all sessions simultaneously.
- **Per-session ctx budget isolation** — context budget calculated per session
  with last-iteration prompt size tracking.

### Fixed

#### Provider / Stream
- **Extract bare OpenAI `tool_call` envelopes from text content** — local
  models sometimes emit `{"tool_calls":[...]}` as plain text alongside (or
  instead of) proper SSE tool_call chunks. These are now detected and
  converted into executable tool calls.
- **Extract Claude-style XML tool calls** — `<TOOLNAME><PARAM>value</PARAM>
  </TOOLNAME>` patterns recovered into real tool calls.
- **Handle singular `tool_call` envelope + malformed JSON** — some models
  send `"tool_call"` (singular) or truncated JSON; both are handled.
- **Suppress local tool-call markers from stream display** — `<!-- tool_calls
  -->` and similar markers no longer appear as visible text in the TUI.
- **Surface real 422 body + helpful hint for Unsloth Studio** — local
  endpoints that return 422 now show the actual error body instead of a
  generic connection error.
- **Bound initial stream handshake + funnel timeouts into retry** — the POST
  → response-headers phase is now time-limited (90s) so a local server that
  accepts TCP but never replies surfaces a `Timeout` error into the existing
  retry/fallback chain instead of sitting on reqwest's 300s default.
- **1h stream idle for local endpoints** — handshake stays tight (90s) as
  the "is the server alive?" detector, but once headers arrive the body
  stream gets a full hour. Covers legitimate large-model prefill on
  Unsloth / llama.cpp / LM Studio / Ollama / MLX where prompt processing
  on a 70B at full context can run tens of minutes before the first token.
  Earlier 30s / 90s / 15min tries all killed genuine turns.
- **Name fallback target in TUI alert** — the stream-error arm now emits
  `Trying fallback 'zhipu/glm-5.1'...` per attempt so users see which
  provider the session is now on after retries exhaust.
- **Sub-agent `AwaitingInput` state + wait_agent polling** — sub-agents now
  transition to `AwaitingInput` at round boundaries instead of sitting in
  `Running` forever. `wait_agent` polls state (250ms) and returns the
  round's output immediately on pause, terminal state, or a partial
  progress preview on timeout — eliminates the deadlock where `wait_agent`
  blocked on `handle.await` for a task that only terminates on input or
  cancel. LLMs no longer give up the parent turn after repeated empty
  "still running" responses.

#### Self-Heal / Phantom Detection
- **Narrow phantom gate, recover empty-reasoning turns** — models that
  produce only reasoning content (no text, no tools) are now recovered
  instead of flagged as phantom.
- **Split thinking per iteration** — reasoning state is cleared between tool
  loop iterations so stale thinking doesn't carry over.
- **Broaden phantom-retry gate for local providers** — local models trigger
  the retry-and-nudge path more aggressively since they're more prone to
  hallucinating tool calls.
- **Adopt Unsloth's blunt anti-code-block nudge** — when a local model emits
  code blocks instead of tool calls, the retry prompt is direct and explicit.
- **Tighten phantom scope** — fewer false positives on legitimate responses
  that happen to mention tools.
- **Detect phantom intent with backtick code references** — models that say
  "let me run `grep -r pattern`" without actually calling the tool are caught.
- **Add investigative intent phrases to phantom detector** — "Let me check…",
  "Let me see…", and similar phrases now trigger the detector.
- **Phantom detector lets loops slide after one retry** — prevents infinite
  retry loops when the model consistently fails to produce tool calls.
- **Preserve tool group on Escape-twice** — cancelling no longer loses the
  visible tool call history.
- **Catch phantom "Let me see:" mid-turn** — the detector catches this
  specific pattern that was slipping through.

#### TUI
- **Per-session message queue with isolated display** — queue messages no
  longer live in the shared input buffer; each session has its own queue.
- **Queue preview was invisible** — height was 1 but the border consumed the
  only row, leaving zero rows for text. Fixed to height 2.
- **Wrap system messages to terminal width** — long RSI summaries and
  compaction notices no longer clip at the right edge.
- **Reduce truncation warning false positives** — responses ending with `:`
  that contain multiple sentences no longer trigger the warning.
- **Persist `last_session` on create_new_session** — restarting now resumes
  the correct session instead of jumping to a different one.
- **Drop ".." from file picker when searching** — Enter picks the top match
  instead of navigating to the parent directory.
- **Keep streaming text + reasoning visible after Escape-twice cancel** —
  cancelled content is preserved in the display instead of vanishing.
- **Mid-sentence truncation retry** — responses that appear truncated
  mid-sentence are retried automatically.
- **Stop reload-on-cancel wiping TUI push** — cancelling no longer triggers
  a full session reload that wipes queued messages.
- **Drop redundant closure around `strip_llm_artifacts`** — minor cleanup
  that was allocating unnecessarily.
- **Persist api_key on first save of custom provider** — custom provider
  keys are now written immediately on first entry.
- **Capture panic location in render `catch_unwind`** — TUI render panics
  now log the exact file and line for easier debugging.
- **Attach opencrabs-frame backtrace to render panic log** — panic traces
  include the custom frame for better error reports.
- **Dialogs.rs panicked indexing PROVIDERS** — fixed crash after Qwen CLI
  provider removal shifted the provider list.
- **Merge consecutive reasoning-only messages in IntermediateText** —
  multiple reasoning blocks between tool calls collapse into one display.
- **Stop tools-v2 JSON bleeding into chat** — `<!-- tools-v2: -->` markers
  with rustc arrow formatting no longer appear as visible text.
- **Route ```think...``` content to ReasoningDelta** — backtick-wrapped
  thinking content renders in the thinking section and persists correctly.

#### Context / Token Counting
- **Persist server `input_tokens` on messages** — server-reported token
  counts are stored on each message instead of relying on in-memory cache.
- **tiktoken fallback was missing system prompt** — counted only messages +
  tools, underestimating context by ~2-4K tokens.

#### Channels
- **Stop silently wiping final response via bad dedup** — the dedup logic
  was matching too aggressively and removing the agent's actual response.
- **Revert 'never strip to empty' guard** — it caused duplicate messages
  by preventing legitimate dedup.
- **Delete stale intermediates on cancelled in-flight call** — Telegram
  no longer leaves orphaned edit messages after cancellation.

#### Qwen / DashScope (migration fixes)
- **401/403 now trigger account rotation** — auth errors cause advancement
  to the next credential instead of immediate failure (applies to remaining
  rotation paths during migration).
- **Invalidate dead OAuth accounts** — when both refresh and retry fail,
  credentials are cleared and persisted so re-authentication is triggered.
- **Tighten auth-invalidate trigger** — single-slot rotation writes are
  skipped to avoid wiping the only valid credential.
- **Handle 401/403 auth errors in stream rotation** — mid-stream auth
  errors now trigger rotation and retry.
- **Raise think-tag safety valve** — long Qwen reasoning blocks no longer
  get partially stripped by the tag filter.

[0.3.24]: https://github.com/adolfousier/opencrabs/compare/v0.3.23...v0.3.24
[0.3.23]: https://github.com/adolfousier/opencrabs/compare/v0.3.22...v0.3.23
[0.3.22]: https://github.com/adolfousier/opencrabs/compare/v0.3.21...v0.3.22
[0.3.21]: https://github.com/adolfousier/opencrabs/compare/v0.3.20...v0.3.21
[0.3.20]: https://github.com/adolfousier/opencrabs/compare/v0.3.19...v0.3.20
[0.3.19]: https://github.com/adolfousier/opencrabs/compare/v0.3.18...v0.3.19
[0.3.18]: https://github.com/adolfousier/opencrabs/compare/v0.3.17...v0.3.18
[0.3.17]: https://github.com/adolfousier/opencrabs/compare/v0.3.16...v0.3.17
[0.3.16]: https://github.com/adolfousier/opencrabs/compare/v0.3.15...v0.3.16
[0.3.15]: https://github.com/adolfousier/opencrabs/compare/v0.3.14...v0.3.15
[0.3.14]: https://github.com/adolfousier/opencrabs/compare/v0.3.13...v0.3.14
[0.3.12]: https://github.com/adolfousier/opencrabs/compare/v0.3.11...v0.3.12
[0.3.11]: https://github.com/adolfousier/opencrabs/compare/v0.3.10...v0.3.11

## [0.3.10] - 2026-04-15

RSI reliability hardening — cycle summaries no longer truncated, phantom
detection reduced to a two-signal requirement, and compaction label
simplified in the README diagram.

### Fixed

#### RSI
- **Stop truncating cycle summary sent to TUI** — cycle summaries now
  display the full text instead of being cut off mid-sentence.
- **Reduce phantom detection false positives** — now requires two signals
  (intent keyphrase + zero tool calls) before flagging a phantom,
  eliminating spurious self-heal triggers.

#### Style
- **Cargo fmt pass** — `rsi.rs` and `helpers.rs` formatting.
- **Simplify compaction label in README diagram** — cleaner visual in
  the architecture reference.

## [0.3.9] - 2026-04-15

Usage Dashboard, config stability overhaul, and TUI polish. The headline
feature is an interactive `/usage` overlay showing cost, tokens, and
sessions broken down by project, model, activity, and tool. Config writes
were rewritten from scratch to stop the recurring config-wipe bug.

### Added

#### Usage Dashboard
- **Interactive usage overlay** — press `/usage` to open a centered TUI
  panel with five cards: Daily Activity, By Project, By Model, Core Tools,
  and By Activity. Tab navigates cards, T/W/M/A filters by time period.
- **Tool execution recording** — every tool call is now logged to a
  `tool_executions` table for per-tool usage analytics in the dashboard.
- **Session auto-categorization** — heuristic classifier tags sessions as
  Development, CI/Deploy, Testing, etc. based on title and tool patterns.
  Categories feed the By Activity card with cost/turns/1-shot% breakdown.
- **Project & model breakdowns** — By Project shows cost, tokens, and
  session count per workspace. By Model shows cost and tokens with display
  labels and estimated-cost markers for free-tier models.

#### TUI
- **Mouse drag-select text copy** — click and drag in the input area to
  select text, automatically copied to clipboard on release.

### Fixed

#### Config Stability
- **Stopped `migrate_if_needed` from wiping `config.toml`** — the migration
  function was overwriting the file on every startup with default values.
- **Replaced toml round-trip with `toml_edit`** — all config writes now use
  lossless parsing so comments, ordering, and unknown keys are preserved.
- **Eliminated all `unwrap_or(empty_table)` write paths** — these were
  silently replacing corrupt or partially-loaded config with blank tables.
- **Mutex lock on config writes** — prevents concurrent writes from racing
  and producing empty files.
- **API key recovery from last-good snapshot** — when `keys.toml` is
  corrupt, recovers credentials from the most recent valid backup.
- **Qwen OAuth credentials passed directly** — OAuth tokens flow through
  the event chain instead of being re-read from a potentially-wiped config.

#### Qwen
- **Enforce `gitCoAuthor=false` before every CLI spawn** — patches
  `~/.qwen/settings.json` programmatically so `Co-authored-by` trailers
  never appear, even after config resets. System prompt rule stays as
  belt-and-suspenders.
- **Detect expired rotation accounts** — Alt+Backspace wipes stale OAuth
  credentials for the current account.

#### Usage Dashboard Polish
- **Right-aligned data columns** — cost, tokens, sessions, and 1-shot%
  columns are pushed flush to the right card edge across all cards.
- **Adaptive card widths** — columns auto-size to actual data width instead
  of using fixed-width assumptions.
- **Real 1-shot% calculation** — replaced fake binary approximation with
  actual per-session turn counting.
- **Half-block bar charts** — visual separation between bars and data.

#### Self-Heal
- **Phantom detection on single intent keyphrase** — catches when the model
  produces a single tool-intent phrase with zero actual tool calls.

#### TUI
- **Prevent impossible provider/model combos** — status bar no longer shows
  model names that don't belong to the active provider.
- **Brain generation background mode** — enters chat immediately while brain
  files generate asynchronously. 120s timeout prevents infinite hangs.
- **Stricter brain prompt** — forbids preamble, closing remarks, and code
  fences so parsing succeeds reliably.
- **Voice step Continue button** — Tab cycles between fields instead of
  advancing screens. Added Continue button matching Channels step.
- **Brain textarea wrapping** — first backspace on untouched template clears
  the field. Large pastes render cleanly with line-wrap.

#### Database
- **Soft-delete sessions** — preserves metadata for usage tracking instead
  of hard-deleting.
- **Strip ANSI codes from tool output** — prevents escape sequences from
  polluting DB persistence.
- **Recreated `tool_executions` schema** — migration fixes column types for
  correct recording.

#### CI
- **Windows CRT static linking** — `LLAMA_STATIC_CRT=1`, `RUSTFLAGS`,
  and `CFLAGS=/MT` ensure all objects use static CRT.
- **Removed Windows CRT overrides** — cleaned up after the fix landed.

### Docs
- **Terminal permissions setup** — macOS, Windows, Linux instructions.
- **Session auto-categorization** documented in README.
- **Python runtime dependency for local TTS** added to install docs.
- **System commands section** — documented OS-specific terminal capabilities.

## [0.3.8] - 2026-04-14

25+ hardening fixes across context tracking, self-heal, provider rotation,
and cross-platform support. RSI moves to fully autonomous mode with restart
resilience and skip-on-unchanged.
Existing users should ask their crab to compare brain files with the
latest templates and apply any diffs.

> **RSI is experimental.** Autonomous self-improvement runs without human
> approval and writes improvements to `~/.opencrabs/rsi/improvements.md`.
> Monitor the file during early testing.

### Changed

- **RSI runs autonomously** — removed human-in-the-loop approval. The
  `self_improve` tool now applies improvements directly and logs them
  to `~/.opencrabs/rsi/improvements.md`. The `propose` action was removed;
  only `apply`, `update`, and `read` remain.
- **RSI uses active provider** — no longer hardcoded to Anthropic; respects
  the current provider and model configuration.
- **RSI reuses persistent session** — one session per cycle instead of
  creating a new one on every improvement run.
- **RSI survives app restarts** — persists `last_cycle` timestamp to disk;
  calculates remaining delay on startup instead of resetting the 1h timer.
- **RSI skips unchanged feedback** — if feedback count hasn't changed since
  last cycle, skips analysis entirely to avoid wasted LLM calls.
- **RSI enriched opportunities** — opportunity descriptions now include
  session ID, model name, provider, and timestamps so the agent knows
  which model/session produced failures.

### Fixed

#### Context & Token Accuracy
- **Real token counts from DB** — replaced estimation with actual values
  stored per request.
- **Reverted cumulative DB token_count** — discovered values were API
  lifetime totals, not per-request; stopped using them for context budget.
- **Silent emergency truncation** — when context exceeds budget, truncates
  to 80% and triggers auto-compaction instead of crashing.
- **Accurate token display after compaction** — context token counter now
  reads from the compaction point, not the full session history. Fixed
  calibration drift that made the counter diverge over time.
- **TUI shows compaction-aware token count** — context display reflects
  tokens since last compaction, not the cumulative total.

#### Self-Heal & Phantom Detection
- **Phantom tool call detection without file paths** — catches when the
  model narrates file changes in prose (e.g. "I updated the config") without
  actually executing any tool calls.
- **Structural phantom detection via imperative statement count** — detects
  hallucinated tool usage by analyzing imperative verb density in responses.
- **Past-tense standalone detection** — catches "Amended.\nCommitted.\n..."
  style phantom narration where the model claims completed actions.
- **Expanded completion claim vocabulary** — git-specific patterns like
  "amended the commit", "bumped the version", and "I've made the changes"
  now trigger phantom retry.
- **Rotation continuation prompt** — when Qwen OAuth rotation happens
  mid-task and the new account returns 0 tool calls, injects a continuation
  prompt so the agent resumes where it left off.

#### Provider & Rotation
- **Fallback walks entire provider chain** — on failure, iterates all
  configured providers while skipping the one that just failed.
- **Rotation token persistence** — saved between sessions so model rotation
  resumes correctly after restart. Fixed model remap and resume contamination.
- **Custom provider API key written before config reload** — prevents the
  key from being lost on `/models` switch or config refresh.

#### Qwen
- **Auto-retry device flow on failure** — Qwen OAuth device flow now retries
  automatically; pressing Enter restarts with a fresh auth code.
- **Normalized Qwen 3.6 Plus variants** — `:free`, `thinking`, and bare
  variants all resolve correctly.

#### TUI
- **Fixed panic on multi-byte characters** — mouse sequence detection no
  longer crashes on Unicode input. Fixed path truncation for wide chars.
- **Voice step Continue button** — Tab now cycles between fields instead
  of advancing to the next onboarding screen. Added Continue button
  matching the Channels step pattern.
- **Brain textarea wrapping + template wipe** — first backspace on untouched
  template clears the entire field. Large markdown pastes render cleanly
  with line-wrap and overflow indicator.
- **Brain generation runs in background** — no longer blocks on "Cooking
  up brain files..." screen. Enters chat immediately, generates in the
  background, writes files directly to workspace. 120s timeout prevents
  infinite hangs.
- **Stricter brain prompt** — explicitly forbids preamble, closing remarks,
  and code fences so parsing succeeds reliably.
- **Rebuild provider before brain gen on fresh install** — prevents
  PlaceholderProvider error when brain step fires before provider is wired.

#### Usage
- **Normalized model names and display labels in `/usage`** — consistent
  naming across providers with human-readable labels.

#### Windows
- **`where.exe` for CLI binary detection** — uses Windows-native `where.exe`
  instead of `which` when running on Windows.
- **Restored `--all-features` for Windows CI builds** — full feature set
  tested in CI again.
- **Fixed MSVC CRT mismatch** — triple-layer fix: `LLAMA_STATIC_CRT=1`
  (cmake path), `RUSTFLAGS=-Ctarget-feature=+crt-static` (Rust linker),
  and `CFLAGS=/MT CXXFLAGS=/MT` (direct MSVC compiler flags). Ensures
  all C/C++ objects use static CRT matching esaxx-rs.

### Docs

- **Python runtime dependency for local TTS** — added install commands
  for all platforms (apt, dnf, brew, winget) in README.

### Contributors

- **Teo Gonzalez Collazo** — PR #71 (Exa search integration)
- **Swoorup** — Issue #72 (readline keyboard shortcuts)

## [0.3.7] - 2026-04-13

OpenRouter streaming fully fixed — all models now use the standard OpenAI
SSE parser with full reasoning, tool calls, and think-tag support.
Qwen rotation hardened against wipe storms and expired accounts.
RSI agent now reads before writing and can surgically update brain files.

### Added

#### Streaming
- **Non-streaming compatibility module** — dedicated `nonstream_compat.rs`
  synthesizes full stream events (reasoning, tool calls, usage with cache)
  from non-streaming JSON responses. Handles OpenRouter upstreams like
  Trinity and Venice that return `chat.completion` blobs instead of SSE.
- **Stream drop exhaustion fallback** — when a provider stream drops
  mid-response, triggers fallback to the next provider in the chain.

#### RSI (Recursive Self-Improvement)
- **`read` action** — RSI agent must read brain files before modifying,
  preventing blind appends and redundancy.
- **`update` action** — surgical find-and-replace on specific sections
  of brain files instead of always appending.
- **Mandatory read-before-write workflow** — RSI prompt enforces:
  read → decide (apply new / update existing / skip redundant) → act.
- **Structured awareness** — RSI routes improvements to the correct
  brain file based on event type taxonomy.
- **Phantom tool call detection** — self-heal layer detects when the
  model narrates file changes in prose without executing tools, and
  auto-retries with a corrective prompt.
- **Proactive MEMORY.md hints** — system prompt nudges the agent to
  persist important context to memory.

### Changed

- **OpenRouter uses standard OpenAI SSE parser** — removed the
  Anthropic-format bypass that broke streaming, reasoning, and tool
  calls for all OpenRouter models. Every model now gets full feature
  support (reasoning_content, tool_call accumulation, think-tag
  filtering, leak detection).
- **Context budget** — stopped subtracting tool overhead from context
  budget, which was causing premature compaction.

### Fixed

- **OpenRouter 429 retry** — exponential backoff before falling to
  next provider, instead of immediately failing.
- **Stream content ordering** — emit content delta before finish_reason
  to prevent truncated final tokens.
- **Qwen rotation wipe storm** — concurrent refresh failures no longer
  cascade-wipe all accounts from keys.toml.
- **Qwen expired rotation accounts** — preserved for re-auth instead
  of being silently dropped.
- **Qwen account split** — metadata in config.toml, secrets in
  keys.toml, fixing #74 and #75.
- **Qwen rotation retry** — disabled retry-on-rate-limit inside
  RotatingQwenProvider to prevent double-retry with outer fallback.
- **TUI footer model** — shows actual provider/model after fallback
  swap instead of stale primary (#73).
- **Runtime deps docs** — added libgomp + libasound to pre-built
  binary requirements.

## [0.3.6] - 2026-04-13

The self-improving release. OpenCrabs now records every tool execution,
provider error, and user correction to a persistent feedback ledger,
analyzes patterns at session start, and can autonomously rewrite its own
brain files — no human approval needed. Plus Qwen OAuth hardening,
readline keyboard shortcuts, responsive table rendering, and Exa search.

Contributors:
- Teo Gonzalez Collazo (PR #71 — Exa search tool update)
- Swoorup (Issue #72 — readline shortcuts)

### Added

#### Recursive Self-Improvement (RSI) — Experimental
- **Feedback ledger** — persistent SQLite table recording every tool
  success/failure, provider error, user correction, and context compaction
  event. Zero overhead — fires and forgets inside the tool loop.
- **Startup digest** — on session open, the last 50 feedback events are
  injected into the system prompt as "Performance History", surfacing
  tool failure rates, recent errors, and user correction counts.
- **`feedback_record` tool** — agent can manually log observations
  (patterns, strategies, user corrections).
- **`feedback_analyze` tool** — query aggregated stats: per-tool success
  rates, recent failures, failure patterns.
- **`self_improve` tool** — autonomously append improvements to brain
  files (SOUL.md, AGENTS.md, TOOLS.md, etc.) and log to
  `~/.opencrabs/rsi/improvements.md`. No human approval required —
  the agent identifies patterns and applies fixes directly.
- **User correction detection** — short negative messages ("wrong",
  "no", "try again", "you broke", "fix it") are auto-recorded as
  `user_correction` events, training the ledger without explicit action.
- **Proactive compaction recording** — auto-compaction events at 65%
  threshold are logged to the ledger.

#### TUI
- **Readline keyboard shortcuts** — Ctrl+P (history up), Ctrl+N
  (history down), Ctrl+A (beginning of line), Ctrl+E (end of line).
  Thanks to Swoorup (Issue #72).
- **Responsive table rendering** — markdown tables now adapt to chat
  width instead of clipping at the terminal edge.
- **Onboarding/dialogs responsive** — dialogs scale to terminal size,
  file picker search added, session directory navigation improved.
- **Eliminated window blinking** on terminal resize.
- **Expired Qwen rotation accounts** show as red in the provider
  selector instead of green.

#### Tools
- **Exa search updated** — new search types (fast, deep-lite, deep,
  deep-reasoning, instant) aligned with latest Exa API. Added
  `x-exa-integration: opencrabs` header. Thanks to Teo Gonzalez Collazo
  (PR #71).

### Changed

- **RSI is experimental** — enable via `~/.opencrabs/config.toml`
  under `[agent]` section. The system self-improves autonomously by
  default, but users should monitor `~/.opencrabs/rsi/improvements.md`
  for changes. Existing users should ask their crab to compare
  `~/.opencrabs/TOOLS.md` with the template and update any changes.
- **Qwen OAuth retry backoff** — changed from 500ms hammer to 3s→6s→
  12s→24s exponential backoff, letting Qwen's free tier recover instead
  of burning quota on instant retries.

### Fixed

- **Qwen OAuth token validation** — validates tokens at startup, falls
  back to next provider on 401/403.
- **Async factory** — `create_provider()` is now fully async, removing
  `block_in_place` calls and fixing potential deadlocks.
- **Ghost custom providers** — cleaned up stale provider entries from
  config, and fixed API key paste handling in the provider selector.
- **Window blinking on resize** — eliminated the flash that occurred
  when the terminal was resized.

## [0.3.5] - 2026-04-12

### Fixed

- **Windows CLI providers restored** — v0.3.4 erroneously hid Claude CLI,
  OpenCode CLI, and Qwen CLI from the provider selector on Windows. These
  providers work correctly on Windows and are now visible again on all
  platforms.

## [0.3.4] - 2026-04-11

Major feature release: **Qwen multi-account OAuth rotation** — configure 2–10
Qwen accounts that rotate automatically on rate-limit, giving effectively N×
the free-tier quota. Also ships critical TUI stability and performance fixes,
a visual redesign of the provider selector, and cross-platform improvements.

### Added

- **Qwen multi-account rotation** — new `RotatingQwenProvider` round-robins
  across N OAuth accounts, auto-advancing on 429 / rate-limit errors. Setup
  via `/models` or `/onboard`: toggle rotation with `Space`, pick 2–10
  accounts (`1` = 10), authenticate each via device flow. Credentials persist
  in `keys.toml` under `[[providers.qwen_accounts]]`. With 3 accounts you get
  **180 req/min** and **3,000 req/day** before fallback kicks in (`e1b2417`,
  `a104290`).
- **Incremental rotation setup** — adding accounts later (e.g. 3→5) only
  authenticates the new ones, preserving existing credentials (`5200146`).
- **Provider credential indicators** — providers with existing API keys, OAuth
  tokens, or CLI binaries in PATH now display in green with a **✓** checkmark
  in both `/models` and `/onboard` provider lists, so you immediately see
  what's already configured (`471ae0a`).

### Fixed

- **TUI garbled display from channel sessions** — two root causes fixed:
  (1) `suppress_stdio()` in the embedding engine did process-wide
  `dup2(devnull, stdout)` which raced with ratatui's terminal writes on fd 1,
  producing partial escape sequences → garbled display. Now skips stdout
  redirection when TUI is active (stderr-only suppression). (2)
  `ChannelProcessingFinished` called `load_session().await` directly, blocking
  the event loop with a DB query and eating queued terminal events. Now
  schedules a debounced refresh instead. Earlier fix (`33f9ab6`) added the
  `ChannelSessionEvent` lifecycle with 500ms debounce for `SessionUpdated`
  events (`33f9ab6`, `5c349d4`).
- **TUI streaming lag** — every `ResponseChunk` forced a full re-render with
  O(n²) markdown re-parsing on the entire growing buffer. Now batches chunks
  within a 30ms time budget and caches parsed markdown between frames. Typing,
  scrolling, and Ctrl+C work normally during streaming (`9098e02`).
- **Context counter not updating after compaction** — `TokenCount` event now
  emitted after both LLM tier-1 compaction and tier-2 hard truncation so the
  footer percentage reflects post-compaction values in real time (`98dc401`).
- **Qwen rotation auth wipe** — opening `/models` with existing rotation
  accounts no longer re-triggers the full OAuth flow and destroys credentials.
  Guard checks persisted account count matches desired count (`8e8bb04`,
  `5200146`).
- **Duplicate messages and truncated response self-heal** — dedup logic
  prevents double-send and rotation state now persists across sessions
  (`de5a040`).
- **Custom provider duplicate entries** — entering a name with capital letters
  (e.g. "Dialagram") no longer creates two entries (one normalized, one empty).
  Names are normalized via `normalize_toml_key()` on Enter in both `/models`
  and `/onboard` (`5200146`).
- **Onboarding missing defaults** — `approval_policy = "auto-always"` and
  `enable_thinking = true` (Qwen) are now persisted to `config.toml` during
  onboarding so they're visible and editable on fresh installs (`c5c326c`).
- **CLI health check** — `qwen-code-cli` binary detection now works correctly
  in the onboarding health check (was falling through to "opencode") (`c5c326c`).

### Changed

- **Shared provider module** — all Qwen rotation logic (state loading, auth
  guards, incremental flow, count-change handling), custom name normalization,
  and credential detection consolidated into `ProviderSelectorState` methods.
  `/models` and `/onboard` now share one code path instead of diverging
  independently (`5200146`).
- **Windows provider list** — CLI providers (claude-cli, opencode-cli,
  qwen-code-cli) are hidden from the provider selector on Windows where they
  cannot work (`5200146`).
- **Windows release build** — now uses `--no-default-features` to exclude
  local-stt/local-tts, matching the CI test matrix.

## [0.3.3] - 2026-04-09

Focused fix release: **Qwen OAuth time-to-first-token drops from 60-70s to
8-15s** by matching the exact retry parameters the Qwen Code CLI uses
internally. Also ships browser auto-screenshot and a couple of UX fixes.

### Fixed

- **Qwen OAuth retry timing** — `qwen_cli_match()` now mirrors the actual
  OpenAI Node SDK defaults used by qwen-code-cli v0.14: 500ms initial delay,
  8s max delay, 3 retries, 25% jitter (was 1.5s/30s/7/30%). Unparseable
  Retry-After headers no longer default to 60s — exponential backoff decides
  instead. Combined effect: TTFB **60-70s → 8-15s** (`e42d46a`).
- **TUI execute_code collapsed card** — now shows the actual code content
  instead of generic "Execute bash" text (`34c4b39`).

### Added

- **Browser auto-screenshot** — `navigate`, `click`, and `type_text` browser
  tools now automatically capture a PNG screenshot after each action and
  attach it as a vision image alongside the tool result. The model can "see"
  the page without a separate `screenshot` call (`1ac8f91`).

### Changed

- **CI release workflow** — release jobs now poll for CI to pass before
  starting builds, preventing releases from broken commits (`182a005`).

## [0.3.2] - 2026-04-08

Big release focused on **making `providers.custom.*` a first-class citizen** —
dozens of OpenAI-compatible backends (dialagram, nvidia, z.ai, lm-studio,
ollama, vllm, any self-hosted endpoint) now stream thinking tokens, tool
calls, and intermediate text to the TUI exactly like the native providers.
Also ships the **native Qwen OAuth provider** with opt-in Qwen3 hybrid
thinking mode and a full round of prompt caching across Anthropic /
OpenRouter / Gemini / Qwen DashScope.

Post-release work adds a **brand new TUI startup experience** (splash screen
replaced by an animated header card overlaying the chat), drag-to-select
text, panic recovery, gaslighting refusal stripping, and a dozen input/render
fixes.

> ⚠️ **Known limitations**
>
> - **Qwen OAuth** still hits upstream rate limits intermittently even with
>   the global singleton limiter and per-model fingerprint headers. The
>   free 1k-req/day tier is fragile — expect occasional 429s that the
>   sticky-fallback chain will swap out of. **Qwen Code CLI** works great
>   today as an alternative, but inherits the CLI's own tool surface and
>   memory model (no native OpenCrabs tool bridge, separate scratch state).
> - **Custom OpenAI-compatible routers that rewrite the stream** (e.g.
>   dialagram's `qwen-3.6-plus-thinking`) can inject sanitized boilerplate
>   reasoning that doesn't reflect the real model's thinking. Not an
>   OpenCrabs bug — prefer native Qwen OAuth, OpenRouter, or Alibaba
>   DashScope for Qwen3.

### Added

#### Native Qwen OAuth provider
- **OAuth device-code flow** — Sign in with a Qwen account directly from
  onboarding (`/onboard`) or the runtime model switcher (`/models`); the
  wizard and model dialog now share a single device-flow state machine
  (`d16a88d`).
- **Qwen3 hybrid thinking toggle** — Qwen3 is a single-weights hybrid
  model that runs in thinking or non-thinking mode depending on a runtime
  flag. Set `enable_thinking = true` in `[providers.qwen]` to opt in when
  you want reasoning tokens; leave unset for fast output. Only the native
  Qwen provider honours the flag (`cf76e49`).
- **Full token lifecycle** — mtime-based credential reload, 401 → refresh
  retry, dynamic `resource_url` → base URL switching, background refresh
  well before expiry (`a306e22`).
- **Prompt caching** via `cache_control` on the last tool definition and
  session-stable metadata headers — keeps the free tier viable for longer
  conversations (`0aa65dc`).
- **Request body parity with `qwen-cli`** — body transform rewrites user /
  system messages to array form, tags only the last tool with cache
  control, adds the required VL flag and metadata, preserves existing
  `max_tokens`, and strips disallowed fields. Tested against a real
  `qwen-cli` HTTP capture (`b2e2c50`, `ee9b1c7`).
- **Gateway fingerprint headers** — `x-stainless-*` SDK identity, stable
  session id, 13-hex prompt id, and `Accept` header matching the official
  client so Qwen's gateway accepts requests as first-party SDK traffic
  (`11f314f`, `a7b3c35`, `c09a8bf`).
- **Global singleton rate limiter** (`QWEN_OAUTH_LIMITER`) — a single
  `LazyLock<Arc<RateLimiter>>` shared across every request regardless of
  provider rebuild / session swap, so the second request no longer
  instantly trips the per-second limit (`488480b`).
- **Sticky fallback swap UI** — the footer and session header now show the
  *active* sub-provider name + model when a rate limit forces a swap, so
  users can see which backend is actually serving them (`c736856`).

#### Prompt caching across providers
- **Anthropic native** — `cache_control` on system prompt and tool
  definitions (`a0e5f68`).
- **OpenRouter** — cache_control forwarded for Anthropic-family models
  routed through OpenRouter (`4752abf`).
- **Gemini** — full `cachedContent` API integration (`ee24e68`).
- **Qwen DashScope** — cache_control on last tool (`0aa65dc`).
- **Cache token accounting** — provider responses now parse OpenAI /
  DashScope cache + reasoning token fields, so the context window display
  reflects real usage across cache-aware providers (`d973808`).

#### TUI — header card replacing splash screen
- **Splash mode removed** — app now starts directly in chat with the last
  session loaded, so user input is available immediately (`e637e4a`).
- **Header card overlay** — animated card replaces the old splash screen,
  overlaying the top of the chat history, then vanishes like the splash
  did. Responsive layout, padding, and wrapping (`76791af`, `8368f97`).
- **Card sizing** — scales as 70% of chat area, capped and centered to
  avoid overwhelming small terminals (`1e7eb5a`, `1094832`, `5f4bc72`).
- **Full startup banner** — `opencrabs` prints the complete banner to the
  terminal on startup and exit, visible in scrollback even when the card
  vanishes (`08470bc`).
- **Help screen shows version** — `/help` now displays the current version,
  provider, and model (`fe3a49a`).
- **App title shortened** to "OpenCrabs AI Agent" for cleaner card display
  (`0e116dd`).
- **Async session load** — session messages load asynchronously on startup
  for instant first paint instead of blocking render (`66512ee`).

#### TUI — input, rendering, and interaction
- **Drag-to-select text with auto-copy** — native mouse selection in the
  TUI, auto-copies the selection on release (`40e9279`).
- **O(N) input render + scroll-to-cursor** — tall pastes no longer cause
  quadratic render cost; cursor position preserved on long inputs
  (`c05f809`).
- **Line navigation inside recalled multiline** — pressing Up/Down inside
  a multi-line history-recalled input navigates lines instead of wiping
  the whole buffer (`5f97d90`, `ee718e5`).
- **Emoji cursor rendering** — grapheme cluster extraction for the cursor
  so multi-byte emoji are highlighted correctly (`6df5774`).
- **Panic recovery** — TUI recovers from render panics and clamps title
  splits instead of crashing (`3460d1b`).
- **SoftBreak as space** — markdown soft breaks now render as a single
  space instead of a hard line break (`e81c62c`).

#### Self-healing gaslighting strip
- **Mid-turn refusal preamble strip** — reasoning models (dialagram,
  qwen-3.6-plus-thinking, etc.) that inject "I can't use tools right now"
  or similar refusal text mid-stream now have those paragraphs stripped
  before the reply reaches the user (`531f172`).
- **Per-paragraph strip + new refusal phrases** — extended phrase list
  covering tool-registry family denials, applied per-paragraph so partial
  refusals don't nuke the entire response (`bebd951`, `df51c3c`).
- **Dialagram refusal detection mid-stream** — detects `<think>`-adjacent
  refusal patterns that fire alongside tool calls and strips them inline
  (`2d80119`).
- **Cross-chunk `tool_calls` leak detection** — detects and surgically
  strips leaked `tool_calls` JSON that some routers inject across SSE chunk
  boundaries (`ff1ff22`, `b9adfb4`).

### Changed

- **Custom providers now feel like native providers.** This was the
  headline investigation of the release — see *Fixed* below for the
  `ContentBlockStop` / reasoning fixes that make tool cards and thinking
  tokens actually reach the TUI on every OpenAI-compatible backend.
- **Per-model rate limiters** — rate limit state is now keyed by the exact
  model id instead of the provider name, so swapping models on the same
  provider no longer drags a stale limiter between them (`1238573`).
- **Zero retries on hard rate limits** — a 429 goes straight to the
  sticky-fallback swap instead of spinning retry backoffs that would burn
  quota on every sub-provider in the chain (`8d8607f`).
- **OpenRouter app-identity headers on every request** — `HTTP-Referer`
  and `X-Title` are attached for every call so OpenRouter's dashboard
  attributes usage correctly (`580a225`).
- **Anthropic OAuth removed** — Anthropic banned OAuth-based third-party
  clients; all dead OAuth code paths and docs are gone (`777d87e`).
- **CLI context management split** — `cli_handles_tools` is now separate
  from `cli_manages_context`, so providers that delegate tool execution
  to an external CLI (Qwen Code, OpenCode, Claude CLI) can still have
  OpenCrabs manage compaction and the context budget (`34b59f2`).
- **Tool path resolution centralized** — every filesystem tool now goes
  through `resolve_tool_path()`, normalising tilde expansion,
  relative-to-working-dir, and symlink handling in one place instead of
  eight copies (`9898aee`).
- **Provider base_url() trait method** — added to the provider trait and
  forwarded through `FallbackProvider`, so rate-limit toasts and swap
  notices can report the actual serving backend (`0d97ea8`).
- **Qwen DashScope wire format parity** — request body matches `qwen-cli`
  capture exactly to avoid upstream rate-limit triggers (`5d8a483`).
- **Qwen 429 retry in-place** — rate limits are retried in the same way
  `qwen-cli` does it instead of bailing to fallback immediately (`994cb6a`).
- **OpenRouter `:free` 429 retry in-place** — free-tier models on OpenRouter
  retry rate limits in-place before falling back (`c46cb81`).

### Fixed

#### Custom providers (the headline)
- **Tool cards no longer get stuck "in-flight" forever** — the OpenAI-compatible
  SSE parser never emitted `ContentBlockStop` events. `helpers.rs` fires
  `ToolStarted` / `ToolCompleted` progress events *exclusively* inside
  `ContentBlockStop`, so without them tool cards stayed visually pending
  even though `tool_loop` had already executed the tool cleanly. Every
  other provider (Anthropic, Gemini, Claude CLI, OpenCode CLI, Qwen Code)
  emits Start/Stop pairs — custom was the only outlier (`f86c0e6`).
- **Thinking / reasoning tokens now reach the TUI** — providers like
  dialagram's `qwen-3.6-plus-thinking` stream `delta.reasoning_content`
  *before* any regular text content. The parser emitted `ReasoningDelta`
  at index 0 without opening a block at that index, and `helpers.rs`'s
  range check silently dropped every reasoning chunk. The parser now
  opens a zero-length text block at index 0 on the first reasoning
  delta so `ReasoningChunk` progress events forward through to the UI
  (`f86c0e6`).
- **Intermediate text no longer vanishes mid-response** — same root
  cause; the text block now receives a matching `ContentBlockStop` before
  tool flushes and before the final `MessageDelta`, so helpers finalizes
  the block and emits `IntermediateText` events in the correct order
  (`f86c0e6`).
- **Duplicate tool lifecycle events on non-CLI providers** — `helpers.rs`
  was firing `ToolStarted` / `ToolCompleted` inside `ContentBlockStop` for
  every provider, not just CLIs. `tool_loop` already owns the full
  lifecycle for non-CLI providers, so every tool event was double-emitted
  (6 events in 85µs for 3 real tool calls observed in logs), bloating the
  TUI tool-call count and leaving phantom "Processing: <tool>" indicators
  when the premature fake completion raced the real one. The emit block
  is now gated behind `if is_cli` (`98afb47`).
- **Hallucinated `{"tool_calls":[...]}` JSON in content deltas** — some
  thinking models echo an OpenAI tool_call envelope as plain text inside
  `delta.content` while *also* emitting the real tool calls through
  `delta.tool_calls`. The text version was polluting assistant messages
  in the TUI. The parser now drops content deltas that start with
  `{"tool_calls"` while still processing `reasoning_content` and
  `finish_reason` on the same chunk (`45181df`).
- **Mid-stream quota / rate-limit errors are handled** — DashScope / Qwen
  / OpenAI all emit `{error: {message, type, code}}` shapes inline in an
  otherwise-200 SSE stream when quota is hit mid-flight. The parser now
  recognises these, maps 429 / "rate limit" / "quota" wording to
  `RateLimitExceeded`, and triggers the sticky-fallback swap instead of
  silently dropping the stream (`3f175a2`).
- **Provider-configured context window honored in the TUI header** — custom
  providers that set `providers.custom.<name>.context_window = 262144`
  were showing "202k/200k" in the ctx display because the header read
  the static `agent.context_limit` fallback while the enforcer used the
  real provider window. `context_window_for_model()` now delegates to
  `context_limit()` so both agree (`6129a88`).
- **Panic-free `open_tag_prefix_len`** — replaced a `chars().take()` that
  could underflow on short strings with `char_indices()` for byte-safe
  slicing (`091487d`).
- **Strip-tag opens split across SSE chunks** — the gaslighting detector
  now handles `<strip>` or similar markers that arrive mid-chunk across
  stream boundaries (`b9adfb4`).
- **Qwen HTTP 529 classified as retryable** — `overloaded_error` (529)
  from Qwen is now mapped to `StreamError` for automatic retry
  (`b29d9f6`).

#### Tool layer
- **`execute_code` now returns actionable error messages** — serde's
  cryptic `missing field 'code'` was being picked up by reasoning models
  as evidence that the whole tool layer was broken, seeding feedback
  loops where the model kept claiming tools were failing regardless of
  real output. The tool now pre-checks required fields and returns
  hints that name the tool + expected shape so the model can correct
  its next call instead of giving up (`cbe9081`).

#### Qwen native
- **`keys.toml` no longer accumulates non-secret config fields** — the
  background token refresher was writing `enabled = true` and
  `default_model = "coder-model"` to `[providers.qwen]` on every refresh,
  causing config drift and overwriting user-edited defaults. Only OAuth
  credentials live in `keys.toml` now, and existing installs self-heal
  on next refresh (`9bb5a80`).
- **Coder model id reverted** — OAuth flow uses the canonical
  `coder-model` id after an earlier mis-rename (`fcdd6af`).
- **Re-authed credentials sync into the running session** — the footer
  updates and the active sub-provider swap reflects the new credentials
  immediately instead of on next restart (`c736856`).

#### Qwen Code CLI
- **Real-time DB persistence of text + tool markers** — intermediate
  streaming state is now written to the session DB as it arrives, so
  resumes never lose partial turns (`0bb166c`).
- **Per-round usage drives ctx %** — the CLI envelope's cumulative
  usage field was inflating the ctx percentage across rounds; now uses
  the per-round delta (`6070960`).

#### Auto-approve propagation
- **`approval_policy = "auto-always"` now actually reaches `tool_loop`** —
  the TUI silently approved in its callback but `tool_loop.auto_approve_tools`
  stayed `false`, and `context.rs` would inject "AUTO-APPROVE OFF — tool
  approval is REQUIRED" into the compaction system prompt, which the LLM
  then echoed back. The flag is now propagated at `AgentService::new()`
  and again on `/approve` toggle via `rebuild_agent_service()` (`90748af`).

#### Agent / image handling
- **Image paths preserved for non-vision models** — when a model doesn't
  support vision, the image path text is now preserved in the user message
  instead of being silently dropped, so the model at least knows a file
  was referenced (`330c544`).

#### Onboarding
- **Brain onboarding auto-format + preview flow** — the wizard now
  auto-formats brain files and shows a preview before writing, catching
  mis-pasted or malformed content before it lands in `~/.opencrabs`
  (`69c87e2`).
- **Telegram test message delivery** — the onboarding Telegram test
  actually sends the message now, and the brain normalize hook runs on
  the delivered content so it matches what the wizard previewed
  (`5634c51`).
- **hf-hub stderr progress bar suppressed** — the initial embedding model
  download no longer dumps a raw hf-hub progress bar over the TUI
  (`29893b5`).

#### Channel fixes
- **New sessions inherit provider from most recent session** —
  channel-created sessions now start with the same provider/model as the
  latest active session instead of falling back to default (`fad8272`).
- **Discord stops defaulting to LM Studio on missing base_url** — custom
  providers without a configured base URL no longer silently fall back to
  LM Studio on Discord (`6819373`).

#### TUI
- **Don't wipe streamed reply when `response.content` is empty** — some
  providers return an empty `content` array on the final complete
  envelope after streaming; the TUI was overwriting the already-streamed
  reply with nothing (`3900c31`).
- **Full scrollback preserved during compaction** — compaction no longer
  truncates the visible scrollback buffer, only the LLM's in-context
  history (`bf6f5f9`).

## [0.3.1] - 2026-04-07
## [0.3.1] - 2026-04-07

### Added
- **F12 mouse capture toggle** — Toggle mouse capture on/off for native terminal text selection without exiting the TUI
- **Bang operator (`!cmd`)** — Run shell commands directly from the TUI input without an LLM round-trip; output shown as a system message (#58)
- **Auto-update on startup** — New `[agent] auto_update` config flag (default `true`) silently installs new releases on startup and hot-restarts; set to `false` to keep the prompt dialog (#59)

### Changed
- **`/evolve` is now programmatic** — Runs the EvolveTool directly instead of going through the LLM, so it can no longer be dropped or refused by a provider (#59)

### Fixed
- **`/new` slash command** — Wire up session creation via `/new` from within the TUI
- **Phantom Thinking label** — Hide Thinking label when reasoning content is empty
- **Compaction marker collapse** — Collapse stored compaction markers into system notices on reload; hide entirely when not applicable
- **OpenCode CLI live tool/text interleaving** — Proper interleaving of tool calls and text responses with footer deduplication
- **OpenCode CLI mid-stream text block dedup** — Clear text blocks on mid-stream flush to prevent duplication
- **OpenCode CLI dropped requests** — Use allow-list of terminal `step_finish` reasons (`stop`/`end_turn`/`max_tokens`); previously an `unknown` reason would prematurely terminate the response
- **Resume provider race fix** — Use `session.model` instead of provider default on resume to avoid wrong model selection
- **Onboarding brain generation** — Run brain file generation as a background task instead of blocking the UI
- **Onboarding Home Base tab cycling** — Tab key now cycles between path and seed inputs
- **Whisper download stderr suppression** — Suppress hf-hub progress bar noise during local whisper model download
- **Sanitize Unicode panic** — Eliminate `to_lowercase()` Unicode-expansion panic in `redact_secrets` and `redact_command` (#61)
- **API streaming think block** — Revert THINK_BLOCK buffer and strip echoed markers from API context
- **Rate-limit handling** — Drop pacer, relax retries to 10/20/30s with exponential backoff for 429s, name fallback in toast
- **Resume provider restore** — Restore session's own provider before running resume to avoid wrong provider selection
- **Message queue on cancel** — Clear message queue on cancel to prevent duplicate user messages

## [0.3.0] - 2026-04-07

### Added
- **Qwen Code CLI provider (full TUI)** — Complete integration with model fetch, CLI
  detection, display name, streaming, tool_use forwarding, and 1k free req/day via
  Qwen OAuth. Supports qwen3-coder-plus, qwen3.5-plus, qwen3.6-plus, and more.
  Install: `brew install qwen-code` or `npm install -g @qwen-code/qwen-code`
- **SIGINT handler + panic hook** — Proper terminal restoration on crash or Ctrl+C
  (no more garbled terminal after interrupt)
- **Mid-stream decode retry** — 3x backoff retry before provider fallback, reducing
  transient stream errors
- **Named provider index constants** — Replaced hardcoded 9/10/11 throughout TUI
  dialogs for maintainability

### Fixed
- **Qwen tool_use persistence** — Forward tool_use blocks so they get persisted and
  rendered correctly in session history
- **Qwen stop_reason looping** — Default missing stop_reason to EndTurn to prevent
  response loops
- **Qwen spawn args + display name** — Correct process arguments, tool mapping, and
  suppress internal tool blocks
- **Qwen static models** — Never invoke qwen subprocess for model list (uses hardcoded
  supported_models)
- **Active provider vision** — Now uses actual active provider instead of iterating
  priority order (was grabbing MiniMax before OpenRouter)
- **Streaming marker leak** — Stop reasoning/tool markers from leaking into visible
  response text
- **TUI input defense** — Stronger protection against mouse escape garbage and focus
  switch escape sequences
- **Telegram photo cleanup** — Extended temp file retention to 24h (images may be
  re-read hours later by analyze_image tool)
- **Custom provider context_window** — Honor custom provider's context_window override
  in compaction budget calculation
- **Subagent test isolation** — Per-thread dir override prevents cross-test pollution

### Changed
- Replaced all hardcoded provider boundary indices (9/10/11) with named constants
  throughout TUI dialogs and onboarding render
- Cargo.toml version 0.3.0
- CHANGELOG entries for 0.3.0

## [0.2.99] - 2026-04-06

### Added
- **Qwen Code CLI provider** — Full integration of Qwen Code as a CLI backend provider.
  1,000 free requests/day via Qwen OAuth, no API key required. Supports qwen3-coder-plus,
  qwen3.5-plus, qwen3.6-plus, and more. 256K context window, streaming, tools.
  Install: `npm install -g @qwen-code/qwen-code` or `brew install qwen-code`.
  Config: `[providers.qwen_code_cli] enabled = true`
- **Vision-first file processing** — All file attachments (PDFs, images) processed via
  vision model before text extraction, across TUI and all channel handlers
- **PDF-to-image rendering utility** — High-quality PDF page rendering for vision analysis
  (`pdf_vision.rs`), replaces plain text extraction with visual inspection
- **Proactive rate limiting for OpenRouter :free models** — Paces requests automatically
  to avoid account-level bans; shared global static limiter across all :free instances
- **File-based subagent progress streaming** — JSON status files (`pending`, `running`,
  `completed`, `failed`) for real-time sub-agent progress via spawn/status tools
- **Subagent provider/model defaults in config** — `[agent]` section with
  `subagent_provider`/`subagent_model`, injected into config.toml on startup

### Fixed
- **Rate limiter first-call wait** — `last_granted=0` treated as sentinel so first call
  no longer sleeps unnecessarily
- **Compaction text leak** — Residual context no longer survives compaction; cancel token
  threading fixed across session restarts
- **Vision registration priority** — `provider.vision_model` now takes priority over
  `image.vision` fallback
- **CI failures** — Binary test assertion and rate_limiter fmt/clippy errors squashed
- **Cosmetic cleanup** — Discord, utils, and test module fixes

### Refactored
- **Brain core injection** — USER.md replaces IDENTITY.md in TUI core brain; IDENTITY.md
  moved to contextual files for on-demand loading (cron/social sessions only)
- **Post-compaction brain recovery** — CODE.md injected as ~300-token best-practices
  summary instead of full 420-line file; explicit `load_brain_file("CODE.md")` directive
  before any code task

## [0.2.98] - 2026-04-05

### Added
- **Auto-fallback on rate limits** — When the primary provider hits a rate/account limit mid-stream, catches `RateLimitExceeded`, saves state, and resumes the same conversation on a fallback provider configured in `providers.fallback`
- **Fallback provider chain from config** — Reads `[providers.fallback]` at startup to build an ordered list of fallback providers. `has_fallback_provider()` and `try_get_fallback_provider()` for runtime queries
- **Telegram resume with full streaming pipeline** — Interrupted Telegram sessions now resume with typing indicator, tool status messages, edit loop, dedup, and rate-limit retry. Previously the user saw silence for minutes
- **Telegram bot commands autocompletion** — Registers all 9 slash commands (`help`, `models`, `usage`, `new`, `sessions`, `stop`, `compact`, `doctor`, `evolve`) via `setMyCommands` after bot auth. No manual BotFather setup needed

### Fixed
- **PDF text extraction** — Extract text from PDF files via `pdf_extract` instead of returning `Unsupported`
- **Context compaction runaway enforcement** — Two-tier budget enforcement: 65% soft trigger (LLM compaction with retries), 90% hard floor (forced truncation to 75%, cannot fail). Pre-truncate target now scales proportionally (85% of max_tokens) instead of hardcoded 170k, supporting custom providers with different context windows. Compaction is now silent to user — summary written to memory log only, no chat spam
- **Telegram duplicate messages** — Edit streaming message in-place instead of delete+send race; cancel guard moved before display queue to prevent stale messages after cancellation
- **Telegram dedup diagnostics** — INFO/WARN logging on the dedup path to trace exactly what's being stripped
- **TUI token counter stuck at 111K** — Removed monotonic guard so CLI-calibrated token count (~41K) reaches display instead of being blocked by the post-compaction tiktoken estimate (~111K)
- **Local timezone in logs** — Log timestamps now show local timezone with `%:z` offset instead of UTC
- **Rate limit detection in CLI errors** — Parses "rate limit", "429", "overloaded", "too many requests", "hit your limit" as `ProviderError::RateLimitExceeded`
- **Telegram resume race on bot auth** — Polls `tg.bot().await` up to 30s before calling `resume_session` to avoid the 328ms startup race

### Refactored
- **Context budget enforcement** — `enforce_context_budget()` with two-tier enforcement: 65% soft LLM compaction, 90% hard truncation floor. Safety truncation to 80% if compaction exhausts all retries. Removed CompactionSummary/Compacting progress events — compaction fully silent to user
- **Telegram resume pipeline** — Routes through `handler::resume_session()` instead of bare agent call with no streaming or feedback

### Testing
- **55 Telegram resume tests** — Cancel tokens, dedup logic, markdown-to-HTML, message splitting, pending approvals, bot wait loop, cancel guard ordering, token counter regression

[0.2.98]: https://github.com/adolfousier/opencrabs/compare/v0.2.97...v0.2.98

## [0.2.97] - 2026-04-04

### Added
- **Agent type system** — Typed subagents (`General`, `Explore`, `Plan`, `Code`, `Research`) with filtered tool registries. Each type gets a curated subset of the parent's tools, preventing recursive spawning and dangerous operations via `ALWAYS_EXCLUDED` list
- **Team orchestration** — `TeamManager` coordinates named groups of agents. New tools: `team_create` (spawn N typed agents as a named team), `team_delete` (cancel and clean up), `team_broadcast` (fan-out messages to all running agents in a team)
- **Subagent provider/model config** — `[agent]` section in config with `subagent_provider` and `subagent_model` fields. Spawned agents inherit the configured provider instead of always loading from global config
- **Subagent input loop** — `send_input` now works: spawned/resumed agents wait for input after completing a round instead of exiting. Enables multi-turn conversations with child agents

### Fixed
- **Tool call descriptions truncating instead of wrapping** — `render_tool_group` now wraps description headers and value lines to terminal width. Removed 80-char pre-truncation of bash commands in `format_tool_description`. Added `file_path`/`filePath` fallbacks for file-related tools
- **Double-escape cancel losing visible content** — Streaming response and active tool group now persisted to DB *before* `handle.abort()` fires, so cancelled content survives reload
- **Claude CLI subprocess leak on cancel** — Stream reader loop monitors `tx.closed()` via `tokio::select!` and kills the child process when the receiver is dropped
- **Telegram duplicate messages on cancel** — Added `cancel_token.is_cancelled()` guard before delivering final response, preventing stale agent results from posting after cancellation
- **Config overwriting existing channel settings** — `apply_config()` now scopes writes to only the current onboarding step. `from_config()` sets `EXISTING_KEY_SENTINEL` for all existing channel data so untouched fields are never overwritten
- **Pane switch not updating model display** — Session provider now swaps the agent to match the session's configured provider instead of overwriting the session
- **Tool input not persisted for CLI segments** — `CliSegment::Tool` now includes `"i"` field for `tool_input`, surviving DB reload

### Testing
- **84 subagent/team tests** — Manager state machine (27), SendInputTool (6), CloseAgentTool (5), WaitAgentTool (7), lifecycle (8), AgentType filtering (10+), TeamManager (10), TeamDeleteTool (4), TeamBroadcastTool (5), registry exclusion (1)
- **HTML comment strip tests aligned** — `strips_malformed_close_tag` → `preserves_malformed_close_tag`, `strips_unclosed_comment` → `preserves_unclosed_comment` to match actual (correct) behavior
- **1,772 total tests** (up from 1,687)

## [0.2.96] - 2026-04-02

### Added
- **OpenRouter reasoning support** — Send `include_reasoning: true` in requests to OpenRouter models. Thinking/reasoning output now displayed in collapsible sections for models that support it (e.g. Qwen 3.6 Plus)
- **Function calling detection** — Warn users when a model does not support tool use. Detects raw tool call JSON in text responses and appends a visible warning with model switch suggestion

### Fixed
- **Thinking/reasoning text truncation** — Reasoning content now wraps to screen width instead of truncating at the right edge. Long lines in collapsible thinking sections reflow properly on narrow terminals
- **LLM artifacts leaking to TUI** — `<!-- reasoning -->` tags, `</invoke>`, `</parameter>` XML fragments no longer rendered as plain text. `strip_llm_artifacts` applied to completed responses, intermediate text, and streaming render
- **Duplicate agent response on rebuild/evolve restart** — Agent responded twice with identical "Back online" messages because both a wake-up message and evolution message fired at startup. Merged into a single message
- **Evolution prompt leaked to user** — Internal `[SYSTEM:` instruction for evolution/rebuild was displayed as a visible user message. Now hidden from chat
- **Windows CI compilation** — `unsafe extern` for FFI blocks (Rust 2024 edition), unreachable code after platform-specific `bail!`, unused `voice_id` variable gated behind `local-tts` feature
- **browser_test example** — Gated behind `browser` feature flag so `--no-default-features` builds don't fail. Un-ignored `examples/` directory so CI has the file
- **Flaky concurrent profile test** — `ProfileRegistry::save()` now uses atomic write with file locking to prevent concurrent readers from seeing partially-written TOML

### Changed
- **`tool_choice: "auto"`** — OpenAI-compatible providers now send `tool_choice: "auto"` when tool definitions are present, enabling function calling on models that require explicit opt-in

## [0.2.95] - 2026-04-02

### Added
- **Up/Down arrow navigation for attached images** — Navigate between attached images in the input area using arrow keys. Visual indicator shows current position (e.g. "2/4"). Previously required detaching and reattaching to reorder
- **Rolling build output for /rebuild** — Build progress now shows as a single updating message with the last 6 compile lines, replacing the previous flood of 200+ individual system messages. Cleared automatically on restart
- **Rolling status quips for CLI providers** — Processing status messages now fire from the first keystroke even before tools have started, via a `processing` flag on the streaming snapshot. Previously required active tool calls to trigger
- **Multi-target cron delivery** — `deliver_to` field now supports comma-separated targets (e.g. `http://...,telegram:-12345`). Each target receives results independently. All existing cron jobs updated to deliver to both agentverse and Telegram

### Fixed
- **CLI token tracking showing near-zero usage** — `CliUsage::total_input()` returned only `input_tokens` (1-3 per message), excluding `cache_creation_input_tokens` (~80K) and `cache_read_input_tokens` (~14K). Every CLI provider message burned real API credits but reported $0.00 cost and ~6 tokens. Now includes all cache tokens in usage tracking, cost calculation, and session stats. `TokenUsage` struct gains `cache_creation_tokens`, `cache_read_tokens`, `billing_cache_creation`, and `billing_cache_read` fields separating context window tracking from billing. Cache-aware pricing (1.25x input rate for cache writes, 0.1x for cache reads)
- **Context window display showing 2.3M tokens** — CLI providers accumulated tiktoken estimates via `add_message()` without calibration, snowballing `context.token_count` to 2.3M and triggering false compaction. Now capped at model's context window. Billing tokens (cumulative across CLI tool rounds) tracked separately from context window (per-call values)
- **Per-session provider isolation** — Changing provider/model in TUI no longer changes it for Telegram/Discord/Slack sessions. Each session's provider is persisted in DB and restored on message receipt via `sync_provider_from_config`. Channel `/models` command no longer mutates global config
- **Custom provider dialog** — "+ New Custom Provider" now shows blank form fields instead of retaining values from previously loaded custom provider (e.g. nvidia). Dialog height increased to show all custom provider fields (Base URL, API Key, Model, Name, Context Window) without truncation
- **Config reload feedback loop causing silent crash** — Writing config inside a ConfigWatcher callback triggered an infinite reload cycle. Two additional triggers found and removed: `Config::write_key()` inside provider creation, and redundant `create_provider()` on every config reload event
- **Queued message ordering and Up-arrow dequeue** — Messages queued while the agent was processing could arrive out of order. Up-arrow now correctly dequeues the last queued message instead of the first
- **E2E test timeouts** — `e2e_opencode_streaming` wrapped with 30s timeout to prevent test suite hang under concurrent load. Gemini fetch tests gracefully skip on API key/network failures instead of crashing the suite

### Security
- **Trello API credentials moved from URL interpolation to query builder** — All 24 Trello client methods refactored to use `authed_get/post/put/delete` helpers that pass `key` and `token` via `reqwest::RequestBuilder::query()` instead of string interpolation. Resolves 24 CodeQL alerts
- **Gemini API key moved to request header** — API key now sent via `x-goog-api-key` header instead of URL query parameter across provider, fetch, and onboarding modules. Resolves 4 CodeQL alerts
- **Image tool API keys moved to request headers** — `analyze_image` and `generate_image` tools now pass API keys via headers instead of URL query strings. Resolves 2 CodeQL alerts
- **CI workflow permissions restricted** — Added top-level `permissions: contents: read` to `ci.yml` and `release.yml`, with explicit `contents: write` only on jobs that need it (`build-release`, `create-release`). Resolves 5 CodeQL alerts
- **Removed API key logging in tests** — Gemini fetch test no longer prints key length or prefix to stderr

### Changed
- **Brain file templates updated** — MEMORY.md template restructured as agent scratchpad for rules, corrections, and preferences. AGENTS.md template adds mandatory memory triggers. TOOLS.md template adds 15 missing tools

> **Existing users:** Your local brain files (`~/.opencrabs/*.md`) may be outdated. Ask your crab: *"Compare my brain files against the latest templates in `src/docs/reference/templates/` and append anything missing."*

### Testing
- **29 token tracking tests** — TokenUsage struct, cache-aware pricing, CLI/API flow, billable accumulation, cost regression, provider format deserialization
- **1,687 total tests** (up from 1,605)

## [0.2.94] - 2026-03-31

### Added
- **Multi-profile support** — Run multiple isolated OpenCrabs instances from a single installation. Each profile gets its own config, memory, sessions, skills, and gateway service under `~/.opencrabs/profiles/<name>/`. Create with `opencrabs profile create <name>`, switch with `opencrabs -p <name>` or `OPENCRABS_PROFILE` env var. Default profile (`~/.opencrabs/`) works exactly as before — zero breaking changes
- **Profile migration** — Copy config and brain files between profiles with `opencrabs profile migrate --from default --to hermes [--force]`. Migrates all `.md` and `.toml` files plus the `memory/` directory. Excludes DB, sessions, logs, and layout state so the target profile starts fresh with the source's personality and configuration
- **Profile export/import** — Share profiles as portable `.tar.gz` archives with `opencrabs profile export <name>` and `opencrabs profile import <path>`
- **Token-lock isolation** — PID-based lock files prevent two profiles from binding the same bot token (Telegram, Discord, Slack, Trello). Stale lock detection automatically cleans up locks from dead processes
- **Profile-aware daemon services** — `opencrabs -p hermes service install` creates profile-specific plist/systemd units (`com.opencrabs.daemon.hermes` / `opencrabs-hermes`). Multiple profile daemons can run simultaneously as separate OS services

### Fixed
- **CLI stream idle timeout too short** — CLI providers run tools internally (cargo build, cargo test, gh commands) that can take several minutes without producing stream events. The 60-second idle timeout caused premature stream termination → retry → fresh CLI session repeating all prior work. Now 10 minutes for CLI providers, 60 seconds for API providers
- **CLI token usage lost on EOF** — When Claude CLI exits without a `Result` message but accumulates token counts from `message_delta` events, the usage was silently discarded. Now flushes accumulated input/output tokens as a final `MessageDelta` + `MessageStop` on EOF
- **Service command compilation on Linux** — `_systemd_name` variable was prefixed with underscore (suppressing "unused" warning on macOS) but referenced without underscore in Linux-only code paths, causing CI build failure on ubuntu-latest
- **TUI duplicate text on streaming responses** — Streaming responses were doubled on screen due to intermediate text emission timing
- **Usage stats wrong totals** — Model name duplication in usage ledger (`opus` vs `opus-4-6`) caused inflated cost tracking. Now merges bare model names with their versioned equivalents
- **IntermediateText not firing for Telegram/channels on CLI providers** — CLI providers weren't emitting intermediate text events to channel handlers, causing silent gaps in multi-tool conversations
- **E2E test suite hanging on slow providers** — Added 30-second timeout to `e2e_opencode_streaming` test to prevent indefinite blocking under concurrent load
- **E2E tests crashing suite on API key/network failures** — Gemini and OpenCode E2E tests now gracefully skip with a warning instead of panicking when API keys are invalid or network is unreachable

### Changed
- **`opencrabs_home()` delegates to profile resolver** — All 30+ call sites automatically resolve to the active profile's directory. Logger, onboarding wizard, and brain file resolver no longer hardcode `~/.opencrabs`
- **Channel manager acquires token locks before spawning** — Telegram, Discord, Slack, and Trello channel connections check for token conflicts before starting. All locks released on TUI exit and daemon shutdown

### Testing
- **57 profile tests** — Name validation (8), token hashing (6), registry CRUD (10), path resolution (3), error messages (4), CRUD lifecycle, export/import roundtrip, token lock acquire/release/stale detection, migration with force/skip/nested dirs, isolation guarantees, daemon service argument generation
- **Usage ledger normalization tests** — Model name merging for bare vs versioned names
- **1,605 total tests**

### Docs
- **README profiles section** — Full command reference, directory structure diagram, token-lock isolation explanation, daemon service management, migration workflow
- **TESTING.md** — Updated with profile test counts and categories

## [0.2.93] - 2026-03-30

### Fixed
- **Crash recovery routes responses back to originating channel** — Previously `pending_requests` always stored `channel="tui"` and recovery only sent responses via TuiEvent. Now each channel (Telegram, Discord, Slack, WhatsApp, Trello) passes its name and `chat_id` through to `run_tool_loop`, which stores them in the DB. On restart, recovery routes responses back to the correct channel using the stored `channel_chat_id`
- **UTF-8 panics on multi-byte string truncation** — Byte-index slicing on multi-byte emoji (e.g. `🔺` at bytes 497..501) caused panics in `context.rs`, `panes.rs`, and Telegram handler. All string truncation now uses `floor_char_boundary`/`ceil_char_boundary` to land on valid UTF-8 boundaries
- **TUI responses vanishing when CLI model ends with tool calls** — Previous fix extracted only trailing text (after last tool) as the final response, but when the model ends with tool calls and no trailing text, `final_text` was empty. Reverted to extracting all text; Telegram dedup now happens in the handler by tracking sent intermediate texts and stripping them from the final response
- **TUI dropping trailing text after tool calls** — `complete_response` now updates the intermediate message instead of skipping it, ensuring text that follows tool call blocks renders correctly
- **Panic protection for Telegram message handler** — Nested `tokio::task::spawn` catches panics in the Telegram message handler instead of silently losing them

### Added
- **New DB migration: `pending_requests_channel_chat_id`** — Adds `channel` and `channel_chat_id` columns to `pending_requests` table for cross-channel crash recovery routing

### Testing
- **Crash recovery and self-healing tests for all channels** — Channel-specific pending request storage, `get_interrupted_for_channel` filtering, `delete_ids` selective deletion, multi-channel coexistence, UTF-8 safe string truncation with emoji/CJK, panic protection pattern verification
- **1,605 total tests** (up from 1,593)

## [0.2.92] - 2026-03-29

### Added
- **Self-healing config recovery** — When `config.toml` becomes corrupted or unloadable, OpenCrabs automatically restores from the last-known-good snapshot saved on every successful write. User sees a notification explaining what was recovered
- **Provider health tracking** — Per-provider success/failure history tracked in `~/.opencrabs/provider_health.json`. `/doctor` slash command shows health stats. Failed providers logged with timestamps for debugging intermittent API issues
- **DB integrity check on startup** — SQLite `PRAGMA integrity_check` runs at boot. If corruption is detected, a notification appears in TUI and all channels instead of silently failing
- **Unknown config key warnings** — Unknown top-level keys in `config.toml` now trigger a startup notification listing the unrecognized keys, catching typos like `[teelgram]` or `[a2a_gatway]`
- **Self-healing user notifications** — All self-healing events (config recovery, provider failures, integrity issues) surface as visible notifications across TUI, Telegram, Discord, Slack, and WhatsApp instead of hidden log entries

### Fixed
- **Telegram intermediate texts vanishing between tool rounds** — Messages sent during multi-tool iterations disappeared because new edits overwrote previous content. Telegram handler now maintains a persistent intermediate message stack with proper ordering
- **Telegram intermediate texts not sticking** — Follow-up fix: intermediate text messages were still being deleted prematurely during rapid tool execution. Reworked the message lifecycle to hold messages until the final response arrives
- **Duplicate final response on Telegram for CLI providers** — CLI providers return all content blocks in a single iteration. IntermediateText emitted the full text, then the final response repeated it. Now IntermediateText only emits text before the last tool block; final response only extracts text after it
- **Reasoning as fallback intermediate text** — When a CLI provider returns reasoning but no visible text between tool rounds, the reasoning content is now used as fallback intermediate text for channels instead of showing nothing
- **Non-focused panes hiding tool calls and thinking text** — `render_simple_message` skipped tool_group messages entirely, so non-focused split panes showed less content than the focused pane. Now shows compact tool call summaries and stripped reasoning text
- **Non-focused pane collapsed tool groups** — Tool groups in non-focused panes now display as single collapsed lines matching the focused pane style, with thinking indicators for reasoning blocks
- **Non-focused panes not scrolled to bottom** — Split panes that weren't focused appeared stuck at the top. Fixed scroll position calculation for inactive panes
- **Inactive split panes stale cache** — Cached render state for background panes wasn't invalidated when new messages arrived. Now clears cache on session updates
- **Tool calls showing running forever after completion** — Tool call status stayed at "running" spinner even after the tool finished. Now correctly transitions to success/failure state
- **Silently dropped errors across config, channels, and persistence** — 14 files had `let _ = ...` or `.ok()` swallowing errors in config writes, channel sends, tool connections, and pane state. All now surface errors via logging or user notifications
- **Remaining silent error drops in tools and channel handlers** — Second pass caught additional swallowed errors in Slack connect, Trello connect, slash commands, Telegram handler, and WhatsApp handler
- **Onboarding config write errors batched** — Config writes during onboarding used individual `let _ =` calls. Replaced with `try_write!` macros that batch errors and surface them at the end of each wizard step
- **Config::load() fallback-to-default** — Render, dialogs, messaging, and cron modules silently fell back to default config when load failed, masking real config issues. Now propagate errors or use the passed-in config reference
- **Custom provider name normalization** — Custom provider names with mixed case or whitespace were treated as different providers. Now normalized on both load and save
- **Case-insensitive tool input key lookup** — Tool input display descriptions used exact-case key matching, failing for providers that return keys in different casing
- **Cached state not cleaned on session delete** — Deleting a session left stale cached pane state behind. Now clears cache entries for the deleted session
- **`gateway` serde alias for A2A config** — Added `gateway` as a serde alias for the A2A config section, plus deduplication of typo warnings
- **Model selector wiping API keys on Enter** — Pressing Enter in the model selector could clear the API key for the selected provider. Now preserves existing keys
- **IntermediateText emission timing for CLI providers** — IntermediateText was emitted after clearing iteration state, losing the accumulated text. Now emits before clearing

### Changed
- **AgentService::new() requires &Config** — Constructor now takes an explicit `&Config` parameter instead of calling `Config::load()` internally. Eliminates hidden I/O, makes dependencies explicit, and enables test injection. All production callers and 11 test files updated

### Testing
- **27 self-healing system tests** — Config snapshot/restore, provider health tracking, DB integrity check, unknown key detection, notification delivery across all channels
- **All test files migrated to `AgentService::new_for_test()`** — 11 test files updated to use the new test constructor
- **1,593 total tests** (up from 1,564)

## [0.2.91] - 2026-03-29

### Added
- **Startup update prompt** — When a new version is available, a centered dialog appears on top of the splash screen asking the user to accept (Enter) or skip (Esc). Accepting triggers `/evolve` automatically; skipping returns to splash so the user sees their current version. After update, the binary restarts and splash shows the new version
- **`/doctor` channel command** — Health check now available directly on Telegram, Discord, Slack, and WhatsApp without going through the LLM. Returns provider status, channel config, voice config, and approval policy
- **Shared text command handler** — New `try_execute_text_command()` in `commands.rs` handles Help, Usage, Evolve, Doctor, and UserSystem commands in one place. All four channel handlers delegate through this shared function, eliminating duplicated command logic
- **Pane session preloading** — Restored split panes now preload their session messages from DB on startup, so pane content is visible immediately instead of blank
- **Persistent pane layout** — Split pane configuration (splits, sizes, focused pane) now saves to `~/.opencrabs/pane_layout.json` on quit and Ctrl+C, and restores on restart

### Fixed
- **UTF-8 char boundary panics** — `split_message()` in all 5 channel handlers (Telegram, Discord, Slack, WhatsApp, Trello) could panic on multi-byte characters (emojis, €, CJK). Now uses `is_char_boundary()` to find safe split points
- **Model switch errors silently swallowed** — Telegram, Discord, and Slack always showed "✅ Model switched" even when provider creation failed. Now surfaces the actual error with `⚠️` prefix
- **CLI provider ARG_MAX crash** — When OpenCode CLI conversation context exceeded OS `ARG_MAX` (~1MB on macOS), the spawn failed with "Argument list too long". The emergency compaction handler now catches this error, auto-compacts context, and retries. If compaction itself fails, falls back to hard truncation (keeps last 24 messages) with a marker telling the agent to use `search_session` for older context
- **`/evolve` hitting provider errors on channels** — `/evolve` was being routed through the LLM instead of executing directly. Now runs as a direct command on all channels (downloads and reinstalls without LLM involvement)
- **CLI tool calls lost on Esc×2 and restart** — Tool call results from CLI providers were not persisted to DB, so they vanished on double-escape cancel or process restart. Now saved alongside regular messages
- **Session not reloaded after double-escape cancel** — After cancelling with Esc×2, the session context was stale. Now reloads from DB to pick up any changes made during the cancelled operation
- **Thinking text unreadable on Telegram** — Thinking/reasoning blocks had poor formatting. Improved readability with proper styling
- **Model selector missing '↓ N more' indicator** — Long model lists didn't show a scroll indicator. Added count of hidden items below the visible list
- **Model list sorted alphabetically instead of by date** — Fetched models now sort newest-first so latest releases appear at the top
- **Pane layout lost on quit** — Split pane configuration was only in memory. Now persists to disk on quit and Ctrl+C

### Changed
- **`config.toml.example`** — Added z.ai GLM provider section with configuration examples

### Testing
- **2 emergency compaction tests** — `ArgTooLongMockProvider` and `ContextLengthMockProvider` verify the retry flow works after compaction/truncation
- **1,564 total tests** (up from 1,562)

## [0.2.90] - 2026-03-27

### Added
- **Daemon health endpoint** — New `[daemon] health_port = 8080` config option. When set, `opencrabs daemon` binds a lightweight `GET /health` endpoint returning 200 OK + JSON status. Useful for systemd watchdog, uptime monitors, and external health probes
- **Shared provider registry** — Single source of truth (`src/utils/providers.rs`) for all LLM provider metadata. TUI `/models`, `/onboard`, and channel `/models` all derive from `KNOWN_PROVIDERS` — no more hardcoded index-based match blocks that fall out of sync

### Fixed
- **Daemon mode Telegram/Discord dying silently** — Channel bots (Telegram long-polling, Discord gateway) would exit on network hiccups or token conflicts without restarting. Added retry loops with 5s backoff so daemon mode auto-reconnects instead of going unresponsive while the process stays alive
- **CLI providers missing from channel `/models`** — Claude CLI and OpenCode CLI were not listed in Telegram/Discord/Slack provider pickers because `configured_providers()` required an explicit `enabled = true` config entry. CLI providers are now always listed since they need no API key — matching TUI behavior
- **Channel providers out of sync with TUI** — Channels were missing zhipu (z.ai GLM), Claude CLI, and OpenCode CLI providers. All provider listings now derive from the shared registry

### Changed
- **CONTRIBUTING.md rewrite** — Anti-stub policy, step-by-step contribution workflows, exact CI commands, "What Gets Your PR Closed" section, and guidance for non-coders to open issues instead of submitting empty PRs

### Testing
- **10 daemon health tests** — DaemonConfig deserialization, health endpoint 200/404 responses, CLI providers always listed, API key providers gated correctly
- **1,562 total tests** (up from 1,424)

## [0.2.89] - 2026-03-27

### Added
- **Telegram rolling status quips** — During long CLI tool runs (subagents, 100+ tool rounds), Telegram now shows rotating fun messages like "☕ Grab a coffee — my sub-agents are on fire right now (42 tools, 2m 15s)". Each quip shows for 5s, vanishes, pauses 2s, then the next one appears. Auto-deletes when real streaming text arrives

### Fixed
- **OpenCode CLI permission rejection** — Non-interactive spawns auto-rejected tool calls (no TTY). Now sets `OPENCODE_PERMISSION` env var to allow all permissions including external directories
- **TUI provider mismatch after restart** — Loading a session overrode the config-enabled provider with the session's stale saved provider. Config is now authoritative — session metadata syncs to the active provider
- **Silent empty responses on stream drop** — When the provider stream dropped repeatedly, the TUI showed an empty response. Now injects a visible error message so the user knows what happened
- **OpenCode CLI tool calls not visible in TUI** — Tool call events were sent as invisible Ping instead of ContentBlock::ToolUse. Now emits proper stream events so helpers.rs fires ToolStarted/ToolCompleted progress events, restoring the expandable tool call groups
- **OpenCode CLI filesystem access** — Existing sessions locked tool execution to their original directory, blocking access to ~/Downloads/ etc. Now spawns at ~/ with explicit `--dir` flag so the sandbox covers the full user home
- **OpenCode CLI `cli_handles_tools` flag** — Was returning false, causing the tool_loop to attempt local re-execution of opencode's internal tool calls. Now correctly returns true
- **Duplicate assistant message for CLI providers** — helpers.rs flushed text as IntermediateText at stream end, then tool_loop emitted the same text again when iteration > 0. Skips the second emission for CLI providers

## [0.2.88] - 2026-03-26

### Added
- **Smart browser detection** — Auto-detect default Chromium-based browser (Chrome, Brave, Edge, Chromium) instead of hardcoded path. Feature flag docs and browser detection docs added to README

### Fixed
- **Slack/WhatsApp markdown formatting** — Messages were sent with raw markdown (`**bold**`, `~~strike~~`). New `markdown_to_mrkdwn` converter transforms to native format (`*bold*`, `~strike~`, `<url|text>`, `*Heading*`) before sending. Applied to handler response paths, streaming paths, and send tools for both Slack and WhatsApp. Discord uses standard markdown natively — no conversion needed
- **Gemini model fetching** — Multiple root causes: `GeminiModel` struct missing `#[serde(rename_all = "camelCase")]` so `supportedGenerationMethods` never deserialized; provider index 3 (Gemini) missing from `supports_model_fetch()` match; `/models` dialog passed `None` API key when navigating between providers
- **Model selector race condition** — Navigating between providers quickly caused stale async fetches to overwrite the current provider's model list (e.g. GPT models appearing under Claude CLI). `ModelSelectorModelsFetched` event now carries the provider index; handler discards results that don't match the currently selected provider
- **Model selector dialog oversized** — Dialog grew to fill the entire terminal with empty space. Height now sizes to content and caps at 75% of terminal height
- **API keys logged in plaintext** — Three locations were logging secret values: fetch entry logging, Gemini-specific logging, and `config/types.rs write_key()`. All removed — only `has_api_key=true/false` is logged now
- **CLI session ID conflicts** — Fresh session IDs per spawn for both Claude CLI and OpenCode CLI to prevent lock conflicts
- **CLI image routing** — CLI providers now route images through `analyze_image` instead of inline encoding
- **CLI error surfacing** — Error results from CLI providers are now surfaced to the user. Added Slack required scopes documentation
- **CLI cache token tracking** — Cache creation and cache read tokens excluded from context window tracking to prevent false compaction triggers

### Changed
- **Unified provider+model selection** — Extracted ~500 lines of duplicate provider/model selection logic from `/models` dialog and `/onboard` wizard into shared `ProviderSelectorState` module (`src/tui/provider_selector.rs`). Both consumers now embed this struct, eliminating sync drift between the two UIs

### Testing
- **21 Slack formatting tests** — Bold conversion, italic unchanged, strikethrough, inline code, code blocks, headings, links, mixed formatting, real-world plan messages, edge cases
- **Onboarding test fixes** — Tests now set API key after reaching ProviderAuth step (detect_existing_key clears it on Workspace→ProviderAuth transition)

## [0.2.87] - 2026-03-26

### Added
- **Full CLI command surface** — 20+ subcommands: `opencrabs status`, `doctor`, `agent` (interactive multi-turn CLI agent + single-message mode), `channel list/doctor`, `memory list/get/stats`, `session list/get`, `db init/stats/clear`, `cron add/list/remove/enable/disable/test`, `logs status/view/clean/open`, `service install/start/stop/restart/status/uninstall` (launchd on macOS, systemd on Linux), `completions` (bash/zsh/fish/powershell via `clap_complete`), `version`, `daemon`, `onboard`. Full CLI reference added to README
- **Split panes** — tmux-style horizontal (`|`) and vertical (`_`) pane splitting in TUI. Each pane runs its own session with independent provider, model, and context. Run 10 sessions side by side, all processing in parallel. `Tab` to cycle focus, `Ctrl+X` to close pane. Pane focus indicator `[n/total]` in status bar. 21 tests covering layout, focus, and management
- **Dynamic tool system** — Define custom tools at runtime via `~/.opencrabs/tools.toml`. HTTP and shell executors, template parameters (`{{param}}`), enable/disable without restart. The `tool_manage` meta-tool lets the agent create, remove, and reload tools on the fly. `DynamicToolRegistry` with `RwLock`-based concurrent access
- **Native browser automation** — Headless Chrome control via CDP (Chrome DevTools Protocol). 7 browser tools: `navigate`, `click`, `type`, `screenshot`, `eval_js`, `extract_content`, `wait_for_element`. Lazy-initialized singleton, stealth mode, persistent profile, display auto-detection. Feature-gated under `browser` (`--features browser`)
- **Multi-agent orchestration** — Spawn independent child agents for parallel task execution. 5 tools: `spawn_agent`, `wait_agent`, `send_input`, `close_agent`, `resume_agent`. Children run in isolated sessions with auto-approve and essential tools
- **DB-persisted channel sessions** — All 5 channels (Telegram, Discord, Slack, WhatsApp, Trello) now persist channel/group sessions in SQLite by title via `find_session_by_title`. Sessions survive process restarts — no more lost context after daemon restart
- **Slack user/channel name resolution** — User display names and channel names resolved via `users.info` and `conversations.info` API on each message. Agent sees "Adolfo Usier" instead of "U066SGWQZFG", stored messages have proper `sender_name` and `channel_name`
- **Slack event dedup + fast ack** — `on_push_event` returns ack immediately via `tokio::spawn`, deduplicates by message timestamp. Eliminates Slack retry storms that caused duplicate processing with slow CLI providers
- **LLM-generated channel greetings** — Channels send a personalized greeting on first connect via Slack `app_mention` handling
- **OpenCode model pricing** — MiMo V2 Pro/Omni, Nemotron 3, Big Pickle, Zen, Go
- **CLI reference in README** — Full CLI command table with descriptions and flags added to Core Features section + Table of Contents

### Fixed
- **Per-channel session isolation** — Owner DMs share TUI session, but groups/channels get isolated per-channel sessions keyed by `channel_id` (Telegram, Discord, Slack). Previously all messages shared the TUI session regardless of source
- **In-memory session HashMap replaced with DB** — Channel sessions were stored in an in-memory `HashMap` that was lost on every restart, creating new sessions each time. Now uses SQLite `find_session_by_title` across all 5 channels
- **Slack duplicate message processing** — Slack retried events when ack took >3s (common with CLI providers). Each retry was processed as a new message, causing cascading cancellations and repeated work. Fixed with timestamp dedup + background spawn
- **Slack empty sender/channel names** — `store_channel_msg` was storing `String::new()` for sender name and `None` for channel name. Channel history showed blank names
- **Streaming response text concatenation** — `IntermediateText` events were not clearing the streaming response buffer, causing text to concatenate across tool rounds
- **Persistent typing indicators** — Telegram and Slack typing indicators now persist during long agent responses
- **Onboarding API key requirement for CLI providers** — CLI providers (Claude CLI, OpenCode CLI) no longer require an API key during onboarding
- **Slack mention detection with unknown bot_user_id** — Falls back to `<@U...>` pattern matching when `auth.test` fails
- **Slack bot token hot-reload** — Bot token is re-read from config at runtime for `auth.test` and API calls
- **Browser stealth mode** — Persistent profile directory, display auto-detection for headed mode
- **CLI provider auto-compaction** — Trigger auto-compaction after token calibration for CLI providers
- **Claude CLI token usage** — Cache creation and cache read tokens now included in usage calculation
- **CLI text/tool interleaving** — Real-time streaming preserves text and tool call ordering; queued messages inject at tool boundaries
- **CLI reasoning bloat** — Stop forwarding reasoning blocks after first tool call to prevent context explosion
- **CLI tool name normalization** — Lowercase tool names from CLI providers now match TUI display

### Changed
- **CLI module refactored** — All types (`Cli`, `Commands`, subcommand enums) and `run()` moved from `mod.rs` to `args.rs`. Module file is now clean module declarations only

### Testing
- **21 split pane tests** — Layout, focus cycling, close, and management
- **Claude CLI cache token tests** — Unit tests for cache token usage calculation
- **Browser headless tests** — Test coverage for headless Chrome integration

## [0.2.86] - 2026-03-23

### Added
- **Tool call context in all channels** — Slack and Discord now show real-time tool call progress with context (e.g. `✅ grep ("pattern")`), matching Telegram's behavior. Each tool call gets its own message that updates on completion
- **Smart tool context hints** — Tool descriptions show meaningful context: `cron_manage ("delete 'daily-report'")` instead of bare `cron_manage`. Handles action+target patterns for cron_manage, plan, task_manager, with smart fallback for unknown tools

### Fixed
- **Claude CLI 60s idle timeout** — CLI streams were killed after 60s of tool execution silence. Now sends Ping keepalives during tool execution and offsets content block indices across tool rounds to prevent collisions
- **OpenCode CLI idle timeout** — Same keepalive fix applied to OpenCode CLI provider for ToolUse, ToolResult, and mid-loop StepFinish events
- **Claude CLI tool calls invisible in TUI** — Tool calls, parameters, and Ctrl+O expansion were completely hidden. Now forwards tool_use content_block_start and input_json_delta as real StreamEvents, with cli_handles_tools() preventing re-execution
- **Queued message display ordering** — Queued messages appeared on top instead of after the assistant response, creating consecutive user/assistant messages. Swapped IntermediateText before QueuedUserMessage
- **Thinking text missing paragraph breaks** — Thinking blocks from different tool rounds were concatenated without separators. Now inserts `\n\n` between rounds
- **Provider wizard reverting to wrong provider** — Wrong index mapping in `new()` and `from_config()` for Claude CLI and OpenCode CLI providers
- **Selected model reverting to Sonnet** — `selected_model` index was never resolved from config after fetching models
- **Agent description in collapsed tool view** — Collapsed tool calls showed "Processing: Agent" instead of "Processing: Agent: Research heyiolo Supabase usage"
- **CLI-normalized tool names** — `format_tool_description` now matches both "Agent" and "agent" for CLI-normalized names
- **Telegram tool completion context lost** — Completion line showed just `✅ tool_name` without the context hint. Now single-line format preserves context
- **Help text padding in provider dialog** — Bottom commands aligned with provider list
- **Images dropped by CLI providers** — `materialize_image()` saves base64 images to temp files for Claude CLI and OpenCode CLI
- **Fallback provider model remapping** — Fallback provider now remaps model to its default when the primary's model is unsupported
- **OpenCode CLI stream break on tool-calls** — Don't break stream on `step_finish` with reason `tool-calls`
- **Cron session isolation** — Dedicated shared "Cron" session prevents cron jobs from polluting TUI session context

## [0.2.85] - 2026-03-22

### Added
- **OpenCode CLI provider** — New `opencode-cli` provider that spawns the local `opencode` binary for free LLM completions — no API key or subscription needed. Includes NDJSON streaming, extended thinking support, and live model fetching via `opencode models`
- **z.ai GLM provider** — New built-in provider for Zhipu AI (z.ai) with two endpoint types: General API and Coding API. Live model fetching, streaming, and tool support. Configurable via onboarding wizard or `/models`
- **Alphabetical provider sorting** — Provider lists in `/models` and `/onboard:provider` dialogs are now sorted alphabetically for easier navigation
- **Visual line navigation** — Up/Down arrow keys navigate wrapped lines visually in the input editor instead of jumping by logical lines. Queued message indicator shows when a message is waiting
- **Native extended thinking support** — `Thinking` variant in `ContentBlock` for native extended thinking content blocks from Anthropic models
- **Cron default provider/model config** — New `[cron]` config section to set default `provider` and `model` for cron jobs independently from interactive sessions
- **Real-time tool streaming events** — Emit `ToolStarted`/`ToolCompleted` events during streaming for real-time TUI tool visibility
- **AI providers README table** — All built-in providers listed in a summary table with auth type, models, and features at a glance
- **wacore 0.4.1 + stable Rust** — Upgraded wacore/whatsapp-rust crates from 0.3 to 0.4.1. Implemented 5 new trait methods (`get_max_prekey_id`, `get_latest_sync_key_id`, `store_sent_message`, `take_sent_message`, `delete_expired_sent_messages`). Added `wa_sent_messages` migration table. Disabled simd feature to drop nightly requirement. `cargo install opencrabs` now works on stable Rust

### Fixed
- **CLI provider onboarding skips API key** — CLI providers (OpenCode CLI) go directly from provider selection to model selection, matching the `/models` dialog behavior
- **`/models` filter/navigate for CLI providers** — Typing to filter and Up/Down navigation now work for CLI provider model lists
- **Anthropic `thinking_delta` SSE parsing** — Handle `thinking_delta` events in the Anthropic SSE stream parser instead of ignoring them
- **Streaming spinner spacing** — Added spacing between streaming content and the status spinner line
- **Thinking blocks skipped in SSE parser** — Skip thinking blocks in Anthropic SSE parser and suppress noisy log output
- **Context management: re-compact instead of hard-truncate** — Removed hard-truncation that blindly dropped messages; now re-compacts context instead, preserving conversation continuity
- **Context budget lowered to 65%** — Prevents MiniMax tool-call degradation that occurred at higher context utilization
- **XML tool-call recovery** — Recover XML tool calls from model output instead of silently dropping them
- **Secret redaction in DB persistence** — Redact secrets from user messages before writing to the database
- **Tool events emitted at ContentBlockStop** — Tool events now fire at `ContentBlockStop` with fully parsed input JSON instead of at `ContentBlockStart` with empty input, fixing TUI tool display timing
- **UTF-8 boundary panic** — Use `floor_char_boundary()` to prevent panics on string truncation at multi-byte character boundaries
- **Input buffer cleared on queued message injection** — Prevents stale input from leaking into the next prompt
- **z.ai inline API errors surfaced** — API error responses from z.ai now displayed in the TUI instead of silently dropping the stream

### Testing
- **21 OpenCode CLI provider tests** — Unit, config, factory, and end-to-end tests covering provider creation, model resolution, and actual CLI completions

## [0.2.84] - 2026-03-20

### Added
- **Cron HTTP webhook delivery** — Generic HTTP webhook URLs now supported as `deliver_to` targets in cron jobs, enabling integration with any HTTP endpoint (Slack incoming webhooks, custom APIs, notification services, etc.)

### Fixed
- **Streaming filter eating XML tags in prose** — The `STRIP_OPEN_TAGS` array in the streaming filter included tool-call XML tags (`<tool_call>`, `<tool_use>`, `<result>`, etc.). When MiniMax M2.7 mentioned these tags in prose (e.g. describing commit history), the filter entered `inside_think=true`, couldn't find a closing tag, and silently dropped all remaining text — truncating entire responses. XML tool-call tags removed from streaming `STRIP_OPEN_TAGS` (keep only `<think>`, `<!-- reasoning -->`, `<!--`)
- **`<result>` tag hallucinations from MiniMax M2.7** — Strip `<result>` XML blocks echoed by MiniMax in response text
- **`<tool_use>` hallucinated XML tags from MiniMax M2.7** — Strip `<tool_use>` wrapper blocks echoed by MiniMax
- **XML tool-call hallucinations parsed as real tool calls** — `acd3477` introduced a parser that converts XML tool-call blocks into actual executable tool calls when MiniMax emits them as text
- **LLM artifacts stripped from Telegram and cron delivery** — Hallucinated `<!-- tools-v2: -->`, `<!-- /tools-v2: -->`, `<think>`, `</think>`, and XML block markers now stripped before delivery to Telegram and cron webhook outputs
- **LLM artifacts stripped from Discord, Slack, and WhatsApp** — Same artifact stripping extended across all remaining channels
- **XML hallucination inline execution reverted** — Inline XML tool-call execution was poisoning context; reverted to pure stripping approach

### Changed
- **API error display includes error_type** — Error responses now include the raw `error_type` field and full Anthropic error body in logs for easier debugging

### Fixed
- **Plan `complete_task` fields made optional** — `success` and `output` fields on `complete_task` are now optional with defaults to prevent plan execution from getting stuck when the LLM omits these fields

## [0.2.83] - 2026-03-18

### Added
- **MiniMax M2.7 as default model** — Updated default model from M2.5 to M2.7 across config, pricing, onboarding, docs, and tests. M2.5 remains available as an option

## [0.2.82] - 2026-03-18

### Added
- **5 sub-agent orchestration tools** — Agents can now spawn independent child agents for parallel task execution. Five new tools: `spawn_agent`, `wait_agent`, `send_input`, `close_agent`, `resume_agent`. Children run in isolated sessions with auto-approve and essential tools (no recursive spawning)

### Fixed
- **"1 tok" streaming output counter** — Output token display reset to "1 tok" after each tool call because the callback accumulator was reset on every TokenCount event. Moved accumulation to TUI side so it persists across tool loop iterations
- **Cancelled requests leave tool calls unstacked** — Late ToolCallStarted/Completed/IntermediateText events arriving after double-Escape cancel now dropped via `is_processing` guard, preventing orphaned tool entries in chat
- **Pending request recovery missing cancel token** — Restarted tasks couldn't be cancelled because the recovery path passed no CancellationToken. Now wires token via new `PendingResumed` TUI event
- **Strip `<param>` tags** — Broaden tool artifact stripping to also remove `<param>` XML blocks
- **Strip `<tool_code>` and `<tool_call>` blocks** — XML tool-call markers now stripped from streaming and iteration text
- **Strip all HTML comments** — HTML comment stripping broadened to prevent marker leaks in LLM output

### Testing
- **11 new tests** for strip_html_comments tool artifact stripping

## [0.2.81] - 2026-03-17

### Fixed
- **Context blows past 200K limit** — `enforce_context_budget` now guarantees context never exceeds 80% of `max_tokens`. Hard-truncation fallback drops oldest messages
- **Segfault on large embeddings** — Documents >32KB now skipped with placeholder to prevent llama-cpp-2 segfault
- **Duplicate agent spawns on resume** — HashSet dedup prevents 4 concurrent tasks instead of 2
- **Thinking indicator vanishes during tools** — Removed `active_tool_group.is_none()` condition
- **Escape/cancel doesn't abort running tools** — Tool execution now races against cancel token via `tokio::select!`
- **Queued messages appear inline** — Messages now appear in conversation flow at exact point consumed
- **"1 tok" bogus context display** — Token calibration rejects results below 100 tokens

## [0.2.80] - 2026-03-16

### Fixed
- **`/model` provider navigation jumps out of order** — Up/Down keys now follow the visual display order (static providers → existing custom names → "+ New Custom") instead of raw index order, matching `/onboard:provider` behavior
- **Queued messages stack as duplicate user bubbles** — Messages sent while the agent is processing no longer appear as dimmed duplicates in chat. They stay in the input area preview until the tool loop consumes them, then appear naturally in the conversation flow

## [0.2.79] - 2026-03-15

### Fixed
- **Infinite retry loop from XML tool-call fallback** — The XML fallback (added in 0.2.77) created synthetic tool IDs that providers like MiniMax rejected with "tool id not found", triggering unstoppable retry loops that couldn't be cancelled. Removed the XML fallback entirely; XML tool_call blocks are now stripped from output
- **`/stop` only killed latest agent call** — `/stop` now cancels all in-flight agent calls instead of only the most recent one

## [0.2.78] - 2026-03-14

### Fixed
- **Crash on multi-byte UTF-8 in repetition detection** — `detect_text_repetition` panicked when slicing the sliding window at a byte offset inside multi-byte characters like `❌` (3 bytes) or `—` (em-dash, 3 bytes). Now advances to the nearest valid char boundary before slicing. Same fix applied to the window drain logic
- **`<!-- tools-v2: -->` markers leaking into Telegram/channel output** — LLM echoes back tool result markers from conversation context. The streaming filter handles them during SSE parsing, but split chunks could let them through. Now stripped from `iteration_text` in the tool loop before emission to channels
- **Test coverage** — 1,423 tests (up from 1,420). Added 3 UTF-8 regression tests for repetition detection

## [0.2.77] - 2026-03-14

### Added
- **XML tool-call fallback** — Providers that emit tool calls as XML text (e.g. MiniMax `<tool_call><invoke name="...">`) are now parsed and executed. XML blocks are stripped from persisted content so raw markup doesn't appear in chat history
- **Self-improving agent instructions** — BOOT.md brain template now includes self-improving directives. Splash screen and taglines updated
- **TUI render clear tests** — 4 tests for ratatui buffer clearing behavior
- **Test coverage** — 1,420 tests (up from 1,406)

### Fixed
- **TUI garbled characters on scroll** — Splash screen logo line overflowed the fixed-width ASCII box by 10 characters (73 vs 63 inner width), writing stale chars into ratatui's double buffer that bled through when scrolling. Fixed logo text to fit within box. Added `Clear` widget before Paragraph render to wipe the entire chat area each frame, preventing any stale buffer content from bleeding through during navigation
- **Removed 128KB stream response cap** — Hard limit on streaming text was removed. Repetition detection (2KB sliding window + 200-byte substring matching), stream idle timeout (60s), user cancellation (`/stop`), and provider-side `max_tokens` are sufficient to handle runaway streams without arbitrarily truncating legitimate long responses

### Changed
- **Splash taglines refined** — Removed duplicated terms, rearranged for clarity

> **Existing users:** After updating, ask your Crabs to check for brain template diffs and update your brain files (e.g. "check for brain template updates and apply them")

## [0.2.76] - 2026-03-13

### Added
- **Streaming text repetition detection** — Detects when providers (e.g. MiniMax) loop the same content indefinitely during streaming. Uses a 2KB sliding window with 200-byte substring matching to catch loops early and terminate the stream cleanly
- **Human-readable error messages** — Cryptic provider errors like "error decoding response body" are now translated to actionable messages suggesting retry or model switch
- **Stream loop detection tests** — 12 tests covering repetition detection, false positive prevention, edge cases, and error message translation
- **Test coverage** — 1,406 tests (up from 1,394)

## [0.2.75] - 2026-03-12

### Added
- **Post-evolve brain update prompt** — After `/evolve` restarts, Crabs announces the new version, diffs brain templates, and offers to update user's brain files
- **WhatsApp error reporting** — Agent errors (session store failures, connection issues) now broadcast to the TUI with specific error messages, log paths, and retry/reset instructions
- **QR render tests** — 8 tests for Unicode QR code rendering (width consistency, expected characters, quiet zone)
- **WhatsApp state tests** — 7 tests for broadcast channel behavior (QR, connected, error channels)
- **Post-evolve tests** — 5 tests for version comparison and evolve message format
- **Test coverage** — 1,394 tests (up from 1,373)
- **Autostart-on-boot instructions** — README now covers systemd (Linux), launchd (macOS), and Task Scheduler (Windows)

### Fixed
- **WhatsApp QR popup width on Windows** — Used `unicode_width::UnicodeWidthStr` instead of `str::len()` for correct display column calculation (3 bytes per Unicode block char was tripling the width)

### Changed
- **Assets consolidated** — Screenshots, icons, and scripts moved from root directories into `src/assets/` and `src/scripts/`
- **SocialCrabs docs expanded** — Full setup guide, natural language usage examples, and per-platform command reference in README
- **GitHub Actions Node.js 24** — Added `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24: true` to CI and release workflows

## [0.2.74] - 2026-03-12

### Added
- **Windows binary icon** — Embedded crab logo as application icon in Windows executables via `winresource` build script
- **Provider name normalization** — Channel commands now match provider names case-insensitively and resolve display names to config IDs (e.g. "GitHub Copilot" → "github-copilot"). Added GitHub Copilot to provider list — thanks @mariodian (#44)
- **Search tools documented in brain** — `brave_search` and `exa_search` now listed in TOOLS.md brain template so the LLM knows they exist when configured. Existing users: ask your Crabs to update its brain files

## [0.2.73] - 2026-03-12

### Added
- **Tool name normalization** — Providers that hallucinate tool names (e.g. MiniMax sending `"Plan: complete_task"` instead of `tool="plan"`) are now auto-corrected, preventing silent "Tool not found" failures
- **Test coverage** — 1,373 tests (up from 1,362). New: tool normalization (10), path traversal (2), custom brain file acceptance (1)

### Fixed
- **File tools restricted to working directory** — `read_file`, `write_file`, and `edit_file` now work with any absolute path on the system. Security is enforced by the approval mechanism, not a directory jail
- **Brain file allowlist too restrictive** — `load_brain_file` now accepts any `.md` file in `~/.opencrabs/`, not just a hardcoded list. User-created files like VOICE.md were silently rejected
- **`load_brain_file("all")` missed user files** — The "all" mode now scans the brain directory for user-created `.md` files in addition to built-in contextual files
- **Plan widget stuck after failed tool call** — If a plan tool call failed silently (e.g. hallucinated tool name), the plan widget stayed on screen indefinitely. Now auto-clears when the response completes and all tasks are done or the agent stops processing
- **Plan widget persists across restarts** — Stale InProgress plan files from previous runs no longer resurrect the plan widget on session load

### Changed
- GitHub Actions updated to Node.js 24 (`FORCE_JAVASCRIPT_ACTIONS_TO_NODE24: true`) ahead of June 2026 forced migration

> **⚠️ Note:** Any `.md` file placed in `~/.opencrabs/` root can now be loaded into brain context via `load_brain_file("all")` or by name. Avoid storing sensitive or non-brain files as `.md` in that directory.

## [0.2.72] - 2026-03-12

### Added
- **Collapse cargo build output** — Tool call details now collapse long `Compiling`/`Downloading`/`Fresh` blocks into single summary lines (e.g. "Compiled 100 crates")
- **Queued message preview** — Follow-up messages typed between tool calls now appear immediately in chat instead of waiting for tool completion
- **Mouse scroll wheel support** — Enable mouse capture for scroll wheel navigation in the TUI
- **CODE.md and SECURITY.md brain templates** — New brain templates seeded on install for coding standards and security patterns
- **Test coverage** — 1,362 tests (up from 1,286). New: provider navigation sync (8), brain templates (8), build output collapse (9), reasoning lines (6), AltGr input (8), system continuation (6), onboarding input helpers (23)

### Fixed
- **GitHub Copilot missing from provider resolution** — Copilot was added to TUI/config but not wired into `active_provider_and_model` candidates, so enabling it in config.toml silently fell through to the default provider
- **Provider navigation order with custom providers** — Custom providers (nvidia, opus, etc.) appeared between static providers visually but navigation jumped to wrong positions because internal index 6 ("+ New Custom") was between static and existing custom providers
- **Channel setup screens cropped on small terminals** — Channel list and all setup forms (Telegram, Discord, WhatsApp, Slack, Trello) now track focused field and scroll to keep it visible
- **Coming-soon channels cluttering the list** — Removed Signal, Google Chat, iMessage placeholders that couldn't be configured
- **Onboarding channel paste duplicating input** — Pasting a key appended to existing sentinel text instead of replacing it. Now uses cursor-aware paste with proper sentinel clearing
- **Windows non-US keyboard layouts** — Accept `/` and other characters that arrive via AltGr (Ctrl+Alt) on international keyboard layouts
- **Reasoning display losing newlines** — Preserve literal `\n` in reasoning/thinking streaming responses
- **Session model name desync** — Always sync display model name when switching sessions
- **Rebuild wake-up noise** — Hide internal rebuild wake-up message from chat history

### Docs
- Expanded README with full tool system documentation, CLI integrations, and companion tools
- Windows Defender troubleshooting section
- **CODE.md brain template updated** — Added problem-solving principles (never suppress errors, never give up on solutions, delete dead code)

> **Note for existing users:** This release adds new brain templates (CODE.md, SECURITY.md) and updates existing ones. If you installed OpenCrabs before v0.2.69, you may be missing these files. Ask your crab to check your brain templates and update them: *"Check my brain templates and update them if any are missing or outdated."*

## [0.2.71] - 2026-03-11

### Fixed
- **Streaming format loss on Copilot provider** — Newline-only stream deltas were dropped by a `.trim().is_empty()` check, stripping all markdown formatting from Copilot responses
- **Session restore on restart** — App now persists the last active session ID to `~/.opencrabs/last_session` and restores it on startup, instead of picking whichever session was most recently modified

## [0.2.70] - 2026-03-11

### Added
- **GitHub Copilot OAuth device flow** — Replaces the old GitHub Models PAT integration. Users authenticate via OAuth device flow (github.com/login/device), no PAT or GitHub CLI required. Automatic token refresh in the background. Works with any active Copilot subscription
- **Hard command blocklist for bash tool** — Catastrophic commands (rm -rf /, mkfs, dd on disks, etc.) are now blocked at the tool level before execution
- **Stable-first nightly-fallback for /evolve** — `cargo install` now tries stable toolchain first, falls back to nightly only if needed
- **Onboarding navigation improvements** — Shift+Tab moves backwards between fields, Ctrl+Backspace clears input, arrow keys navigate channel and provider setup screens
- **Test coverage** — 1,286 tests (up from 1,218). New: onboarding field navigation (36), Copilot provider (8), evolve tests (23), audio sanitization tests

### Fixed
- **keys.toml merge for GitHub provider** — OAuth tokens saved to keys.toml were never loaded back into config (pre-existing bug masked by old gh CLI fallback)

## [0.2.69] - 2026-03-11

### Added
- **GitHub Models provider** ([#41](https://github.com/adolfousier/opencrabs/issues/41)) — New provider with auto-detection via `gh auth token`. No API key needed for users already authenticated with the GitHub CLI. Supports GPT-4o, GPT-4.1, o3/o4-mini and all GitHub Models catalog
- **Custom provider management** — All `providers.custom.*` entries now appear as individual selectable items in both `/models` and `/onboard:providers`. Users can add unlimited custom providers (nvidia, ollama, lmstudio, etc.) and switch between them with a single keypress
- **Context window configuration** — New `context_window` field for custom/local providers in both UI screens and config.toml. Enables auto-compaction for models not recognized by name (e.g. local LLMs via LM Studio or Ollama)
- **Shift+Tab navigation** ([#43](https://github.com/adolfousier/opencrabs/issues/43)) — Move backwards between fields in all onboarding and setup screens. Shift+Tab reverses through fields, Escape goes back to the previous screen
- **CODE.md brain template** — Coding standards template for brain files: modular architecture, testing, security-first patterns
- **Test coverage** — 1,218 tests (up from 1,118). New: `context_window_test.rs` (14), `custom_provider_test.rs` (27), plus expanded voice onboarding, evolve, and file extract tests

### Fixed
- **`/models` crash on custom providers** — Index out of bounds panic when selecting existing custom providers (indices 7+) in the model selector dialog
- **Base URL corruption** — Switching between custom providers appended URLs instead of replacing them (e.g. `https://nvidia.com/v1http://127.0.0.1:1234`)
- **Provider index mapping** — Corrected index resolution for GitHub Models, model selection, and display names across onboarding and model selector
- **Session footer sync** — Provider/model details now update in the footer immediately after onboarding or model change
- **Onboarding quick-jump** — Shows provider/model details instead of generic "Settings saved" message
- **Factory hardening** — Provider factory never crashes on missing API keys; falls back gracefully through the provider chain
- **Custom provider list order** — Existing custom providers now appear before "+ New Custom Provider" button in both provider lists

### Docs
- Document nightly toolchain requirement for `cargo install` with system dependency instructions
- Add native TTS/STT comparison row to framework feature table

## [0.2.68] - 2026-03-10

### Added
- **Crash recovery dialog** — When the TUI crashes, a raw-terminal dialog lets users browse GitHub releases and roll back to older versions. Detects install method (pre-built binary, cargo install, source) and uses the correct upgrade strategy
- **Install method detection** — New `InstallMethod` enum (`Source`, `CargoInstall`, `PrebuiltBinary`) with runtime detection. Used by crash recovery and `/evolve`
- **Queued message preview** — Follow-up questions typed between tool calls now show a dimmed preview in the input box. Press Up to recall and edit before sending
- **Test coverage** — 1,118 tests (up from 1,089). New: queued message lifecycle (15), install method detection (6), OpenAI max_completion_tokens (4), evolve install-method dispatch (4)

### Fixed
- **OpenAI newer models error** (#40) — gpt-4.1-*, gpt-5-*, o1-*, o3-*, o4-* models reject `max_tokens`; now sends `max_completion_tokens` for these model families
- **TTS voice selection crash on missing python3 venv** (#39) — `local_tts_available()` now probes `python3 -c "import venv"` before offering local TTS. Error messages include platform-specific install instructions (e.g. `apt install python3-venv`)
- **Queued messages dropped between tool calls** — Messages queued during tool execution were flushed before tool groups completed, causing them to appear in the wrong position. Now flushed after tool group finalization
- **Evolve install-method awareness** — `/evolve` now dispatches to the correct upgrade path based on `InstallMethod::detect()`. Source builds suggest `/rebuild` instead of attempting binary download

## [0.2.67] - 2026-03-10

### Changed
- **Local STT engine: whisper-rs → rwhisper (candle)** — Replaced whisper-rs (ggml C) with rwhisper (candle-transformers, pure Rust). Eliminates ggml symbol conflicts with llama-cpp-sys-2 (issue #38). No C++ build dependency for whisper. Metal GPU acceleration on macOS
- **Panic strategy: abort → unwind** — Release builds now use `panic = "unwind"` so panics in rwhisper's internal threads produce backtraces instead of instant core dumps

### Added
- **Evolve health check + rollback** — `/evolve` now runs a health check on the new binary before swapping, backs up the current binary, and automatically rolls back if the new version fails post-swap verification
- **Runtime capability detection** — `local_stt_available()` (compile-time feature check) and `local_tts_available()` (runtime python3 probe, cached via OnceLock). Onboarding hides Local radio buttons when unavailable; mode cycling clamps to Off/API only
- **Wizard config reset** — `from_config()` resets saved Local STT/TTS mode to Off at load time if the capability is absent on the machine
- **Audio sanitization** — Scrub NaN/Inf from decoded audio, pad short audio to minimum 1s (16000 samples) to prevent candle tensor panics
- **Comprehensive test coverage** — 950 tests (up from ~840). New: evolve version comparison, audio sanitization, TTS/STT availability cycling, capability detection, wizard reset, codec support. Added `TESTING.md` with full documentation
- **TESTING.md** — Full test coverage documentation: 256+ tests across 12 modules with category breakdown

### Fixed
- **TUI broken on Linux (fd race)** — `suppress_stdout()` used `dup2` to redirect fd 1 during model loading, racing with TUI's `EnterAlternateScreen`. Removed process-wide fd redirection; background preload delayed 2s to start after alternate screen
- **Stdout bleed in /onboard:voice** — Restored `suppress_stdout()` as `pub(crate)` for `download_model()` and `LocalWhisper::new()` — safe since TUI is already in alternate screen
- **rwhisper crash on CPU Linux** — "illegal instruction core dumped" on older CPUs. Fixed via audio validation, padding, and panic=unwind
- **Local STT transcription timeout** — Added 300s timeout to prevent indefinite hangs
- **Typing indicator delay** — Show typing indicator immediately when processing voice messages
- **Removed unnecessary sandybridge rustflag** — Global `target-cpu=sandybridge` in `.cargo/config.toml` was unnecessary and spammed warnings on non-x86 platforms
- **TTS voice selection not persisting** — Enter on a downloaded Piper voice re-triggered download instead of advancing. Config was never saved because `apply_config()` was never reached
- **Linux CI missing ALSA dev** — `libasound2-dev` not installed on Ubuntu runners, breaking `--all-features` builds. Added to CI and release workflows including ARM64 cross-compile
- **Release workflow resilience** — Individual platform build failures no longer block the GitHub Release creation

### Docs
- Document `RUSTFLAGS="-C target-cpu=native"` for AVX1-only CPUs (Sandy Bridge) in README
- Add `local-stt` and `local-tts` to feature flags table in README

## [0.2.66] - 2026-03-09

### Fixed
- **Windows MSVC build** — Duplicate ggml symbols (LNK2005) from whisper-rs-sys and llama-cpp-sys-2 resolved with `/FORCE:MULTIPLE` linker flag. aws-lc-sys `__builtin_bswap` errors fixed by forcing CMake builder on Windows
- **TTS reads markdown literally** — Strip formatting markers (`**`, `` ` ``, ```` ``` ````, `#`, bullets) before sending text to Piper TTS. Code block content is kept and read aloud naturally
- **STT transcript cleanup** — Collapse whitespace in whisper transcription output
- **Single WhatsApp bot instance** — Onboarding subscribes to agent bot events via broadcast channels instead of creating a separate bot instance that conflicts with the agent

## [0.2.65] - 2026-03-09

### Added
- **Local TTS via Piper** — On-device text-to-speech using Piper (Python venv + ONNX voice models). Six voice presets (Ryan, Amy, Lessac, Kristin, Joe, Cori). Configurable via `tts_mode = "local"` and `local_tts_voice` in config.toml. Gated behind `local-tts` feature flag (enabled by default)
- **Off/API/Local mode for TTS** — TTS mode selector in `/onboard:voice` with three options: Off, API (OpenAI TTS), Local (Piper). Matches the existing STT mode selector
- **Voice preview after download** — Plays "Hey! I am {name}. Nice to meet you!" via system audio (afplay/aplay) after a Piper voice model downloads
- **WhatsApp session reset** — Press R on the WhatsApp onboarding screen to delete session.db and re-pair with a fresh QR code

### Fixed
- **Telegram voice waveform missing** — `pcm_to_opus` was producing WAV (RIFF header) instead of OGG/Opus. Now properly encodes via `opusic-sys` with OGG container (RFC 7845) and resamples Piper's 22050 Hz to 48000 Hz
- **Voice switching race condition** — `PiperDownloadProgress` events arriving after `PiperDownloadComplete` re-set progress, blocking re-download on voice switch
- **TTS config not persisted via quick-jump** — `/onboard:voice` quick-jump returned `WizardAction::Cancel` which dropped settings. New `QuickJumpDone` action calls `apply_config()` before closing
- **Piper venv never installed** — `setup_piper_venv()` was defined but never called before downloading voice models
- **Voice preview used wrong voice name** — `PiperDownloadComplete` event now carries the `voice_id` string instead of reading the wizard's selection index
- **Removed unnecessary `whisper-rs-sys` dependency** — Explicit dep removed; `whisper-rs` pulls it in transitively
- **Windows build failure** — Whisper log callback used wrong type (`u32` vs `ggml_log_level`) causing cross-platform compilation error
- **Release workflow duplicate test job** — Removed redundant test job from release.yml that was blocking releases since v0.2.60

## [0.2.64] - 2026-03-09

### Added
- **Local TTS via Piper** — On-device text-to-speech using Piper (Python venv + ONNX voice models). Six voice presets (Ryan, Amy, Lessac, Kristin, Joe, Cori). Configurable via `tts_mode = "local"` and `local_tts_voice` in config.toml. Gated behind `local-tts` feature flag (enabled by default)
  - `src/channels/voice/local_tts.rs` (new), `src/channels/voice/service.rs`, `src/channels/voice/mod.rs`, `Cargo.toml`
- **Off/API/Local mode for TTS** — TTS mode selector in `/onboard:voice` with three options: Off, API (OpenAI TTS), Local (Piper). Matches the existing STT mode selector
  - `src/tui/onboarding/voice.rs`, `src/tui/onboarding/types.rs`, `src/config/types.rs`
- **Voice preview after download** — Plays "Hey! I am {name}. Nice to meet you!" via system audio (afplay/aplay) after a Piper voice model downloads
  - `src/channels/voice/local_tts.rs`

### Fixed
- **Telegram voice waveform missing** — `pcm_to_opus` was producing WAV (RIFF header) instead of OGG/Opus. Telegram's `send_voice` API requires OGG/Opus to display the voice waveform. Now properly encodes via `opusic-sys` (already linked) with OGG container (RFC 7845) and resamples Piper's 22050 Hz to 48000 Hz. Zero new system dependencies
  - `src/channels/voice/local_tts.rs`, `Cargo.toml`
- **Voice switching race condition** — `PiperDownloadProgress` events arriving after `PiperDownloadComplete` re-set `tts_voice_download_progress` to `Some(0.0)`, blocking re-download on voice switch. Now ignores stale progress after completion and resets download state on voice navigation
  - `src/tui/app/state.rs`, `src/tui/onboarding/voice.rs`
- **TTS config not persisted via quick-jump** — `/onboard:voice` quick-jump returned `WizardAction::Cancel` which dropped settings without saving. New `QuickJumpDone` action calls `apply_config()` before closing
  - `src/tui/onboarding/types.rs`, `src/tui/onboarding/input.rs`, `src/tui/app/dialogs.rs`
- **Piper venv never installed** — `setup_piper_venv()` was defined but never called before downloading voice models. Added `pathvalidate` to pip install (required by piper-tts)
  - `src/tui/app/dialogs.rs`, `src/channels/voice/local_tts.rs`
- **Voice preview used wrong voice name** — `PiperDownloadComplete` event now carries the `voice_id` string instead of reading the wizard's selection index (which could change during async download)
  - `src/tui/events.rs`, `src/tui/app/state.rs`, `src/tui/app/dialogs.rs`
- **Removed unnecessary `whisper-rs-sys` dependency** — Explicit `whisper-rs-sys` dep removed; `whisper-rs` pulls it in as transitive dep. Log suppression now uses `whisper_rs::set_log_callback` instead of direct sys FFI
  - `Cargo.toml`, `src/channels/voice/local_whisper.rs`

## [0.2.63] - 2026-03-08

### Added
- **Local voice transcription (whisper.cpp)** — Full on-device speech-to-text via whisper.cpp behind the `local-stt` feature flag. Send voice notes on Telegram, WhatsApp, Discord, or Slack and get instant local transcription — zero API calls, zero latency, zero cost. Configurable via `stt_mode = "local"` and `local_stt_model` in config.toml. Supports tiny/base/small/medium models with automatic download via `/onboard:voice`. OGG/Opus decoding via `symphonia-adapter-libopus` for Telegram voice notes. STT dispatch routes between Groq Whisper API and local whisper.cpp based on config
  - `src/channels/voice/service.rs`, `src/channels/voice/local_whisper.rs`, `src/channels/voice/mod.rs`, `Cargo.toml`
- **Channel hot-reload** — Channels are now dynamically spawned/stopped when `channels.*.enabled` changes in config.toml at runtime. No restart needed. New `ChannelManager` reconciles running agents against config on every reload
  - `src/channels/manager.rs` (new), `src/channels/mod.rs`, `src/cli/ui.rs`
- **21 voice STT dispatch tests** — STT routing, config defaults, audio decode, codec registration, mock API dispatch
  - `src/tests/voice_stt_dispatch_test.rs` (new), `src/tests/mod.rs`

### Fixed
- **whisper.cpp TUI bleeding** — Suppressed whisper.cpp/ggml stderr output via no-op C log callback. Model loading and inference debug lines no longer bleed into the TUI
  - `src/channels/voice/local_whisper.rs`, `Cargo.toml` (`whisper-rs-sys` dependency)
- **Onboarding model fallback writing `"default"` to config** — `selected_model_name()` fell back to literal `"default"` when no models were loaded, which got written to config.toml and caused MiniMax API to reject all requests. Now returns empty string; caller skips write
  - `src/tui/onboarding/models.rs`
- **Voice setup test** — Updated `test_voice_setup_defaults` assertion to match new STT mode select as first field
  - `src/tui/onboarding/tests.rs`
- **Windows CI flaky test** — `test_concurrent_sessions_independent` used `tokio::join!` with in-memory SQLite causing contention on Windows. Runs sequentially now
  - `src/brain/agent/service/tests/parallel_sessions.rs`

## [0.2.62] - 2026-03-08

### Added
- **Provider sync across TUI and channels** — Model/provider switches now propagate to all agents (TUI, Telegram, Discord, Slack, WhatsApp) via config. Each channel syncs its provider on every message, and TUI syncs on config reload
  - `src/channels/commands.rs`, `src/tui/app/state.rs`, `src/config/types.rs`
- **Channel commands persist to session history** — `/help`, `/models`, `/usage`, `/sessions`, `/new`, `/stop` now save to session DB so they appear in TUI history and give the agent context
  - `src/channels/commands.rs`
- **Crate-level docs for docs.rs** — Rewritten landing page with current providers, channels, features, and architecture table. Added `rust-version = 1.91` (MSRV)
  - `src/lib.rs`, `src/main.rs`, `Cargo.toml`

### Fixed
- **sqlx → rusqlite upgrade path** — Auto-detect databases previously managed by sqlx (`_sqlx_migrations` table with `user_version=0`) and stamp migration version so existing databases don't fail on startup
  - `src/db/database.rs`
- **TUI model switch ordering** — Write `default_model` to config before `rebuild_agent_service()` so the provider loads the correct model instead of the stale one
  - `src/tui/app/dialogs.rs`
- **Channel model switch errors surfaced** — `switch_model` now returns errors to the user instead of silently dropping them. Model change is persisted to session history. Custom providers supported
  - `src/channels/commands.rs`, `src/channels/telegram/agent.rs`, `src/channels/discord/agent.rs`, `src/channels/slack/handler.rs`
- **`/models` hanging on OpenRouter/custom providers** — Added 10-second timeout on `fetch_models()`. Prefers config models (instant) over live fetch. Falls back to current model if fetch fails
  - `src/channels/commands.rs`
- **`/models` showing stale current model** — Provider picker now reads from config instead of the channel's separate (stale) AgentService instance
  - `src/channels/commands.rs`
- **slack_send blocks schema for OpenAI** — Added missing `items` field to `blocks` array schema. OpenAI strictly validates JSON schemas and rejects arrays without `items`. Closes #36
  - `src/brain/tools/slack_send.rs`
- **Flaky parallel_sessions test** — Fixed SQLite contention in concurrent write test by running sequentially (test validates provider isolation, not write concurrency)
  - `src/brain/agent/service/tests/parallel_sessions.rs`

### Removed
- **Stale model from channel prefix** — Removed `| Model: X` from all channel system instructions since channel agents have separate AgentService instances making it unreliable
  - `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`

## [0.2.61] - 2026-03-07

### Added
- **Cross-platform setup script** — `scripts/setup.sh` detects OS (macOS, Debian/Ubuntu, Fedora/RHEL, Arch, WSL) and installs all build dependencies (cmake, pkg-config, build tools) plus Rust nightly. One-liner: `bash <(curl -sL .../scripts/setup.sh)`
  - `scripts/setup.sh` (new)

### Fixed
- **Daily date-based config backup** — Config, keys, and commands files now use date-based backup filenames instead of overwriting a single backup

### Docs
- **Per-platform build prerequisites** — README now documents macOS (`brew install cmake pkg-config`), Fedora (`dnf`), and Arch (`pacman`) dependencies alongside existing Debian/Ubuntu instructions. Added one-liner setup reference
  - `README.md`

## [0.2.60] - 2026-03-07

### Added
- **A2A Send tool** — Agent-to-agent communication via A2A Protocol RC v1.0. Four actions: `discover` (fetch Agent Card), `send` (create task with message), `get` (poll task status), `cancel` (abort task). JSON-RPC 2.0 over HTTP with optional Bearer token auth
  - `src/brain/tools/a2a_send.rs` (new), `src/brain/tools/mod.rs`, `src/cli/ui.rs`
- **18 unit tests** for a2a_send — schema validation, approval logic, parameter validation, response text extraction, auth headers, Default impl
  - `src/brain/tools/a2a_send.rs`

### Fixed
- **Cron jobs spawn new sessions** — Cron scheduler now shares the TUI's active session via `Arc<Mutex<Option<Uuid>>>` instead of creating new sessions. Falls back to initial session, then most recent — never spawns new
  - `src/cron/scheduler.rs`, `src/cli/ui.rs`
- **`/compact` fails silently** — Compaction errors were logged but not shown to user. Now returns visible error message with troubleshooting hints
  - `src/brain/agent/service/tool_loop.rs`

### Improved
- **Channel context injection** — All channel handlers (Telegram, Discord, Slack, WhatsApp) now inject last 30 group messages as context before responding, so the agent stays aware of conversation flow
  - `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`
- **Telegram passive logging** — Voice, photo, and document messages in groups are now logged to `channel_messages` table after text extraction
  - `src/channels/telegram/handler.rs`
- **`/compact` on all channels** — Wired `ChannelCommand::Compact` to Telegram, Discord, Slack, and WhatsApp handlers
  - `src/channels/commands.rs`, `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`

### Docs
- **A2A README update** — Accurate A2A section with api_key config, message/stream endpoints, a2a_send tool docs, two-agent connection guide, Bearer auth examples, security & persistence notes
  - `README.md`

### Tests
- 6 unit tests for cron session resolution logic
  - `src/tests/cron_test.rs`

## [0.2.59] - 2026-03-07

### Added
- **Fallback provider chain** — Configure multiple fallback providers that are tried in sequence when the primary fails. Supports single (`provider = "openrouter"`) or array (`providers = ["openrouter", "anthropic"]`). Runtime retry wraps the primary provider transparently — no code changes needed downstream
  - `src/brain/provider/fallback.rs` (new), `src/brain/provider/factory.rs`, `src/config/types.rs`
- **Per-provider vision model** — Set `vision_model` in any provider config. The LLM calls `analyze_image` as a tool, which uses the vision model on the same provider API to describe images — giving any model vision capability without swapping the chat model. Falls back to Gemini vision when configured. MiniMax auto-injects `vision_model = "MiniMax-Text-01"` on first run
  - `src/brain/tools/provider_vision.rs` (new), `src/brain/provider/factory.rs`
- **Session working directory persistence** — `/cd` changes now persist to DB per session, restored on session switch. Shown as `~/path` badge in sessions screen
  - `src/db/models.rs`, `src/services/session.rs`, `src/tui/app/messaging.rs`, `src/tui/render/sessions.rs`, `src/migrations/20260307000001_add_session_working_dir.sql`
- **35 new tests** — Fallback chain config (9), runtime fallback behavior (10), vision model wiring (7), factory integration (4), active provider vision discovery (6)
  - `src/tests/fallback_vision_test.rs`

### Fixed
- **Update checker semver comparison** — Used string inequality instead of proper version comparison. Now uses `is_newer()` with lexicographic semver segments, and detects source builds via `source_cargo_version()`
  - `src/brain/tools/evolve.rs`
- **Home directory in TUI paths** — Footer and help screen showed full `/Users/username/...` paths. Now collapsed to `~/...`
  - `src/tui/render/input.rs`, `src/tui/render/help.rs`

### Docs
- **Fallback & vision docs** — Updated TOOLS.md, AGENTS.md, and BOOT.md templates with fallback provider config and vision_model documentation

> **Existing users:** Your local brain files at `~/.opencrabs/` are not updated automatically. Ask your Crab to fetch the latest templates from `src/docs/reference/templates/` and merge updates into your workspace brain files. New features: `[providers.fallback]` for provider chain failover, `vision_model` per provider. Also ask your Crab if you have image/vision setup in place — if not, it can help configure it. If you have multiple providers with API keys already set, your Crab can wire up fallback protection in config.toml for you.

## [0.2.58] - 2026-03-07

### Fixed
- **Vision images in OpenAI-compatible providers** — `ContentBlock::Image` was silently dropped because `OpenAIMessage.content` only supported strings. Changed to `serde_json::Value` to support polymorphic content (string or array with `image_url` parts). Fixes image/vision failures on Telegram and all channels
  - `src/brain/provider/custom_openai_compatible.rs`

### Docs
- **Image & file handling in brain templates** — Added `<<IMG:path>>` documentation to AGENTS.md and TOOLS.md templates so the agent knows how to handle incoming images from channels instead of hallucinating non-existent tools
  - `src/docs/reference/templates/AGENTS.md`, `src/docs/reference/templates/TOOLS.md`

> **Existing users:** Your local brain files at `~/.opencrabs/` are not updated automatically. Ask your Crab to compare templates at `src/docs/reference/templates/` against `~/.opencrabs/TOOLS.md` and `~/.opencrabs/AGENTS.md` and patch in the new image handling sections.

## [0.2.57] - 2026-03-07

### Added
- **Two-step `/models` flow** — `/models` now shows a provider picker first, then model picker for the selected provider. Works across Telegram (inline buttons), Discord (buttons), Slack (action buttons), and WhatsApp (plain text). Handles providers without `/models` endpoint via config fallback
  - `src/channels/commands.rs`, `src/channels/telegram/agent.rs`, `src/channels/telegram/handler.rs`, `src/channels/discord/agent.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`
- **`/new` and `/sessions` commands** — Create new sessions and switch between recent sessions from any channel. Inline buttons on Telegram/Discord/Slack, plain text on WhatsApp. Owner uses shared TUI session, non-owners get per-user sessions
  - `src/channels/commands.rs`, `src/channels/telegram/agent.rs`, `src/channels/telegram/handler.rs`, `src/channels/discord/agent.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`
- **User-defined slash commands on channels** — Custom commands from `commands.toml` (e.g. `/credits`) now work from Telegram, Discord, Slack, and WhatsApp. `action = "prompt"` forwards to the agent, `action = "system"` displays directly
  - `src/channels/commands.rs`, `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`
- **Custom commands in /help** — User-defined commands now appear in a "Custom Commands" section on both channel `/help` and the TUI help screen, sorted alphabetically with descriptions
  - `src/channels/commands.rs`, `src/tui/render/help.rs`
- **Emoji picker** — Type `:` followed by a shortcode to trigger an emoji autocomplete popup in the TUI. Arrow keys to navigate, Tab/Enter to insert, Esc to dismiss. Powered by the `emojis` crate
  - `src/tui/app/state.rs`, `src/tui/app/input.rs`, `src/tui/render/input.rs`, `src/tui/render/mod.rs`, `Cargo.toml` (`emojis = "0.8.0"`)
- **VOICE.md template** for voice configuration docs
  - `src/docs/reference/templates/VOICE.md`
- **"Why OpenCrabs?" README section** — security & binary size comparison vs Node.js frameworks
  - `README.md`

### Fixed
- **Context counter accuracy** — System brain tokens are now counted, and token counts no longer drop between requests
  - `src/brain/agent/service/builder.rs`, `src/brain/agent/service/context.rs`, `src/brain/agent/service/tool_loop.rs`, `src/tui/app/state.rs`
- **Stream bleed between sessions** — Streaming state is now cleared on session switch, preventing leftover content from appearing in a new session
  - `src/tui/app/messaging.rs`
- **Session switch confirmation shows name** — Channel callbacks now display the session title (e.g. "Chat") instead of a truncated UUID
  - `src/channels/telegram/agent.rs`, `src/channels/discord/agent.rs`, `src/channels/slack/handler.rs`

### Removed
- **HTTP gateway onboarding step** — Removed 339 lines of dead code. The gateway was inherited from OpenClaw's web UI design but never used; OpenCrabs runs via TUI/daemon and the A2A server handles external connections
  - `src/config/types.rs`, `src/tui/onboarding/` (7 files), `src/tui/onboarding_render.rs`, `src/tui/render/help.rs`, `src/brain/tools/config_tool.rs`

### Tests
- 277-line context tracking test suite for brain token counting
  - `src/brain/agent/service/tests/context_tracking.rs` (new)
- 14 unit tests for channel commands: `format_number`, `format_help`, `provider_display_name`, `match_user_command_inner`
  - `src/channels/commands.rs`

## [0.2.56] - 2026-03-06

### Added
- **Daily release check notification** -- Background task checks GitHub for new releases on startup (after 10s delay) and every 24 hours. When an update is available, shows a temporary system message in chat prompting the user to `/evolve`. Reuses the existing `check_for_update()` extracted from the evolve tool
  - `src/brain/tools/evolve.rs`, `src/tui/app/state.rs`

### Fixed
- **Context counter showing 243/200K when real usage was 19K** -- Two compounding bugs. First, MiniMax and OpenAI compatible providers send real usage in a final `choices: []` chunk after the `finish_reason` chunk. We emitted `MessageStop` on `finish_reason` and never captured the real token counts, falling back to tiktoken estimates. Second, the calibration formula subtracted `tool_count * 500` from input tokens to isolate message-only count. With ~38 tools that was 19,000 subtracted from 19,286 estimated input, leaving 286 tokens as the context count shown to the user
  - `src/brain/provider/custom_openai_compatible.rs`, `src/brain/agent/service/helpers.rs`, `src/brain/agent/service/tool_loop.rs`
- **Streaming stop_reason overwritten by deferred usage delta** -- When providers send a usage-only `MessageDelta` after the `finish_reason` chunk, `stop_reason` was overwritten with `None`. Now only updates `stop_reason` when the delta carries one
  - `src/brain/agent/service/helpers.rs`

### Tests
- 5 new streaming usage unit tests covering deferred vs inline usage patterns, tool calls with deferred usage, content preservation, and zero-start override
  - `src/brain/agent/service/tests/streaming_usage.rs` (new)

## [0.2.55] - 2026-03-06

### Added
- **Cumulative usage ledger** — New `usage_ledger` table tracks all token/cost usage permanently. Deleting or compacting sessions no longer resets usage stats. All-time totals in TUI, channel commands, and `/usage` tool now read from the ledger. Migration auto-backfills from existing sessions
  - `src/db/repository/usage_ledger.rs` (new), `src/migrations/20260306000001_add_usage_ledger.sql` (new), `src/services/session.rs`, `src/tui/render/dialogs.rs`, `src/tui/app/state.rs`, `src/brain/tools/slash_command.rs`, `src/channels/commands.rs`

### Fixed
- **Compaction overhaul — zero-truncation, DB persistence, exhaustive summaries** (closes #29) — Complete rewrite of the compaction system. Context is NEVER truncated before summarization — the full conversation reaches the LLM. Compaction prompt expanded to 10-section exhaustive format (chronological analysis, code snippets, user preferences with exact quotes, recovery playbook with `gh` CLI, personalized continuation message). Manual `/compact` now calls the real compaction pipeline instead of faking it. Compaction markers persist to DB so restarts load only from the last compaction point forward. All 5 compaction paths (manual, pre-loop, mid-loop x2, emergency) now persist markers. 24 compaction tests
  - `src/brain/agent/service/context.rs`, `src/brain/agent/service/tool_loop.rs`, `src/brain/agent/context.rs`, `src/tests/compaction_test.rs`, `src/tui/app/messaging.rs`, `src/docs/reference/templates/AGENTS.md`
- **TUI context counter wrong after restart** — After compacting and restarting, the TUI showed 200K/200K because `load_session` counted ALL DB messages instead of only post-compaction ones. Now filters through `messages_from_last_compaction` to match what the agent actually sees
  - `src/tui/app/messaging.rs`, `src/brain/agent/service/context.rs`
- **`/compact` placeholder visible in chat** — The internal `[SYSTEM: Compact context now...]` trigger message no longer shows as a user message in chat. The TUI already displays a "Compacting context..." system message
  - `src/tui/app/messaging.rs`
- **Metal destructor crash on macOS exit** — Replaced `std::process::exit` with `libc::_exit` to skip C atexit handlers that trigger llama.cpp's Metal GPU device destructor assertion on Apple Silicon. Clean exit, no more backtrace spam
  - `src/main.rs`, `Cargo.toml` (`libc = "0.2.182"`)
- **Empty context sent to compaction summarizer** — Fixed `trim_to_target` gutting context before the summarizer saw it. Removed dead method entirely
  - `src/brain/agent/context.rs`, `src/brain/agent/service/tool_loop.rs`, `src/tests/compaction_test.rs`

### Upgrade Notes
> **Existing users:** Update your brain files to get the new compaction behavior docs:
> ```sh
> cp src/docs/reference/templates/AGENTS.md ~/.opencrabs/AGENTS.md
> ```
> Or ask your agent: *"Update AGENTS.md from the repo template"*
>
> The `usage_ledger` migration runs automatically on first start — your existing session usage is backfilled so no data is lost.
>
> **Always check brain files for updates after upgrading** — templates evolve with each release and your local copies may be outdated.

## [0.2.54] - 2026-03-06

### Added
- **`/evolve` — binary self-update from GitHub releases** — New tool and slash command that checks the latest GitHub release, downloads the platform-specific binary, atomically replaces the current executable, and exec()-restarts into the new version. No Rust toolchain required. Fallback to legacy asset naming for backward compatibility with older releases. Available as `/evolve` slash command, `evolve` agent tool, and in the command palette
  - `src/brain/tools/evolve.rs` (new), `src/brain/tools/mod.rs`, `src/cli/ui.rs`, `src/tui/app/messaging.rs`, `src/tui/app/state.rs`, `src/tui/render/help.rs`, `src/brain/tools/slash_command.rs`, `Cargo.toml` (`flate2`, `tar`)
- **Versioned release assets** — CI now produces assets named `opencrabs-v{version}-{platform}.tar.gz` (e.g. `opencrabs-v0.2.54-macos-arm64.tar.gz`) instead of versionless names, making downloads unambiguous
  - `.github/workflows/release.yml`

### Fixed
- **Smarter post-compaction brain recovery** — Instead of blindly loading all brain files after compaction (which bloated context), the agent now receives a pre-compaction snapshot of the last 8 messages alongside the summary. This lets it analyze what context it needs and selectively load only relevant brain files. 22 new end-to-end compaction tests
  - `src/brain/agent/service/context.rs`, `src/brain/agent/service/tool_loop.rs`, `src/tests/compaction_test.rs` (new), `src/tests/mod.rs`

### Upgrade Notes
> **Existing users:** Update your brain files to include `/evolve` tool docs:
> ```sh
> cp src/docs/reference/templates/TOOLS.md ~/.opencrabs/TOOLS.md
> cp src/docs/reference/templates/AGENTS.md ~/.opencrabs/AGENTS.md
> ```
> Or ask your agent: *"Update TOOLS.md and AGENTS.md from the repo templates"*
>
> **Future updates:** After this version, just type `/evolve` to update — no manual steps needed.

## [0.2.53] - 2026-03-05

### Added
- **Cron jobs — full production implementation** (`ae79eee`) (closes #28) — Background `CronScheduler` polls DB every 60s, executes due jobs in isolated agent sessions with configurable provider/model/thinking. CLI subcommands: `cron add/list/remove/enable/disable` with name/UUID resolution. `CronManageTool` agent tool (5 actions: create/list/delete/enable/disable) with approval gates on create/delete. Telegram delivery via Bot API HTTP POST, Discord/Slack logged only. 43 tests covering CLI parsing, repository CRUD, cron expression validation, scheduler logic, and agent tool operations
  - `src/cron/mod.rs` (new), `src/cron/scheduler.rs` (new), `src/brain/tools/cron_manage.rs` (new), `src/cli/cron.rs` (new), `src/tests/cron_test.rs` (new), `src/brain/tools/mod.rs`, `src/cli/mod.rs`, `src/cli/ui.rs`, `src/lib.rs`
- **Passive message capture for Discord, Slack, and WhatsApp** (`027377a`) — All non-directed group messages are now stored in `channel_messages` table for `channel_search` tool access. Previously only Telegram captured messages. Discord captures at allowed_channels/dm_only/mention drop points and directed messages. Slack captures at the same drop points. WhatsApp captures all text messages after content extraction. Connect tools updated to pass `ChannelMessageRepository`. 24 tests for channel_search repository and tool operations
  - `src/channels/discord/handler.rs`, `src/channels/discord/agent.rs`, `src/channels/slack/handler.rs`, `src/channels/slack/agent.rs`, `src/channels/whatsapp/handler.rs`, `src/channels/whatsapp/agent.rs`, `src/brain/tools/discord_connect.rs`, `src/brain/tools/slack_connect.rs`, `src/brain/tools/whatsapp_connect.rs`, `src/cli/ui.rs`, `src/tests/channel_search_test.rs` (new)

### Fixed
- **Agent loses brain context after compaction** (`21c119e`) (closes #27) — After auto-compaction, the agent no longer reloads brain files (SOUL.md, AGENTS.md, USER.md, TOOLS.md) and answers without its identity, capabilities, or user preferences. Post-compaction instruction now mandates calling `load_brain_file` with `name="all"` as the first action before continuing the task
  - `src/brain/agent/service/tool_loop.rs`

### Upgrade Notes
> **Existing users:** Your local brain files at `~/.opencrabs/` are not auto-updated. To get the latest `cron_manage` tool docs and `channel_search` guidance, update your brain files from the repo templates:
> ```sh
> cp src/docs/reference/templates/TOOLS.md ~/.opencrabs/TOOLS.md
> cp src/docs/reference/templates/AGENTS.md ~/.opencrabs/AGENTS.md
> ```
> Or ask your agent: *"Update TOOLS.md and AGENTS.md from the repo templates"* — it can use `write_opencrabs_file` to do it for you.

## [0.2.52] - 2026-03-05

### Added
- **Reply-to-message context across all channels** (closes #26) — When a user replies to a specific message, the agent now receives the quoted message text and sender as context. Previously the agent had no way to know what message was being referenced
  - **Telegram** (`c1f51be`) — Extracts `reply_to_message()` text and sender. Bot replies labeled "assistant", user replies show sender name
  - **Discord** (`26dc53e`) — Extracts `referenced_message` content and author. Bot replies labeled "assistant"
  - **Slack** (`b00c8bb`) — Detects thread replies via `thread_ts`. Slack events don't embed parent message text, so thread context is noted without parent content
  - **WhatsApp** (`00d4e02`) — Extracts quoted message from `ExtendedTextMessage` context_info. Sender shown as phone number from participant JID
- **Cron jobs DB layer** (`43e7448`) — New `cron_jobs` table migration, `CronJob` model with full scheduling fields (cron expression, timezone, provider, model, thinking, auto_approve, deliver_to), `CronJobRepository` with insert/list/find/delete/enable/disable/update_last_run. Foundation for scheduled isolated sessions via CLI or agent `cron_manage` tool
  - `src/migrations/20260305000002_add_cron_jobs.sql` (new), `src/db/repository/cron_job.rs` (new), `src/db/models.rs`, `src/db/repository/mod.rs`, `Cargo.toml` (`cron = "0.15"`)

### Fixed
- **Native text selection restored** (`6962572`) — Disabled mouse capture that was blocking terminal text selection. Users can now select and copy text normally with left-click drag + keyboard copy
- **API key look-alike in test fixture** (`eb0b3f2`) — Replaced realistic-looking Google API key pattern in sanitize test with clear fake placeholder to avoid false positive leak alerts

### Improved
- **Brain templates updated** (`3e5033b`, `a590fce`, `43e7448`) — TOOLS.md template: `telegram_send` 16→19 actions (`get_chat_administrators`, `get_chat_member_count`, `get_chat_member`), added `channel_search` tool with `list_chats`/`recent`/`search` operations, empty state guidance for agents, `cron_manage` tool (5 actions: create/list/delete/enable/disable), system CLI tools reference (gh, gog, docker, ssh, node, etc.) with full gh and gog command docs. Updated `commands.toml.example` with `/chats` and `/history` example commands
- **README.md** (`43e7448`) — Added "Cron Jobs & Heartbeats" section with CLI examples, agent tool description, options table, HEARTBEAT.md usage, and heartbeat vs cron comparison

## [0.2.51] - 2026-03-05

### Added
- **Telegram message history capture and search** (`20c6008`) — Passive capture of Telegram group messages into new `channel_messages` table. New `channel_search` tool with `list_chats`, `recent`, and `search` operations. Telegram Bot API cannot fetch history, so the handler stores all group messages (directed and non-directed) as they arrive for on-demand retrieval. New migration, `ChannelMessageRepository`, and `ChannelMessage` model. Discord/Slack already have API-based history fetching via existing tools
  - `src/migrations/20260305000001_add_channel_messages.sql` (new), `src/db/repository/channel_message.rs` (new), `src/db/models.rs`, `src/db/mod.rs`, `src/brain/tools/channel_search.rs` (new), `src/brain/tools/mod.rs`, `src/channels/telegram/handler.rs`, `src/channels/telegram/agent.rs`, `src/brain/tools/telegram_connect.rs`, `src/cli/ui.rs`
- **Telegram chat and member info** (`20c6008`) — `get_chat` (chat details), `get_chat_administrators` (admin list with roles), `get_chat_member_count`, `get_chat_member` (user status lookup). Agent previously had no way to query Telegram chats or members. 19 telegram_send actions total
  - `src/brain/tools/telegram_send.rs`
- **Click-to-select and right-click-to-copy messages** (`5498970`, `d7fe7d7`) — Left-click highlights a message with subtle background, right-click copies clean content to clipboard via `pbcopy`/`xclip`/`xsel` with 2s cyan notification. Separate notification system from error messages. Line-to-message mapping built during render for coordinate lookup
  - `src/tui/app/state.rs`, `src/tui/app/input.rs`, `src/tui/events.rs`, `src/tui/render/chat.rs`, `src/tui/runner.rs`
- **Vim-style cross-platform input bindings** (`fbb92ad`, `de05b5c`, `8309804`) — `Ctrl+J` (newline), `Ctrl+W` (delete word), `Ctrl+U` (delete to line start). macOS Option key doesn't send ALT modifier in terminals, so Alt+Enter/Alt+Backspace never worked — vim bindings are the reliable cross-platform alternative. Crossterm `DISAMBIGUATE_ESCAPE_CODES` keyboard enhancement. Comprehensive delete-word key matching across terminal encodings (Backspace+modifiers, DEL `0x7f`, raw Ctrl+H/W). Up/Down arrows jump to start/end of line before entering history on single-line input. Home/End and Ctrl+U are line-aware in multiline
  - `src/tui/app/state.rs`, `src/tui/app/input.rs`, `src/tui/events.rs`, `src/tui/runner.rs`
- **Detailed Telegram logging** (`255a293`) — Verbose tracing for group/channel interactions to diagnose message routing
  - `src/channels/telegram/handler.rs`

### Fixed
- **Context display showed raw API tokens including tool schema overhead** (`2532b51`) — `AgentResponse.context_tokens` now uses calibrated `context.token_count` (message-only) instead of raw API `input_tokens` which included ~22k tool schema overhead for 44 tools. Display no longer shows inflated 210k/200k
  - `src/brain/agent/service/tool_loop.rs`, `src/brain/agent/service/tests/basic.rs`
- **Owner detection used HashSet random iteration order** (`89ae548`) — `HashSet::iter().next()` is non-deterministic, causing the wrong user to be identified as owner. Fixed to use `tg_cfg.allowed_users.first()` (Vec order from config = deterministic, first entry = owner)
  - `src/channels/telegram/handler.rs`
- **Forward TokenCount events to TUI during channel interactions** (`b98027a`) — TUI now receives real-time token count updates when messages arrive via Telegram/Discord/Slack/WhatsApp
  - `src/channels/telegram/handler.rs`
- **Restore most recent session from DB on daemon restart** (`c423410`) — Daemon no longer starts with a blank session after restart
  - `src/cli/ui.rs`

### Improved
- **Help screen colors** (`eb10a96`) — Orange section titles, cyan command keys matching TUI theme. Added INPUT EDITING section documenting all keybindings
  - `src/tui/render/help.rs`
- **README keyboard shortcuts** — Updated with vim bindings, mouse actions, multiline navigation
  - `README.md`

## [0.2.50] - 2026-03-04

### Changed
- **Config hot-reload via `watch` channel** — Replaced per-channel `Mutex` copies of config (allowlists, voice, respond_to, idle_timeout) with a single `tokio::sync::watch<Config>` channel. All channels now read the latest config per-message from the watch receiver. Removed `allowed_users`/`allowed_phones` HashSet fields from all channel states and 4 separate allowlist callbacks in `ui.rs`
  - `src/channels/factory.rs`, `src/channels/{telegram,discord,slack,whatsapp}/{mod,agent,handler}.rs`, `src/cli/ui.rs`, `src/brain/tools/{telegram,discord,slack,whatsapp}_connect.rs`, `src/brain/tools/whatsapp_send.rs`
- **TTS voice/model read from `[providers.tts]`** — Added `voice` and `model` fields to `TtsProviders` so `voice = "echo"` under `[providers.tts]` is actually picked up. Previously serde silently ignored the field
  - `src/config/types.rs`, `src/channels/factory.rs`, `src/channels/{telegram,discord,slack,whatsapp}/handler.rs`
- **Default TTS voice changed from "ash" to "echo"**; both `stt_enabled` and `tts_enabled` now default to `false` (user must opt in)
  - `src/config/types.rs`

### Fixed
- **Telegram "Session not found" after TUI quit** — The retry logic checked for `"SessionNotFound"` (camelCase) but the error Display produces `"Session not found"` (lowercase), so recovery never triggered. Now correctly matches and creates a fresh session (closes #24)
  - `src/channels/telegram/handler.rs`
- **Duplicate data delivery on Telegram** — LLM sent data both as streaming text response AND via `telegram_send`, resulting in the same content appearing twice. Added channel context prefix to all handlers telling the LLM its text response is auto-delivered (closes #23)
  - `src/channels/{telegram,discord,slack,whatsapp}/handler.rs`
- **Groq API key in test fixture** — Replaced real-format Groq key in `redact_secrets` test with obviously fake placeholder (closes #25)
  - `src/utils/sanitize.rs`

## [0.2.49] - 2026-03-04

### Added
- **Channel commands (`/help`, `/usage`, `/models`, `/stop`)** — All four commands now work on Telegram, Discord, Slack, and WhatsApp. Shared `commands.rs` module handles parsing; each channel renders platform-native responses (inline keyboards, action rows, Block Kit buttons)
  - `src/channels/commands.rs` (new), `src/channels/mod.rs`, `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`
- **`/stop` cancels running agent on channels** — `CancellationToken` per session, equivalent to double-Escape in TUI. Immediately aborts streaming/tool loop mid-run
  - `src/channels/telegram/mod.rs`, `src/channels/discord/mod.rs`, `src/channels/slack/mod.rs`, `src/channels/whatsapp/mod.rs`, all handler files
- **`/models` interactive model switching on channels** — Platform-native buttons (Telegram `InlineKeyboardMarkup`, Discord `ActionRow`, Slack Block Kit) with `model:` callback handlers
  - `src/channels/telegram/agent.rs`, `src/channels/discord/agent.rs`, `src/channels/slack/handler.rs`
- **Agent `slash_command` tool returns real data** — `/models`, `/usage`, `/help`, `/doctor`, `/sessions` now execute and return actual context instead of "TUI-only" errors, enabling the agent to read config, check health, and switch models via `config_manager`
  - `src/brain/tools/slash_command.rs`, `src/brain/tools/trait.rs`, `src/brain/agent/service/tool_loop.rs`
- **`service_context` on `ToolExecutionContext`** — Tools can now access `ServiceContext` for DB queries (used by `/usage` and `/sessions`)

### Fixed
- **Image API key stored under wrong path** — Onboarding wrote to flat `[image]` section in keys.toml instead of `[providers.image.gemini]`, inconsistent with all other provider keys. Added `ImageProviders` struct, merge logic, and legacy fallback
  - `src/config/types.rs`, `src/tui/onboarding/config.rs`
- **Channel commands section in README** — Documented `/help`, `/usage`, `/models`, `/stop` for all channels including WhatsApp
- **`keys.toml` parse errors now surface visibly** — Invalid TOML (e.g. unquoted emails) previously caused silent key merge failure, breaking provider startup with no error. Now prints warning to stderr and logs error. `/doctor` validates keys.toml syntax
  - `src/config/types.rs`, `src/brain/provider/factory.rs`, `src/brain/tools/slash_command.rs`

### Improved
- **Telegram tool calls as individual messages** — Each tool call now gets its own message (context + result) instead of all tools stacked in the response. Response streams cleanly at the bottom
  - `src/channels/telegram/handler.rs`
- **Intermediate agent texts visible on Telegram** — Agent commentary between tool rounds (e.g. "Found one! Let me reply to this:") now appears as individual messages, matching TUI behavior
  - `src/channels/telegram/handler.rs`

## [0.2.48] - 2026-03-04

### Added
- **Telegram thinking/reasoning stream** — Live `💭` reasoning content streams during inference, vanishes on tool calls and response chunks, keeping the conversation clean
  - `src/channels/telegram/handler.rs`
- **`quick_jump` mode for `/onboard:<step>` deep-links** — Any `/onboard:step` (except ModeSelect) opens locked to that single step: no progress dots, centered title, Enter confirms, Esc exits to chat. Step-change detection reverts navigation attempts
  - `src/tui/onboarding/wizard.rs`, `src/tui/onboarding/input.rs`, `src/tui/app/messaging.rs`, `src/tui/onboarding_render.rs`
- **Deferred health re-check on Enter** — In `/doctor` quick_jump mode, Enter resets all checks to Pending (visible flash), tick resolves them next frame. Reloads config from disk so external changes are picked up
  - `src/tui/onboarding/config.rs`, `src/tui/onboarding/fetch.rs`, `src/tui/app/state.rs`
- **YOLO (permanent) approval button on all channels** — Telegram, Discord, Slack, and WhatsApp now offer a 🔥 YOLO button alongside Always (session), persisting `auto-always` to config.toml so approval survives restarts
  - `src/channels/telegram/handler.rs`, `src/channels/telegram/agent.rs`, `src/channels/discord/handler.rs`, `src/channels/discord/agent.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`, `src/channels/whatsapp/mod.rs`, `src/utils/approval.rs`, `src/utils/mod.rs`

### Fixed
- **Redundant `check_approval_policy()` in tool loop** — Removed config-level short-circuit that was bypassing per-tool approval logic, fixing 3 approval policy test failures on CI
  - `src/brain/agent/service/tool_loop.rs`
- **CI and Release workflows running redundantly on tag push** — Added `tags-ignore: v*` to CI, added test gate (`needs: test`) to Release workflow
  - `.github/workflows/ci.yml`, `.github/workflows/release.yml`
- **`/doctor` standalone mode (closes #21)** — No onboarding chrome, Enter/Esc exit, removed redundant `/onboard:health` command
  - `src/tui/onboarding_render.rs`, `src/tui/app/state.rs`, `src/tui/render/help.rs`
- **Trello API Token not loaded in `from_config()`** — Health check falsely reported "No API Token provided" even when configured
  - `src/tui/onboarding/wizard.rs`
- **Model selector filter not working (closes #20)** — Filter text was typed but never applied to the displayed model list
  - `src/tui/render/dialogs.rs`
- **UTF-8 crash on multi-byte text in all channels** — 12 unsafe byte-index string slices replaced with `truncate_str()`, fixing panic on accented/emoji characters (e.g. Portuguese `õ`)
  - `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/whatsapp/handler.rs`
- **`quick_jump` blocking all step navigation** — Guard was catching internal step changes (field switching, channel sub-steps), not just step completion. Moved guard into `next_step()` so only step completion exits in deep-link mode
  - `src/tui/onboarding/input.rs`, `src/tui/onboarding/navigation.rs`, `src/tui/onboarding/wizard.rs`
- **Approval policy not persisting from channels** — Channels only offered "Always (session)" which wrote `auto-session`, downgrading the default YOLO policy. Now properly offers both session and permanent options
- **Updated README and commands.toml.example** with all `/onboard:*` sub-commands, `/doctor`, `/whisper`
  - `README.md`, `commands.toml.example`

## [0.2.47] - 2026-03-03

### Changed
- **Centralized tool approval into shared `utils::approval` module** — Replaced per-channel `auto_approve_session: Mutex<bool>` fields in Discord, Slack, Telegram, and WhatsApp with a single config-driven source of truth. Two new functions (`check_approval_policy`, `persist_auto_session_policy`) read/write `config.toml` directly, and the core `tool_loop.rs` checks policy first before delegating to any channel callback. Approval callbacks moved from `mod.rs` to `handler.rs` as free functions across all channels
  - `src/utils/approval.rs` (new), `src/utils/mod.rs`, `src/brain/agent/service/tool_loop.rs`, `src/channels/discord/mod.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/mod.rs`, `src/channels/slack/handler.rs`, `src/channels/telegram/mod.rs`, `src/channels/telegram/handler.rs`, `src/channels/whatsapp/mod.rs`, `src/channels/whatsapp/handler.rs`, `src/channels/trello/handler.rs`

### Fixed
- **Telegram streaming message stuck at top between tool calls** — Streaming now uses separate `tools` and `response` fields with a `recreate` flag that deletes the old message and creates a fresh one below the approval buttons after each tool completion, so the conversation flows naturally downward instead of getting stuck above approval messages. Thanks @opryshok for reporting #17 and #16 — your bug reports directly drove this fix and the v0.2.46 improvements
  - `src/channels/telegram/handler.rs`, `src/channels/telegram/agent.rs`
- **Race condition in approval registration across all channels** — Pending approval is now registered BEFORE sending the approval message (not after), preventing a window where the user could click before the handler was ready
  - `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/telegram/handler.rs`
- **TUI "Always" approval choice not persisting** — Clicking "AllowAlways" in the TUI now writes `approval_policy = "auto-session"` to `config.toml` so the choice survives restarts and is respected by all channels
  - `src/tui/app/input.rs`, `src/tui/app/messaging.rs`, `src/tui/app/state.rs`

### Added
- **Tracing/logging across all channel approval flows** — Every approval request, response, and edge case now logs via `tracing::info!` / `tracing::warn!` for easier debugging
  - `src/channels/discord/agent.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/telegram/agent.rs`, `src/channels/telegram/handler.rs`, `src/channels/whatsapp/handler.rs`
- **Cross-channel approval awareness** — TUI reads approval policy from `config.toml` on session create/load, so a policy set via Telegram or any other channel is picked up everywhere
  - `src/tui/app/messaging.rs`, `src/tui/app/state.rs`

## [0.2.46] - 2026-03-03

### Fixed
- **Telegram tool approval stuck after clicking Yes/Always** (`3716bf9`) — Three root causes fixed: (1) `ApprovalCallback` now returns `(bool, bool)` where the second bool propagates "Always" back into `tool_context.auto_approve`, so it persists across the entire tool loop instead of resetting after a few steps. (2) Race condition: pending approval is registered BEFORE sending the message, not after. (3) Tool input truncated to 3500 chars to prevent silent Telegram API rejection on long inputs. Closes #17
  - `src/brain/agent/service/types.rs`, `src/brain/agent/service/tool_loop.rs`, `src/channels/telegram/mod.rs`, `src/channels/discord/mod.rs`, `src/channels/slack/mod.rs`, `src/channels/whatsapp/handler.rs`, `src/tui/app/state.rs`, `src/cli/ui.rs`
- **Telegram missing tool call context and formatting** (`3716bf9`) — `ToolCompleted` events were being dropped by the progress callback; tool indicators used unsupported markdown. Now shows tool start/completion with proper labels. Improved `markdown_to_telegram_html` with headers, links, lists, underscore italic, and strikethrough. Closes #16
  - `src/channels/telegram/handler.rs`
- **Multiline Up/Down arrow keys never navigated lines** (`e27f59b`) — The multiline branch consumed all Up/Down events when the buffer contained newlines, even at boundaries (cursor at position 0 or end), blocking fall-through to history navigation. Now yields at boundaries: Up at position 0 falls through to history, Down at end of buffer does nothing
  - `src/tui/app/input.rs`
- **Light mode unreadable — user messages and UI text invisible on light terminals** (`009e8e3`) — Removed hardcoded dark user message background `Rgb(30,30,38)`. Replaced `Color::White` (invisible on light backgrounds) with `Color::Reset` (terminal's default foreground) across all render files. Diff backgrounds changed from dark RGB to ANSI named colors (Green/Red/DarkGray) that adapt to both themes
  - `src/tui/render/chat.rs`, `src/tui/render/input.rs`, `src/tui/render/help.rs`, `src/tui/render/dialogs.rs`, `src/tui/render/sessions.rs`, `src/tui/render/tools.rs`
- **Streaming token count duplicated ctx counter in input bar** (`2f8bc09`) — The per-chunk tiktoken counter was adding `ctx + output` together and feeding it back into `last_input_tokens`, making the tool group display show the same "28K ctx" already in the input bar, plus a duplicate timer. Now output tokens are tracked separately and displayed as a per-response count next to the timer: `(7s · 42 tok)`. The duplicate ctx+timer block below tool groups is removed
  - `src/tui/events.rs`, `src/tui/app/state.rs`, `src/tui/app/messaging.rs`, `src/tui/render/chat.rs`, `src/cli/ui.rs`

### Changed
- **Removed auto-backup logic** (`e142698`) — Git handles versioning; the custom backup mechanism was redundant

## [0.2.45] - 2026-03-03

### Added
- **Real-time token count during streaming** (`65a0278`) — The context usage display in the input box now increments live as the model responds: each streaming chunk is counted via tiktoken (cl100k_base) and fires a `TokenCountUpdated` event, so the counter ticks up token by token (e.g. `45K → 45.1K → 45.3K`) instead of jumping at the end of each API round-trip. The API-reported real count resets the baseline after each response, keeping the display accurate across multi-tool loops
  - `src/cli/ui.rs`
- **Elapsed time + ctx in thinking indicator** (`65a0278`) — The "OpenCrabs is thinking..." spinner now shows elapsed seconds and current context size: `⠙ OpenCrabs is thinking... 3s · 45K ctx`
  - `src/tui/render/mod.rs`
- **Running token count below active tool groups** (`65a0278`) — While tool calls execute, a subtle `45K ctx · 3s` line is rendered below the live tool group so you can see context growth during multi-tool sequences
  - `src/tui/render/chat.rs`
- **`opencrabs daemon` command** (`be61993`) — New headless subcommand: same full channel setup (Telegram, Discord, Slack, WhatsApp) as the TUI, but no terminal UI. Blocks on Ctrl-C. Designed for use by the systemd/LaunchAgent service installed during onboarding. Fixes the daemon not working after `opencrabs init` (issue #12)
  - `src/cli/mod.rs`, `src/cli/ui.rs`
- **28 CLI parsing unit tests** (`be61993`) — Full test coverage for all CLI subcommands including the new `daemon` command. Wired into `lib.rs` under `#[cfg(test)]`
  - `src/tests/cli_test.rs`, `src/tests/mod.rs`, `src/lib.rs`
- **Hot-reload for all three config files** (`1675fd2`) — `config_watcher` now watches `config.toml`, `keys.toml`, and `commands.toml`. Changing any of them is picked up within ~300ms without restart. Provider is swapped live when keys change (via `AgentService::swap_provider`). TUI refreshes approval policy and slash commands on reload
  - `src/utils/config_watcher.rs`, `src/cli/ui.rs`, `src/tui/app/state.rs`
- **`config.toml` and `commands.toml` annotated examples** (`4fdc1a6`) — Full annotated `config.toml` example added to the README Configuration section. New `commands.toml` section with complete syntax and action types reference. New `commands.toml.example` file in the project root matching the style of `keys.toml.example`. Two new Table of Contents entries added
  - `README.md`, `commands.toml.example` (new)

### Fixed
- **Daemon service not starting after install** (`be61993`) — systemd `ExecStart` was missing the `daemon` subcommand arg and `systemctl --user start` was never called after enable. macOS LaunchAgent plist was also missing the `daemon` arg in `ProgramArguments`. Both fixed. Closes #12
  - `src/tui/onboarding/config.rs`
- **config_watcher test hanging the test runner** (`be61993`) — Blocking `rx.recv()` loop inside `spawn_blocking` kept the tokio runtime from shutting down after tests. Fixed with a 200ms-poll loop and hard 3s deadline so the blocking thread exits cleanly
  - `src/utils/config_watcher.rs`
- **Nightly rustfmt CI failures** (`3208ac7`) — `telegram/mod.rs` and `whatsapp/handler.rs` had formatting differences between local stable `rustfmt` and the nightly toolchain used by CI. Fixed by running `cargo fmt` through the pinned nightly toolchain from `rust-toolchain.toml`
  - `src/channels/telegram/mod.rs`, `src/channels/whatsapp/handler.rs`
- **Redundant `.max(0)` on usize after `saturating_sub`** (`00fc64d`) — Clippy `unnecessary_min_or_max` lint: `usize::saturating_sub(1)` already clamps at 0, `.max(0)` was always a no-op. Removed from three fields in onboarding channels
  - `src/tui/onboarding/channels.rs`
- **llama-cpp-2 Metal segfault on macOS 26 arm64** (`118ea65`) — Bumped `llama-cpp-2` from `0.1.134` to `0.1.137` which includes the upstream Metal fix. Thanks @Pibomeister (PR #13)
  - `Cargo.toml`, `Cargo.lock`

### Changed
- **Default approval policy changed to `auto-always` for new users** (`3ed02ef`) — New installations no longer prompt before every tool call. The agent works autonomously out of the box. Existing users with `approval_policy` set in `config.toml` are unaffected (serde `default` only applies when the field is absent). To opt back into per-call prompts: run `/approve` → "Approve-only (always ask)"
  - `src/config/types.rs`, `README.md`
- **Telegram allowlist hot-reload extended to Discord and Slack** (`2b9b8c6`, `bd95b52`) — `allowed_users` lists for all three text channels now update at runtime when `config.toml` changes, without restart. Builds on the allowlist hot-reload foundation contributed by @Pibomeister (PR #14)
  - `src/channels/telegram/mod.rs`, `src/channels/discord/handler.rs`, `src/channels/slack/handler.rs`, `src/utils/config_watcher.rs`

## [0.2.44] - 2026-03-02

### Added
- **Google Gemini provider** (`e715536`) — Full `Provider` trait implementation against the Gemini REST API (`generativelanguage.googleapis.com/v1beta`). Streaming via SSE, tool use with `functionDeclarations`/`functionCall`/`functionResponse`, vision (multimodal `inlineData`), 1M–2M token context window. Live model list fetched from the Gemini API during onboarding and `/models`. Auth via `?key=` query param
  - `src/brain/provider/gemini.rs` (new), `src/brain/provider/factory.rs`, `src/brain/provider/mod.rs`
- **Image generation & vision tools** (`e715536`) — Two new agent tools powered by `gemini-3.1-flash-image-preview` ("Nano Banana"), independent of the main chat provider:
  - `generate_image` — Generate an image from a text prompt; saves PNG to `~/.opencrabs/images/`; returns file path for channel delivery
  - `analyze_image` — Analyze an image file path or URL via Gemini vision; works even when the main model doesn't support vision
  - `src/brain/tools/generate_image.rs` (new), `src/brain/tools/analyze_image.rs` (new), `src/brain/tools/mod.rs`
- **ImageSetup onboarding step** (`e715536`, `1336b89`, `f534b24`) — Step 7 in Advanced mode (after VoiceSetup, before Daemon). Toggle Vision Analysis and Image Generation independently; API key input with mask/replace mode; existing key detection. Model labeled as `gemini-3.1-flash-image-preview (🍌 Nano Banana)`. Persistent "get a free key at aistudio.google.com" hint shown when no key is set. Navigation: Space/↑↓ to toggle, Tab/Enter to continue, BackTab/Esc to go back
  - `src/tui/onboarding/types.rs`, `src/tui/onboarding/wizard.rs`, `src/tui/onboarding/navigation.rs`, `src/tui/onboarding/fetch.rs`, `src/tui/onboarding/config.rs`, `src/tui/onboarding_render.rs`
- **`/onboard:image` deep-link** (`e715536`) — Jump directly to the ImageSetup step from chat at any time
  - `src/tui/app/messaging.rs`
- **On-demand brain file loading** (`3224048`) — `build_core_brain()` replaces `build_system_brain()` at startup — injects only SOUL.md + IDENTITY.md (~1-2k tokens). All other brain files listed in a memory index; loaded by the agent via `load_brain_file(name)` tool on demand. `name="all"` loads everything. Dramatically reduces baseline token overhead for every message
  - `src/brain/prompt_builder.rs`, `src/brain/tools/load_brain_file.rs` (new), `src/cli/ui.rs`
- **`write_opencrabs_file` tool** (`8f3d648`) — Writes any file inside `~/.opencrabs/` (brain files, config, keys). Replaces the broken agent pattern of using `edit_file`/`write_file` which are locked to the working directory by `validate_path_safety()`
  - `src/brain/tools/write_opencrabs_file.rs` (new), `src/brain/tools/mod.rs`
- **`respond_to` selector in Telegram/Discord/Slack onboarding** (`9ecc8f0`) — New field in each channel's setup step; choose `all` / `dm_only` / `mention` mode during onboarding instead of editing config.toml manually
  - `src/tui/onboarding/types.rs`, `src/tui/onboarding/fetch.rs`, `src/tui/onboarding_render.rs`, `src/tui/onboarding/config.rs`
- **Google Image API Key in health check** (`6923174`) — When image features are enabled, the health check step verifies the Google AI key is present
  - `src/tui/onboarding/config.rs`
- **`send_file` action — discord_send and slack_send** (`905e9ef`) — New action uploads a local file as a native attachment. Discord: file attachment in channel. Slack: file upload via API. Both tools now at 17 actions
  - `src/brain/tools/discord_send.rs`, `src/brain/tools/slack_send.rs`
- **`add_attachment` action — trello_send** (`ac44fc3`) — New action uploads a local image or file as a Trello card attachment via multipart upload; returns the hosted Trello URL. Tool now at 22 actions
  - `src/brain/tools/trello_send.rs`, `src/channels/trello/client.rs`
- **Full file/image/audio input pipeline across all channels** (`9aed2ea`, `5bc33f5`) — Unified `classify_file(bytes, mime, filename) → FileContent` utility routes incoming files across every channel: images → vision pipeline (`<<IMG:path>>`), text/code/data files → extracted inline (up to 8 000 chars), audio → STT, PDFs → note to paste or use `analyze_image`. Trello: card attachments are fetched and processed on every incoming comment. Slack: voice/STT support added (was missing). All channels now handle images, text files, documents, and audio with consistent behavior
  - `src/utils/file_extract.rs` (new), `src/utils/mod.rs`, `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/whatsapp/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/slack/agent.rs`, `src/brain/tools/slack_connect.rs`, `src/channels/trello/client.rs`, `src/channels/trello/handler.rs`, `src/channels/trello/models.rs`
- **TUI text file input** (`3e4460e`) — Paste or type any text file path in the TUI input field — the file is read and inlined automatically as `[File: name]\n```\ncontent\n``` `. Works at paste time and submit time. Supports `.txt`, `.md`, `.json`, `.yaml`, `.toml`, `.rs`, `.py`, `.go`, `.sql`, and 20+ other formats
  - `src/tui/app/state.rs`, `src/tui/app/messaging.rs`

### Fixed
- **Generated images delivered as native media across all channels** (`60584ff`) — `<<IMG:path>>` markers in agent replies are now unwrapped and delivered natively on every channel: Telegram `send_photo`, WhatsApp image message, Discord file attachment, Slack file upload, Trello card attachment + `![filename](url)` embed in comment. Previously the raw marker string was sent as plain text
  - `src/channels/telegram/handler.rs`, `src/channels/discord/handler.rs`, `src/channels/whatsapp/handler.rs`, `src/channels/slack/handler.rs`, `src/channels/trello/handler.rs`
- **Trello outgoing images — upload attachment + embed inline** (`3e4460e`) — Agent replies containing `<<IMG:path>>` on Trello are now uploaded as card attachments via `add_attachment_to_card` and embedded in the comment as `![filename](url)`. Previously the marker was silently dropped
  - `src/channels/trello/handler.rs`
- **Channel tool approval + TUI real-time updates follow-up** (`248b719`) — Follow-up fixes to tool approval flows and TUI live-update reliability across all remote channels after the v0.2.43 multi-channel expansion
  - `src/channels/*/mod.rs`, `src/brain/agent/service/tool_loop.rs`, `src/tui/app/state.rs`
- **Clippy: collapse nested if blocks** (`2595550`) — Fixed two `collapsible_if` lint errors in `messaging.rs` (TUI text file detection) and `whatsapp/handler.rs` (document attachment handling)
  - `src/tui/app/messaging.rs`, `src/channels/whatsapp/handler.rs`
- **TUI silent message queue after errors** (`dc815ce`) — After any agent error, `processing_sessions` was never cleared for the current session, causing all subsequent `send_message` calls to be silently queued with no agent running. Fixed by unconditionally removing the session from `processing_sessions` and `session_cancel_tokens` in the `TuiEvent::Error` handler before branching on current vs background session
  - `src/tui/app/state.rs`
- **TUI real-time updates during channel tool loops** (`b44f1ff`) — Remote channel tool loops (Telegram, WhatsApp, etc.) were not firing `session_updated_tx` on each chunk, causing the TUI to only refresh at the end of a long tool sequence. Now fires after every tool call completion
  - `src/brain/agent/service/tool_loop.rs`
- **Attachment input shows "Image #N" instead of full path** (`2609583`) — Attachment display in the TUI input bar was showing the full file path; now shows `Image #N`, `Document #N` placeholders matching the `<<IMG:...>>` / `<<DOC:...>>` injection format
  - `src/tui/render/input.rs`
- **WhatsApp TTS — upload media before sending audio message** (`135b4d6`) — TTS audio was being sent via `send_audio` before uploading to WhatsApp media servers, causing delivery failures. Now uploads first, then sends with the returned media ID
  - `src/channels/whatsapp/handler.rs`
- **WhatsApp handler regression — empty `allowed_phones` + connect tool** (`5a32d49`) — Empty `allowed_phones` in config was incorrectly blocking all messages including the owner. `whatsapp_connect` tool now correctly writes the config entry. Owner bypass re-validated
  - `src/channels/whatsapp/handler.rs`, `src/brain/tools/whatsapp_connect.rs`
- **WhatsApp security — block owner→contact processing** (`0fc8b2e`) — Messages sent by the owner *to* a contact were being processed as if the contact sent them, exposing the agent to arbitrary tool execution from outgoing messages
  - `src/channels/whatsapp/handler.rs`
- **WhatsApp outgoing `allowed_users` enforcement** (`1707e0f`) — Outgoing messages to contacts not in `allowed_users` were being processed; now strictly gated
  - `src/channels/whatsapp/handler.rs`
- **Context display reset immediately after compaction** (`2c4ca8e`) — After `/compact`, the context percentage in the TUI header was not resetting to the new value until the next message; now resets immediately
  - `src/tui/app/state.rs`

### Changed
- **Per-channel config structs** (`f28e229`) — Replaced the single flat `ChannelConfig` with 8 dedicated structs (`TelegramConfig`, `DiscordConfig`, `SlackConfig`, `WhatsAppConfig`, `TrelloConfig`, etc.) for cleaner config parsing, better type safety, and simpler channel-specific fields. Trello `board_ids` replaces the previous `allowed_channels` field
  - `src/config/types.rs`, all channel modules, `src/tui/onboarding/`

## [0.2.43] - 2026-03-02

### Added
- **Telegram full control — 16 actions + live streaming + approval buttons** (`c1ba37c`) — `telegram_send` tool expanded from `send` to 16 actions: `send`, `reply`, `edit`, `delete`, `pin`, `unpin`, `forward`, `send_photo`, `send_document`, `send_location`, `send_poll`, `send_buttons`, `get_chat`, `ban_user`, `unban_user`, `set_reaction`. LLM response streams live into a Telegram message with `▋` cursor (edits every 1.5 s). Session resilience: re-fetches bot from DB if lost across restarts. Idle session timeout per-user
  - `src/brain/tools/telegram_send.rs`, `src/channels/telegram/handler.rs`, `src/channels/telegram/mod.rs`
- **Discord full control — 16 actions + session idle timeout** (`3459d0b`) — `discord_send` tool expanded to 16 actions mirroring Telegram: `send`, `reply`, `edit`, `delete`, `pin`, `unpin`, `forward`, `send_photo`, `send_document`, `send_location`, `send_poll`, `send_buttons`, `get_guild`, `kick_user`, `ban_user`, `set_reaction`. Idle session timeout for per-user sessions
  - `src/brain/tools/discord_send.rs`, `src/channels/discord/`
- **Slack full control — 16 actions + sender context injection** (`89c9e71`) — `slack_send` tool expanded to 16 actions: `send`, `reply`, `react`, `unreact`, `edit`, `delete`, `pin`, `unpin`, `get_messages`, `get_channel`, `list_channels`, `get_user`, `list_members`, `kick_user`, `set_topic`, `send_blocks`. Non-owner messages now prepend sender identity `[Slack message from {uid} in channel {ch}]`
  - `src/brain/tools/slack_send.rs`, `src/channels/slack/handler.rs`
- **WhatsApp typing indicator** (`9f3b1fa`) — Sends `composing` chat state on message receipt, `paused` on completion so the user sees a native typing indicator while the agent processes
  - `src/channels/whatsapp/handler.rs`
- **Tool approval — 3-button UI across all channels** (`f6b8523`, `586cccd`, `816147c`) — All four remote channels now show ✅ Yes / 🔁 Always (session) / ❌ No approval prompts matching the TUI, powered by channel-native interactive elements (WhatsApp `ButtonsMessage`, Telegram inline keyboard, Discord `CreateButton`, Slack `SlackBlockButtonElement`). "Always" sets session-level `auto_approve_session` flag — no further prompts for that session
  - `src/channels/whatsapp/mod.rs`, `src/channels/telegram/mod.rs`, `src/channels/discord/mod.rs`, `src/channels/slack/mod.rs`
- **Tool input context in Telegram streaming indicator** (`3da472a`, `af4b96b`) — Streaming status line now shows a brief hint of what the tool is doing (e.g. `⚙ bash: git status`) so the user has context while waiting
  - `src/channels/telegram/handler.rs`
- **TUI auto-refresh when remote channels process messages** (`7b95209`) — After every `run_tool_loop` completion, `AgentService` fires a `session_updated_tx` notification. The TUI listens, calling `load_session` if the updated session is the current one (and not already being processed by the TUI), or marking it as unread otherwise. Real-time TUI updates when Telegram/WhatsApp/Discord/Slack messages are processed — no manual session switch required
  - `src/brain/agent/service/builder.rs`, `src/brain/agent/service/tool_loop.rs`, `src/tui/events.rs`, `src/tui/app/state.rs`, `src/cli/ui.rs`

### Fixed
- **SQLite WAL mode + larger pool** (`1ec5c3b`) — Enables write-ahead logging so concurrent reads (TUI) and writes (channel agents) don't block each other; pool size increased from 5 to 20 connections. Eliminates channel concurrency timeouts
  - `src/services/` (DB setup)
- **WhatsApp sender identity** (`00cc01b`) — Strips device suffix from JID (`:N@s.whatsapp.net` → `@s.whatsapp.net`) before phone-number comparison; injects `[WhatsApp message from {name} ({phone})]` for non-owner messages; fetches contact display name when available
  - `src/channels/whatsapp/handler.rs`
- **WhatsApp reply to chat JID instead of device JID** (`24c1e5d`) — Was replying to the device-scoped JID (`:0@s.whatsapp.net`) causing delivery failures in group chats and multi-device setups; now replies to the canonical chat JID
  - `src/channels/whatsapp/handler.rs`
- **Inject sender context for non-owner Discord and Telegram messages** (`e00374a`) — Non-owner messages now prepend `[Discord/Telegram message from {name} (ID {uid}) in channel {ch}]` so the agent knows who it's talking to instead of assuming the owner
  - `src/channels/discord/handler.rs`, `src/channels/telegram/handler.rs`
- **Secret sanitization — redact API keys from all display surfaces** (`436808e`, `d3a2380`) — New `utils::redact_tool_input()` function recursively walks tool input JSON, redacting values for sensitive keys (`authorization`, `api_key`, `token`, `secret`, `password`, `bearer`, etc.) and inline bash command patterns (`Bearer xxx`, `api_key=xxx`, URL passwords). Applied to TUI tool history, TUI approval dialogs, and all four remote channel approval messages
  - `src/utils/sanitize.rs` (new), `src/tui/render/tools.rs`, `src/channels/*/mod.rs`
- **WhatsApp upstream log noise suppressed** (`f6b8523`) — Added `whatsapp_rust::client=error` and `whatsapp_rust=warn` directives to filter upstream TODO stub log lines
  - `src/logging/logger.rs`

### Changed
- **Context budget enforcement refactored** (`d8ab8f0`) — Extracted repeated 80%/90% compaction logic into `enforce_context_budget()` helper on `AgentService`. 80 %: triggers LLM compaction. 90 %: hard-truncates to 80 % first, then compacts. Up to 3 retries on LLM compaction failure, then warns user to run `/compact`
  - `src/brain/agent/service/tool_loop.rs`
- **`send_message_with_tools_and_callback`** — Per-call approval and progress callback overrides; remote channels pass their own callbacks without touching service-level defaults
  - `src/brain/agent/service/messaging.rs`, `src/brain/agent/service/tool_loop.rs`

## [0.2.42] - 2026-03-01

### Added
- **Native Trello channel** (`80c7b05`) — TrelloAgent authenticates and makes credentials available for tool use. Default mode is tool-only — the AI acts on Trello only when explicitly asked via `trello_send`. Opt-in polling available via `poll_interval_secs` in config; when enabled, only responds to explicit `@bot_username` mentions from allowed users. Board names resolved automatically — mix human-readable names and 24-char IDs freely
  - `src/channels/trello/` (agent, client, handler, models, mod)
- **`trello_connect` tool** (`80c7b05`) — Verify credentials, resolve boards by name, persist to config, spawn agent, confirm with open card count. Accepts comma-separated board names or IDs
  - `src/brain/tools/trello_connect.rs`
- **`trello_send` tool — 21 actions** (`80c7b05`) — Full Trello control without exposing credentials in URLs: `add_comment`, `create_card`, `move_card`, `find_cards`, `list_boards`, `get_card`, `get_card_comments`, `update_card`, `archive_card`, `add_member_to_card`, `remove_member_from_card`, `add_label_to_card`, `remove_label_from_card`, `add_checklist`, `add_checklist_item`, `complete_checklist_item`, `list_lists`, `get_board_members`, `search`, `get_notifications`, `mark_notifications_read`
  - `src/brain/tools/trello_send.rs`
- **`/onboard:<step>` deep-links** (`e4975e4`) — Jump directly to any onboarding step: `/onboard:provider`, `/onboard:channels`, `/onboard:voice`, `/onboard:health`, etc. `/doctor` alias for `/onboard:health`
  - `src/tui/app/messaging.rs`, `src/tui/app/state.rs`, `src/tui/render/help.rs`

### Fixed
- **WhatsApp voice notes silently dropped** (`8e29655`) — Handler was skipping all non-text messages including voice notes (ptt). Now only skips if no text AND no audio AND no image
  - `src/channels/whatsapp/handler.rs`
- **STT key missing from channel factory** (`8e29655`, `d0a7651`) — `ChannelFactory` was built with `config.voice` which has `stt_provider=None`. All channel agents (WhatsApp, Discord, dynamic Telegram) now receive the fully resolved `VoiceConfig` with `stt_provider`/`tts_provider` populated
  - `src/cli/ui.rs`
- **Channel `allowed_users` unified** (`e4975e4`) — Removed `allowed_ids` from `ChannelConfig`, unified into `allowed_users: Vec<String>` with backward-compat deserializer accepting legacy TOML integer arrays. Fixed health check false failures: Discord and Slack channel IDs were read from wrong field
  - `src/config/types.rs`, channel agents
- **Channel config not passed to agents** (`406503b`) — `telegram_connect`, `discord_connect`, `slack_connect` now pass `respond_to` and `allowed_channels` from persisted config to agent constructors (previously hardcoded to defaults)
  - `src/brain/tools/telegram_connect.rs`, `discord_connect.rs`, `slack_connect.rs`
- **Tool expand (`Ctrl+O`) shows full params** (`0aba196`) — Expanded tool view now shows complete untruncated input params line by line. In-flight calls show a "running..." spinner. DB-reconstructed entries degrade gracefully
  - `src/tui/render/tools.rs`, `src/tui/app/state.rs`
- **Error/warning messages auto-dismiss after 2.5 s** (`4408a69`, `d0a7651`) — Timer resets correctly on user action; covers all clear-sites in input, messaging, and dialogs
  - `src/tui/app/dialogs.rs`, `input.rs`, `messaging.rs`
- **Thinking indicator sticky above input** (`406503b`) — Moved out of scrollable chat into a dedicated layout chunk — never scrolls away
  - `src/tui/render/mod.rs`, `chat.rs`
- **`/onboard` resets to first screen** (`d0a7651`) — Pre-loads existing config values while resetting to `ModeSelect` so health check shows correct state
  - `src/tui/app/messaging.rs`, `src/tui/onboarding/wizard.rs`
- **CI Windows build** (`001ed00`) — Replaced removed `aws-bedrock`/`openai` features with `telegram,discord,slack` in Windows CI workflow
  - `.github/workflows/ci.yml`
- **Trello agent tool-only by default** (`7ca6b6b`) — Removed automatic polling and auto-replies. Agent starts in tool-only mode (credentials stored, no polling). `poll_interval_secs` in `[channels.trello]` config opts in to polling; even then only @mentions from allowed users trigger a response. Adds `poll_interval_secs: Option<u64>` to `ChannelConfig`
  - `src/channels/trello/agent.rs`, `src/config/types.rs`, `src/cli/ui.rs`, `src/brain/tools/trello_connect.rs`, `config.toml.example`

## [0.2.41] - 2026-03-01

### Fixed
- **WhatsApp onboarding — always test connection on Enter** (`676ab29`) — Pressing Enter on the phone field now always triggers a test message, matching Telegram/Discord/Slack behavior. Previously the test was gated on `whatsapp_connected`, so re-opening the app with an existing session silently skipped the test and just advanced
  - `src/tui/onboarding/channels.rs`
- **WhatsApp onboarding — reconnect from existing session for test** (`676ab29`) — When no live client is in memory (app reopened after prior pairing), `test_whatsapp_connection` now calls `reconnect_whatsapp()` which reuses the stored `session.db` without wiping it — no new QR scan required
  - `src/brain/tools/whatsapp_connect.rs`, `src/tui/app/dialogs.rs`
- **WhatsApp test message includes brand header** (`676ab29`) — Test message now prepends `🦀 *OpenCrabs*\n\n` (the `MSG_HEADER` constant) so it reads consistently with all other WhatsApp messages sent by the agent
  - `src/tui/app/dialogs.rs`
- **WhatsApp onboarding — post-QR UX overhaul** (`676ab29`) — After scanning the QR code the popup dismisses, the wizard advances to the phone allowlist field, shows any previously configured number (sentinel pattern), and allows confirm-or-replace before testing. Navigation keys (Tab/BackTab/S) always work regardless of test state; only Enter is blocked while a test is in-flight
  - `src/tui/onboarding/channels.rs`, `src/tui/onboarding_render.rs`, `src/tui/app/dialogs.rs`, `src/tui/app/state.rs`, `src/brain/tools/whatsapp_connect.rs`
- **Clippy `collapsible_match` errors** (`ff66828`) — Collapsed nested `if`-in-`match` arms into match guards across `input.rs` (WhatsApp paste handler) and `markdown.rs` (`Tag::BlockQuote`, `TagEnd::Heading`, `TagEnd::Item`, `Event::HardBreak|SoftBreak`)
  - `src/tui/onboarding/input.rs`, `src/tui/markdown.rs`
- **CI nightly clippy/rustfmt** (`a65c0ab`) — Added `rustfmt` and `clippy` components to `rust-toolchain.toml` so nightly CI jobs resolve the tools without network fallback; pinned workflow to `main` branch trigger
  - `rust-toolchain.toml`, `.github/workflows/`

## [0.2.40] - 2026-02-28

### Added
- **Live plan checklist widget** (`7e1b4db`) — A real-time task panel appears above the input box whenever the agent is executing a plan. Shows plan title, progress bar (`N/M  ████░░  X%`), and per-task status rows (`✓` completed, `▶` in-progress, `·` pending, `✗` failed) with per-status colors. Height is `min(task_count + 2, 8)` rows; zero height when no plan is active. Panel is session-isolated — each session tracks its own plan file (`~/.opencrabs/agents/session/.opencrabs_plan_{uuid}.json`) and reloads on session switch
  - `src/tui/render/plan_widget.rs` (new), `src/tui/render/mod.rs`, `src/tui/app/state.rs`, `src/tui/app/messaging.rs`

### Fixed
- **Live ctx counter during agent tool loops** (`1cb46a9`) — `TokenCountUpdated` events now sync `last_input_tokens` so the `ctx: N/M` display in the status bar ticks up live during streaming and tool execution instead of freezing until `ResponseComplete`
  - `src/tui/app/state.rs`
- **Ctx shows base context on session load and new session** (`1cb46a9`) — Status bar no longer starts at `–` or `0` on a fresh session. It immediately reflects system prompt + tool definition token cost via `base_context_tokens()` (system prompt tokens + tool count × 60)
  - `src/brain/agent/service/builder.rs`, `src/tui/app/messaging.rs`
- **Plan tool auto-approves on finalize** (`9fca3ec`) — `finalize` now sets `PlanStatus::Approved` directly and instructs the agent to begin executing tasks immediately. Previously the tool returned `PendingApproval` and printed "STOP — wait for user response", causing a double-approval (tool dialog + follow-up message) and blocking task execution
  - `src/brain/tools/plan_tool.rs`, `src/brain/prompt_builder.rs`
- **`read_only_mode` dead code removed** (`9fca3ec`) — Remnant field and all callers from the deleted Plan Mode feature purged from `ToolExecutionContext`, tool implementations, `send_message_with_tools_and_mode`, A2A handlers, and tests
  - `src/tui/app/messaging.rs`, `src/tui/app/state.rs`, `src/brain/tools/bash.rs`, `src/brain/tools/edit_file.rs`, `src/brain/tools/write_file.rs`, `src/brain/tools/code_exec.rs`, `src/brain/tools/notebook.rs`
- **MiniMax `</think>` block stripping** (`9b0b8d0`) — MiniMax sometimes closes reasoning blocks with `</think>` instead of `<!-- /reasoning -->`. Extended the think-tag filter to handle this closing variant
  - `src/brain/provider/custom_openai_compatible.rs`

### Changed
- **Complete TUI color overhaul — gray, orange, and cyan palette** (`2796889`, `3d88f11`, `a33fddc`) — All three legacy accent colors replaced for a cohesive warm-neutral scheme:
  - `Color::Blue` / `Rgb(70,130,180)` → `Color::Gray` / `Rgb(120,120,120)` — borders, titles, section headers
  - `Color::Yellow` / `Rgb(184,134,11)` → `Color::Rgb(215,100,20)` muted orange — active/pending states, ctx warning, approval badge
  - `Color::Green` / green-dominant `Rgb` values → `Color::Cyan` / `Rgb(60–80,165–190,165–190)` — success states, completed tasks, diff additions, ctx-ok indicator
  - `src/tui/render/chat.rs`, `src/tui/render/dialogs.rs`, `src/tui/render/help.rs`, `src/tui/render/input.rs`, `src/tui/render/plan_widget.rs`, `src/tui/render/sessions.rs`, `src/tui/render/tools.rs`, `src/tui/onboarding_render.rs`

## [0.2.39] - 2026-02-28

### Added
- **Status bar below input** (`02220e7`, `9dd4cab`) — Persistent one-line status bar replaces the old sticky overlay. Displays session name (orange), provider / model, working directory, and approval policy badge. Session and directory were moved from the header into the status bar; the full-width header bar was removed entirely
  - `src/tui/render/mod.rs`, `src/tui/render/input.rs`
- **Immediate thinking spinner in chat** (`57ffc40`) — A spinner and "OpenCrabs is thinking..." line appears in the chat area as soon as a request is submitted, before any streaming content arrives. Eliminates the blank gap while the provider is warming up
  - `src/tui/render/chat.rs`
- **Per-session context token cache** (`57ffc40`) — When switching between sessions or reloading, the last known input token count is restored from an in-memory cache instead of showing `–`. Accurate token counts are re-confirmed on the next API response
  - `src/tui/app/state.rs`, `src/tui/app/messaging.rs`

### Fixed
- **ctx shows accurate token count for providers that report zero usage** (`033043f`) — Providers like MiniMax always return `usage: {total_tokens: 0}` in streaming responses. The provider now uses its pre-computed `message_tokens + tool_schema_tokens` (serialised OpenAI JSON) as the fallback, so the ctx display (e.g. `29K/200K`) matches the debug log exactly instead of showing the lower raw-text estimate (~14K)
  - `src/brain/provider/custom_openai_compatible.rs`, `src/brain/agent/service/tool_loop.rs`
- **Compact app title in sessions/help screens** (`bc80a0f`) — Removed blank lines and border from the app title block in non-chat screens. Title now occupies exactly one row, reclaiming vertical space
  - `src/tui/render/mod.rs`
- **Extra blank space below chat history** (`d469f01`) — Scroll calculation used `reserved = 3` left over from removed borders/overlay. Changed to `reserved = 1` (top padding only), eliminating the gap at the bottom of the chat area
  - `src/tui/render/chat.rs`
- **Duplicate thinking indicators removed** (`aa08d68`, `57ffc40`) — "OpenCrabs is thinking" was appearing twice: once as an inline tool-group hint and once in the status bar. Removed both; the single spinner in the chat area is the sole indicator
  - `src/tui/render/chat.rs`, `src/tui/render/input.rs`
- **Muted orange replaces bright yellow** (`02220e7`) — `Color::Yellow` replaced with `Color::Rgb(215, 100, 20)` for ctx percentage, sessions spinner, and pending-approval badge. Intentional dark-golden `Rgb(184, 134, 11)` unchanged
  - `src/tui/render/input.rs`, `src/tui/render/sessions.rs`

## [0.2.38] - 2026-02-27

### Fixed
- **Splash screen shows actual custom provider name** — `resolve_provider_from_config()` was returning the hardcoded string `"Custom"` instead of the actual provider name (e.g. `"nvidia"`, `"moonshot"`). Now correctly returns the name key from `providers.active_custom()`
  - `src/config/types.rs`
- **Full request payload in debug logs** — Removed `.take(1000)` truncation from OpenAI-compatible request debug log. The API request itself was never truncated; only the log display was. Now logs the full payload for accurate debugging
  - `src/brain/provider/custom_openai_compatible.rs`
- **Standalone reasoning render during thinking-only phase** — Providers that emit reasoning before any response text (e.g. Kimi K2.5, DeepSeek) now render a visible `🦀 OpenCrabs is thinking...` block with live reasoning content while `streaming_response` is still empty. Previously the screen was blank until the first response chunk
  - `src/tui/render/chat.rs`
- **Streaming redraws per chunk** — Drain loop in runner now breaks immediately on `ResponseChunk` events, triggering a redraw after each text chunk. Previously `ReasoningChunk` events also broke the loop, preventing response text from rendering in real-time on some providers
  - `src/tui/runner.rs`
- **Approval dialog shows full tool parameters** — Tool approval dialog previously truncated parameter values at 60 characters. Now renders all parameters line-by-line without truncation so the full context is visible when deciding whether to approve
  - `src/tui/render/tools.rs`
- **Tool approval waits indefinitely** — Removed 120-second timeout on tool approval callbacks. The dialog now waits as long as needed for the user to approve or deny
  - `src/tui/app/state.rs`
- **Green dot pulse slowed** — Animated `●` dot in tool call groups now pulses on a ~1.6s cycle (`animation_frame / 8`) instead of the previous fast flicker (`animation_frame / 3`)
  - `src/tui/render/tools.rs`

### Removed
- **Plan Mode completely removed** (~1400 lines deleted) — All plan execution code, UI, keyboard shortcuts, and state removed. Includes `plan_exec.rs` module, `AppMode::Plan` variant, `PlanApprovalState`/`PlanApprovalData` structs, Ctrl+P/Ctrl+A/Ctrl+R/Ctrl+I shortcuts, plan approval intercept in input handler, plan help screen section, and plan re-exports. Plan Mode section removed from README
  - `src/tui/app/plan_exec.rs` (deleted), `src/tui/app/input.rs`, `src/tui/app/messaging.rs`, `src/tui/app/mod.rs`, `src/tui/app/state.rs`, `src/tui/events.rs`, `src/tui/mod.rs`, `src/tui/render/chat.rs`, `src/tui/render/help.rs`, `src/tui/render/mod.rs`, `src/tui/render/tools.rs`, `README.md`

## [0.2.37] - 2026-02-26

### Added
- **Per-session provider selection** (`5689cd9`) — Each session can now have its own LLM provider. Configure per-session via `/models` or in `config.toml` under `[session.*.provider]`. Parallel execution of multiple sessions with different providers supported
  - `src/brain/agent/service.rs`, `src/brain/mod.rs`, `src/tui/app/state.rs`, `src/tui/render.rs`, `config.toml.example`
- **Arrow key navigation in multiline input** (`9b544f9`) — Arrow Up/Down now navigate between lines in the multiline input field, not just recall history. Cursor moves within the multiline content as expected
  - `src/tui/app/input.rs`, `src/tui/render/input.rs`
- **Test units for multi-session and multi-model** (`cf7ff0d`) — Added unit tests covering session-aware approval policies, model switching within sessions, and provider key isolation
  - `src/brain/agent/service/tests/approval_policies.rs`, `src/brain/agent/service/tests/basic.rs`

### Fixed
- **Session-aware tool approvals** (`846f228`) — Tool approval policies now correctly apply per-session. Approval state is stored with session ID, not globally. Async model fetching improved with better error handling
  - `src/brain/agent/service.rs`, `src/brain/mod.rs`, `src/tui/app/state.rs`
- **Custom provider name field** (`c22a05a`) — Onboarding now pre-fills the custom provider name field. Model fetching uses existing key if available instead of requiring re-entry. Provider name displays correctly in `/models` dialog
  - `src/tui/onboarding.rs`, `src/tui/app/dialogs.rs`, `src/brain/provider/custom_openai_compatible.rs`

### Refactored
- **Split `agent/service.rs`** (`8f9c160`) — Extracted into module directory: `service/builder.rs`, `service/context.rs`, `service/helpers.rs`, `service/messaging.rs`, `service/mod.rs`. Improved code organization and testability
- **Split `render.rs`** (`6247666`) — Extracted 3312-line file into `render/` module directory with `render/mod.rs`, `render/input.rs`, `render/dialogs.rs`, `render/components.rs`
- **Cargo fmt pass** (`d02fcf7`) — Full codebase formatting enforcement

## [0.2.36] - 2026-02-26

### Fixed
- **Custom provider `/models` dialog** (`fc0626c`) — Model name is now a free-text input instead of a hardcoded list. Labels show the actual provider name (e.g. "Moonshot") instead of generic "Custom". Onboarding flow updated to match
  - `src/tui/app/dialogs.rs`, `src/tui/render.rs`, `src/tui/onboarding.rs`, `src/config/types.rs`, `src/tui/app/state.rs`, `src/brain/provider/anthropic.rs`, `src/brain/provider/custom_openai_compatible.rs`, `README.md`
- **Input UX improvements** (`7804ab3`) — Esc scrolls viewport to bottom of conversation. Arrow Up recalls previously cleared/stashed input text. Cursor renders as a block highlighting the current character instead of a thin line. Escape timer resets when processing completes so next Esc behaves correctly
  - `src/tui/app/input.rs`, `src/tui/app/messaging.rs`, `src/tui/app/state.rs`, `src/tui/render.rs`
- **Strip Kimi HTML comment markup** (`47b1d58`) — Kimi K2.5 embeds reasoning and hallucinated tool calls as HTML comments (`<!-- reasoning -->`, `<!-- tools-v2: -->`) in the content field. Extended `filter_think_tags` and `strip_think_blocks` to strip these alongside `<think>`. Fixed `extract_reasoning` to handle multiple reasoning blocks per message. Added Moonshot/Kimi pricing (K2.5, K2 Turbo, K2) to compiled-in defaults and `usage_pricing.toml.example`
  - `src/brain/provider/custom_openai_compatible.rs`, `src/pricing.rs`, `src/tui/app/messaging.rs`, `usage_pricing.toml.example`
- **`/models` provider switch: never overwrite user API keys** (`5120bf5`) — Killed sentinel string `"__EXISTING_KEY__"` from `/models` dialog entirely. Replaced with boolean flag `model_selector_has_existing_key`. Only writes to `keys.toml` when user actually types a new key. Disables all other providers on disk before enabling selected one. Added `is_real_key` guard in `merge_provider_keys` for all providers
  - `src/config/types.rs`, `src/tui/app/dialogs.rs`, `src/tui/app/state.rs`, `src/tui/render.rs`
- **Model change context hint for agent** (`ce8e422`) — When user switches model via `/models`, a `[Model changed to X (provider: Y)]` hint is prepended to the next user message via `pending_context` (same mechanism as `/cd`), so the LLM is aware of the switch. TUI status message also shown in chat. Custom provider uses user-configured name (e.g. "nvidia") instead of generic label. Fallback provider key changed from `providers.custom.default` to `providers.custom` to avoid stale config entries
  - `src/tui/app/dialogs.rs`, `src/tui/app/messaging.rs`

## [0.2.35] - 2026-02-26

### Added
- **Animated tool call dots** — Green `●` dot pulses (`●`/`○`) while tools are actively processing, stays solid when finished. Visually distinguishes active tool execution from completed groups
- **Inline thinking indicator during tool execution** — "OpenCrabs is thinking..." now renders inline above the active tool group instead of as a sticky overlay, preventing overlap with tool call content
- **`.github/CODEOWNERS`** — Auto-assigns `@adolfousier` as reviewer on all PRs

### Fixed
- **TUI spacing improvements** — Removed double blank lines between messages and tool groups. Added proper spacing before thinking sections and between thinking hint and expanded content
- **Inline code background removed** — `bg(Color::Black)` on backtick code spans in markdown renderer removed for cleaner look. Thinking hints use subtle `Rgb(90,90,90)` text with no background
- **Sudo prompt bleeding into TUI** — Added `-p ""` flag to `sudo -S` to suppress sudo's native "Password:" prompt from writing directly to the terminal
- **`cargo fmt` full codebase pass** — Enforced official Rust style guide across 92 files
- **Test fixes** — `stream_complete()` tests updated to destructure `(LLMResponse, Option<String>)` tuple return with reasoning assertions. `write_secret_key` doctest fixed (missing import + Result return type)

## [0.2.34] - 2026-02-26

### Added
- **Reasoning/thinking persistence** — MiniMax (and other providers that emit `reasoning_content`) now accumulate thinking content during streaming, persist it to DB with `<!-- reasoning -->` markers, and reconstruct it on session reload. Reasoning is rendered as a collapsible "Thinking" section on assistant messages
- **Real-time message persistence per step** — Assistant text is written to DB after each tool iteration, not just at the end. Crash or disconnect mid-task no longer loses intermediate text
- **Collapsible reasoning UI** — Ctrl+O now toggles both tool groups and reasoning sections. Collapsed by default, expandable inline with dimmed italic style matching the streaming "Thinking..." indicator

### Fixed
- **MiniMax intermediate text lost on reload** — Tool call indices from OpenAI-compatible providers collided with the text content block at index 0 in `stream_complete()`, overwriting accumulated text. Tool indices now offset by +1. Fixes [#10](https://github.com/adolfousier/opencrabs/issues/10)
- **TUI unresponsive after onboarding** — `rebuild_agent_service()` only attached the approval callback, dropping `progress_callback`, `message_queue_callback`, `sudo_callback`, and `working_directory`. All callbacks are now preserved from the existing agent service. Fixes [#10](https://github.com/adolfousier/opencrabs/issues/10)
- **Tool loop false positives eliminated** — Replaced 115-line per-tool signature matching with 7-line universal input hash. Different arguments = different hash = no false detection. Same args repeated 8 times = real loop
- **Chat history lost on mid-task exit** — Exiting while the agent was between tool iterations discarded the conversation. Now persists accumulated text before exit
- **Clippy warnings** — Collapsed nested `if` statements in `service.rs` and `input.rs`

## [0.2.33] - 2026-02-25

### Added
- **Streaming `/rebuild`** — Live compiler output streamed to chat during build. On success, binary is `exec()`-replaced automatically (no prompt, no restart). Auto-clones repo for binary-only users if no source tree found
- **Centralized `usage_pricing.toml`** — Runtime-editable pricing table for all providers (Anthropic, OpenAI, MiniMax, Google, DeepSeek, Meta). Edit live, changes take effect on next `/usage` open without restart. Written automatically on first run during onboarding
- **All-time `/usage` breakdown** — Shows cost grouped by model across all sessions. Historical sessions with stored tokens but zero cost get estimated costs (yellow `~$X.XX` prefix). Unknown models shown as `$0.00` instead of silently ignored
- **`/cd` context injection** — When user changes working directory via `/cd`, a context hint is queued and prepended to the next message so the LLM knows about the directory change without the user having to explain. Uses new `pending_context` vec on App state
- **Tool approval policy preservation across compaction** — Compaction summary prompt now includes `## Tool Approval Policy` section. All 4 continuation messages (pre-loop, mid-tool-loop, emergency, mid-loop) inject `CRITICAL: Tool approval is REQUIRED` when auto-approve is off. Agent can no longer "forget" approval policy after context resets
- **Dropped stream detection + retry** — Detects when provider streams end without `[DONE]`/`MessageStop` (stop_reason is None). Retries up to 2 times transparently, discarding partial responses. After 2 failures, proceeds gracefully with partial response

### Fixed
- **Context compaction streamed, not frozen** — `compact_context` uses `stream_complete` so the TUI event loop stays alive during compaction. Previously froze the UI for 2-5 minutes on large contexts
- **Compaction summary visible in chat** — Summary fires via `CompactionSummary` progress event after streaming, rendered in chat so user can see what was preserved
- **TUI state reset post-compaction** — Resets `streaming_response` + `active_tool_group` on compaction so the UI is clean for continuation
- **Compaction request budget cap** — Capped at 75% of context window with 16k token overhead (was 8k). Prevents the compaction request itself from exceeding the provider limit (was sending 359k tokens)
- **Real-time context counter** — Live token count updates in header during streaming
- **`/models` paste support** — API keys can be pasted into the model selection dialog
- **Pricing: $0 cost for all sessions** — `PricingConfig` struct used `HashMap<String, Vec<PricingEntry>>` but TOML has `entries = [...]` wrapper. Added `ProviderBlock` to match schema correctly
- **Pricing: MiniMax $0** — Stream chunks don't include model name. Falls back to request model
- **Pricing: legacy format migration** — Auto-migrates `[[usage.pricing.X]]` on-disk format to current schema
- **Clippy: collapsible_if** — Fixed in `rebuild.rs` and `pricing.rs`

## [0.2.32] - 2026-02-24

### Added
- **A2A Bearer token authentication** -- JSON-RPC endpoint (`/a2a/v1`) now supports `Authorization: Bearer <key>` when `api_key` is configured. Agent card and health endpoints remain public for discovery. Key can be set in `config.toml` or `keys.toml` under `[a2a]`
- **A2A task persistence** -- Tasks are persisted to SQLite (`a2a_tasks` table, auto-migration) on create, complete, fail, and cancel. Active tasks are restored from DB on server startup so in-flight work survives restarts
- **A2A SSE streaming (`message/stream`)** -- Real-time task updates via Server-Sent Events per A2A spec. Each SSE `data:` line is a JSON-RPC 2.0 response containing a `Task`, `TaskStatusUpdateEvent` (with `final: true` on completion), or `TaskArtifactUpdateEvent`. Agent card now advertises `streaming: true`

## [0.2.31] - 2026-02-24

### Fixed
- **Tool calls stacking into one giant group on reload** — Removed cross-iteration merge logic that collapsed all consecutive tool groups into a single "N tool calls" block, eating intermediate text between iterations. Each iteration's `<!-- tools-v2: -->` marker now produces its own collapsible group, matching live session behavior
- **Tool group ordering during live streaming** — IntermediateText handler flushed the previous iteration's tool group *after* pushing the new step's text, causing tools to appear below the wrong text. Now flushes tools first, matching DB order
- **Ctrl+O blocked during approval** — All non-approval keys were eaten when an approval dialog was pending, preventing users from collapsing expanded tool groups to see the approval. Ctrl+O now works during approval
- **Auto-collapse tool groups on approval** — When an approval request arrives, all tool groups are automatically collapsed so the approval dialog is immediately visible without manual intervention
- **EXA MCP fallback on empty API key** — Empty string API key (`""`) caused EXA to attempt direct API mode instead of free MCP. Now treats empty keys as absent, correctly falling back to MCP (aaefd3d)
- **Brave search registered without enabled flag** — `brave_search` tool registered whenever an API key existed, ignoring `enabled = false` in config.toml. Now requires both `enabled = true` and a valid API key

## [0.2.30] - 2026-02-24

### Added
- **Agent-to-Agent (A2A) Protocol** — HTTP gateway implementing A2A Protocol RC v1.0 for peer-to-peer agent communication via JSON-RPC 2.0. Supports `message/send`, `tasks/get`, `tasks/cancel`. Contributed by [@koatora20](https://github.com/koatora20) in [#9](https://github.com/adolfousier/opencrabs/pull/9)
- **Bee Colony Debate** — Multi-agent structured debate protocol based on ReConcile (ACL 2024) confidence-weighted voting. Configurable rounds with knowledge-enriched context from QMD memory search
- **Dynamic Agent Card** — `/.well-known/agent.json` endpoint with skills generated from the live tool registry
- **A2A Documentation** — Config example, README section with curl examples, TOOLS.md/SECURITY.md/BOOTSTRAP.md reference templates updated

### Fixed
- **Tool calls vanishing from TUI** — Tool call context (the collapsible bullet with tool names and output) disappeared from the chat after the agent responded. Tool group was being attached to a previous assistant message instead of rendered inline before the current response. Now matches the DB reload layout: tool calls appear above the response text, visible in both live and reloaded sessions
- **Tool loop false positives** — `web_search` and `http_request` calls with different arguments were treated as identical by the loop detector, killing legitimate multi-search flows. Signatures now include query/URL arguments. Thresholds raised (8 default, 4 for modification tools) with a 50-call history window
- **Tool call groups splitting on session reload** — Each tool-loop iteration wrote a separate DB marker, so "2 tool calls" became two "1 tool call" entries on reload. Fixed in v0.2.31
- **Brave search registered without enabled flag** — `brave_search` tool was available to the agent even when `enabled = false` in config.toml. Now requires both `enabled = true` and API key
- **EXA MCP fallback on empty API key** — Empty string API key (`""`) in keys.toml caused EXA to use direct API mode instead of free MCP mode. Now treats empty keys as absent, correctly falling back to MCP
- **A2A: Removed unused `rusqlite` dependency** — A2A handler no longer pulls in rusqlite; uses existing SQLite infrastructure
- **A2A: UTF-8 slicing safety** — Fixed potential panic on multi-byte characters in message truncation
- **A2A: Restrictive CORS by default** — No cross-origin requests allowed unless `allowed_origins` is explicitly configured
- **A2A: Handler module split** — Monolithic `handler.rs` split into `handler/mod.rs`, `handler/service.rs`, `handler/processing.rs` for maintainability

### Changed
- **A2A: Agent card uses tool registry** — Skills reflect actual available tools instead of hardcoded list
- **A2A: Server wiring** — Proper integration with AppState, config, and tool registry
- **Web search defaults in README** — Updated to reflect DuckDuckGo + EXA as default (no key needed), Brave as optional

## [0.2.29] - 2026-02-24

### Added
- **Tool Parameter Normalization** — Centralized alias map in tool registry corrects common LLM parameter name mistakes (`query`→`pattern`, `cmd`→`command`, `file`→`path`) before validation. Works across all tools
- **Brain Tool Reference** — System prompt lists exact required parameter names for each tool
- **TOOLS.md Parameter Table** — New user template includes tool parameter quick-reference table

### Fixed
- **Token Counting for OpenAI-Compatible Providers** — `stream_complete` now reads `input_tokens` from `MessageDelta` events. Previously always 0 for MiniMax and other OpenAI-compatible providers, causing incorrect session token totals and context percentage
- **Session Search UTF-8 Crash** — Fixed panic on multi-byte characters when truncating message content (`floor_char_boundary` instead of raw byte slice)
- **Session Search Deadlock** — Search uses `try_lock()` on embedding engine mutex with FTS-only fallback when backfill is running
- **Embedding Backfill Lock Contention** — Processes one document at a time, releasing engine lock between each
- **Tool Loop False Positive** — `session_search` loop detector signature includes `operation:query` to distinguish calls
- **Grep Traversal Performance** — Skips `target/`, `node_modules/`, `.git/` and other heavy directories; default limit of 200 matches
- **Thinking Indicator Overlap** — "OpenCrabs is thinking..." no longer overlaps chat content
- **App Exit Hang** — `process::exit()` prevents tokio runtime hanging on `spawn_blocking` threads
- **Ctrl+C Force Exit** — Cancel token + 1-second timeout fallback when tools are stuck

### Changed
- **App Module Split** — `app.rs` (4,960 lines) split into `state.rs`, `input.rs`, `messaging.rs`, `plan_exec.rs`, `dialogs.rs` with `mod.rs` declarations only
- **Doc Comments** — Converted `//` to `///` doc comments across codebase
- **7 Test Fixes** — Fixed `test_create_provider_no_credentials` (PlaceholderProvider) and 6 onboarding tests (config pollution, channel routing)

## [0.2.28] - 2026-02-23

### Added
- **Brain Setup Persistence** — BrainSetup step loads existing `USER.md`/`IDENTITY.md` from workspace as truncated preview on re-run. No extra files — brain files are the source of truth
- **Brain Setup Skip** — `Esc` to skip, unchanged inputs skip regeneration, empty inputs skip gracefully
- **Brain Regeneration Context** — On re-run, LLM receives current workspace brain files (not static templates), preserving manual edits as context. Generated content overwrites existing files
- **Splash Auto-Close** — Splash screen auto-closes after 3 seconds
- **Slack Debug Logging** — Added debug tracing for Slack message routing (user, channel, bot_id)

### Fixed
- **Model List Isolation** — Minimax and Custom provider model lists no longer mix. Each provider loads only its own models from `config.toml.example`. Previously `load_default_models()` dumped all providers into one shared list
- **Workspace Path Trim** — Workspace path is trimmed on confirm, preventing ghost directories from trailing spaces
- **HealthCheck Skipping BrainSetup** — HealthCheck step returned `WizardAction::Complete` immediately, skipping BrainSetup. Now returns `WizardAction::None` to advance to BrainSetup
- **Brain File Overwrite on Regeneration** — `apply_config()` skipped writing brain files if they already existed, even after regeneration. Now overwrites when AI-generated content is available

### Changed
- **Renamed `about_agent` → `about_opencrabs`** — Field and label renamed from "Your Agent" to "Your OpenCrabs" for clarity

## [0.2.27] - 2026-02-23

### Added
- **Named Custom Providers** — Define multiple named OpenAI-compatible providers via `[providers.custom.<name>]` (e.g. `lm_studio`, `ollama`). First enabled one is used. Legacy flat `[providers.custom]` format still supported

### Fixed
- **Stream Deduplication** — Fixed duplicated agent messages in chat when using LM Studio and other custom providers. Some providers send the full response in the final chunk's `message` field — falling back to `message` after receiving delta content duplicated everything
- **Database Path Tilde Expansion** — `~` in database path config was treated literally, creating a `~/` directory inside the repo. Added `expand_tilde()` to resolve to actual home directory
- **WhatsApp Onboarding** — Fixed WhatsApp channel setup to include QR code pairing step with auto-advance, skip and retry
- **Channel Onboarding Allowed Lists** — Fixed missing allowed users/channels/phones input fields on Telegram, Discord, WhatsApp and Slack setup screens

### Changed
- **README** — Provider examples updated to named custom provider format (`[providers.custom.lm_studio]`)
- **config.toml.example** — Database path uses smart default, custom providers use named format

## [0.2.26] - 2026-02-22

### Added
- **Streaming Tool Call Accumulation** — OpenRouter and Custom providers now correctly handle streaming tool calls. Added `StreamingToolCall`/`StreamingFunctionCall` structs with optional fields for incremental SSE deserialization, plus `ToolCallAccum` state machine that accumulates `id`, `name`, and `arguments` across chunks and emits on `finish_reason: "tool_calls"` or `[DONE]`
- **Input Sanitization** — Paste handler strips `\r\n`, takes first line only, trims whitespace. Storage layer (`write_secret_key`, `write_key`) also sanitizes before writing to TOML files
- **Auto-append `/chat/completions`** — Custom provider factory auto-appends `/chat/completions` to base URLs that don't include it, preventing silent 404s
- **Provider + Model in Completion** — Onboarding completion message now shows which provider and model were selected

### Fixed
- **Streaming Tool Calls Failing on OpenRouter/Custom** — Root cause: `StreamingToolCall` struct required `id` and `type` fields but SSE continuation chunks only send `index` + `function.arguments`. Made all fields optional except `index`. Removed unused `type` field
- **API Key Header Panic** — `headers()` used `.expect()` which panicked on invalid key characters (e.g. `\r` from paste). Now returns `Result<HeaderMap, ProviderError>` with descriptive error
- **Log Directory Path** — Logs were stored in `cwd/.opencrabs/logs/` (inside the repo) instead of `~/.opencrabs/logs/` (user workspace). Fixed `LogConfig`, `get_log_path()`, and `cleanup_old_logs()` to use home directory
- **Config/Keys Overwrite** — `Config::save()` was called in `app.rs` and `onboarding.rs`, destructively overwriting the entire TOML file. Replaced all instances with individual `write_key()`/`write_secret_key()` calls that read-modify-write without losing unrelated sections
- **Custom Provider Using Wrong Field** — Custom provider used `custom_api_key` while all other providers used `api_key_input`. Unified to `api_key_input` across all providers
- **Sentinel Prepended to Key** — `__EXISTING_KEY__` sentinel was prepended to actual API key on paste. Fixed `CustomApiKey` handlers to clear sentinel before appending new input
- **URL Appended to Key** — Pasting from clipboard could include `\r` and trailing URL text in API key field. Added paste sanitization at input handler and storage layer

### Changed
- **Renamed `openai.rs` → `custom_openai_compatible.rs`** — Reflects that this module handles all OpenAI-compatible APIs (OpenRouter, Minimax, Custom, LM Studio, Ollama), not just official OpenAI
- **Onboarding Simplified** — Removed ~300 lines of dead in-memory config construction from `apply_config()`; all config writes now use individual `write_key()`/`write_secret_key()` calls
- **keys.toml is Single Secret Source** — All API keys, bot tokens, and search keys are stored in `~/.opencrabs/keys.toml`. No more env vars or OS keyring for secrets. `config.toml` holds non-sensitive settings only

## [0.2.25] - 2026-02-21

### Added
- **Token Usage for MiniMax/OpenRouter** — Added `stream_options: {include_usage: true}` to streaming requests; extracts and logs token usage from final chunk
- **Shutdown Logo** — Shows ASCII logo with rolling goodbye message on terminal when exiting

### Fixed
- **Duplicate Messages** — Fixed duplicate assistant messages appearing when IntermediateText already added content
- **Tool Call Flow** — Tool calls now appear as separate messages after assistant text, flowing naturally between steps
- **Empty Content Rendering** — Fixed assistant messages showing empty during session (was showing correctly after restart)
- **Thinking Indicator** — Moved "OpenCrabs is thinking..." indicator to sticky position at bottom of chat (above input field), always visible to users

### Changed
- **Message Ordering** — Queued messages now appear at very bottom of conversation (after all assistant/tool messages), above input field
- **README** — Added GitHub stars call-to-action

## [0.2.24] - 2026-02-21

### Added
- **MiniMax Provider Support** — Added MiniMax as new LLM provider (OpenAI-compatible). Does not have /models endpoint, uses config_models for model list
- **Onboarding Wizard** — Full onboarding flow for first-time setup with provider selection
- **Model Selector** — Slash command `/models` to change provider and model with live fetching, search filter
- **Tool Call Expanded View** — Ctrl+O expands tool context with gray background; diff coloring (+ green, - red)
- **API Keys in keys.toml** — API keys now stored in separate `~/.opencrabs/keys.toml` (chmod 600)
- **STT/TTS Provider Config** — Added `providers.stt.groq` and `providers.tts.openai` config sections

### Fixed
- **MiniMax Tool Calls** — Fixed tool call parsing for MiniMax (empty arguments issue)
- **Context Compaction Crash** — Fixed orphaned tool_result crash after compaction
- **Onboarding Persistence** — Provider selection and settings now persist correctly
- **Model Selector Flow** — Multiple fixes for persistence, search, scrolling, Enter key behavior
- **Compaction Crash (400 — Orphaned tool_result)** — After any trim or compaction, a `user(tool_result)` message could be left at the front of history without its preceding `assistant(tool_use)`. The Anthropic API rejects this with a 400 error, crashing the next compaction attempt. Fixed at three layers: `trim_to_fit` and `trim_to_target` now call `drop_leading_orphan_tool_results()` after each removal; `compact_with_summary` advances `keep_start` past any leading orphaned tool_result messages; `compact_context` skips them before sending to the API as a safety net. Conversation continues normally after compaction with no tool call drops
- **Compaction Summary as Assistant Message** — Compaction summary was stored in a `details` field and hidden behind Ctrl+O. Now rendered as a real assistant chat message in the conversation flow. Tool calls that follow appear below it as normal tool groups with Ctrl+O expand/collapse
- **config.toml Model Priority over .env** — `ANTHROPIC_MAX_MODEL` env var was overwriting the model set in `config.toml`, reversing the intended priority. Now `config.toml` wins; `.env` is only a fallback when no model is configured in TOML
- **Stale Terminal on exec() Restart** — `/rebuild` hot-restart left stale rendered content from the previous process visible briefly. Terminal is now fully cleared immediately after the new process takes over

### Changed
- **Remove Qwen and Azure** — These providers are no longer supported
- **README Updated** — Added MiniMax documentation, keys.toml instructions

## [0.2.23] - 2026-02-20

### Added
- **session_search Tool** — Hybrid FTS5+vector search across all chat sessions (list/search operations)
- **History Paging** — Cap initial display at 200k tokens, Ctrl+O loads 100k more from DB
- **Onboarding Model Filter** — Type to search models, Esc clears filter

### Fixed
- **Onboard Centering** — Header/footer center independently, content block centers as uniform group
- **Onboard Scroll** — ProviderAuth tracks focused_line for proper scroll anchoring
- **Content Clipping** — Content no longer clips top border on overflow screens

### Changed
- **Compaction Display** — Now clears TUI display fully, shows summary as fresh start
- **Render history_marker** — Rendered as dim italic in chat view

## [0.2.22] - 2026-02-19

### Added
- **`/cd` Command** — Change working directory at runtime via slash command or agent NLP. Opens a directory picker (same UI as `@` file picker). Persists to `config.toml`. Agent can also call `config_manager` with `set_working_directory`
- **`slash_command` Tool** — Agent-callable tool to invoke any slash command programmatically: `/cd`, `/compact`, `/rebuild`, `/approve`, and all user-defined commands from `commands.toml`. Makes the agent aware of and able to trigger any slash command
- **Edit Diff Context** — Edit tool now includes a compact unified diff in its output. Renderer colors `+` lines green, `-` lines red, `@@` lines cyan — giving both user and agent clear visual context of changes

### Fixed
- **Stderr Bleeding into TUI** — Replaced all `unsafe` libc `dup2`/`/dev/null` hacks with `llama-cpp-2`'s proper `send_logs_to_tracing(LogOptions::default().with_logs_enabled(false))` API. Called once at engine init — kills all llama.cpp C-level stderr output permanently. Removed `libc` dependency entirely
- **Compaction Summary Never Visible** — System messages were rendered as a single `Span` on one `Line` — Ratatui clips at terminal width, so multi-paragraph summaries were silently swallowed. Fixed: newline-aware rendering with `⚡` yellow label. Compaction summary now goes into expandable `details` (Ctrl+O to read)
- **Tool Approval Disappearing** — Removed 4 `messages.retain()` calls that deleted approval messages immediately after denial, before the user could see or interact with them

### Changed
- **Install Instructions** — README now includes "Make It Available System-Wide" section with symlink/copy instructions
- **Brain Templates** — BOOT.md, TOOLS.md, AGENTS.md updated to document `/cd` and `config_manager` working directory control

## [0.2.21] - 2026-02-19

### Changed
- **Module Restructure** — Merged `src/llm/` (agent, provider, tools, tokenizer) into `src/brain/`. Brain is now the single intelligence layer — no split across two top-level modules
- **Channel Consolidation** — Moved `src/slack/`, `src/telegram/`, `src/whatsapp/`, `src/discord/`, and `src/voice/` into `src/channels/`. All messaging integrations + voice (STT/TTS) live under one module with feature-gated submodules
- **Ctrl+O Expands All** — Ctrl+O now toggles expand/collapse on ALL tool call groups in the session, not just the most recent one

### Fixed
- **Tool Approval Not Rendering** — Fixed approval prompts not appearing in long-context sessions when user had scrolled up. `auto_scroll` is now reset to `true` when an approval arrives, ensuring the viewport scrolls to show it
- **Tool Call Details Move** — Fixed `use of moved value` for tool call details field in ToolCallCompleted handler

## [0.2.20] - 2026-02-19

### Added
- **`/whisper` Command** — One-command setup for system-wide voice-to-text. Auto-downloads WhisperCrabs binary, launches floating mic button. Speak from any app, transcription auto-copies to clipboard
- **`SystemMessage` Event** — New TUI event variant for async tasks to push messages into chat

### Fixed
- **Embedding Stderr Bleed** — Suppressed llama.cpp C-level stderr during `embed_document()` and `embed_batch_with_progress()`, not just model load. Fixes garbled TUI output during memory indexing
- **Slash Autocomplete Dedup** — User-defined commands that shadow built-in names no longer show twice in autocomplete dropdown
- **Slash Autocomplete Width** — Dropdown auto-sizes to fit content instead of hardcoded 40 chars. Added inner padding on all sides
- **Help Screen** — Added missing `/rebuild` and `/whisper` to `/help` slash commands list
- **Cleartext Logging (CodeQL)** — Removed all `println!` calls from provider factory that wrote to stdout (corrupts TUI). Kept `tracing::info!` for structured logging
- **Stray Print Statements** — Removed debug `println!` from wacore encoder, replaced `eprintln!` in onboarding tests with silent returns

### Changed
- **Docker Files Relocated** — Moved `docker/` from project root to `src/docker/`, updated all references in README and compose.yml
- **Clippy Clean** — Fixed collapsible_if warnings in onboarding and app, `map_or` → `is_some_and`

## [0.2.19] - 2026-02-18

### Changed
- **Cleaner Chat UI** — Replaced role labels with visual indicators: `❯` for user messages, `●` for assistant messages. User messages get subtle dark background for visual separation. Removed horizontal dividers and input box title for a cleaner look
- **Alt+Arrow Word Navigation** — Added `Alt+Left` / `Alt+Right` as alternatives to `Ctrl+Left` / `Ctrl+Right` for word jumping (macOS compatibility)
- **Branding** — Thinking/streaming indicators now show `🦀 OpenCrabs` instead of model name

## [0.2.18] - 2026-02-18

### Added
- **OpenRouter Provider** -- First-class OpenRouter support in onboarding wizard. One API key, 400+ models including free and stealth models (DeepSeek, Llama, Mistral, Qwen, Gemma, and more). Live model list fetched from `openrouter.ai/api/v1/models`
- **Live Model Fetching** -- `/models` command and onboarding wizard now fetch available models live from provider APIs (Anthropic, OpenAI, OpenRouter). When a new model drops, it shows up immediately — no binary update needed. Falls back to hardcoded list if offline
- **`Provider::fetch_models()` Trait Method** -- All providers implement async model fetching with graceful fallback to static lists

### Changed
- **Onboarding Wizard** -- Provider step 2 now shows live model list fetched from API after entering key. Shows "(fetching...)" while loading. OpenRouter added as 5th provider option
- **Removed `cargo publish` from CI** -- Release workflow no longer attempts crates.io publish (was never configured, caused false failures)

## [0.2.17] - 2026-02-18

### Changed
- **QMD Vector Search + RRF** -- qmd's `EmbeddingEngine` (embeddinggemma-300M, 768-dim GGUF) wired up alongside FTS5 with Reciprocal Rank Fusion. Local model, no API key, zero cost, works offline. Auto-downloads ~300MB on first use, falls back to FTS-only when unavailable
- **Batch Embedding Backfill** -- On startup reindex, documents missing embeddings are batch-embedded via qmd. Single-file indexes (post-compaction) embed immediately when engine is warm
- **Discord Voice (STT + TTS)** -- Discord bot now transcribes audio attachments via Groq Whisper and replies with synthesized voice (OpenAI TTS) when enabled
- **WhatsApp Voice (STT)** -- WhatsApp bot now transcribes voice notes via Groq Whisper. Text replies only (media upload for TTS pending)
- **CI Release Workflow** -- Fixed nightly toolchain for all build targets, added ARM64 cross-linker config
- **AVX CPU Guard** -- Embedding engine checks for AVX support at init; gracefully falls back to FTS-only on older CPUs
- **Stderr Suppression** -- llama.cpp C-level stderr output redirected to /dev/null during model load to prevent TUI corruption

## [0.2.16] - 2026-02-18

### Changed
- **QMD Crate for Memory Search** -- Replaced homebrew FTS5 implementation with the `qmd` crate (BM25 search, SHA-256 content hashing, collection management). Upgraded `sqlx` to 0.9 (git main) to resolve `libsqlite3-sys` linking conflict
- **Brain Files Indexed** -- Memory search now indexes workspace brain files (`SOUL.md`, `IDENTITY.md`, `MEMORY.md`, etc.) alongside daily compaction logs for richer search context
- **Dynamic Welcome Messages** -- All channel connect tools (Telegram, Discord, Slack, WhatsApp) now instruct the agent to craft a creative, personality-driven welcome message on successful connection instead of hardcoded greetings
- **WhatsApp Welcome Removed** -- Replaced hardcoded WhatsApp welcome spawn with agent-generated message via `whatsapp_send` tool
- **Patches Relocated** -- Moved `wacore-binary` patch from `patches/` to `src/patches/`, stripped benchmarks and registry metadata

### Added
- **Discord `channel_id` Parameter** -- Optional `channel_id` input on `discord_connect` so the bot can send welcome messages immediately after connection
- **Slack `channel_id` Parameter** -- Optional `channel_id` input on `slack_connect` for the same purpose
- **Telegram Owner Chat ID** -- `telegram_connect` now sets the owner chat ID from the first allowed user at connection time
- **QMD Memory Benchmarks** -- Criterion benchmarks for qmd store operations: index file (203µs), hash skip (18µs), FTS5 search (381µs–2.4ms), bulk reindex 50 files (11.3ms), store open (1.7ms)

## [0.2.15] - 2026-02-17

### Changed
- **Built-in FTS5 Memory Search** -- Replaced external QMD CLI dependency with native SQLite FTS5 full-text search. Zero new dependencies (uses existing `sqlx`), always-on memory search with no separate binary to install. BM25-ranked results with porter stemming and snippet extraction
- **Memory Search Always Available** -- Sidebar now shows "Memory search" with a permanent green dot instead of conditional "QMD search" that required an external binary
- **Targeted Index After Compaction** -- After context compaction, only the updated daily memory file is indexed (via `index_file`) instead of triggering a full `qmd update` subprocess
- **Startup Background Reindex** -- On launch, existing memory files are indexed in the background so `memory_search` is immediately useful for returning users

### Added
- **FTS5 Memory Module** -- New async API: `get_pool()` (lazy singleton), `search()` (BM25 MATCH), `index_file()` (single file, hash-skip), `reindex()` (full walk + prune deleted). Schema: `memory_docs` content table + `memory_fts` FTS5 virtual table with sync triggers
- **Memory Search Tests** -- Unit tests for FTS5 init, index, search, hash-based skip, and content update re-indexing
- **Performance Benchmarks in README** -- Real release-build numbers: ~0.4ms/query, ~0.3ms/file index, 15ms full reindex of 50 files
- **Resource Footprint Table in README** -- Branded stats table with binary size, RAM, storage, and FTS5 search latency

### Removed
- **QMD CLI Dependency** -- Removed all `Command::new("qmd")` subprocess calls: `is_qmd_available()`, `ensure_collection()`, `search()` (sync), `reindex_background()`

## [0.2.14] - 2026-02-17

### Added
- **Discord Integration** -- Full Discord bot with message forwarding, per-user session routing, image attachment support, proactive messaging via `discord_send` tool, and dynamic connection via `discord_connect` tool
- **Slack Integration** -- Full Slack bot via Socket Mode (no public endpoint needed) with message forwarding, session sharing, proactive messaging via `slack_send` tool, and dynamic connection via `slack_connect` tool
- **Secure Bot Messaging: `respond_to` Mode** -- New `respond_to` config field for all platforms: `"mention"` (default, most secure), `"all"` (old behavior), or `"dm_only"`. DMs always get a response regardless of mode
- **Channel Allowlists** -- New `allowed_channels` config field restricts which group channels bots are active in. Empty = all channels. DMs always pass
- **Bot @Mention Detection** -- Discord checks `msg.mentions` for bot user ID, Telegram checks `@bot_username` or reply-to-bot, Slack checks `<@BOT_USER_ID>` in text. Bot mention text is stripped before sending to agent
- **Bot Identity Caching** -- Discord stores bot user ID from `ready` event, Telegram fetches `@username` via `get_me()` at startup, Slack fetches bot user ID via `auth.test` at startup
- **Troubleshooting Section in README** -- Documents the known session corruption issue where agent hallucinates tool calls, with workaround (start new session)

### Fixed
- **Pending Tool Approvals Hanging Agent** -- Approval callbacks were never resolved on cancel, error, supersede, or agent completion, causing the agent to hang indefinitely. All code paths now properly deny pending approvals with `response_tx.send()`
- **Stale Approval Cleanup** -- Cancel (Escape), error handler, new request, and agent completion all now send deny responses before marking approvals as denied
- **Rustls Crypto Provider for Slack** -- Install `ring` crypto provider at startup before any TLS connections, fixing Slack Socket Mode panics

### Changed
- **Proactive Message Branding Removed** -- `discord_send`, `slack_send`, `telegram_send` tools no longer prepend `MSG_HEADER` to outgoing messages
- **Agent Logging** -- Improved iteration logging: shows "completed after N tool iterations" or "responded with text only"
- **Auto-Approve Feedback** -- Selecting "Allow Always" now shows a system message confirming auto-approve is enabled for the session

## [0.2.13] - 2026-02-17

### Added
- **Proactive WhatsApp Messaging** -- New `whatsapp_send` agent tool lets the agent send messages to the user (or any allowed phone) at any time, not just in reply to incoming messages
- **WhatsApp Welcome Message** -- On successful QR pairing, the agent sends a fun random crab greeting to the owner's WhatsApp automatically
- **WhatsApp Message Branding** -- All outgoing WhatsApp messages are prefixed with `🦀 *OpenCrabs*` header so users can distinguish agent replies from their own messages
- **WhatsApp `device_sent_message` Unwrapping** -- Recursive `unwrap_message()` handles WhatsApp's nested message wrappers (`device_sent_message`, `ephemeral_message`, `view_once_message`, `document_with_caption_message`) to extract actual text content from linked-device messages
- **Fun Startup/Shutdown Messages** -- Random crab-themed greetings on launch and farewell messages on exit (10 variants each)

### Fixed
- **WhatsApp Self-Chat Messages Ignored** -- Messages from the user's own phone were dropped because `is_from_me: true`; now only skips messages with the agent's `MSG_HEADER` prefix to prevent echo loops while accepting user messages from linked devices
- **WhatsApp Phone Format Mismatch** -- Allowlist comparison failed because config stored `+351...` but JID user part was `351...`; `sender_phone()` now strips `@s.whatsapp.net` suffix, allowlist check strips `+` prefix
- **Model Name Missing from Thinking Spinner** -- "is thinking" showed without model name because `session.model` could be `Some("")`; added `.filter(|m| !m.is_empty())` fallback to `default_model_name`
- **WhatsApp SQLx Store Device Serialization** -- Device state now serialized via `rmp-serde` (MessagePack) instead of broken `bincode`; added `rmp-serde` dependency under whatsapp feature

### Changed
- **`wacore-binary` Direct Dependency** -- Added as direct optional dependency for `Jid` type access (needed by `whatsapp_send` and `whatsapp_connect` tools for JID parsing)

### Removed
- **`/model` Slash Command** -- Removed redundant `/model` command; `/models` already provides model switching with selected-model display

## [0.2.12] - 2026-02-17

### Added
- **WhatsApp Integration** -- Chat with your agent via WhatsApp Web. Connect dynamically at runtime ("connect my WhatsApp") or from the onboarding wizard. QR code pairing displayed in terminal using Unicode block characters, session persists across restarts via SQLite
- **WhatsApp Image Support** -- Send images to the agent via WhatsApp; they're downloaded, base64-encoded, and forwarded to the AI backend for multimodal analysis
- **WhatsApp Connect Tool** -- New `whatsapp_connect` agent tool: generates QR code, waits for scan (2 min timeout), spawns persistent listener, updates config automatically
- **Onboarding: Messaging Setup** -- New step in both QuickStart and Advanced onboarding modes to enable Telegram and/or WhatsApp channels right after provider auth
- **Channel Factory** -- Shared `ChannelFactory` for creating channel agent services at runtime, used by both static startup and dynamic connection tools
- **Custom SQLx WhatsApp Store** -- `wacore::store::Backend` implementation using the project's existing `sqlx` SQLite driver, avoiding the `libsqlite3-sys` version conflict with `whatsapp-rust-sqlite-storage` (Diesel-based). 15 tables, 33 trait methods, full test coverage
- **Nightly Rust Requirement** -- `wacore-binary` requires `#![feature(portable_simd)]`; added `rust-toolchain.toml` pinning to nightly. Local patch for `wacore-binary` fixes `std::simd::Select` API breakage on latest nightly

### Changed
- **Version Numbering** -- Corrected from 0.2.2 to 0.2.11 (following 0.2.1), this release is 0.2.12

## [0.2.11] - 2026-02-16

### Fixed
- **Context Token Display** -- TUI context indicator showed inflated values (e.g. `640K/200K`) because `input_tokens` was accumulated across all tool-loop iterations instead of using the last API call's actual context size; now `AgentResponse.context_tokens` tracks the last iteration's `input_tokens` for accurate display while `usage` still accumulates for correct billing
- **Per-Message Token Count** -- `DisplayMessage.token_count` now shows only output tokens (the actual generated content) instead of the inflated `input + output` sum which double-counted shared context
- **Clippy Warning** -- Fixed `redundant_closure` warning in `trim_messages_to_budget`

### Changed
- **Compaction Threshold** -- Lowered auto-compaction trigger from 80% to 70% of context window for earlier, safer compaction with more headroom
- **Token Counting** -- `trim_messages_to_budget` now uses tiktoken (`cl100k_base`) instead of `chars/3` heuristic; history budget targets 60% of context window (was 70%) to leave more room for tool results

### Added
- **2 New Tests** -- `test_context_tokens_is_last_iteration_not_accumulated` and `test_context_tokens_equals_input_tokens_without_tools` verifying correct context vs billing token separation (450 total)

### Removed
- **Dead Code** -- Removed unused `format_token_count` function and its 5 tests from `render.rs`

## [0.2.1] - 2026-02-16

### Added
- **Config Management Tool** -- New `config_manager` agent tool with 6 operations: `read_config`, `write_config`, `read_commands`, `add_command`, `remove_command`, `reload`; the agent can now read/write `config.toml` and `commands.toml` at runtime
- **Commands TOML Migration** -- User-defined slash commands now stored in `commands.toml` (`[[commands]]` array) instead of `commands.json`; existing `commands.json` files auto-migrate on first load
- **Settings TUI Screen** -- Press `S` for a real Settings screen showing: current provider/model, approval policy, user commands summary, QMD memory search status, and file paths (config, brain, working directory)
- **Approval Policy Persistence** -- `/approve` command now saves the selected policy to `[agent].approval_policy` in `config.toml`; policy is restored on startup instead of always defaulting to "ask"
- **AgentConfig Section** -- New `[agent]` config section with `approval_policy` ("ask" / "auto-session" / "auto-always") and `max_concurrent` (default: 4) fields
- **Live Config Reload** -- `Config::reload()` method and `TuiEvent::ConfigReloaded` event for refreshing cached config values after tool writes
- **Config Write Helper** -- `Config::write_key(section, key, value)` safely merges key-value pairs into `config.toml` without overwriting unrelated sections
- **Command Management Helpers** -- `CommandLoader::add_command()` and `CommandLoader::remove_command()` for atomic command CRUD
- **20 New Tests** -- 14 onboarding tests (key handlers, mode select, provider navigation, API key input, field flow, validation, model selection, workspace/health/brain defaults) + 6 config tests (AgentConfig defaults, TOML parsing, write_key merge, save round-trip) -- 443 total

### Changed
- **config.toml.example** -- Added `[agent]` and `[voice]` example sections with documentation
- **Commands Auto-Reload** -- After `ConfigReloaded` event, user commands are refreshed from `commands.toml`

## [0.2.0] - 2026-02-15

### Added
- **3-Tier Memory System** -- OpenCrabs now has a layered memory architecture: (1) **Brain MEMORY.md** -- user-curated durable memory loaded into system brain every turn, (2) **Daily Memory Logs** -- auto-compaction summaries saved to `~/.opencrabs/memory/YYYY-MM-DD.md` with multiple compactions per day stacking in the same file, (3) **Memory Search** -- `memory_search` tool backed by QMD for semantic search across all past daily logs
- **Memory Search Tool** -- New `memory_search` agent tool searches past conversation logs via QMD (`qmd query --json`); gracefully degrades if QMD is not installed, returning a hint to use `read_file` on daily logs directly
- **Compaction Summary Display** -- Auto-compaction at 80% context now shows the full summary in chat as a system message instead of running silently; users see exactly what the agent remembered
- **Scroll While Streaming** -- Users can scroll up during streaming without being yanked back to the bottom; `auto_scroll` flag disables on user scroll, re-enables when scrolled back to bottom or on message send
- **QMD Auto-Index** -- After each compaction, `qmd update` is triggered in the background to keep the memory search index current
- **Memory Module** -- New `src/memory/mod.rs` module with QMD wrapper: availability check, collection management, search, and background re-indexing
- **Path Consolidation** -- All data now lives under `~/.opencrabs/` (config, database, brain, memory, history, logs)
- **Context Budget Awareness** -- Tool definition overhead (~500 tokens per tool) now factored into context usage calculation, preventing "prompt too long" errors

### Changed
- **Compaction Target** -- Compaction summaries now write to daily logs (`~/.opencrabs/memory/YYYY-MM-DD.md`) instead of appending to brain workspace `MEMORY.md`; brain `MEMORY.md` remains user-curated and untouched by auto-compaction
- **Local Timestamps** -- Daily memory logs use `chrono::Local` instead of UTC for human-readable timestamps

## [0.1.9] - 2026-02-15

### Added
- **Cursor Navigation** -- Full cursor movement in input: Left/Right arrows, Ctrl+Left/Right word jump, Home/End, Delete key, Backspace at cursor position, word delete (Alt/Ctrl+Backspace), character and paste insertion at cursor position, cursor renders at correct position
- **Input History Persistence** -- Command history saved to `~/.config/opencrabs/history.txt` (one line per entry), loaded on startup, appended on each send, capped at 500 entries, survives restarts
- **Real-time Streaming** -- Added `stream_complete()` method that streams text chunks from the provider via `StreamingChunk` progress events, replacing the old blocking `provider.complete()` call
- **Streaming Spinner** -- Animated spinner shows `"claude-opus is responding..."` with streamed text below; `"thinking..."` spinner shows only before streaming begins
- **Inline Plan Approval** -- Plan approval now renders as an interactive inline selector with arrow keys (Approve / Reject / Request Changes / View Plan) instead of plain text Ctrl key instructions
- **Telegram Photo Support** -- Incoming photos download at largest resolution, saved to temp file, forwarded as `<<IMG:path>>` caption; image documents detected via `image/*` MIME type; temp files cleaned up after 30 seconds
- **Error Message Rendering** -- `app.error_message` is now rendered in the chat UI (was previously set but never displayed)
- **Default Model Name** -- New sessions show the actual provider model name (e.g. `claude-opus-4-6`) as placeholder instead of generic "AI"
- **Debug Logging** -- `DEBUG_LOGS_LOCATION` env var sets custom log directory; `--debug` CLI flag enables debug mode
- **8 New Tests** -- `stream_complete_text_only`, `stream_complete_with_tool_use`, `streaming_chunks_emitted`, `markdown_to_telegram_html_*`, `escape_html`, `img_marker_format` (412 total)

### Fixed
- **SSE Parser Cross-Chunk Buffering** -- TCP chunks splitting JSON events mid-string caused `EOF while parsing a string` errors and silent response drops; parser now buffers partial lines across chunks with `Arc<Mutex<String>>`, only parsing complete newline-terminated lines
- **Stale Approval Cleanup** -- Old `Pending` approval messages permanently hid streaming responses; now cleared on new message send, new approval request, and response completion
- **Approval Dialog Reset** -- `approval_auto_always` reset on session create/load; inline "Always" now sets `approval_auto_session` (resets on session change) instead of `approval_auto_always`
- **Brain File Path** -- Brain prompt builder used wrong path for workspace files
- **Abort During Streaming** -- Cancel token properly wired through streaming flow for Escape×2 abort

### Changed
- **README** -- Expanded self-sustaining section with `/rebuild` command, `SelfUpdater` module, session persistence, brain live-editing documentation

## [0.1.8] - 2026-02-15

### Added
- **Image Input Support** -- Paste image paths or URLs into the input; auto-detected and attached as vision content blocks for multimodal models (handles paths with spaces)
- **Attachment Indicator** -- Attached images show as `[IMG1:filename.png]` in the input box title bar; user messages display `[IMG: filename.png]`
- **Tool Context Persistence** -- Tool call groups are now saved to the database and reconstructed on session reload; no more vanishing tool history
- **Intermediate Text Display** -- Agent text between tool call batches now appears interleaved in the chat, matching Claude Code's behavior

### Fixed
- **Tool Descriptions Showing "?"** -- Approval dialog showed "Edit ?" instead of file paths; fixed parameter key mismatches (`path` not `file_path`, `operation` not `action`)
- **Raw Tool JSON in Chat** -- `[Tool: read_file]{json}` was dumped into assistant messages; now only text blocks are displayed, tool calls shown via the tool group UI
- **Loop Detection Wrong Keys** -- Tool loop detection used `file_path` for read/write/edit; fixed to `path`
- **Telegram Text+Voice Order** -- Text reply now always sent first, voice note follows (was skipping text on TTS success)

### Changed
- **base64 dependency** -- Re-added `base64 = "0.22.1"` for image encoding (was removed in dep cleanup but now needed)

## [0.1.7] - 2026-02-14

### Added
- **Voice Integration (STT)** -- Incoming Telegram voice notes are transcribed via Groq Whisper (`whisper-large-v3-turbo`) and processed as text by the agent
- **Voice Integration (TTS)** -- Agent replies to voice notes with audio via OpenAI TTS (`gpt-4o-mini-tts`, `ash` voice); falls back to text if TTS is disabled or fails
- **Onboarding: Telegram Setup** -- New wizard step with BotFather instructions, bot token input (masked), and user ID guidance; auto-detects existing env/keyring values
- **Onboarding: Voice Setup** -- New wizard step for Groq API key (STT) and TTS toggle with `ash` voice label; auto-detects `GROQ_API_KEY` from environment
- **Sessions Dialog: Context Info** -- `/sessions` now shows token count per session (`12.5K tok`, `2.1M tok`) and live context window percentage for the current session with color coding (green/yellow/red)
- **Tool Descriptions in Approval** -- Approval dialog now shows actual file paths and parameters (e.g. "Edit /src/tui/render.rs") instead of raw tool names ("edit_file")
- **Shared Telegram Session** -- Owner's Telegram messages now use the same session as the TUI terminal; no more separate sessions that could pick the wrong model

### Changed
- **Provider Priority** -- Factory order changed to Qwen → Anthropic → OpenAI; Anthropic is now always preferred over OpenAI for text generation
- **OPENAI_API_KEY Isolation** -- `OPENAI_API_KEY` no longer auto-creates an OpenAI text provider; it is only used for TTS (`gpt-4o-mini-tts`), never for text generation unless explicitly configured
- **Async Terminal Events** -- Replaced blocking `crossterm::event::poll()` with async `EventStream` + `tokio::select!` to prevent TUI freezes during I/O-heavy operations

### Fixed
- **Model Contamination** -- `OPENAI_API_KEY` in `.env` was causing GPT-4 to be used for text instead of Anthropic Claude; multi-layered fix across factory, env overrides, and TTS key sourcing
- **Navigation Slowdown** -- TUI became sluggish after losing terminal focus due to synchronous 100ms blocking poll in async context
- **Context Showing 0%** -- Loading an existing session showed 0% context; now estimates tokens from message content until real API usage arrives
- **Approval Spam** -- "edit_file -- approved" messages no longer clutter the chat; approved tool calls are silently removed since the tool group already shows execution progress
- **6 Clippy Warnings** -- Fixed collapsible_if (5) and manual_find (1) across onboarding and telegram modules

## [0.1.6] - 2026-02-14

### Added
- **Telegram Bot Integration** -- Chat with OpenCrabs via Telegram alongside the TUI; bot runs as a background task with full tool access (file ops, search, bash, etc.)
- **Telegram Allowlist** -- Only allowlisted Telegram user IDs can interact; `/start` command shows your ID for easy setup
- **Telegram Markdown→HTML** -- Agent responses are formatted as Telegram-safe HTML with code blocks, inline code, bold, and italic support
- **Telegram Message Splitting** -- Long responses automatically split at 4096-char Telegram limit, breaking at newlines
- **Grouped Tool Calls** -- Multiple tool calls in a single agent turn now display as a collapsible group with tree lines (├─ └─) instead of individual messages
- **Claude Code-Style Approval** -- Tool approval dialog rewritten as vertical selector with `❯ Yes / Always / No` matching Claude Code's UX
- **Emergency Compaction Retry** -- If the LLM provider returns "prompt too long", automatically compact context and retry instead of failing

### Changed
- **Token Estimation** -- Changed from `chars/4` to `chars/3` for more conservative estimation, preventing context overflows that the old estimate missed
- **Compaction Accounts for Tools** -- Auto-compaction threshold now reserves ~500 tokens per registered tool for schema overhead, preventing "prompt too long" errors
- **Telegram Feature Default** -- `telegram` feature now included in default features (no need for `--features telegram`)

### Fixed
- **Context % Showing 2369%** -- `context_usage_percent()` was summing all historical token counts; now uses only the latest response's `input_tokens`
- **TUI Lag After First Request** -- `active_tool_group` wasn't cleaned up on error/abort paths, causing UI to hang
- **Telegram Bot No Response** -- Bot was calling `send_message` (no tools) instead of `send_message_with_tools`; also needed `auto_approve_tools: true` since there's no TUI for approval

## [0.1.5] - 2026-02-14

### Added
- **Context Usage Indicator** -- Input box shows live `Context: X%` with color coding: green (<60%), yellow (60-80%), red (>80%) so you always know how close you are to the context limit
- **Auto-Compaction** -- When context usage exceeds 80%, automatically sends conversation to the LLM for a structured breakdown summary (Current Task, Key Decisions, Files Modified, Current State, Important Context, Errors & Solutions), saves to MEMORY.md, and trims context keeping the last 8 messages + summary for seamless continuation
- **`/compact` Command** -- Manually trigger context compaction at any time via slash command
- **Brave Search Tool** -- Real-time web search via Brave Search API (set `BRAVE_API_KEY`); great if you already have a Brave API key or want a free-tier option
- **EXA Search Tool** -- Neural-powered web search via EXA AI; works out of the box via free hosted MCP endpoint (no API key needed). Set `EXA_API_KEY` for direct API access with higher rate limits

### Changed
- **EXA Always Available** -- EXA search registers unconditionally via free MCP endpoint; Brave still requires `BRAVE_API_KEY`

## [0.1.4] - 2026-02-14

### Added
- **Inline Tool Progress** -- Tool executions now show inline in chat with human-readable descriptions (e.g. "Read src/main.rs", "bash: cargo check", "Edited src/app.rs") instead of invisible spinner
- **Expand/Collapse Tool Details** -- Press Ctrl+O to expand or collapse tool output details on completion messages, inspired by Claude Code's UX
- **Abort Processing** -- Press Escape twice within 3 seconds to cancel an in-progress agent request via CancellationToken
- **Active Input During Processing** -- Input box stays active with cursor visible while agent is processing; border remains steel blue
- **Processing Guard** -- Prevents sending a second message while one is already processing; shows "Please wait or press Esc x2 to abort"
- **Progress Callback System** -- New `ProgressCallback` / `ProgressEvent` architecture emitting `Thinking`, `ToolStarted`, and `ToolCompleted` events from agent service to TUI
- **LLM-Controlled Bash Timeout** -- Bash tool now accepts `timeout_secs` from the LLM (capped at 600s), default raised from 30s to 120s

### Changed
- **Silent Auto-Approved Tools** -- Auto-approved tool calls no longer spam the chat; only completion descriptions shown
- **Approval Never Times Out** -- Tool approval requests wait indefinitely until the user acts (no more 5-minute timeout)
- **Approval UI De-Emojified** -- All emojis removed from approval rendering; clean text-only UI
- **Yolo Mode Always Visible** -- All three approval tiers (Allow once, Allow all session, Yolo mode) always visible with color-coding (green/yellow/red) in inline approval

### Fixed
- **Race Condition on Double Send** -- Added `is_processing` guard in `send_message()` preventing overlapping agent requests

## [0.1.3] - 2026-02-14

### Added
- **Inline Tool Approval** — Tool permission requests now render inline in chat instead of a blocking overlay dialog, with three options: Allow once, Allow all for this task, Allow all moving forward
- **`/approve` Command** — Resets tool approval policy back to "always ask"
- **Word Deletion** — Ctrl+Backspace and Alt+Backspace delete the last word in input
- **Scroll Support** — Arrow keys and Page Up/Down now scroll Help, Sessions, and Settings screens
- **Tool Approval Docs** — README section documenting inline approval keybindings and options

### Changed
- **Ctrl+C Behavior** — First press clears input, second press within 3 seconds quits (was immediate quit)
- **Help Screen** — Redesigned as 2-column layout filling full terminal width instead of narrow single column
- **Status Bar Removed** — Bottom status bar eliminated for cleaner UI; mode info shown in header only
- **Ctrl+H Removed** — Help shortcut removed (use `/help` instead); fixes Ctrl+Backspace conflict where terminals send Ctrl+H for Ctrl+Backspace

### Removed
- **MCP Module** — Deleted empty placeholder `src/mcp/` directory (unused stubs, zero functionality)
- **Overlay Approval Dialog** — Replaced by inline approval in chat
- **Bottom Status Bar** — Removed entirely for more screen space

## [0.1.2] - 2026-02-14

### Added
- **Onboarding Wizard** — 8-step wizard with QuickStart/Advanced modes for first-time setup
- **AI Brain Personalization** — Generates all 6 workspace brain files (SOUL, IDENTITY, USER, AGENTS, TOOLS, MEMORY) from user input during onboarding
- **Session Management** — `/sessions` command, rename sessions (R), delete sessions (D) from session list
- **Mouse Scroll** — Mouse wheel scrolls chat history
- **Dynamic Input Height** — Input area grows with content, 1-line default
- **Screenshots** — Added UI screenshots to README (splash, onboarding, chat)

### Changed
- **Unified Anthropic Provider** — Auto-detects OAuth tokens vs API keys from env/keyring
- **Pre-wrapped Chat Lines** — Consistent left padding for all chat messages
- **Updated Model List** — Added `claude-opus-4-6`, `gpt-5.1-codex-mini`, `gemini-3-flash-preview`, `qwen3-coder-next`
- **Cleaner UI** — Removed emojis, reordered status bar
- **README** — Added screenshots, updated structure

[0.1.2]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.2

## [0.1.1] - 2026-02-14

### Added
- **Dynamic Brain System** — Replace hardcoded system prompt with brain loader that reads workspace MD files (SOUL, IDENTITY, USER, AGENTS, TOOLS, MEMORY) per-turn from `~/opencrab/brain/workspace/`
- **CommandLoader** — User-defined slash commands via `commands.json`, auto-reloaded after each agent response
- **SelfUpdater** — Build/test/restart via Unix `exec()` for hot self-update (`/rebuild` command)
- **RestartPending Mode** — Confirmation dialog in TUI after successful rebuild
- **Onboarding Docs** — Scaffolding for onboarding documentation

### Changed
- **system_prompt → system_brain** — Renamed across entire codebase to reflect dynamic brain architecture
- **`/help` Fixed** — Opens Help dialog instead of pushing text message into chat

[0.1.1]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.1

## [0.1.0] - 2026-02-14

### Added
- **Anthropic OAuth Support** — Claude Max / setup-token authentication via `ANTHROPIC_MAX_SETUP_TOKEN` with automatic `sk-ant-oat` prefix detection, `Authorization: Bearer` header, and `anthropic-beta: oauth-2025-04-20` header
- **Claude 4.x Models** — Support for `claude-opus-4-6`, `claude-sonnet-4-5-20250929`, `claude-haiku-4-5-20251001` with updated pricing and context windows
- **`.env` Auto-Loading** — `dotenvy` integration loads `.env` at startup automatically
- **CHANGELOG.md** — Project changelog following Keep a Changelog format
- **New Branding** — OpenCrab ASCII art, "Shell Yeah! AI Orchestration at Rust Speed." tagline, crab icon throughout

### Changed
- **Rust Edition 2024** — Upgraded from edition 2021 to 2024
- **All Dependencies Updated** — Every crate bumped to latest stable (ratatui 0.30, crossterm 0.29, pulldown-cmark 0.13, rand 0.9, dashmap 6.1, notify 8.2, git2 0.20, zip 6.0, tree-sitter 0.25, thiserror 2.0, and more)
- **Rebranded** — "OpenCrab AI Assistant" renamed to "OpenCrab AI Orchestration Agent" across all source files, splash screen, TUI header, system prompt, and documentation
- **Enter to Send** — Changed message submission from Ctrl+Enter (broken in many terminals) to plain Enter; Alt+Enter / Shift+Enter inserts newline for multi-line input
- **Escape Double-Press** — Escape now requires double-press within 3 seconds to clear input, preventing accidental loss of typed messages
- **TUI Header Model Display** — Header now shows the provider's default model immediately instead of "unknown" until first response
- **Splash Screen** — Updated with OpenCrab ASCII art, new tagline, and author attribution
- **Default Max Tokens** — Increased from 4096 to 16384 for modern Claude models
- **Default Model** — Changed from `claude-3-5-sonnet-20240620` to `claude-sonnet-4-5-20250929`
- **README.md** — Complete rewrite: badges, table of contents, OAuth documentation, updated providers/models, concise structure (764 lines vs 3,497)
- **Project Structure** — Moved `tests/`, `migrations/`, `benches/`, `docs/` inside `src/` and updated all references

### Fixed
- **pulldown-cmark 0.13 API** — `Tag::Heading` tuple to struct variant, `Event::End` wraps `TagEnd`, `Tag::BlockQuote` takes argument
- **ratatui 0.29+** — `f.size()` replaced with `f.area()`, `Backend::Error` bounds added (`Send + Sync + 'static`)
- **rand 0.9** — `thread_rng()` replaced with `rng()`, `gen_range()` replaced with `random_range()`
- **Edition 2024 Safety** — Removed unsafe `std::env::set_var`/`remove_var` from tests, replaced with TOML config parsing

### Removed
- Outdated "Claude Max OAuth is NOT supported" disclaimer (it now is)
- Sprint history and "coming soon" filler from README
- Old "Crusty" branding and attribution

[0.3.10]: https://github.com/adolfousier/opencrabs/compare/v0.3.10...v0.3.11
[0.3.9]: https://github.com/adolfousier/opencrabs/compare/v0.3.9...v0.3.10
[0.3.8]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.8
[0.3.7]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.7
[0.3.6]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.6
[0.3.5]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.5
[0.3.4]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.4
[0.3.3]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.3
[0.3.2]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.2
[0.3.1]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.1
[0.3.0]: https://github.com/adolfousier/opencrabs/releases/tag/v0.3.0
[0.2.99]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.99
[0.2.98]: https://github.com/adolfousier/opencrabs/compare/v0.2.97...v0.2.98
[0.2.97]: https://github.com/adolfousier/opencrabs/compare/v0.2.96...v0.2.97
[0.2.96]: https://github.com/adolfousier/opencrabs/compare/v0.2.95...v0.2.96
[0.2.95]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.95
[0.2.94]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.94
[0.2.93]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.93
[0.2.92]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.92
[0.2.91]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.91
[0.2.90]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.90
[0.2.89]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.89
[0.2.88]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.88
[0.2.87]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.87
[0.2.86]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.86
[0.2.85]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.85
[0.2.84]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.84
[0.2.83]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.83
[0.2.82]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.82
[0.2.81]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.81
[0.2.80]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.80
[0.2.79]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.79
[0.2.78]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.78
[0.2.77]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.77
[0.2.76]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.76
[0.2.75]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.75
[0.2.74]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.74
[0.2.73]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.73
[0.2.72]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.72
[0.2.71]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.71
[0.2.70]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.70
[0.2.69]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.69
[0.2.68]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.68
[0.2.67]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.67
[0.2.66]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.66
[0.2.65]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.65
[0.2.64]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.64
[0.2.63]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.63
[0.2.62]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.62
[0.2.61]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.61
[0.2.60]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.60
[0.2.59]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.59
[0.2.58]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.58
[0.2.57]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.57
[0.2.56]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.56
[0.2.55]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.55
[0.2.54]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.54
[0.2.53]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.53
[0.2.52]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.52
[0.2.51]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.51
[0.2.50]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.50
[0.2.49]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.49
[0.2.48]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.48
[0.2.47]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.47
[0.2.46]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.46
[0.2.45]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.45
[0.2.44]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.44
[0.2.43]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.43
[0.2.42]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.42
[0.2.41]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.41
[0.2.40]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.40
[0.2.39]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.39
[0.2.38]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.38
[0.2.37]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.37
[0.2.36]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.36
[0.2.35]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.35
[0.2.34]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.34
[0.2.33]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.33
[0.2.32]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.32
[0.2.31]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.31
[0.2.30]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.30
[0.2.29]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.29
[0.2.28]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.28
[0.2.27]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.27
[0.2.26]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.26
[0.2.25]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.25
[0.2.24]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.24
[0.2.23]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.23
[0.2.22]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.22
[0.2.21]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.21
[0.2.20]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.20
[0.2.19]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.19
[0.2.18]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.18
[0.2.17]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.17
[0.2.16]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.16
[0.2.15]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.15
[0.2.14]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.14
[0.2.13]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.13
[0.2.12]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.12
[0.2.11]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.11
[0.2.1]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.1
[0.2.0]: https://github.com/adolfousier/opencrabs/releases/tag/v0.2.0
[0.1.9]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.9
[0.1.8]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.8
[0.1.7]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.7
[0.1.6]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.6
[0.1.5]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.5
[0.1.4]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.4
[0.1.3]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.3
[0.1.2]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.2
[0.1.1]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.1
[0.1.0]: https://github.com/adolfousier/opencrabs/releases/tag/v0.1.0
[0.3.34]: https://github.com/adolfousier/opencrabs/compare/v0.3.33...v0.3.34

[0.3.35]: https://github.com/adolfousier/opencrabs/compare/v0.3.34...v0.3.35
[0.3.36]: https://github.com/adolfousier/opencrabs/compare/v0.3.35...v0.3.36

[0.3.37]: https://github.com/adolfousier/opencrabs/compare/v0.3.36...v0.3.37

[0.3.38]: https://github.com/adolfousier/opencrabs/compare/v0.3.37...v0.3.38
[0.3.39]: https://github.com/adolfousier/opencrabs/compare/v0.3.38...v0.3.39
[0.3.40]: https://github.com/adolfousier/opencrabs/compare/v0.3.39...v0.3.40

[0.3.41]: https://github.com/adolfousier/opencrabs/compare/v0.3.40...v0.3.41
[0.3.42]: https://github.com/adolfousier/opencrabs/compare/v0.3.41...v0.3.42
[0.3.43]: https://github.com/adolfousier/opencrabs/compare/v0.3.42...v0.3.43
[0.3.44]: https://github.com/adolfousier/opencrabs/compare/v0.3.43...v0.3.44
[0.3.45]: https://github.com/adolfousier/opencrabs/compare/v0.3.44...v0.3.45
[0.3.46]: https://github.com/adolfousier/opencrabs/compare/v0.3.45...v0.3.46
[0.3.47]: https://github.com/adolfousier/opencrabs/compare/v0.3.46...v0.3.47
[0.3.48]: https://github.com/adolfousier/opencrabs/compare/v0.3.47...v0.3.48
[0.3.49]: https://github.com/adolfousier/opencrabs/compare/v0.3.48...v0.3.49
[0.3.50]: https://github.com/adolfousier/opencrabs/compare/v0.3.49...v0.3.50
[0.3.51]: https://github.com/adolfousier/opencrabs/compare/v0.3.50...v0.3.51
[0.3.52]: https://github.com/adolfousier/opencrabs/compare/v0.3.51...v0.3.52
[0.3.53]: https://github.com/adolfousier/opencrabs/compare/v0.3.52...v0.3.53
[0.3.54]: https://github.com/adolfousier/opencrabs/compare/v0.3.53...v0.3.54
[0.3.55]: https://github.com/adolfousier/opencrabs/compare/v0.3.54...v0.3.55
[0.3.56]: https://github.com/adolfousier/opencrabs/compare/v0.3.55...v0.3.56
[0.3.57]: https://github.com/adolfousier/opencrabs/compare/v0.3.56...v0.3.57
[0.3.58]: https://github.com/adolfousier/opencrabs/compare/v0.3.57...v0.3.58
[0.3.59]: https://github.com/adolfousier/opencrabs/compare/v0.3.58...v0.3.59
[0.3.60]: https://github.com/adolfousier/opencrabs/compare/v0.3.59...v0.3.60
[0.3.61]: https://github.com/adolfousier/opencrabs/compare/v0.3.60...v0.3.61
[0.3.62]: https://github.com/adolfousier/opencrabs/compare/v0.3.61...v0.3.62
[0.3.63]: https://github.com/adolfousier/opencrabs/compare/v0.3.62...v0.3.63
[0.3.64]: https://github.com/adolfousier/opencrabs/compare/v0.3.63...v0.3.64
[0.3.65]: https://github.com/adolfousier/opencrabs/compare/v0.3.64...v0.3.65
[0.3.66]: https://github.com/adolfousier/opencrabs/compare/v0.3.65...v0.3.66
[0.3.67]: https://github.com/adolfousier/opencrabs/compare/v0.3.66...v0.3.67
[0.3.68]: https://github.com/adolfousier/opencrabs/compare/v0.3.67...v0.3.68
[0.3.69]: https://github.com/adolfousier/opencrabs/compare/v0.3.68...v0.3.69

[0.3.70]: https://github.com/adolfousier/opencrabs/compare/v0.3.69...v0.3.70
[0.3.71]: https://github.com/adolfousier/opencrabs/compare/v0.3.70...v0.3.71
[0.3.72]: https://github.com/adolfousier/opencrabs/compare/v0.3.71...v0.3.72
[0.3.73]: https://github.com/adolfousier/opencrabs/compare/v0.3.72...v0.3.73
[0.3.74]: https://github.com/adolfousier/opencrabs/compare/v0.3.73...v0.3.74
[0.3.75]: https://github.com/adolfousier/opencrabs/compare/v0.3.74...v0.3.75
[0.3.77]: https://github.com/adolfousier/opencrabs/compare/v0.3.75...v0.3.77
[0.3.78]: https://github.com/adolfousier/opencrabs/compare/v0.3.77...v0.3.78
[0.3.79]: https://github.com/adolfousier/opencrabs/compare/v0.3.78...v0.3.79
[0.3.80]: https://github.com/adolfousier/opencrabs/compare/v0.3.79...v0.3.80
[0.3.81]: https://github.com/adolfousier/opencrabs/compare/v0.3.80...v0.3.81
[0.3.82]: https://github.com/adolfousier/opencrabs/compare/v0.3.81...v0.3.82
[0.3.83]: https://github.com/adolfousier/opencrabs/compare/v0.3.82...v0.3.83
[0.5.0]: https://github.com/adolfousier/opencrabs/compare/v0.3.83...v0.5.0
[0.5.3]: https://github.com/adolfousier/opencrabs/compare/v0.5.2...v0.5.3
[0.5.4]: https://github.com/adolfousier/opencrabs/compare/v0.5.3...v0.5.4
[0.5.5]: https://github.com/opencrabs/opencrabs/compare/v0.5.4...v0.5.5
[0.5.10]: https://github.com/opencrabs/opencrabs/compare/v0.5.5...v0.5.10
[0.5.11]: https://github.com/opencrabs/opencrabs/compare/v0.5.10...v0.5.11
[0.5.12]: https://github.com/opencrabs/opencrabs/compare/v0.5.11...v0.5.12
[0.5.13]: https://github.com/opencrabs/opencrabs/compare/v0.5.12...v0.5.13
[0.5.14]: https://github.com/opencrabs/opencrabs/compare/v0.5.13...v0.5.14
[0.5.15]: https://github.com/opencrabs/opencrabs/compare/v0.5.14...v0.5.15
