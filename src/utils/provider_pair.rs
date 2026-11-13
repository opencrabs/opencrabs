//! `provider/model` pair parsing, shared by every non-interactive switch
//! surface (#461 family: CLI set-model, direct-arg /models on channels and
//! the TUI). The FIRST slash splits, so model names that contain slashes
//! survive intact (`openrouter/tencent/hy3:free` → `openrouter` +
//! `tencent/hy3:free`).

pub fn parse_pair(pair: &str) -> Result<(String, String), String> {
    match pair.split_once('/') {
        Some((provider, model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
            Ok((provider.trim().to_string(), model.trim().to_string()))
        }
        _ => Err(format!(
            "'{pair}' is not a provider/model pair — expected e.g. \
             openrouter/tencent/hy3:free (the first slash splits)"
        )),
    }
}

/// The model name with every leading `provider/` peeled off.
///
/// A custom OpenAI-compatible provider may store its catalogue with its own
/// prefix (`ngodeai/glm-5.3-flash`) while every other provider stores bare
/// names. A pin can therefore arrive bare, self-prefixed, or doubled, and all
/// three name the same model (#2289).
pub fn bare_model<'a>(provider: &str, model: &'a str) -> &'a str {
    let provider = provider.trim();
    let mut rest = model.trim();
    if provider.is_empty() {
        return rest;
    }
    let prefix = format!("{provider}/");
    while let Some(stripped) = rest.strip_prefix(&prefix) {
        rest = stripped;
    }
    rest
}

/// Compose the `provider/model` spelling for a message without doubling the
/// prefix. Sub-vendor paths stay intact: `inferhub` serving `alicn/...` renders
/// `inferhub/alicn/...`, because only the provider's own prefix is peeled.
pub fn display_model(provider: &str, model: &str) -> String {
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return model.to_string();
    }
    let bare = bare_model(provider, model);
    if bare.is_empty() {
        return model.to_string();
    }
    format!("{provider}/{bare}")
}

/// Resolve `model` to the spelling the catalogue itself uses.
///
/// Tried in order: the exact spelling, then the provider-prefixed one, then the
/// bare name, then the provider prefix re-applied to that bare name (which is
/// what resolves a doubled `provider/provider/model` pin against a catalogue
/// that stores `provider/model`). `None` means the catalogue holds no entry for
/// this model, so the caller decides what to do instead of this function
/// guessing.
pub fn canonical_model(provider: &str, model: &str, supported: &[String]) -> Option<String> {
    let provider = provider.trim();
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    if supported.iter().any(|candidate| candidate == model) {
        return Some(model.to_string());
    }
    if !provider.is_empty() {
        let prefixed = format!("{provider}/{model}");
        if supported.iter().any(|candidate| candidate == &prefixed) {
            return Some(prefixed);
        }
        let bare = bare_model(provider, model);
        if bare != model {
            if supported.iter().any(|candidate| candidate == bare) {
                return Some(bare.to_string());
            }
            let recombined = format!("{provider}/{bare}");
            if supported.iter().any(|candidate| candidate == &recombined) {
                return Some(recombined);
            }
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    fn cat(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn bare_model_peels_every_provider_prefix() {
        assert_eq!(bare_model("ngodeai", "ngodeai/glm-5"), "glm-5");
        assert_eq!(bare_model("ngodeai", "ngodeai/ngodeai/glm-5"), "glm-5");
        assert_eq!(bare_model("ngodeai", "glm-5"), "glm-5");
        assert_eq!(bare_model("ngodeai", "other/glm-5"), "other/glm-5");
        assert_eq!(bare_model("", "glm-5"), "glm-5");
    }

    #[test]
    fn display_model_never_doubles_the_prefix() {
        assert_eq!(display_model("ngodeai", "ngodeai/glm-5"), "ngodeai/glm-5");
        assert_eq!(display_model("ngodeai", "glm-5"), "ngodeai/glm-5");
        assert_eq!(display_model("inferhub", "alicn/q"), "inferhub/alicn/q");
        assert_eq!(display_model("ngodeai", ""), "");
    }

    #[test]
    fn canonical_model_prefers_the_catalogue_spelling() {
        let prefixed = cat(&["ngodeai/glm-5.3-flash", "ngodeai/glm-5.3-air"]);
        assert_eq!(
            canonical_model("ngodeai", "glm-5.3-flash", &prefixed).as_deref(),
            Some("ngodeai/glm-5.3-flash")
        );
        assert_eq!(
            canonical_model("ngodeai", "ngodeai/ngodeai/glm-5.3-flash", &prefixed).as_deref(),
            Some("ngodeai/glm-5.3-flash")
        );
        let bare = cat(&["glm-5.3-flash"]);
        assert_eq!(
            canonical_model("ngodeai", "ngodeai/glm-5.3-flash", &bare).as_deref(),
            Some("glm-5.3-flash")
        );
        assert_eq!(canonical_model("ngodeai", "glm-9", &prefixed), None);
        assert_eq!(canonical_model("ngodeai", "", &prefixed), None);
    }

    #[test]
    fn exact_catalogue_entry_is_returned_byte_identical() {
        let pinned = "Qwen-Ambassador/Qwen3.7-Max".to_string();
        let supported = vec![pinned.clone()];
        assert_eq!(
            canonical_model("modelscope-qwen", &pinned, &supported),
            Some(pinned)
        );
    }
}
