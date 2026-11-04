---
name: motion-graphics
description: "Build videos and motion graphics with Remotion: narrated step-by-step tutorials for an app (install, setup, channel connect), with voice-over, subtitles and a drawn UI. Pick the playbook below that fits the request. (/motion-graphics, tutorial video, onboarding video, remotion, voice-over, subtitles)"
---

# Motion Graphics

Videos are code here: Remotion renders every frame as a function of time, so a video is a repo you can diff, re-render and keep in sync with the docs it teaches.

## Pick a playbook

| The user wants | Playbook |
|---|---|
| A step-by-step tutorial for an app: install, first run, setup wizard, connecting a channel or provider | **Tutorials** (below) |

Nothing else ships yet. For a promo, trailer or anything not in the table, say there is no playbook for it, and borrow only the rules that still apply from the tutorials playbook (the shared rules, the checks, the commit discipline). Never present a guessed pipeline as the skill's.

## Shared rules (every playbook)

1. **Plan before building.** Write the storyboard as a file (scene, time, on-screen text, position, sound, VO) and get the user's yes before any code or render.
2. **Every claim on screen has a source:** the repo, a release asset, a measurement, or the live docs. Anything else is cut or labelled "dramatized".
3. **No real secrets, ever.** Keys, tokens and passwords are hardcoded masks (`sk-••••••••`). Never blur: a blur can be reversed and the raw file still holds the secret.
4. **Use what the user already has configured.** VO comes from their configured TTS provider and every line goes back through their configured STT. Never fall back to a different engine silently.
5. **Prove the render, do not narrate it.** Exit code, frame count, duration, loudness, and STT on the final file. Look at a contact sheet of stills before every full render.
6. **Commit after every step** and never publish (upload, post, push) without the owner's explicit yes.
7. **Do not compete with CI.** If this machine also runs CI, wait for it to go idle before a full render.

## Playbook: Tutorials

One video per docs section. A drawn terminal or app UI, voice-over, burned-in subtitles plus an `.srt`, chapters and a copy-paste description. v1 covers terminal and TUI apps.

### Starter

Clone `https://github.com/opencrabs/onboard-tutorials`. It holds the shared episode engine (drawn terminal, wizard mock, cards, subtitles, timeline builder), the `vo.sh` / `stt.sh` / `fit.sh` / `probe.mjs` / `stale.mjs` scripts and two finished episodes to copy from. Its README explains how to add an episode. Do not rebuild the engine from scratch.

### Steps

1. **Scope the episode.** One docs section, one goal, and the payoff (the thing working) shown at the end. If the section needs more than about 3 minutes, split it into two episodes.
2. **Run the commands first.** Execute every command the episode teaches in a clean container of the target OS (`docker run --rm -it ubuntu:24.04` and similar), never on the user's own machine. If a docs command fails, fix the docs first, so the video and the page teach the same working commands.
3. **Script for attention.**
   - The action starts by 0:15. At most one "what you need" card.
   - No checks the audience does not need (do not make a 24.04 viewer check their glibc).
   - Each error appears at the step where it fails, never in a troubleshooting chapter after the success.
   - One recommendation per choice. Alternatives go in the description.
   - The VO never reads out what is already on screen. Something moves every few seconds.
   - Target 2 to 3 minutes.
4. **Draw the UI from the app's real render code.** Read the source that draws each screen (titles, options, footer keys, colours, layout) and copy it. Never screen-record the agent's own terminal, and never invent a screen. Keep an "illustrative terminal · recreated UI" label on screen, and label example replies as examples.
5. **Masks.** API keys, passwords and tokens are fixed mask strings in the mock. A sudo prompt echoes nothing, as a real terminal does.
6. **Voice.** Write VO lines in plain words, short sentences. Generate them with the configured TTS, trim dead air, then run every line back through the configured STT: every word must come back. Re-record lines that drop words.
7. **Check, then render.**
   - Typecheck, then render stills from every chapter into a contact sheet and look at it: overlaps, wrapped titles, cards covering what the VO talks about.
   - Full render. Read the exit code and the frame count, `ffprobe` the duration and streams, measure loudness (about -16 LUFS for tutorials).
   - Run STT on the final mp4.
   - Export the `.srt`, chapter timestamps and the description with every command ready to copy.
8. **Keep it in sync.** Add the episode to `videos.json` (docs page, section anchor, pinned docs commit). `npm run stale` flags the video when that docs section changes.
9. **Hand it over.** Draft the posts the owner asked for (for example a YouTube description, plus a short chat post with the link and the key commands). Commit. Upload a compressed copy for review only where the owner asked. Publishing is the owner's call.

### Done when

- Render exit 0 and every frame rendered.
- STT on the final file returns every VO line.
- Contact sheet checked after the last layout change.
- `.srt`, chapters and description exported, `videos.json` entry added.
- Committed. Not published until the owner says so.
