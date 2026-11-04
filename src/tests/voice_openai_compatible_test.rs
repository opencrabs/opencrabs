//! Tests for OpenAI-compatible TTS and STT providers.
//!
//! Uses mockito to simulate `/v1/audio/speech` and `/v1/audio/transcriptions`
//! endpoints without hitting real APIs.

#[cfg(test)]
mod tests {
    use crate::channels::voice::openai_stt;
    use crate::channels::voice::openai_tts;

    // ─── URL building ───────────────────────────────────────────────────────

    #[test]
    fn build_endpoint_url_with_trailing_slash() {
        let url =
            openai_tts::build_endpoint_url("http://localhost:11434/", "v1/audio/speech").unwrap();
        assert_eq!(url, "http://localhost:11434/v1/audio/speech");
    }

    #[test]
    fn build_endpoint_url_without_trailing_slash() {
        let url =
            openai_tts::build_endpoint_url("http://localhost:11434", "v1/audio/speech").unwrap();
        assert_eq!(url, "http://localhost:11434/v1/audio/speech");
    }

    #[test]
    fn build_endpoint_url_openai_style() {
        let url =
            openai_tts::build_endpoint_url("https://api.openai.com", "v1/audio/speech").unwrap();
        assert_eq!(url, "https://api.openai.com/v1/audio/speech");
    }

    #[test]
    fn build_endpoint_url_groq_style() {
        let url = openai_tts::build_endpoint_url(
            "https://api.groq.com/openai",
            "v1/audio/transcriptions",
        )
        .unwrap();
        assert_eq!(url, "https://api.groq.com/openai/v1/audio/transcriptions");
    }

    #[test]
    fn build_endpoint_url_invalid_base() {
        let result = openai_tts::build_endpoint_url("not-a-url", "v1/audio/speech");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid base URL"));
    }

    // ─── TTS ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn tts_empty_text_rejected() {
        let result = openai_tts::synthesize_speech(
            "",
            "fake-key",
            "echo",
            "gpt-4o-mini-tts",
            "http://localhost:9999",
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty text"));
    }

    #[tokio::test]
    async fn tts_mocks_successful_synthesis() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/audio/speech")
            .match_header("authorization", "Bearer test-api-key")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "model": "gpt-4o-mini-tts",
                "input": "hello world",
                "voice": "echo",
                "response_format": "opus"
            })))
            .with_status(200)
            .with_header("content-type", "audio/ogg")
            .with_body(b"\x00\x01\x02\x03\x04\x05fake-opus-bytes")
            .create_async()
            .await;

        let result = openai_tts::synthesize_speech(
            "hello world",
            "test-api-key",
            "echo",
            "gpt-4o-mini-tts",
            &mock_url,
        )
        .await;

        assert!(result.is_ok());
        let bytes = result.unwrap();
        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn tts_server_error_propagated() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/audio/speech")
            .with_status(401)
            .with_body(r#"{"error": "Invalid API key"}"#)
            .create_async()
            .await;

        let result = openai_tts::synthesize_speech(
            "hello",
            "wrong-key",
            "echo",
            "gpt-4o-mini-tts",
            &mock_url,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("401"));
        assert!(err.contains("Invalid API key"));
    }

    #[tokio::test]
    async fn tts_connection_refused_on_bad_url() {
        let result =
            openai_tts::synthesize_speech("hello", "key", "echo", "model", "http://localhost:1")
                .await;
        assert!(result.is_err());
        // Connection refused or timeout — either is acceptable
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("send") || err.contains("connect") || err.contains("Connection"),
            "Should be a connection error: {}",
            err
        );
    }

    // ─── STT ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stt_mocks_successful_transcription() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/audio/transcriptions")
            .match_header("authorization", "Bearer test-stt-key")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"text": "hello from mock transcription"}"#)
            .create_async()
            .await;

        let result = openai_stt::transcribe_audio(
            vec![0x00, 0x01, 0x02, 0x03],
            "test-stt-key",
            "whisper-large-v3-turbo",
            &mock_url,
        )
        .await;

        assert!(result.is_ok());
        let text = result.unwrap();
        assert_eq!(text, "hello from mock transcription");
    }

    #[tokio::test]
    async fn stt_server_error_propagated() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/audio/transcriptions")
            .with_status(429)
            .with_body(r#"{"error": "Rate limit exceeded"}"#)
            .create_async()
            .await;

        let result =
            openai_stt::transcribe_audio(vec![0x00, 0x01], "key", "whisper", &mock_url).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("429"));
        assert!(err.contains("Rate limit"));
    }

    #[tokio::test]
    async fn stt_invalid_json_response() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        // Returns 200 but body is not valid JSON for TranscriptionResponse
        let _mock = server
            .mock("POST", "/v1/audio/transcriptions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"not_text": "missing text field"}"#)
            .create_async()
            .await;

        let result =
            openai_stt::transcribe_audio(vec![0x00, 0x01], "key", "whisper", &mock_url).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("parse") || err.contains("text") || err.contains("Failed"));
    }

    // ─── STT on the chat route (built-in endpoint) ──────────────────────────

    #[test]
    fn chat_route_is_chosen_only_for_the_built_in_host() {
        assert!(openai_stt::serves_audio_on_chat_route(
            "https://inference.opencrabs.com"
        ));
        assert!(openai_stt::serves_audio_on_chat_route(
            "https://inference.opencrabs.com/v1"
        ));
        assert!(!openai_stt::serves_audio_on_chat_route(
            "https://api.openai.com"
        ));
        assert!(!openai_stt::serves_audio_on_chat_route(
            "https://api.groq.com/openai"
        ));
        assert!(!openai_stt::serves_audio_on_chat_route(
            "http://localhost:11434"
        ));
    }

    #[tokio::test]
    async fn chat_route_falls_back_to_reasoning_content() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        // Some models leave content empty and put the answer in reasoning_content.
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer test-stt-key")
            .match_body(mockito::Matcher::Regex(
                r#""input_audio":\{"data":"AAECAw==","format":"ogg"\}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"choices":[{"message":{"content":"","reasoning_content":"hello from audio"}}]}"#,
            )
            .create_async()
            .await;

        let result = openai_stt::transcribe_audio_chat(
            vec![0x00, 0x01, 0x02, 0x03],
            "test-stt-key",
            "mimo/mimo-v2.6-flash",
            &mock_url,
        )
        .await;

        mock.assert_async().await;
        assert_eq!(result.unwrap(), "hello from audio");
    }

    #[tokio::test]
    async fn chat_route_reads_content() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"choices":[{"message":{"content":"content transcript","reasoning_content":""}}]}"#,
            )
            .create_async()
            .await;

        let result = openai_stt::transcribe_audio_chat(vec![0x00], "key", "model", &mock_url).await;

        assert_eq!(result.unwrap(), "content transcript");
    }

    #[tokio::test]
    async fn chat_route_prefers_content_over_the_thinking_trace() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        // Live wire check 2026-10-19: with a spoken note, qwen3.8-omni-flash answers in
        // content and puts a ~1200 char thinking trace in reasoning_content. Reading
        // reasoning_content first would hand the user the model's thoughts.
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"choices":[{"message":{"content":"the actual transcript","reasoning_content":"Okay, the user wants a transcription. Let me listen to the audio..."}}]}"#,
            )
            .create_async()
            .await;

        let result = openai_stt::transcribe_audio_chat(vec![0x00], "key", "model", &mock_url).await;

        assert_eq!(
            result.unwrap(),
            "the actual transcript",
            "content must win whenever it is non-empty"
        );
    }

    #[tokio::test]
    async fn chat_route_errors_when_both_text_fields_are_empty() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"choices":[{"message":{"content":"","reasoning_content":""}}]}"#)
            .create_async()
            .await;

        let result = openai_stt::transcribe_audio_chat(vec![0x00], "key", "model", &mock_url).await;

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no text"),
            "empty transcript must be an error, got: {err}"
        );
    }

    #[tokio::test]
    async fn chat_route_propagates_http_error() {
        let mut server = mockito::Server::new_async().await;
        let mock_url = server.url();

        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body(r#"{"error": "upstream exploded"}"#)
            .create_async()
            .await;

        let result = openai_stt::transcribe_audio_chat(vec![0x00], "key", "model", &mock_url).await;

        let err = result.unwrap_err().to_string();
        assert!(err.contains("500"), "got: {err}");
        assert!(err.contains("upstream exploded"), "got: {err}");
    }
}
