//! Class-kill guard for the Telegram dispatcher startup path (#2228).
//!
//! In teloxide 0.17 `Dispatcher::dispatch_with_listener` is a wrapper that
//! `.expect()`s the startup `get_me` result (`dispatcher.rs:391-395`), while
//! `try_dispatch_with_listener` returns the error. A single timeout on that one
//! call therefore panicked the dispatcher task, and a panic leaves through the
//! task boundary, not the reconnect loop around it, so the loop's `warn!`,
//! sleep and reconnect never ran. One dropped packet at startup meant Telegram
//! intake was gone until the process restarted, with nothing but a stray stderr
//! line to show for it.
//!
//! These scans keep the non-panicking entry point in place. A future edit that
//! swaps it back fails here instead of in production.

const AGENT_SRC: &str = include_str!("../channels/telegram/agent.rs");

#[test]
fn the_dispatcher_starts_through_the_non_panicking_entry_point() {
    assert!(
        AGENT_SRC.contains(".try_dispatch_with_listener("),
        "the Telegram dispatcher must start through try_dispatch_with_listener so a failed \
         get_me returns an error the reconnect loop can handle (#2228)"
    );
}

#[test]
fn no_call_site_uses_the_panicking_dispatch_entry_point() {
    // Note the leading dot: `.dispatch_with_listener(` cannot match
    // `.try_dispatch_with_listener(`.
    assert!(
        !AGENT_SRC.contains(".dispatch_with_listener("),
        "dispatch_with_listener panics when the startup get_me fails, and the panic skips the \
         reconnect loop entirely; use try_dispatch_with_listener (#2228)"
    );
}

#[test]
fn a_failed_dispatcher_start_is_logged_before_the_reconnect_sleep() {
    let failed_at = AGENT_SRC.find("dispatcher could not start").expect(
        "a failed dispatcher start must be logged, not swallowed by a dropped JoinHandle (#2228)",
    );

    // `get`, not `[failed_at..]`: the offset comes from `str::find`, so it is
    // always a char boundary, but indexing says "I prove this" to nobody and
    // trips the byte-slice ratchet (#2230). `get` cannot panic at all.
    let after = AGENT_SRC
        .get(failed_at..)
        .expect("failed_at comes from str::find, so it is always a char boundary");
    let retry_at = after.find("reconnecting in 5s").expect(
        "the failure log must be followed by the reconnect notice so the operator sees a retry",
    );

    assert!(
        retry_at < 200,
        "the reconnect notice should sit right after the failure log, found it {retry_at} bytes \
         later"
    );
}
