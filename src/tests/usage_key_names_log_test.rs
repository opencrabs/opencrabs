//! The usage object's key names are logged at debug, never its values (#2152).

#[test]
fn both_response_paths_log_usage_key_names() {
    let src = include_str!("../brain/provider/custom_openai_compatible.rs");
    assert!(
        src.matches("log_usage_keys(").count() >= 3,
        "the helper must run on the non-streaming body and on each streaming event"
    );
}

#[test]
fn the_helper_logs_key_names_not_values() {
    let src = include_str!("../brain/provider/custom_openai_compatible.rs");
    let start = src.find("fn log_usage_keys").expect("helper present");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("helper end");
    let helper = &body[..end];
    assert!(
        helper.contains("keys.push(key.clone())"),
        "key names are logged"
    );
    assert!(
        !helper.contains("val.to_string()") && !helper.contains("value.to_string()"),
        "the helper must never log a value"
    );
}
