//! Local-file link delivery (#1916): what the bot does with a markdown link
//! whose target is a real file on disk.
//!
//! A `[label](/path/report.pdf)` link is what the model writes when it means
//! "the file is here". The reader is a channel user with NO filesystem
//! access, so the link is not the deliverable: the FILE is. Before #1916 the
//! link rendered as an inert entity whose URL was the raw path, Telegram had
//! no scheme to resolve, and the reader got a dead link and no file.
//!
//! Two halves, one file:
//!
//! 1. **The scanner** (`mod scan`). Which references become a `LocalFile`
//!    and which stay as literal text. The marker policy is the one place
//!    the file family deliberately differs from the image family: a
//!    resolved file becomes an attachment and its link is replaced by a
//!    visible `📎 <label>` marker, while a REJECTED candidate stays
//!    byte-identical and is reported, because a link carries its own label
//!    and a silent strip would delete the reader's only clue about what
//!    was referenced.
//! 2. **The delivery leg** (`mod floor`). A resolved link ships exactly ONE
//!    document carrying the link label as its caption, and a file the
//!    channel could not deliver comes back as a failure entry the caller can
//!    NAME, never as a silent drop. The two failure classes are distinct: a
//!    path that cannot be READ is a reference problem (`Unreadable`), while
//!    a file the API refuses is a delivery problem (`DeliveryFailed`).
//! 3. **The bubble links** (`mod links`, #1918). A delivered file's `📎`
//!    marker becomes a `t.me` link to the document bubble, so a channel
//!    reader can tap the marker and land on the file. Links exist only for
//!    public supergroup and channel chats; a private chat keeps the plain
//!    marker. The recorded span is a record of WHAT the scanner wrote, never
//!    of WHERE it sits — the rewrites that run AFTER the scan
//!    (`strip_echoed_plan_title`, `extract_react_marker`, the dedup ladder)
//!    shift it — so the span is tried first and the marker's own bytes are
//!    searched for when it has moved. A marker that vanished, or that the
//!    model also wrote itself (#2001), stays plain rather than splicing a
//!    link over unrelated words.
//! 4. **The rich rewrite** (`mod rich`, #1918). On the rich plane the file
//!    goes one better than a marker: each resolvable reference is rewritten
//!    IN PLACE into a `tg://document?id=` reference and the document rides
//!    the bubble's media array, so the file renders at its position in the
//!    report. Already-delivered files consume their link and rewrite to
//!    nothing; remote links, code spans and unresolvable paths stay
//!    byte-identical.
//! 5. **The intermediate plane** (`mod intermediate`, #1918). A rich report
//!    emitted mid-turn runs the same scan: resolvable files inline into its
//!    media array, and when the rich send declines, each file drops to the
//!    floor as its own document so the marker never points at a file the
//!    chat never received. A repeated intermediate re-delivers nothing.
//! 6. **The marker consume** (`mod marker_consume`, #2001). The `📎`
//!    marker is the harness's to write, but the capability line tells the
//!    model what the marker looks like, so the model may prefix its own
//!    `📎 ` before the reference. Every emit site consumes that prefix
//!    first, so the reader sees exactly ONE marker either way.
//!
//! Deliberately NOT covered here: the notice wording battery, the regen
//! ladder, and the extraction rules of the image family. Each has its own
//! home.

/// Minimal byte string that passes the magic-byte sniff for PNG. Only the
/// image-reference test below needs it: the file family has no format gate,
/// so its fixtures use PDF bytes instead.
const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x01\x02\x03\x04\x05\x06\x07";

// ---------------------------------------------------------------------------
// the scanner: resolution, removal, and what must be LEFT ALONE
// ---------------------------------------------------------------------------

mod scan {
    use super::PNG_BYTES;
    use crate::utils::image::{
        LocalFileScan, LocalImageFailure, LocalImageFailureReason, TELEGRAM_DOCUMENT_MAX_BYTES,
        append_file_failure_notice, extract_local_files, file_failure_notice, file_marker_text,
        validate_local_file,
    };
    use std::path::{Path, PathBuf};

    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write fixture");
        path
    }

    fn paths(scan: &LocalFileScan) -> Vec<PathBuf> {
        scan.attachments.iter().map(|f| f.path.clone()).collect()
    }

    fn failure(raw: &str) -> LocalImageFailure {
        LocalImageFailure {
            raw: raw.to_string(),
            resolved: None,
            reason: LocalImageFailureReason::NotFound,
        }
    }

    // -----------------------------------------------------------------------
    // Resolution, the visible marker, and what must be LEFT ALONE
    // -----------------------------------------------------------------------

    #[test]
    fn a_resolved_link_becomes_a_visible_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let scan = extract_local_files(
            &format!("before [Q3 report]({}) after", pdf.display()),
            None,
        );
        assert_eq!(scan.text, "before 📎 Q3 report after");
        assert_eq!(paths(&scan), vec![pdf]);
        assert_eq!(scan.attachments[0].caption.as_deref(), Some("Q3 report"));
        let span = scan.attachments[0]
            .marker_span
            .clone()
            .expect("the scan records the marker's span");
        assert_eq!(&scan.text[span], "📎 Q3 report");
        assert!(scan.failures.is_empty());
    }

    #[test]
    fn an_empty_label_yields_no_caption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let scan = extract_local_files(&format!("[]({})", pdf.display()), None);
        assert_eq!(paths(&scan), vec![pdf]);
        assert_eq!(
            scan.attachments[0].caption, None,
            "an empty label is not a caption"
        );
    }

    #[test]
    fn an_empty_label_falls_back_to_the_basename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let scan = extract_local_files(&format!("[]({})", pdf.display()), None);
        // The fallback basename is visible marker text, so a dotted name
        // carries the autolink breaker (#1938): the client must not read
        // `q3.pdf` as a domain.
        assert_eq!(scan.text, "📎 q3.\u{200b}pdf", "the marker is never blank");
        let span = scan.attachments[0]
            .marker_span
            .clone()
            .expect("the scan records the marker's span");
        assert_eq!(&scan.text[span], "📎 q3.\u{200b}pdf");
    }

    #[test]
    fn a_dotted_label_does_not_autolink_as_a_domain() {
        // The live specimen (#1938): the marker text `1918-fix-state.md` was
        // rendered by the client as a link to the Moldova ccTLD. The dot is
        // followed by a zero-width space: no renderer shows ink for it, and
        // the client's `name.tld` pattern no longer matches.
        let out = file_marker_text("1918-fix-state.md", "/tmp/1918-fix-state.md");
        assert_eq!(
            out, "1918-fix-state.\u{200b}md",
            "the dot the client keys on is followed by an invisible breaker"
        );
        assert!(
            !out.contains(".md"),
            "no bare `.md` survives for the client's TLD detector to match"
        );
        assert_eq!(
            out.replace('\u{200b}', ""),
            "1918-fix-state.md",
            "the breaker is pure ink: removing it restores the file's own name"
        );
    }

    #[test]
    fn a_dot_with_no_alphanumeric_after_gets_no_breaker() {
        // The disarmer targets the autolinker's pattern, not every dot: a
        // trailing dot has no domain head after it, and the text stays
        // byte-identical. A dot before a digit is the same pattern shape
        // (`1.2` looks like a short domain to a matcher) and gets the
        // breaker too.
        assert_eq!(file_marker_text("done.", "/tmp/done."), "done.");
        assert_eq!(
            file_marker_text("v1.2 final", "/tmp/v1"),
            "v1.\u{200b}2 final",
            "the pattern is the dot plus what follows, not the token's shape"
        );
    }

    #[test]
    fn the_marker_span_indexes_the_final_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        // Leading AND trailing whitespace force the trim rebase: the span
        // was recorded while the buffer still carried the leading run, so
        // an unrebased span would index the WRONG bytes of the final text.
        let scan = extract_local_files(&format!("  \n [Q3 report]({})  ", pdf.display()), None);
        assert_eq!(scan.text, "📎 Q3 report");
        let span = scan.attachments[0]
            .marker_span
            .clone()
            .expect("the scan records the marker's span");
        assert_eq!(&scan.text[span], "📎 Q3 report");
    }

    #[test]
    fn two_files_attach_in_order_of_appearance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = write_file(dir.path(), "a.pdf", PDF_BYTES);
        let b = write_file(dir.path(), "b.pdf", PDF_BYTES);
        let scan = extract_local_files(
            &format!("[A]({}) then [B]({})", a.display(), b.display()),
            None,
        );
        assert_eq!(paths(&scan), vec![a, b]);
        assert_eq!(scan.text, "📎 A then 📎 B");
        let span_a = scan.attachments[0]
            .marker_span
            .clone()
            .expect("the scan records the marker's span");
        let span_b = scan.attachments[1]
            .marker_span
            .clone()
            .expect("the scan records the marker's span");
        assert_eq!(&scan.text[span_a], "📎 A");
        assert_eq!(&scan.text[span_b], "📎 B");
    }

    #[test]
    fn an_angle_bracket_target_holds_spaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "Q3 final.pdf", PDF_BYTES);
        let scan = extract_local_files(&format!("x [Q3](<{}>) y", pdf.display()), None);
        assert_eq!(scan.text, "x 📎 Q3 y");
        assert_eq!(paths(&scan), vec![pdf]);
    }

    #[test]
    fn a_title_after_the_target_is_ignored_and_the_label_captions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let scan = extract_local_files(
            &format!("x [Q3 report]({} \"quarterly\") y", pdf.display()),
            None,
        );
        assert_eq!(scan.text, "x 📎 Q3 report y");
        assert_eq!(scan.attachments[0].caption.as_deref(), Some("Q3 report"));
    }

    #[test]
    fn a_relative_target_resolves_against_the_base_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("reports")).expect("mkdir");
        let pdf = write_file(&dir.path().join("reports"), "q3.pdf", PDF_BYTES);
        let scan = extract_local_files("see [report](reports/q3.pdf) here", Some(dir.path()));
        assert_eq!(scan.text, "see 📎 report here");
        assert_eq!(paths(&scan), vec![pdf]);
        assert_eq!(scan.attachments[0].caption.as_deref(), Some("report"));
    }

    // -----------------------------------------------------------------------
    // What the family must LEAVE ALONE
    // -----------------------------------------------------------------------

    #[test]
    fn a_relative_target_without_a_base_dir_stays_literal() {
        let text = "see [report](reports/q3.pdf) here";
        let scan = extract_local_files(text, None);
        assert_eq!(scan.text, text, "no base directory to resolve against");
        assert!(scan.attachments.is_empty());
        assert!(scan.failures.is_empty());
    }

    #[test]
    fn a_remote_link_is_left_alone() {
        let text = "see [the site](https://example.com/a) for details";
        let scan = extract_local_files(text, None);
        assert_eq!(scan.text, text, "Telegram resolves a real URL itself");
        assert!(scan.attachments.is_empty());
        assert!(scan.failures.is_empty(), "a URL is not a missing file");
    }

    #[test]
    fn an_image_reference_is_not_claimed_by_the_file_family() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = write_file(dir.path(), "chart.png", PNG_BYTES);
        let text = format!("x ![chart]({}) y", png.display());
        let scan = extract_local_files(&text, None);
        assert_eq!(scan.text, text, "the image family owns `![...](...)`");
        assert!(scan.attachments.is_empty());
        assert!(scan.failures.is_empty());
    }

    #[test]
    fn a_link_inside_a_code_span_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let text = format!("`[Q3 report]({})`", pdf.display());
        let scan = extract_local_files(&text, None);
        assert_eq!(
            scan.text, text,
            "a code span is documentation, not a deliverable"
        );
        assert!(scan.attachments.is_empty());
        assert!(scan.failures.is_empty());
    }

    #[test]
    fn a_link_inside_a_code_fence_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let text = format!("```\n[Q3 report]({})\n```", pdf.display());
        let scan = extract_local_files(&text, None);
        assert_eq!(scan.text, text);
        assert!(scan.attachments.is_empty());
    }

    #[test]
    fn an_escaped_link_stays_literal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        let text = format!("\\[Q3 report]({})", pdf.display());
        let scan = extract_local_files(&text, None);
        assert_eq!(scan.text, text);
        assert!(scan.attachments.is_empty());
    }

    #[test]
    fn a_non_file_scheme_or_anchor_is_left_alone() {
        for text in [
            "mail [me](mailto:a@b.c)",
            "call [me](tel:+1234)",
            "see [anchor](#section)",
            "see [nothing]()",
        ] {
            let scan = extract_local_files(text, None);
            assert_eq!(scan.text, text, "{text} must survive byte-identical");
            assert!(scan.attachments.is_empty(), "{text}");
            assert!(scan.failures.is_empty(), "{text}");
        }
    }

    #[test]
    fn prose_that_merely_looks_like_a_link_is_untouched() {
        for text in ["see [1] and [2] for details", "a [b] c", "[standalone]"] {
            let scan = extract_local_files(text, None);
            assert_eq!(scan.text, text);
            assert!(scan.attachments.is_empty());
            assert!(scan.failures.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Rejections: reported AND left in the text
    // -----------------------------------------------------------------------

    #[test]
    fn a_missing_file_is_reported_and_the_link_survives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone.pdf");
        let raw = format!("[Q3 report]({})", missing.display());
        let text = format!("x {raw} y");
        let scan = extract_local_files(&text, Some(dir.path()));
        assert_eq!(
            scan.text, text,
            "a rejected link keeps its label: it is the reader's only clue"
        );
        assert!(scan.attachments.is_empty());
        assert_eq!(scan.failures.len(), 1);
        assert_eq!(scan.failures[0].reason, LocalImageFailureReason::NotFound);
        assert_eq!(scan.failures[0].raw, raw);
        assert_eq!(
            scan.failures[0].resolved.as_deref(),
            Some(missing.as_path())
        );
    }

    #[test]
    fn a_directory_is_not_a_regular_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("reports");
        std::fs::create_dir(&sub).expect("mkdir");
        let scan = extract_local_files(&format!("[x]({})", sub.display()), None);
        assert!(scan.attachments.is_empty());
        assert_eq!(scan.failures[0].reason, LocalImageFailureReason::NotAFile);
    }

    #[test]
    fn an_empty_file_is_reported_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = write_file(dir.path(), "empty.pdf", b"");
        let scan = extract_local_files(&format!("[x]({})", empty.display()), None);
        assert!(scan.attachments.is_empty());
        assert_eq!(scan.failures[0].reason, LocalImageFailureReason::Empty);
    }

    #[test]
    fn a_file_past_the_document_ceiling_is_reported_as_too_large() {
        let dir = tempfile::tempdir().expect("tempdir");
        let big = dir.path().join("big.pdf");
        // A sparse file: `set_len` costs no disk and no time, where writing
        // 50 MiB of fixture bytes would.
        std::fs::File::create(&big)
            .expect("create")
            .set_len(TELEGRAM_DOCUMENT_MAX_BYTES + 1)
            .expect("set_len");
        let scan = extract_local_files(&format!("[x]({})", big.display()), None);
        assert!(scan.attachments.is_empty());
        assert_eq!(scan.failures[0].reason, LocalImageFailureReason::TooLarge);
    }

    #[test]
    fn validate_accepts_a_readable_non_empty_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_file(dir.path(), "q3.pdf", PDF_BYTES);
        assert!(validate_local_file(&pdf).is_ok());
    }

    // -----------------------------------------------------------------------
    // The notice: the honest line when delivery did not happen
    // -----------------------------------------------------------------------

    #[test]
    fn no_failures_produces_no_file_notice() {
        assert!(file_failure_notice(&[]).is_none());
    }

    #[test]
    fn a_file_notice_names_the_reference_and_the_reason() {
        let notice =
            file_failure_notice(&[failure("[Q3 report](/root/reports/q3.pdf)")]).expect("notice");
        assert!(notice.contains("File not attached"), "{notice}");
        assert!(notice.contains("a file"), "{notice}");
        assert!(
            notice.contains("[Q3 report](/root/reports/q3.pdf)"),
            "{notice}"
        );
        assert!(notice.contains("file not found"), "{notice}");
    }

    #[test]
    fn appending_a_file_notice_keeps_the_body_and_adds_a_blank_line() {
        let body = append_file_failure_notice("hello", &[failure("[x](/nope)")]);
        assert!(body.starts_with("hello\n\n⚠️ File not attached"), "{body}");
    }

    #[test]
    fn an_empty_body_becomes_the_file_notice_alone() {
        let body = append_file_failure_notice("   ", &[failure("[x](/nope)")]);
        assert!(body.starts_with("⚠️ File not attached"), "{body}");
    }

    #[test]
    fn appending_without_failures_returns_the_body_untouched() {
        assert_eq!(append_file_failure_notice("hello", &[]), "hello");
    }
}

// ---------------------------------------------------------------------------
// the delivery leg: one document bubble per link, captioned by the label
// ---------------------------------------------------------------------------

mod floor {
    use crate::channels::telegram::delivery::{DeliveredFile, send_local_files};
    use crate::utils::image::{LocalFile, LocalImageFailureReason};
    use std::path::{Path, PathBuf};

    /// The paths of what landed, in order. Every floor assertion reads this
    /// projection; the message ids are asserted separately where they matter.
    fn delivered_paths(delivered: &[DeliveredFile]) -> Vec<PathBuf> {
        delivered.iter().map(|f| f.path.clone()).collect()
    }

    const CHAT: i64 = 133_526_395;

    /// A minimal PDF header: the bytes a real report starts with. The file
    /// family has no format gate (Telegram renders no document preview, so
    /// there is nothing to decode), which makes this a realistic fixture
    /// rather than a required signature; nothing in the send path inspects it.
    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";

    /// A response shaped like a successful text send. Only the ROUTING is
    /// under test, so the body mirrors the proven shape rather than inventing
    /// a media object.
    const SEND_MESSAGE_OK: &str = r#"{"ok":true,"result":{"message_id":701,"date":1757166400,"chat":{"id":133526395,"type":"private"},"text":"ok"}}"#;

    /// What Telegram answers for an upload it will not take.
    const REFUSED: &str =
        r#"{"ok":false,"error_code":413,"description":"Request Entity Too Large"}"#;

    /// A bot pointed at the mockito server.
    ///
    /// MOCK PATHS ARE PASCALCASE FOR EVERY TELOXIDE REQUEST: teloxide builds
    /// the method segment from the payload struct name, so
    /// `bot.send_document(..)` hits `/botTESTTOKEN/SendDocument`, never the
    /// lowercase form the Bot API docs use. A lowercase mock never matches:
    /// mockito serves its own unmatched 501, whose empty body teloxide
    /// reports as `InvalidJson` and the send reads as a network failure.
    fn test_bot(server: &mockito::ServerGuard) -> teloxide::Bot {
        teloxide::Bot::with_client(
            "TESTTOKEN",
            reqwest_teloxide::Client::builder().build().unwrap(),
        )
        .set_api_url(server.url().parse().unwrap())
    }

    fn file_at(path: PathBuf, caption: Option<&str>) -> LocalFile {
        LocalFile {
            path,
            caption: caption.map(str::to_string),
            marker_span: None,
        }
    }

    fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write fixture");
        path
    }

    // -----------------------------------------------------------------------
    // the failure-entry contract
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn an_unreadable_file_returns_a_failure_entry() {
        // The reference resolves to a path that is not there by the time the
        // send runs. The caller must be able to NAME it: a document the model
        // announced that vanishes with no notice is the #502 failure mode
        // (silent in both directions).
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone.pdf");
        let server = mockito::Server::new_async().await;
        let bot = test_bot(&server);

        let (delivered, failures) = send_local_files(
            uuid::Uuid::new_v4(),
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &[file_at(missing.clone(), None)],
        )
        .await;

        assert!(delivered.is_empty(), "nothing was delivered");
        assert_eq!(failures.len(), 1, "the unreadable file must be reported");
        assert_eq!(failures[0].raw, missing.display().to_string());
        assert_eq!(
            failures[0].reason,
            LocalImageFailureReason::Unreadable,
            "a path that cannot be read is a REFERENCE problem, not a delivery one"
        );
    }

    #[tokio::test]
    async fn a_file_the_channel_refuses_returns_a_delivery_failure() {
        // The file is readable and inside the ceiling, so the reference is
        // fine: the API is what says no. That distinction is what routes the
        // correction, because a reference problem can be repaired by
        // rewriting the link and a delivery problem cannot.
        let dir = tempfile::tempdir().expect("tempdir");
        let report = write_fixture(dir.path(), "q3.pdf", PDF_BYTES);
        let mut server = mockito::Server::new_async().await;
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .with_status(413)
            .with_header("content-type", "application/json")
            .with_body(REFUSED)
            .expect(1)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let (delivered, failures) = send_local_files(
            uuid::Uuid::new_v4(),
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &[file_at(report.clone(), Some("Q3 report"))],
        )
        .await;

        document_mock.assert_async().await;
        assert!(
            delivered.is_empty(),
            "the channel refused it, so nothing was delivered"
        );
        assert_eq!(failures.len(), 1, "the refusal must be reported");
        assert_eq!(
            failures[0].reason,
            LocalImageFailureReason::DeliveryFailed,
            "a file the channel refused is a DELIVERY problem: the reference itself was fine"
        );
    }

    // -----------------------------------------------------------------------
    // the promise: one document bubble, captioned by the link label
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_resolved_link_ships_exactly_one_document_captioned_by_its_label() {
        // The positive control for the two tests above, and the feature's
        // core assertion at once: the instrument must be able to return an
        // empty failure list (or "it reported a failure" proves nothing about
        // which inputs fail), and the caption must ride the request as the
        // link LABEL, not the path, which the user cannot open anyway.
        let dir = tempfile::tempdir().expect("tempdir");
        let report = write_fixture(dir.path(), "q3.pdf", PDF_BYTES);
        let mut server = mockito::Server::new_async().await;
        // `expect(1)` is the whole point: a second bubble for one link is the
        // #502/#360 duplicate, one media family over. The body match pins the
        // caption, since the label is the only part of the link the user gets
        // to read in the chat.
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .match_body(mockito::Matcher::Regex("Q3 report".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            .expect(1)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let (delivered, failures) = send_local_files(
            uuid::Uuid::new_v4(),
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &[file_at(report.clone(), Some("Q3 report"))],
        )
        .await;

        document_mock.assert_async().await;
        assert_eq!(
            delivered_paths(&delivered),
            vec![report],
            "the delivered path comes back"
        );
        // The bubble id is the half the #1918 link builder consumes: the
        // marker is spliced into a t.me link to THIS id.
        assert_eq!(delivered[0].message_id, 701, "the bubble id comes back");
        assert!(failures.is_empty(), "a delivered file reports no failure");
    }

    #[tokio::test]
    async fn a_delivered_document_arrives_named_after_its_file() {
        // #1937: `InputFile::memory` carries no filename and teloxide's
        // guess for raw bytes is empty, so Telegram labelled every delivered
        // document "file" with no MIME. The multipart part's filename is
        // where the name travels from, and the extension is what teloxide
        // derives the MIME type from, so the body must carry both.
        let dir = tempfile::tempdir().expect("tempdir");
        let report = write_fixture(dir.path(), "q3-report.pdf", PDF_BYTES);
        let mut server = mockito::Server::new_async().await;
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .match_body(mockito::Matcher::Regex(
                // The name travels in the part's filename attribute; teloxide
                // sets no per-part content type, so the MIME is not asserted.
                r#"filename="q3-report\.pdf""#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            .expect(1)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let (_, failures) = send_local_files(
            uuid::Uuid::new_v4(),
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &[file_at(report, Some("Q3 report"))],
        )
        .await;

        document_mock.assert_async().await;
        assert!(failures.is_empty(), "a named document still delivers");
    }

    #[tokio::test]
    async fn a_link_without_a_label_ships_the_document_with_no_caption() {
        // `[](path)`: an empty label is not a caption. Sending `Some("")`
        // would make Telegram render an empty caption bubble; the scanner
        // already drops the empty label, and this pins that the send path
        // does not resurrect it.
        let dir = tempfile::tempdir().expect("tempdir");
        let report = write_fixture(dir.path(), "q3.pdf", PDF_BYTES);
        let mut server = mockito::Server::new_async().await;
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            .expect(1)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let (delivered, failures) = send_local_files(
            uuid::Uuid::new_v4(),
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &[file_at(report.clone(), None)],
        )
        .await;

        document_mock.assert_async().await;
        assert_eq!(delivered_paths(&delivered), vec![report]);
        assert!(failures.is_empty());
    }

    #[tokio::test]
    async fn each_link_ships_its_own_bubble_in_order() {
        // Two links are two documents, not one, and the order the reply wrote
        // them in is the order they arrive, so the reply's prose still reads
        // top-to-bottom against the bubbles beside it.
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_fixture(dir.path(), "q3.pdf", PDF_BYTES);
        let second = write_fixture(dir.path(), "q4.pdf", PDF_BYTES);
        let mut server = mockito::Server::new_async().await;
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            .expect(2)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let (delivered, failures) = send_local_files(
            uuid::Uuid::new_v4(),
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &[
                file_at(first.clone(), Some("Q3")),
                file_at(second.clone(), Some("Q4")),
            ],
        )
        .await;

        document_mock.assert_async().await;
        assert_eq!(
            delivered_paths(&delivered),
            vec![first, second],
            "delivered in the reply's order"
        );
        assert!(failures.is_empty());
    }
}

// ---------------------------------------------------------------------------
// the bubble link: a delivered file's marker points at its t.me bubble
// ---------------------------------------------------------------------------

mod links {
    use crate::channels::telegram::delivery::{
        file_message_link, link_file_markers, strip_echoed_plan_title,
    };
    use crate::utils::image::{LocalFile, LocalFileScan, extract_local_files};
    use std::path::Path;

    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";
    const SUPERGROUP_ID: i64 = -100_123_456_789_012;

    fn write_fixture(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, PDF_BYTES).expect("write fixture");
        path
    }

    /// A `Chat` from its wire form: the kind variants carry their own
    /// payloads, and building them from JSON keeps the test honest about
    /// what Telegram actually sends instead of hand-filling internals.
    fn chat_of_kind(v: serde_json::Value) -> teloxide::types::Chat {
        serde_json::from_value(v).expect("chat json")
    }

    #[test]
    fn a_supergroup_bubble_gets_a_message_link() {
        let chat = chat_of_kind(serde_json::json!({
            "id": SUPERGROUP_ID, "type": "supergroup", "title": "Ops",
        }));
        let link =
            file_message_link(&chat.kind, SUPERGROUP_ID, None, 701).expect("supergroup links");
        // The internal id drops the -100 marker; anything else would 404.
        assert_eq!(link, "https://t.me/c/123456789012/701");
    }

    #[test]
    fn a_topic_message_links_into_its_topic() {
        let chat = chat_of_kind(serde_json::json!({
            "id": SUPERGROUP_ID, "type": "supergroup", "title": "Ops", "is_forum": true,
        }));
        let thread = teloxide::types::ThreadId(teloxide::types::MessageId(42));
        let link = file_message_link(&chat.kind, SUPERGROUP_ID, Some(thread), 701)
            .expect("topic messages link");
        // The middle segment lands the reader INSIDE the topic, not at the
        // top of a thread they then have to search.
        assert_eq!(link, "https://t.me/c/123456789012/42/701");
    }

    #[test]
    fn a_channel_bubble_gets_a_message_link() {
        const CHANNEL_ID: i64 = -100_999_888_777_666;
        let chat = chat_of_kind(serde_json::json!({
            "id": CHANNEL_ID, "type": "channel", "title": "Announcements",
        }));
        let link =
            file_message_link(&chat.kind, CHANNEL_ID, None, 5).expect("channel bubbles link");
        assert_eq!(link, "https://t.me/c/999888777666/5");
    }

    #[test]
    fn a_private_chat_has_no_message_link() {
        let chat = chat_of_kind(serde_json::json!({
            "id": 133_526_395, "type": "private", "first_name": "A",
        }));
        assert!(file_message_link(&chat.kind, 133_526_395, None, 701).is_none());
    }

    #[test]
    fn a_basic_group_has_no_message_link() {
        let chat = chat_of_kind(serde_json::json!({
            "id": -456_789, "type": "group", "title": "Team",
        }));
        assert!(file_message_link(&chat.kind, -456_789, None, 701).is_none());
    }

    #[test]
    fn a_delivered_files_link_is_spliced_onto_its_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files("Here is [Q3 report](q3.pdf).", Some(dir.path()));
        assert_eq!(scan.text, "Here is 📎 Q3 report.");

        let linked = link_file_markers(
            &scan.text,
            &scan,
            &[(pdf, "https://t.me/c/123456789012/701".to_string())],
        );
        assert_eq!(
            linked, "Here is [📎 Q3 report](https://t.me/c/123456789012/701).",
            "the marker becomes a link to the bubble, everything else is untouched"
        );
    }

    #[test]
    fn prose_around_a_spliced_marker_is_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files("before [x](q3.pdf) after", Some(dir.path()));
        let linked = link_file_markers(
            &scan.text,
            &scan,
            &[(pdf, "https://t.me/c/1/1".to_string())],
        );
        assert!(linked.starts_with("before "));
        assert!(linked.ends_with(" after"));
        assert!(linked.contains("[📎 x](https://t.me/c/1/1)"));
    }

    #[test]
    fn a_marker_that_moved_still_links() {
        // The recorded span is a record of WHAT the scanner wrote, not of WHERE
        // it now sits. `text` starts as a copy of `scan.text` and the rewrites
        // after the scan — the echoed-title strip, the react directive, the
        // dedup ladder — can each move a marker. Comparing the span against the
        // marker's bytes reads that mismatch as "the marker is gone" and leaves
        // it plain; searching for the marker's own bytes finds it where it
        // actually is.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files("Report [Q3 report](q3.pdf).", Some(dir.path()));
        assert_eq!(scan.text, "Report 📎 Q3 report.");

        let moved = format!("(redacted) {}", scan.text);
        let linked = link_file_markers(&moved, &scan, &[(pdf, "https://t.me/c/1/701".to_string())]);
        assert_eq!(
            linked, "(redacted) Report [📎 Q3 report](https://t.me/c/1/701).",
            "the span is stale but the marker is still there exactly once, so it \
             links to the bubble that holds the file"
        );
    }

    #[test]
    fn a_marker_after_a_real_rewrite_still_links() {
        // The live path, in the order production actually runs it: the scan
        // happens FIRST, and the rewrites that can move a marker come AFTER it.
        // `strip_echoed_plan_title` drops the plan title the model echoed back
        // (#837), which shifts every following byte left, so the span the
        // scanner recorded no longer names the marker.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files(
            "Q3 numbers\n\nReport [Q3 report](q3.pdf).",
            Some(dir.path()),
        );
        assert_eq!(scan.text, "Q3 numbers\n\nReport 📎 Q3 report.");
        let rewritten = strip_echoed_plan_title(&scan.text, "Q3 numbers");

        assert_ne!(rewritten, scan.text, "the transform has to actually fire");
        assert_eq!(
            rewritten, "Report 📎 Q3 report.",
            "the echoed title line is gone, and everything after it moved"
        );
        // The span the scanner recorded no longer names the marker — the exact
        // condition the old guard refused on, so this test fails on the pre-fix
        // code instead of passing for the wrong reason.
        let span = scan.attachments[0]
            .marker_span
            .clone()
            .expect("the scan records the span it emitted the marker at");
        let real = rewritten
            .find("📎 Q3 report")
            .expect("the marker itself survives the rewrite");
        assert!(
            real < span.start,
            "the rewrite moved the marker to {real}, left of the recorded span {span:?}"
        );

        let linked = link_file_markers(
            &rewritten,
            &scan,
            &[(pdf, "https://t.me/c/1/701".to_string())],
        );
        assert_eq!(
            linked, "Report [📎 Q3 report](https://t.me/c/1/701).",
            "the marker links even though the transform moved it out from under \
             its own span"
        );
    }

    #[test]
    fn a_marker_whose_text_vanished_stays_plain() {
        // The other half of the same rule: a rewrite that does not merely move
        // the marker but REMOVES it — the redaction pass, the dedup ladder —
        // leaves no bytes to find, and nothing may be spliced over whatever
        // took their place. The refusal the old guard made for the wrong
        // reason is still the right answer here.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files("Report [Q3 report](q3.pdf).", Some(dir.path()));
        let rewritten = "Report attached.".to_string();
        let linked = link_file_markers(
            &rewritten,
            &scan,
            &[(pdf, "https://t.me/c/1/701".to_string())],
        );
        assert_eq!(
            linked, rewritten,
            "no marker bytes survive the rewrite, so there is nothing to link"
        );
    }

    #[test]
    fn the_scanner_marker_links_even_when_the_model_echoed_it() {
        // #2001: the model can write the marker's own bytes into its prose —
        // the very `📎 <label>` the scanner emits for the file it attached. The
        // recorded span still names the occurrence the SCANNER wrote, so that
        // one links and the model's own copy keeps its bytes exactly as
        // written: the link never lands on the model's words.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let body = "I wrote 📎 Q3 report in the draft. Attached: 📎 Q3 report";
        let marker = "📎 Q3 report";
        // The span names the marker the SCANNER emitted — the second one; the
        // model's copy sits earlier in the prose.
        let emitted_at = body.rfind(marker).expect("the body carries it twice");
        let scan = LocalFileScan {
            text: body.to_string(),
            attachments: vec![LocalFile {
                path: pdf.clone(),
                caption: Some("Q3 report".to_string()),
                marker_span: Some(emitted_at..emitted_at + marker.len()),
            }],
            failures: Vec::new(),
        };
        assert_eq!(
            link_file_markers(body, &scan, &[(pdf, "https://t.me/c/1/701".to_string())]),
            "I wrote 📎 Q3 report in the draft. Attached: \
             [📎 Q3 report](https://t.me/c/1/701)",
            "only the scanner's own occurrence is cut, so the model's copy keeps \
             its bytes"
        );
    }

    #[test]
    fn two_references_to_one_file_both_link() {
        // The same file named twice is two attachments carrying two identical
        // markers and two valid spans. Each links at its own offset; a search
        // that refuses "two occurrences" as ambiguous would turn two working
        // links into none, even though nothing moved.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files(
            "First [Q3 report](q3.pdf), then [Q3 report](q3.pdf) again.",
            Some(dir.path()),
        );
        assert_eq!(
            scan.attachments.len(),
            2,
            "one attachment per markdown reference, no path dedup"
        );
        let linked = link_file_markers(
            &scan.text,
            &scan,
            &[(pdf, "https://t.me/c/1/701".to_string())],
        );
        assert_eq!(
            linked
                .matches("[📎 Q3 report](https://t.me/c/1/701)")
                .count(),
            2,
            "both markers link, each at its own recorded span: {linked:?}"
        );
    }

    #[test]
    fn two_attachments_that_resolve_to_one_offset_splice_once() {
        // One file named twice survives the scan as two attachments carrying
        // two identical markers at two recorded spans. A later rewrite drops
        // one of the two occurrences and shifts the text, so BOTH recorded
        // spans go stale. Each then falls back to the marker's own bytes,
        // which now exist exactly once, and both resolve to the SAME offset.
        // Cutting that one range twice splices a second link into the first
        // one's bytes, and because the marker's last character is multi-byte
        // the second cut ends inside it: a `replace_range` panic. One
        // occurrence carries one link.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let marker = "📎 Reporté";
        let marker_len = marker.len();
        let scan = LocalFileScan {
            text: format!("A: {marker} B: {marker}"),
            attachments: vec![
                LocalFile {
                    path: pdf.clone(),
                    caption: Some("Reporté".to_string()),
                    marker_span: Some(3..3 + marker_len),
                },
                LocalFile {
                    path: pdf.clone(),
                    caption: Some("Reporté".to_string()),
                    marker_span: Some(3 + marker_len + 4..3 + 2 * marker_len + 4),
                },
            ],
            failures: Vec::new(),
        };
        // Moved: a prefix was added and the second occurrence was dropped, so
        // neither recorded span still names the marker where it claims to.
        let moved = format!("(x) A: {marker}");
        let linked = link_file_markers(&moved, &scan, &[(pdf, "https://t.me/c/1/701".to_string())]);
        assert_eq!(
            linked,
            format!("(x) A: [{marker}](https://t.me/c/1/701)"),
            "one surviving occurrence takes exactly one link"
        );
        assert_eq!(
            linked.matches("https://t.me/c/1/701").count(),
            1,
            "a single occurrence must not be spliced twice: {linked:?}"
        );
    }

    #[test]
    fn an_absent_link_keeps_the_plain_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");
        let scan = extract_local_files("Here is [Q3 report](q3.pdf).", Some(dir.path()));
        // No links at all: a private chat (no link form) or a refusal. The
        // marker is the reader's anchor either way; it never disappears.
        assert_eq!(link_file_markers(&scan.text, &scan, &[]), scan.text);
        // A DIFFERENT file's link must not claim this marker either.
        let other = write_fixture(dir.path(), "other.pdf");
        let linked = link_file_markers(
            &scan.text,
            &scan,
            &[(other, "https://t.me/c/1/2".to_string())],
        );
        assert_eq!(linked, scan.text);
    }
}

// ---------------------------------------------------------------------------
// the rich rewrite: a resolvable link becomes an in-place tg://document ref
// ---------------------------------------------------------------------------

mod rich {
    use crate::utils::image::{DOC_ID_PREFIX, extract_local_files, rewrite_local_files};
    use std::path::{Path, PathBuf};

    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";

    fn write_fixture(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, PDF_BYTES).expect("write fixture");
        path
    }

    #[test]
    fn a_resolved_link_is_rewritten_in_place_as_a_document_reference() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files(
            "Read [report](q3.pdf) first.",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );
        assert_eq!(
            rw.rich, "Read ![📎 report](tg://document?id=doc0) first.",
            "the reference takes the link's exact position"
        );
        assert_eq!(rw.entries.len(), 1, "one entry per rewritten reference");
        assert_eq!(rw.entries[0].id, "doc0");
        assert_eq!(rw.entries[0].file.path, pdf);
        assert_eq!(
            rw.entries[0].file.caption.as_deref(),
            Some("report"),
            "the label captions the document, as the floor does"
        );
    }

    #[test]
    fn two_files_get_doc0_and_doc1_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_fixture(dir.path(), "a.pdf");
        let second = write_fixture(dir.path(), "b.pdf");

        let rw = rewrite_local_files(
            "[A](a.pdf) then [B](b.pdf)",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );
        assert_eq!(
            rw.rich,
            "![📎 A](tg://document?id=doc0) then ![📎 B](tg://document?id=doc1)"
        );
        assert_eq!(rw.entries[0].file.path, first);
        assert_eq!(rw.entries[1].file.path, second);
    }

    #[test]
    fn a_remote_link_is_left_byte_identical() {
        let rw = rewrite_local_files(
            "See [docs](https://example.com/x.pdf) online.",
            None,
            DOC_ID_PREFIX,
            &[],
            true,
        );
        assert_eq!(rw.rich, "See [docs](https://example.com/x.pdf) online.");
        assert!(rw.entries.is_empty());
    }

    #[test]
    fn a_link_inside_a_code_span_is_left_byte_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files(
            "run `cat [x](q3.pdf)` now",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );
        assert_eq!(rw.rich, "run `cat [x](q3.pdf)` now");
        assert!(rw.entries.is_empty());
    }

    #[test]
    fn a_missing_file_stays_a_verbatim_link() {
        // A rejected candidate is the SCAN's to report; the rewrite stays
        // silent and the reader keeps the link that carries its label.
        let dir = tempfile::tempdir().expect("tempdir");
        let rw = rewrite_local_files(
            "See [ghost](nope.pdf).",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );
        assert_eq!(rw.rich, "See [ghost](nope.pdf).");
        assert!(rw.entries.is_empty());
    }

    #[test]
    fn an_already_delivered_file_is_consumed_not_referenced() {
        // The file is already in the chat (an intermediate shipped it).
        // Inlining it AGAIN would put a second copy in the chat, and leaving
        // the link would point at a file the reader already has: consume the
        // reference and record nothing.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files(
            "Read [report](q3.pdf) first.",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[pdf],
            true,
        );
        assert_eq!(rw.rich, "Read  first.", "the consumed link leaves quietly");
        assert!(rw.entries.is_empty());
    }

    #[test]
    fn a_custom_prefix_namespaces_the_ids() {
        // Entries are matched to references BY ID inside one message's media
        // array, so two families cannot share a prefix.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files("[r](q3.pdf)", Some(dir.path()), "attachment", &[], true);
        assert_eq!(rw.rich, "![📎 r](tg://document?id=attachment0)");
    }

    #[test]
    fn an_empty_label_falls_back_to_the_basename_in_the_alt() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files("[](q3.pdf)", Some(dir.path()), DOC_ID_PREFIX, &[], true);
        assert_eq!(
            rw.rich, "![📎 q3.\u{200b}pdf](tg://document?id=doc0)",
            "the alt is disarmed exactly as the text marker is (#1938)"
        );
        assert_eq!(rw.entries[0].file.caption, None, "no label, no caption");
    }

    #[test]
    fn the_scan_and_the_rewrite_agree_on_what_is_a_file() {
        // One input, both walks: the marker form (floor) and the reference
        // form (rich) must claim the SAME references, or a link could be
        // reported twice or not at all.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");
        let input = "[ok](q3.pdf) and [remote](https://x/y) and `[/code/q.pdf](q.pdf)`";

        let scan = extract_local_files(input, Some(dir.path()));
        let rw = rewrite_local_files(input, Some(dir.path()), DOC_ID_PREFIX, &[], true);
        assert_eq!(scan.attachments.len(), rw.entries.len(), "same claims");
        assert_eq!(scan.attachments[0].path, rw.entries[0].file.path);
        // The scanner keeps everything it did not claim byte-identical, and
        // so must the rewrite, minus the one claim.
        assert!(scan.text.contains("📎"));
        assert!(rw.rich.contains("[remote](https://x/y)"));
        assert!(rw.rich.contains("`[/code/q.pdf](q.pdf)`"));
    }

    #[test]
    fn a_dotted_label_is_disarmed_in_the_alt_but_not_the_caption() {
        // The rich alt carries the same marker text the floor does, so the
        // disarmer must reach it too (#1938). The CAPTION is not marker
        // text: it is the file's own metadata and stays verbatim, so a
        // reader who saves the file gets the real name back.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "1918-fix-state.md");

        let rw = rewrite_local_files(
            "See [1918-fix-state.md](1918-fix-state.md) now.",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );

        assert_eq!(
            rw.rich, "See ![📎 1918-fix-state.\u{200b}md](tg://document?id=doc0) now.",
            "the alt is disarmed exactly as the text marker is"
        );
        assert_eq!(
            rw.entries[0].file.caption.as_deref(),
            Some("1918-fix-state.md"),
            "the caption stays verbatim: only visible marker text is disarmed"
        );
    }

    #[test]
    fn a_markdown_reference_inlines_when_inlining_is_opted_into() {
        // #1968 scope pin: with inlining opted into, a `.md` file inlines
        // exactly as a `.pdf` does. The default keeps markdown detached
        // (some clients cannot open an inlined markdown document); the flag
        // exists for setups that prefer inlining.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "notes.md");
        write_fixture(dir.path(), "notes.markdown");

        for name in ["notes.md", "notes.markdown"] {
            let rw = rewrite_local_files(
                &format!("See [{name}]({name})."),
                Some(dir.path()),
                DOC_ID_PREFIX,
                &[],
                true,
            );
            assert!(
                rw.rich.contains("tg://document?id=doc0"),
                "{name} must inline by default"
            );
            assert_eq!(rw.entries.len(), 1, "{name} must produce its entry");
        }
    }

    #[test]
    fn a_markdown_reference_stays_a_marker_when_inlining_is_off() {
        // #1968: at the detached default, a `.md` reference becomes the same
        // visible marker the text plane carries and records NO entry, so the
        // media array never gains the document and the file floor ships it
        // detached. A `.pdf` in the SAME body is untouched: the scope is
        // markdown only.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "notes.md");
        write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files(
            "Notes [here](notes.md) and [report](q3.pdf) attached.",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            false,
        );
        assert!(
            rw.rich.contains("📎 here"),
            "the markdown reference becomes the marker, not a media reference"
        );
        assert!(
            rw.rich.contains("![📎 report](tg://document?id=doc0)"),
            "the pdf takes the first media id and inlines as before"
        );
        assert_eq!(rw.entries.len(), 1, "only the pdf becomes media");
        assert_eq!(
            rw.entries[0].file.path.file_name(),
            Some(std::ffi::OsStr::new("q3.pdf"))
        );
    }

    #[test]
    fn a_hand_written_document_reference_passes_through_byte_identical() {
        // The rewriter owns references it EMITS; a tg:// link the model
        // wrote itself is a telegram media ref, not a local path, and
        // rewriting it would double-encode a reference that already points
        // at the wire.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files(
            "See ![sheet](tg://document?id=q3) and [report](q3.pdf).",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );

        assert!(
            rw.rich.contains("![sheet](tg://document?id=q3)"),
            "a hand-written tg:// reference survives untouched: {:?}",
            rw.rich
        );
        assert_eq!(rw.entries.len(), 1, "only the local link produces an entry");
        assert_eq!(rw.entries[0].id, "doc0");
    }
}

// ---------------------------------------------------------------------------
// the intermediate plane: a rich report's files ship (inline) or mark (floor)
// ---------------------------------------------------------------------------

mod intermediate {
    use crate::channels::telegram::TelegramState;
    use crate::channels::telegram::flow::StreamingState;
    use crate::channels::telegram::intermediates::deliver_intermediate_message;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const CHAT: i64 = 133_526_395;
    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";
    const RICH_OK: &str = r#"{"ok":true,"result":{"message_id":812}}"#;
    const SEND_MESSAGE_OK: &str = r#"{"ok":true,"result":{"message_id":701,"date":1757166400,"chat":{"id":133526395,"type":"private"},"text":"ok"}}"#;

    fn write_fixture(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, PDF_BYTES).expect("write fixture");
        path
    }

    fn test_bot(server: &mockito::ServerGuard) -> teloxide::Bot {
        teloxide::Bot::with_client(
            "TESTTOKEN",
            reqwest_teloxide::Client::builder().build().unwrap(),
        )
        .set_api_url(server.url().parse().unwrap())
    }

    /// Minimal StreamingState for the intermediate plane (field set mirrors
    /// the house pattern in src/tests/telegram_state_test.rs, plus the
    /// #1918 delivered ledger this module reads back).
    fn streaming() -> Arc<std::sync::Mutex<StreamingState>> {
        Arc::new(std::sync::Mutex::new(StreamingState {
            is_dm: true,
            pending_suggestions: None,
            pending_trailer: None,
            delivered_files: Vec::new(),
            media_intermediates: Vec::new(),
            msg_id: None,
            thinking: String::new(),
            tool_msgs: Vec::new(),
            display_queue: Vec::new(),
            open_group_msg_id: None,
            rich_transport_failures: 0,
            flow_entries: Vec::new(),
            flow_status: None,
            flow_rich: false,
            response: String::new(),
            final_bubble: None,
            dirty: false,
            recreate: false,
            header_preview: None,
            compacting: false,
            sections: Default::default(),
            retained_goal: None,
            applied_plan_kb: Default::default(),
            tool_round_count: 0,
            tools_started_at: None,
            turn_started_at: std::time::Instant::now(),
            flow_outcome: None,
            bg_indicator: None,
            bg_count: None,
            subagent_counts: Default::default(),
            sent_intermediates: Vec::new(),
            intermediate_bubbles: Vec::new(),
            voice_msg_ids: Vec::new(),
            processing: true,
            is_cli: false,
        }))
    }

    #[tokio::test]
    async fn a_report_with_a_file_inlines_it_in_the_rich_media_array() {
        // The rich plane owns the file: the document rides the bubble's media
        // array at its reference, so there is NO detached document bubble,
        // and the file is recorded as delivered so the final leg cannot
        // ship a second copy.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let mut server = mockito::Server::new_async().await;
        let rich_mock = server
            .mock("POST", "/botTESTTOKEN/sendRichMessage")
            .match_body(mockito::Matcher::Regex(
                "tg://document\\?id=doc0".to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(RICH_OK)
            .expect(1)
            .create_async()
            .await;
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            // expect(0) is the assertion: an inlined document never also
            // ships as its own detached bubble.
            .expect(0)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let st = streaming();
        let tg = TelegramState::new();
        let ok = deliver_intermediate_message(
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &st,
            &tg,
            uuid::Uuid::new_v4(),
            dir.path(),
            "Report ready: [q3.pdf](q3.pdf)\n\nSome prose so the report has a body.",
        )
        .await;

        assert!(ok, "the rich bubble is the delivery");
        rich_mock.assert_async().await;
        document_mock.assert_async().await;
        let s = st.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            s.delivered_files
                .iter()
                .map(|d| d.path.clone())
                .collect::<Vec<_>>(),
            vec![pdf],
            "an inlined file counts as delivered for the final leg"
        );
        assert_eq!(
            s.delivered_files[0].message_id, 812,
            "the recorded address is the rich bubble that carries the file"
        );
        assert_eq!(s.sent_intermediates.len(), 1);
        // #1939: the media-bearing bubble is the only copy of the file, so
        // its id belongs to the undeletable ledger, never to the delete
        // list the rich fallback consumes.
        assert_eq!(
            s.media_intermediates.len(),
            1,
            "the bubble is recorded as media-bearing"
        );
        assert!(
            s.intermediate_bubbles.is_empty(),
            "a media-bearing bubble is never deletable"
        );
    }

    #[tokio::test]
    async fn a_declined_rich_send_ships_the_file_as_its_own_bubble() {
        // When the rich send fails, the floor is the file's only remaining
        // leg: it ships as its own document bubble and the marker text
        // carries the reader's anchor. A marker with no delivery behind it
        // would point at a file that never reaches the chat.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let mut server = mockito::Server::new_async().await;
        // The rich ladder fires twice on a dead endpoint: markdown+media
        // first, then the HTML dialect fallback. Both decline, and only then
        // does the file drop to the floor.
        let rich_mock = server
            .mock("POST", "/botTESTTOKEN/sendRichMessage")
            .with_status(500)
            .with_body("boom")
            .expect(2)
            .create_async()
            .await;
        let document_mock = server
            .mock("POST", "/botTESTTOKEN/SendDocument")
            .match_body(mockito::Matcher::Regex("q3".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            .expect(1)
            .create_async()
            .await;
        let text_mock = server
            .mock("POST", "/botTESTTOKEN/SendMessage")
            // The marked body carries the autolink breaker (#1938): a
            // literal dot followed by U+200B, so the naive `q3.pdf`
            // pattern (any ONE char) no longer matches.
            .match_body(mockito::Matcher::Regex("q3\\.\\u{200b}pdf".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(SEND_MESSAGE_OK)
            .expect(1)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let st = streaming();
        let tg = TelegramState::new();
        let ok = deliver_intermediate_message(
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &st,
            &tg,
            uuid::Uuid::new_v4(),
            dir.path(),
            "Report ready: [q3.pdf](q3.pdf)\n\nProse body for the bubble.",
        )
        .await;

        rich_mock.assert_async().await;
        document_mock.assert_async().await;
        text_mock.assert_async().await;
        assert!(ok);
        let s = st.lock().unwrap_or_else(|e| e.into_inner());
        // #1939: on this leg each document IS its own bubble, so the pair
        // points at the document itself, and that id never joins
        // `intermediate_bubbles` (only the text chunks do): the rich
        // fallback deletes the text around the file, never the file.
        assert_eq!(
            s.delivered_files,
            vec![crate::channels::telegram::delivery::DeliveredFile {
                path: pdf,
                message_id: 701,
            }],
            "the pair points at the document's own bubble"
        );
    }

    #[tokio::test]
    async fn a_repeated_intermediate_does_not_ship_its_file_twice() {
        // The dedup ledger keeps the PRE-scan text, so an identical
        // intermediate misses no check and returns before the scan runs:
        // the file shipped the first time, and the second attempt must add
        // no request at all.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "q3.pdf");
        let mut server = mockito::Server::new_async().await;
        let rich_mock = server
            .mock("POST", "/botTESTTOKEN/sendRichMessage")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(RICH_OK)
            .expect(1)
            .create_async()
            .await;

        let bot = test_bot(&server);
        let st = streaming();
        let tg = TelegramState::new();
        let text = "Report ready: [q3.pdf](q3.pdf)\n\nProse body for the bubble.";
        let first = deliver_intermediate_message(
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &st,
            &tg,
            uuid::Uuid::new_v4(),
            dir.path(),
            text,
        )
        .await;
        let second = deliver_intermediate_message(
            &bot,
            teloxide::types::ChatId(CHAT),
            None,
            &st,
            &tg,
            uuid::Uuid::new_v4(),
            dir.path(),
            text,
        )
        .await;

        assert!(first);
        assert!(second, "the repeat reports success without re-delivering");
        rich_mock.assert_async().await; // exactly ONE rich send happened
    }

    // The shield invariant (#1921): a tg://document reference reaches the
    // wire only with its media entry. The probe is where that is enforced.
    #[cfg(unix)]
    #[test]
    fn the_probe_reports_an_unreadable_file_instead_of_its_bytes() {
        use crate::utils::image::{LocalFile, probe_document_bytes};
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let good = write_fixture(dir.path(), "good.pdf");
        let sealed = dir.path().join("sealed.pdf");
        std::fs::write(&sealed, b"%PDF-1.7\n").expect("write sealed");
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000))
            .expect("seal the file");

        let files = vec![
            LocalFile {
                path: good.clone(),
                caption: Some("good".to_string()),
                marker_span: None,
            },
            LocalFile {
                path: sealed.clone(),
                caption: Some("sealed".to_string()),
                marker_span: None,
            },
        ];

        let (readable, failures) = probe_document_bytes(&files);

        assert_eq!(
            readable.keys().collect::<Vec<_>>(),
            vec![&good],
            "exactly the readable file comes back with bytes"
        );
        assert_eq!(failures.len(), 1, "one failure, naming the sealed file");
        assert_eq!(failures[0].resolved, Some(sealed.clone()));
        assert_eq!(
            failures[0].reason,
            crate::utils::image::LocalImageFailureReason::Unreadable
        );
    }
}

// ---------------------------------------------------------------------------
// the rich fallback vs a media-bearing intermediate (#1939)
// ---------------------------------------------------------------------------

mod fallback {
    use crate::channels::telegram::delivery::{
        DeliveredFile, file_message_link, link_file_markers,
    };
    use crate::channels::telegram::intermediates::fallback_would_duplicate;
    use crate::utils::image::extract_local_files;
    use std::path::Path;

    const SUPERGROUP_ID: i64 = -100_123_456_789_012;

    fn write_fixture(dir: &Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"%PDF-1.7 fixture").expect("write fixture");
        p
    }

    fn chat_of_kind(v: serde_json::Value) -> teloxide::types::Chat {
        serde_json::from_value(v).expect("chat json")
    }

    #[test]
    fn a_media_intermediate_that_carries_the_body_suppresses_the_resend() {
        // The fallback arm's premise is that it REPLACES what it deletes. A
        // media-bearing bubble is not in the delete list (its media array is
        // the only copy of the documents), so re-sending the body it already
        // carries would leave the reader with it twice (#1939). The
        // normalization is the dedup ladder's own, so both agree.
        let body = "Here is the report, inline.";
        let media = vec![(teloxide::types::MessageId(90_465), body.to_string())];
        assert!(
            fallback_would_duplicate(&media, body),
            "the bubble already delivered this body and cannot be replaced"
        );
        // The same body with different whitespace still counts: that is what
        // "the body" means everywhere else in this file's dedup pins.
        assert!(fallback_would_duplicate(
            &media,
            "Here  is the\nreport, inline."
        ));
        // A media bubble whose text differs suppresses nothing: an
        // unrelated answer must still reach the reader.
        assert!(!fallback_would_duplicate(
            &media,
            "An entirely different answer."
        ));
        // No media-bearing intermediates: the arm's original premise holds
        // and the check must never block it.
        assert!(!fallback_would_duplicate(&[], body));
    }

    #[test]
    fn the_fallback_links_a_marker_to_the_intermediates_bubble() {
        // When the rich plane owned the documents, the final leg's `delivered`
        // list is empty, so its own links list is too. The ledger's pair is
        // the document's real address; merged into the SAME list, the one
        // splice links both origins by one rule. This drives the merge the
        // way delivery.rs composes it: recover the pair, build the chat
        // kind's link over the intermediate's id, splice.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");
        let body = "Report: [Q3 report](q3.pdf).";
        let scan = extract_local_files(body, Some(dir.path()));
        assert_eq!(scan.text, "Report: 📎 Q3 report.");

        let ledger = vec![DeliveredFile {
            path: pdf.clone(),
            message_id: 91_047,
        }];
        let chat = chat_of_kind(serde_json::json!({
            "id": SUPERGROUP_ID, "type": "supergroup", "title": "g"
        }));
        let mut links: Vec<(std::path::PathBuf, String)> = Vec::new();
        for file in &ledger {
            if let Some(link) = file_message_link(&chat.kind, SUPERGROUP_ID, None, file.message_id)
            {
                links.push((file.path.clone(), link));
            }
        }
        assert_eq!(
            link_file_markers(&scan.text, &scan, &links),
            "Report: [📎 Q3 report](https://t.me/c/123456789012/91047).",
            "the marker points at the bubble that actually holds the document"
        );

        // A chat without a link form yields no link from the same pair, so
        // the marker stays plain rather than pointing nowhere.
        let dm = chat_of_kind(serde_json::json!({
            "id": 777_001, "type": "private", "first_name": "a"
        }));
        assert!(file_message_link(&dm.kind, -456_789, None, 91_047).is_none());
    }
}

mod marker_consume {
    use crate::utils::image::{
        DOC_ID_PREFIX, LocalFileScan, extract_local_files, rewrite_local_files,
    };
    use std::path::{Path, PathBuf};

    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";

    fn write_fixture(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, PDF_BYTES).expect("write fixture");
        path
    }

    fn paths(scan: &LocalFileScan) -> Vec<PathBuf> {
        scan.attachments.iter().map(|f| f.path.clone()).collect()
    }

    #[test]
    fn a_model_written_marker_prefix_is_not_doubled_on_the_text_plane() {
        // #2001: the capability line tells the model the reference becomes a
        // `📎 <label>` marker, so a model may prefix that marker itself. The
        // harness consumes the model's own `📎 ` before emitting its own, so
        // the reader sees exactly ONE marker rather than `📎 📎 <label>`.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");

        let scan = extract_local_files(
            &format!("before 📎 [Q3 report]({}) after", pdf.display()),
            Some(dir.path()),
        );

        assert_eq!(
            scan.text, "before 📎 Q3 report after",
            "the model's own 📎 is consumed, not doubled"
        );
        assert_eq!(scan.text.matches('📎').count(), 1, "exactly one marker");
        assert_eq!(paths(&scan), vec![pdf]);
    }

    #[test]
    fn a_run_of_model_prefixes_collapses_to_one_marker() {
        // The consume is a loop over trailing `📎` runs, so more than one
        // model-written prefix still leaves room for exactly one harness
        // marker, and the loop terminates because every pass shortens the
        // buffer.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");

        let scan = extract_local_files(
            &format!("📎 📎 [Q3 report]({})", pdf.display()),
            Some(dir.path()),
        );

        assert_eq!(scan.text, "📎 Q3 report", "one marker from a whole run");
        assert_eq!(scan.text.matches('📎').count(), 1);
        assert_eq!(paths(&scan), vec![pdf]);
    }

    #[test]
    fn a_model_written_marker_prefix_is_not_doubled_in_the_rich_plane() {
        // #2001, rich plane: a model's own `📎 ` before the reference must
        // not survive beside the harness alt, or the reader gets
        // `📎 ![📎 <label>](tg://document?id=...)`.
        let dir = tempfile::tempdir().expect("tempdir");
        let pdf = write_fixture(dir.path(), "q3.pdf");

        let rw = rewrite_local_files(
            &format!("before 📎 [Q3 report]({}) after", pdf.display()),
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            true,
        );

        assert_eq!(
            rw.rich.matches('📎').count(),
            1,
            "exactly one marker: {:?}",
            rw.rich
        );
        assert_eq!(
            rw.rich, "before ![📎 Q3 report](tg://document?id=doc0) after",
            "the model's own 📎 is consumed, not doubled"
        );
        assert_eq!(rw.entries.len(), 1);
    }

    #[test]
    fn the_detached_marker_arm_consumes_the_prefix_too() {
        // The markdown detached default (#1968) emits the same visible
        // marker on the rich plane, so the same doubling is possible there
        // and the same consume guards it. No entry is recorded, as with any
        // detached-default hit.
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path(), "notes.md");

        let rw = rewrite_local_files(
            "before 📎 [notes](notes.md)",
            Some(dir.path()),
            DOC_ID_PREFIX,
            &[],
            false,
        );

        assert_eq!(
            rw.rich.matches('📎').count(),
            1,
            "exactly one marker: {:?}",
            rw.rich
        );
        assert_eq!(
            rw.rich, "before 📎 notes",
            "the marker replaces the model's prefix"
        );
        assert!(
            rw.entries.is_empty(),
            "an opt-out hit records no media entry"
        );
    }
}
