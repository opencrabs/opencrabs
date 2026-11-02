#!/usr/bin/env python3
"""Regenerate the Seeker docs index from the live sitemap.

Every path written to `src/ui_core/docs.rs` is one the docs site actually
publishes, and every title is the page's own `<title>`, so a row can never
point at a page that was renamed or dropped. Run it after the docs ship a new
page:

    python3 scripts/gen-docs-index.py

The run is deliberately strict. A published page with no keywords stops the
script instead of arriving unsearchable, and a keyword keyed to a page the site
no longer serves stops it instead of surviving as a dead row. The pinned
`SITEMAP` const is rewritten too: it is the coverage contract the test suite
reads, so a page that ships later fails the suite rather than silently missing
from the card.
"""
import re
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TARGET = REPO / "src" / "ui_core" / "docs.rs"
BASE = "https://docs.opencrabs.com"

# Keywords are the words a user types when they do not know the page name.
KEYWORDS = {
    "/introduction.html": "introduction overview what is opencrabs about start",
    "/getting-started/installation.html": "install setup download binary brew cargo build upgrade update",
    "/getting-started/configuration.html": "config configuration settings keys config.toml profiles env",
    "/getting-started/onboarding.html": "onboard first run wizard provider key setup",
    "/getting-started/cli-commands.html": "cli command commands flags shell terminal headless help",
    "/getting-started/troubleshooting.html": "error broken fails fix problem logs doctor debug",
    "/brain/providers-overview.html": "provider providers overview catalogue free local ollama models list",
    "/brain/providers.html": "provider setup api key anthropic openai gemini deepseek switch model",
    "/brain/images.html": "image images vision screenshot picture generate photo",
    "/brain/tools.html": "tool tools tools.toml dynamic tool_manage search discover",
    "/brain/commands.html": "custom commands commands.toml slash user command",
    "/brain/memory.html": "memory remember recall memory_search notes",
    "/brain/brain-files.html": "brain files soul agents user identity persona",
    "/channels/overview.html": "channels connect overview telegram discord slack whatsapp trello signal",
    "/channels/telegram.html": "telegram bot token botfather chat group channel",
    "/channels/discord.html": "discord bot token guild server channel",
    "/channels/slack.html": "slack socket mode bot token app token workspace",
    "/channels/whatsapp.html": "whatsapp qr pairing phone number connect",
    "/channels/trello.html": "trello board card api key token",
    "/features/sessions.html": "session conversation resume history rail",
    "/features/cron-jobs.html": "cron schedule timer job recurring",
    "/features/plans.html": "plan checklist tasks approval",
    "/features/a2a.html": "a2a agent to agent protocol rpc peer gateway",
    "/features/voice.html": "voice tts stt speech transcribe dictation audio speak whisper",
    "/development/building.html": "build compile from source cargo release",
    "/development/architecture.html": "architecture design internals modules structure",
    "/development/testing.html": "test tests suite cargo coverage",
    "/development/contributing.html": "contributing issue pr pull request workflow commit style",
    "/development/security.html": "security audit cve secrets permissions policy",
    "/changelog.html": "changelog release notes changes version whats new",
    "/user-stories.html": "stories case studies examples users",
}

# A page whose <title> is too generic to read alone gets a clearer row title.
RETITLE = {
    "/channels/overview.html": "Channels overview",
    "/brain/images.html": "Images and vision",
    "/brain/tools.html": "Built-in tools",
    "/brain/commands.html": "Custom commands",
    "/brain/memory.html": "Memory system",
    "/brain/brain-files.html": "Brain files",
    "/brain/providers.html": "Provider setup",
    "/brain/providers-overview.html": "Supported providers",
    "/features/a2a.html": "Agent to agent (A2A)",
    "/features/voice.html": "Voice (TTS and STT)",
    "/features/cron-jobs.html": "Cron jobs",
    "/development/building.html": "Building from source",
    "/user-stories.html": "User stories",
}


def fetch(url: str) -> str:
    with urllib.request.urlopen(url, timeout=30) as response:
        return response.read().decode("utf-8", "replace")


def sitemap_paths() -> list[str]:
    xml = fetch(f"{BASE}/sitemap.xml")
    locs = re.findall(r"<loc>([^<]+)</loc>", xml)
    return [loc.replace(BASE, "") for loc in locs]


def page_title(path: str) -> str:
    match = re.search(r"<title>(.*?)</title>", fetch(BASE + path), re.S)
    title = match.group(1).strip() if match else path
    return title.split(" - OpenCrabs")[0].replace("&amp;", "and")


def main() -> int:
    paths = sitemap_paths()
    # The individual case studies are long posts, not reference pages: indexed
    # they would outrank the page that actually answers the same word (the
    # Telegram story beating /channels/telegram.html). The hub stays.
    docs = [path for path in paths if not path.startswith("/stories/") and path != "/"]
    missing = [path for path in docs if path not in KEYWORDS]
    if missing:
        print(f"pages with no keywords: {missing}", file=sys.stderr)
        return 1
    extra = [path for path in KEYWORDS if path not in docs]
    if extra:
        print(f"keywords for unpublished pages: {extra}", file=sys.stderr)
        return 1

    entries = []
    for path in docs:
        title = RETITLE.get(path) or page_title(path)
        entries.append(
            "    DocPage {\n"
            f'        title: "{title}",\n'
            f'        path: "{path}",\n'
            f'        keywords: "{KEYWORDS[path]}",\n'
            "    },"
        )
    block = "pub const PAGES: &[DocPage] = &[\n" + "\n".join(entries) + "\n];\n"
    block += "\n/// Every path https://docs.opencrabs.com/sitemap.xml publishes. The\n"
    block += "/// coverage test reads this, so a page that ships later fails the suite\n"
    block += "/// instead of silently missing from the card.\n"
    block += "pub const SITEMAP: &[&str] = &[\n"
    block += "".join(f'    "{path}",\n' for path in paths)
    block += "];\n"

    # Replace the two const blocks in place and leave everything else alone.
    # A range rewrite is what deleted the helper functions last time: they sit
    # between SITEMAP and the tests, and both ends moved.
    source = TARGET.read_text()
    source = re.sub(
        r"pub const PAGES: &\[DocPage\] = &\[.*?\n\];",
        lambda _m: block.split("\n\n/// Every path")[0],
        source,
        count=1,
        flags=re.S,
    )
    sitemap = block.split("/// Every path", 1)[1]
    sitemap = "/// Every path" + sitemap
    source = re.sub(
        r"/// Every path https://docs\.opencrabs\.com/sitemap\.xml publishes\..*?\n\];",
        lambda _m: sitemap.rstrip("\n"),
        source,
        count=1,
        flags=re.S,
    )
    TARGET.write_text(source)
    print(f"{len(docs)} pages indexed, {len(paths)} sitemap paths pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())