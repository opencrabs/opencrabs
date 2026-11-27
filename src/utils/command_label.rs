//! Turn a shell command into a label a human can read at a glance (#790, #791).
//!
//! Every long-running command the agent issues starts by entering the working
//! directory, so a label that keeps the `cd` keeps the one part that is
//! identical for every command and drops the part that identifies it. At the 28
//! characters the input-border indicator allows, that leaves a field showing
//! nothing but a path.
//!
//! Multi-line commands carry a second problem: a newline reaching a single-line
//! renderer is not drawn, so the tail of one line touches the head of the next
//! and produces a token that was never in the command (`opencrabs` + `echo` ->
//! `opencrabsecho`). That reads as a real command and is not one, which is
//! worse than truncation.
//!
//! Both are fixed here, once, so the TUI rows, the border indicator and the
//! channel summaries cannot drift apart.

/// A single-line, human-readable label for `command`.
///
/// Leading `cd <path>` segments are dropped, every remaining segment is kept,
/// and segments are joined with `"; "` so nothing can fuse across a break.
/// Returns an empty string for an empty command; callers supply their own
/// placeholder.
pub fn command_label(command: &str) -> String {
    let segments = split_segments(command);
    if segments.is_empty() {
        return String::new();
    }

    // Strip only LEADING `cd`s. A `cd` in the middle or at the end is doing
    // something the reader needs to see, and dropping it would misdescribe the
    // command. Taking only the last segment (the previous behaviour) silently
    // discarded real work: `cd /x && cargo build && cargo test` labelled as
    // `cargo test` alone.
    let mut kept: Vec<&String> = segments.iter().skip_while(|s| is_bare_cd(s)).collect();

    // A command that is nothing but `cd` still has to render as something.
    if kept.is_empty() {
        kept = segments.iter().collect();
    }

    kept.iter()
        .map(|s| collapse_whitespace(s))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Is this segment a bare directory change and nothing else?
///
/// Segments are already split on every separator, so everything after `cd ` is
/// the path, spaces and quotes included.
fn is_bare_cd(segment: &str) -> bool {
    segment
        .strip_prefix("cd ")
        .is_some_and(|rest| !rest.trim().is_empty())
}

/// Split on top-level `\n`, `;` and `&&`, ignoring separators inside quotes.
///
/// Quote tracking is what makes this safe to run on arbitrary commands: a
/// naive `split(';')` would cut `echo "a; b"` in half and invent a segment.
/// The scan itself lives in `shell_scan` because the background-task
/// classifier needs the same distinction between syntax and data.
fn split_segments(command: &str) -> Vec<String> {
    let scanned = crate::utils::shell_scan::scan(command);
    let is_syntax = |c: &crate::utils::shell_scan::ShellChar| !c.literal && !c.quote_mark;

    let mut segments = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    while let Some(c) = scanned.get(i) {
        let double_amp = c.ch == '&'
            && is_syntax(c)
            && scanned
                .get(i + 1)
                .is_some_and(|n| n.ch == '&' && is_syntax(n));
        if double_amp {
            segments.push(std::mem::take(&mut current));
            i += 2;
            continue;
        }
        if is_syntax(c) && (c.ch == '\n' || c.ch == ';') {
            segments.push(std::mem::take(&mut current));
        } else {
            current.push(c.ch);
        }
        i += 1;
    }
    segments.push(current);

    segments
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Collapse runs of whitespace so a command formatted across columns reads as
/// one line. Any newline inside a segment is impossible (they are separators),
/// so this only tidies runs of spaces and tabs.
fn collapse_whitespace(segment: &str) -> String {
    segment.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How long a path argument may be before its directories are folded away.
///
/// The input-border indicator has 28 characters for the whole command, so a
/// path that eats more than this leaves nothing for the verb that names the
/// task. Short paths fit inside it and are left exactly as typed.
const PATH_BUDGET: usize = 16;

/// Fold the directories out of every long path argument in `label` (#3204).
///
/// A command's directories are the least informative part of it and the longest.
/// `bash "$HOME/.opencrabs/projects/opencrabs/files/gate-combo.sh"` spent its
/// whole field on `bash "$HOME/.opencrabs/projects/opencrabs/fi…` and never
/// reached `gate-combo`, which is the only part that says what is running. The
/// same command now reads `bash "~/…/gate-combo.sh"` and fits the terminal's
/// 28 columns as well as the window's 64.
///
/// The rule: a path longer than [`PATH_BUDGET`] keeps its leading anchor (the
/// home marker, the first directory of an absolute path, or the first segment
/// of a relative one) and its file name, and drops what sits between. Quotes
/// and an `--option=` prefix are preserved, so the label still looks like the
/// command that was run. Anything that does not get shorter is left alone, so
/// a label without long paths renders byte-identical to before.
pub fn shrink_paths(label: &str) -> String {
    label
        .split(' ')
        .map(shrink_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One whitespace-delimited token of a label, shrunk if it carries a long path.
fn shrink_token(token: &str) -> String {
    // `--config=/long/path/file.toml` keeps its option and loses only the
    // directories inside the value.
    if let Some((option, value)) = token.split_once('=')
        && option.starts_with('-')
        && let Some(shrunk) = shrink_path(value)
    {
        return format!("{option}={shrunk}");
    }
    shrink_path(token).unwrap_or_else(|| token.to_string())
}

/// `token` with its directories folded away, or `None` when it is not a path
/// that would actually get shorter.
fn shrink_path(token: &str) -> Option<String> {
    let (open, rest) = split_leading_quote(token);
    let (inner, close) = split_trailing_quote(rest);
    if inner.chars().count() <= PATH_BUDGET {
        return None;
    }

    let path = normalize_home(inner);
    if path.matches('/').count() < 2 {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    let file = trimmed.rsplit('/').next().filter(|f| !f.is_empty())?;
    let anchor = trimmed.split('/').next().unwrap_or("");
    if file == anchor {
        return None;
    }

    let head = if anchor.is_empty() {
        // Absolute path: keep `/` and the first real directory, so the reader
        // still sees where on the filesystem the work lives.
        let first = trimmed.trim_start_matches('/').split('/').next()?;
        if first.is_empty() {
            return None;
        }
        format!("/{first}")
    } else {
        anchor.to_string()
    };

    let shrunk = format!("{head}/…/{file}");
    if shrunk.chars().count() >= token.chars().count() {
        return None;
    }
    Some(format!("{open}{shrunk}{close}"))
}

/// `$HOME` and `${HOME}` mean the same directory `~` does, and `~` is what the
/// label should print: it is shorter and reads as "your own files".
fn normalize_home(path: &str) -> String {
    path.replace("${HOME}", "~").replace("$HOME", "~")
}

/// The opening quote character, if the token starts with one.
fn split_leading_quote(token: &str) -> (&str, &str) {
    match token.as_bytes().first() {
        Some(b'"' | b'\'') => token.split_at(1),
        _ => ("", token),
    }
}

/// The closing quote character, if the token ends with one.
fn split_trailing_quote(token: &str) -> (&str, &str) {
    match token.as_bytes().last() {
        Some(b'"' | b'\'') => token.split_at(token.len() - 1),
        _ => (token, ""),
    }
}
