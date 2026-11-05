#!/usr/bin/env python3
"""Generate or check the Seeker docs index against the live docs sitemap.

Run ``python3 scripts/gen-docs-index.py`` after publishing docs. Use ``--check``
to compare the committed Rust index with the live site without writing it.
Unknown reference pages and metadata for removed pages stop both operations.
"""
import argparse
from dataclasses import dataclass
import html
import json
import re
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TARGET = REPO / "src" / "ui_core" / "docs.rs"
BASE = "https://docs.opencrabs.com"

# Every published reference page needs useful words a user might type.
KEYWORDS = {
    "/introduction.html": "introduction overview what is opencrabs about start",
    "/getting-started/installation.html": "install setup download binary brew cargo build upgrade update",
    "/getting-started/configuration.html": "config configuration settings keys config.toml profiles env",
    "/getting-started/onboarding.html": "onboard first run wizard provider key setup",
    "/getting-started/cli-commands.html": "cli command commands flags shell terminal headless help",
    "/getting-started/troubleshooting.html": "error broken fails fix problem logs doctor debug daemon trouble stuck down restart service not starting",
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
    "/features/acp.html": "acp editor integration zed client protocol agent",
    "/features/browser.html": "browser automation navigate click type screenshot chrome cdp web",
    "/features/companion-tools.html": "companion tools utilities helper helpers",
    "/features/cron-jobs.html": "cron schedule timer job recurring automation automate every morning daily reminder routine",
    "/features/document-generation.html": "document documents generate excel xlsx word docx pdf powerpoint pptx spreadsheet report",
    "/features/dynamic-tools.html": "dynamic tools tool_manage tools.toml create enable disable reload runtime",
    "/features/goal.html": "goal goals autonomous loop objective manage",
    "/features/mission-control.html": "mission control analytics dashboard metrics usage tools performance report",
    "/features/multi-agent.html": "multi agent agents orchestration team spawn delegate parallel models providers",
    "/features/plans.html": "plan checklist tasks approval",
    "/features/profiles.html": "profile profiles multi profile isolated workspace configuration switch",
    "/features/rtk.html": "rtk token savings context compression reduce tokens",
    "/features/self-healing.html": "self healing recover recovery error errors phantom tool gaslighting",
    "/features/self-improvement.html": "self improvement rsi evolve improve feedback brain files proposals",
    "/features/sessions.html": "session sessions conversation resume history rail favorite favourite pin star sidebar side bar open close show hide reveal collapse drawer list",
    "/features/skills.html": "skill skills system workflow workflows slash skill.md",
    "/features/split-panes.html": "split panes pane layout terminal sidebar",
    "/features/themes.html": "theme themes appearance appearances color colour change open switch customize customization preview style styles dark light palette",
    "/features/usage-dashboard.html": "usage dashboard token tokens cost costs statistics stats spending",
    "/features/a2a.html": "a2a agent to agent protocol rpc peer gateway",
    "/features/voice.html": "voice tts stt speech transcribe dictation audio speak whisper",
    "/features/web-scrape.html": "web scrape scraping fetch url markdown website extract",
    "/development/building.html": "build compile from source cargo release",
    "/development/architecture.html": "architecture design internals modules structure",
    "/development/testing.html": "test tests suite cargo coverage",
    "/development/contributing.html": "contributing issue pr pull request workflow commit style",
    "/development/security.html": "security audit cve secrets permissions policy",
    "/changelog.html": "changelog release notes changes version whats new",
    "/user-stories.html": "stories case studies examples users",
}

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

EXCLUDED_PATHS = {
    "/": "site landing page",
    "/toc.html": "table of contents",
    "/getting-started/videos.html": "video gallery",
}
EXCLUDED_PREFIXES = {"/stories/": "case study"}


@dataclass(frozen=True)
class Page:
    title: str
    path: str
    keywords: str


def fetch(url: str) -> str:
    with urllib.request.urlopen(url, timeout=30) as response:
        return response.read().decode("utf-8", "replace")


def sitemap_paths() -> list[str]:
    xml = fetch(f"{BASE}/sitemap.xml")
    return [loc.replace(BASE, "") for loc in re.findall(r"<loc>([^<]+)</loc>", xml)]


def page_title(path: str) -> str:
    match = re.search(r"<title>(.*?)</title>", fetch(BASE + path), re.S)
    title = html.unescape(match.group(1).strip()) if match else path
    return title.split(" - OpenCrabs")[0]


def exclusion_reason(path: str) -> str | None:
    if path in EXCLUDED_PATHS:
        return EXCLUDED_PATHS[path]
    for prefix, reason in EXCLUDED_PREFIXES.items():
        if path.startswith(prefix):
            return reason
    return None


def reference_paths(paths: list[str]) -> list[str]:
    return [path for path in paths if exclusion_reason(path) is None]


def expected_pages(paths: list[str], title_lookup=page_title) -> list[Page]:
    pages = []
    for path in reference_paths(paths):
        if path not in KEYWORDS:
            continue
        pages.append(Page(RETITLE.get(path) or title_lookup(path), path, KEYWORDS[path]))
    return pages


def quoted(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def render_index(source: str, pages: list[Page], paths: list[str]) -> str:
    entries = []
    for page in pages:
        entries.append(
            "    DocPage {\n"
            f"        title: {quoted(page.title)},\n"
            f"        path: {quoted(page.path)},\n"
            f"        keywords: {quoted(page.keywords)},\n"
            "    },"
        )
    pages_block = "pub const PAGES: &[DocPage] = &[\n" + "\n".join(entries) + "\n];"
    sitemap_block = (
        "/// Every path https://docs.opencrabs.com/sitemap.xml publishes. The\n"
        "/// coverage test reads this, so a page that ships later fails the suite\n"
        "/// instead of silently missing from the card.\n"
        "pub const SITEMAP: &[&str] = &[\n"
        + "".join(f"    {quoted(path)},\n" for path in paths)
        + "];"
    )
    source, count = re.subn(
        r"pub const PAGES: &\[DocPage\] = &\[.*?\n\];",
        lambda _match: pages_block,
        source,
        count=1,
        flags=re.S,
    )
    if count != 1:
        raise ValueError("could not locate the PAGES block")
    source, count = re.subn(
        r"/// Every path https://docs\.opencrabs\.com/sitemap\.xml publishes\..*?\n\];",
        lambda _match: sitemap_block,
        source,
        count=1,
        flags=re.S,
    )
    if count != 1:
        raise ValueError("could not locate the SITEMAP block")
    return source


STRING = r'"(?:\\.|[^"\\])*"'
PAGE_RE = re.compile(
    rf"DocPage\s*\{{\s*title:\s*(?P<title>{STRING}),\s*"
    rf"path:\s*(?P<path>{STRING}),\s*keywords:\s*(?P<keywords>{STRING}),\s*\}},",
    re.S,
)


def parse_pages(source: str) -> list[Page]:
    return [
        Page(*(json.loads(match.group(name)) for name in ("title", "path", "keywords")))
        for match in PAGE_RE.finditer(source)
    ]


def parse_sitemap(source: str) -> list[str]:
    match = re.search(r"pub const SITEMAP: &\[&str\] = &\[(.*?)\n\];", source, re.S)
    if not match:
        return []
    return [json.loads(value) for value in re.findall(STRING, match.group(1))]


def drift_report(source: str, paths: list[str], title_lookup=page_title) -> list[str]:
    current = parse_pages(source)
    current_by_path = {page.path: page for page in current}
    expected = expected_pages(paths, title_lookup)
    expected_by_path = {page.path: page for page in expected}
    reference = reference_paths(paths)
    problems = []

    unclassified = [path for path in reference if path not in KEYWORDS]
    if unclassified:
        problems.append(f"unclassified URLs, add keywords or an exclusion: {unclassified}")
    missing = [path for path in reference if path not in current_by_path]
    if missing:
        problems.append(f"missing pages: {missing}")
    removed = [path for path in current_by_path if path not in reference]
    if removed:
        problems.append(f"removed pages still indexed: {removed}")
    stale_keywords = [
        path for path in expected_by_path.keys() & current_by_path.keys()
        if expected_by_path[path].keywords != current_by_path[path].keywords
    ]
    if stale_keywords:
        problems.append(f"stale keywords: {sorted(stale_keywords)}")
    stale_titles = [
        path for path in expected_by_path.keys() & current_by_path.keys()
        if expected_by_path[path].title != current_by_path[path].title
    ]
    if stale_titles:
        problems.append(f"stale titles: {sorted(stale_titles)}")
    orphaned = [path for path in KEYWORDS if path not in reference]
    if orphaned:
        problems.append(f"keyword metadata for removed pages: {orphaned}")

    old_sitemap = parse_sitemap(source)
    if old_sitemap != paths:
        added = [path for path in paths if path not in old_sitemap]
        deleted = [path for path in old_sitemap if path not in paths]
        problems.append(f"sitemap drift, added: {added}, removed: {deleted}")

    if not unclassified and not orphaned:
        expected_source = render_index(source, expected, paths)
        if expected_source != source and not problems:
            problems.append("generated PAGES or SITEMAP content differs from the generator")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="check without writing docs.rs")
    args = parser.parse_args(argv)
    source = TARGET.read_text()
    paths = sitemap_paths()

    if args.check:
        problems = drift_report(source, paths)
        if problems:
            for problem in problems:
                print(problem, file=sys.stderr)
            return 1
        print(f"docs index is current: {len(reference_paths(paths))} pages, {len(paths)} sitemap paths")
        return 0

    reference = reference_paths(paths)
    unclassified = [path for path in reference if path not in KEYWORDS]
    orphaned = [path for path in KEYWORDS if path not in reference]
    if unclassified or orphaned:
        if unclassified:
            print(f"unclassified URLs, add keywords or an exclusion: {unclassified}", file=sys.stderr)
        if orphaned:
            print(f"keyword metadata for removed pages: {orphaned}", file=sys.stderr)
        return 1

    pages = expected_pages(paths)
    TARGET.write_text(render_index(source, pages, paths))
    print(f"{len(pages)} pages indexed, {len(paths)} sitemap paths pinned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
