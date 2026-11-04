use crate::brain::provider::error::*;

#[test]
fn test_error_retryable() {
    let rate_limit = ProviderError::RateLimitExceeded("Try again later".to_string());
    assert!(rate_limit.is_retryable());

    let invalid_key = ProviderError::InvalidApiKey;
    assert!(!invalid_key.is_retryable());

    let server_error = ProviderError::ApiError {
        status: 500,
        message: "Internal Server Error".to_string(),
        error_type: None,
    };
    assert!(server_error.is_retryable());

    let client_error = ProviderError::ApiError {
        status: 400,
        message: "Bad Request".to_string(),
        error_type: None,
    };
    assert!(!client_error.is_retryable());
}

#[test]
fn flaky_404_is_retryable_but_model_not_found_404_is_not() {
    // #748: a flaky provider's transient 404 must retry with backoff...
    let flaky = ProviderError::ApiError {
        status: 404,
        message: "Not Found".to_string(),
        error_type: None,
    };
    assert!(
        flaky.is_retryable(),
        "a non-model 404 is a transient infra hiccup and must retry"
    );

    // ...but a genuine model-not-found 404 stays permanent.
    let model_404 = ProviderError::ApiError {
        status: 404,
        message: "The model gpt-x is not found".to_string(),
        error_type: Some("model_not_found".to_string()),
    };
    assert!(
        !model_404.is_retryable(),
        "a model-not-found 404 is permanent, not retryable"
    );
}

#[test]
fn repetitive_tool_guardrail_500_is_not_retryable() {
    // #740: the repetition guardrail 500 will 500 again on retry with the same
    // poisoned history — surface it fast so the tool loop can prune and retry.
    let poison = ProviderError::ApiError {
        status: 500,
        message: "Repetitive tool calls detected in the conversation history. The same tool call \
                  with identical name and arguments has been repeated across multiple consecutive \
                  rounds."
            .to_string(),
        error_type: Some("invalid_request_error".to_string()),
    };
    assert!(
        !poison.is_retryable(),
        "the repetition guardrail 500 must not be retried in place"
    );

    // A plain 500 is still retryable.
    let plain = ProviderError::ApiError {
        status: 500,
        message: "Internal Server Error".to_string(),
        error_type: None,
    };
    assert!(plain.is_retryable());
}

#[test]
fn test_status_code() {
    let error = ProviderError::ApiError {
        status: 429,
        message: "Too many requests".to_string(),
        error_type: Some("rate_limit_error".to_string()),
    };
    assert_eq!(error.status_code(), Some(429));

    let invalid_key = ProviderError::InvalidApiKey;
    assert_eq!(invalid_key.status_code(), None);
}

// #2204: the connector's single deadline covers DNS, TCP and TLS together, and
// on expiry hyper reports "tcp connect error" whichever phase actually hung. A
// resolver that never answered therefore read as a slow server. These pin the
// three branches without touching the network, by feeding the classifier the
// shapes it has to tell apart.

#[test]
fn a_connect_timeout_whose_name_does_not_resolve_names_dns() {
    let label = describe_timeout(true, DnsProbe::Failed, " (api.example)");
    assert_eq!(
        label, "DNS lookup failed (api.example)",
        "a resolver error inside the connect window must not read as a slow server"
    );
}

#[test]
fn a_connect_timeout_whose_resolver_hangs_names_dns() {
    let label = describe_timeout(true, DnsProbe::TimedOut, " (api.example)");
    assert_eq!(
        label, "DNS lookup failed (api.example)",
        "a resolver that never answered is a DNS failure, not a slow server"
    );
}

#[test]
fn a_connect_timeout_whose_name_resolves_names_the_connection() {
    let label = describe_timeout(true, DnsProbe::Resolved, " (api.example)");
    assert_eq!(
        label, "connect timed out (api.example)",
        "the name answered, so the wait was the connection"
    );
}

#[test]
fn a_timeout_after_the_connection_is_up_keeps_the_request_label() {
    let label = describe_timeout(false, DnsProbe::Resolved, " (api.example)");
    assert_eq!(
        label, "request timed out (api.example)",
        "a socket that was already up did not fail to resolve anything"
    );
}

#[test]
fn a_url_without_a_host_still_labels_the_connect_timeout() {
    let label = describe_timeout(true, DnsProbe::Skipped, "");
    assert_eq!(label, "connect timed out");
}

#[test]
fn an_ip_literal_never_touches_the_resolver() {
    assert_eq!(
        probe_dns("192.0.2.1", std::time::Duration::from_secs(2)),
        DnsProbe::Resolved,
        "an address cannot fail to resolve, so the probe must not spawn a lookup"
    );
    assert_eq!(
        probe_dns("::1", std::time::Duration::from_secs(2)),
        DnsProbe::Resolved
    );
}
