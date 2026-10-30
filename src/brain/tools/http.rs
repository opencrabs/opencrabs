//! HTTP Client Tool
//!
//! Make HTTP requests to external APIs (REST endpoints, webhooks, etc.)

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use reqwest::{Client, Method, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration as StdDuration;

/// HTTP client tool for external API integration
pub struct HttpClientTool;

#[derive(Debug, Deserialize, Serialize)]
struct HttpInput {
    /// HTTP method
    method: String,

    /// URL to request
    url: String,

    /// Optional: Request headers
    #[serde(default)]
    headers: HashMap<String, String>,

    /// Optional: Request body (for POST, PUT, PATCH)
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<Value>,

    /// Optional: Query parameters
    #[serde(default)]
    query: HashMap<String, String>,

    /// Optional: Timeout in seconds (default: 30, max: 120)
    #[serde(default = "default_timeout")]
    timeout_secs: u64,

    /// Optional: Follow redirects (default: true)
    #[serde(default = "default_true")]
    follow_redirects: bool,
}

fn default_timeout() -> u64 {
    30
}

fn default_true() -> bool {
    true
}

fn parse_method(method_str: &str) -> Result<Method> {
    match method_str.to_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        "HEAD" => Ok(Method::HEAD),
        "OPTIONS" => Ok(Method::OPTIONS),
        _ => Err(ToolError::InvalidInput(format!(
            "Unsupported HTTP method: {}. Supported: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
            method_str
        ))),
    }
}

/// Hard cap on a response body, applied to the read AND to the rendered
/// output, for every content type (#2104). The tool documents one number, so
/// plain text and JSON both obey it, and a huge or hostile response is never
/// fully buffered just to be cut afterwards.
const MAX_BODY_BYTES: usize = 10_000;

/// Bound on the READ itself, one byte above the body cap.
///
/// The extra byte is how the read learns the stream had more to give without
/// trusting `Content-Length`: a body that fills it is longer than the cap, so
/// it is reported cut. Kept as its own constant because the two bounds answer
/// different questions. `MAX_BODY_BYTES` is what the model sees; this one is
/// what the process is ever willing to hold, so a hostile multi-gigabyte body
/// costs the same memory as a small one.
const MAX_READ_BYTES: usize = MAX_BODY_BYTES + 1;

/// Read at most [`MAX_READ_BYTES`]. A body longer than the cap is cut here,
/// before it is buffered, so the bound holds whatever the server declares.
async fn read_body_capped(response: &mut reqwest::Response) -> Result<(String, bool)> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| ToolError::Execution(format!("Failed to read response body: {}", e)))?;
        let Some(chunk) = chunk else {
            break;
        };
        let room = MAX_READ_BYTES - buf.len();
        if chunk.len() >= room {
            buf.extend_from_slice(&chunk[..room]);
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    let truncated = buf.len() > MAX_BODY_BYTES;
    buf.truncate(MAX_BODY_BYTES);
    Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
}

/// Append a body under [`MAX_BODY_BYTES`], with an honest note when anything
/// was cut. `declared_len` is the server's `Content-Length` when it sent one,
/// which is the only way to name a total for a body we stopped reading.
fn push_body_capped(out: &mut String, body: &str, read_truncated: bool, declared_len: Option<u64>) {
    if body.len() <= MAX_BODY_BYTES {
        out.push_str(body);
        if read_truncated {
            out.push_str(&format!(
                "\n... (body cut at {MAX_BODY_BYTES} bytes while reading; the response is longer)"
            ));
        }
        return;
    }
    let total = declared_len
        .map(|n| format!("{n} bytes total"))
        .unwrap_or_else(|| format!("more than {MAX_BODY_BYTES} bytes"));
    out.push_str(&format!(
        "{}... (truncated, {})",
        crate::utils::truncate_str(body, MAX_BODY_BYTES),
        total
    ));
}

#[async_trait]
impl Tool for HttpClientTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "Make HTTP requests to any HTTP/HTTPS URL — plain web pages and REST APIs alike. Supports GET, POST, PUT, PATCH, DELETE with headers, query parameters, and JSON bodies. \
         Use a GET to FETCH AND READ THE CONTENT of a specific web page or URL: the response body is returned as text/HTML, so this is the right tool when the user gives you a link and asks to check / read / summarize what's on it — far cheaper than browser_navigate. \
         (If a GET returns a JS-only shell with no real content, or the page needs login/clicking/JS-rendered data, THEN fall back to browser_navigate.) \
         Also useful for integrating with GitHub, Slack, Jira, databases, and other web services."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "method": {
                    "type": "string",
                    "description": "HTTP method",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
                },
                "url": {
                    "type": "string",
                    "description": "URL to request (must be valid HTTP/HTTPS URL)"
                },
                "headers": {
                    "type": "object",
                    "description": "Request headers as key-value pairs",
                    "additionalProperties": {
                        "type": "string"
                    },
                    "default": {}
                },
                "body": {
                    "description": "Request body (JSON, for POST/PUT/PATCH)"
                },
                "query": {
                    "type": "object",
                    "description": "Query parameters as key-value pairs",
                    "additionalProperties": {
                        "type": "string"
                    },
                    "default": {}
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Request timeout in seconds (default: 30, max: 120)",
                    "default": 30,
                    "minimum": 1,
                    "maximum": 120
                },
                "follow_redirects": {
                    "type": "boolean",
                    "description": "Follow HTTP redirects (default: true)",
                    "default": true
                }
            },
            "required": ["method", "url"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn requires_approval(&self) -> bool {
        true // External HTTP requests require approval
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: HttpInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;

        // Validate URL
        if !input.url.starts_with("http://") && !input.url.starts_with("https://") {
            return Err(ToolError::InvalidInput(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        // Validate timeout
        if input.timeout_secs == 0 || input.timeout_secs > 120 {
            return Err(ToolError::InvalidInput(
                "Timeout must be between 1 and 120 seconds".to_string(),
            ));
        }

        // Validate method
        parse_method(&input.method)?;

        // Validate body is only used with appropriate methods
        if input.body.is_some() {
            let method = parse_method(&input.method)?;
            if !matches!(method, Method::POST | Method::PUT | Method::PATCH) {
                return Err(ToolError::InvalidInput(
                    "Body can only be used with POST, PUT, or PATCH methods".to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: HttpInput = serde_json::from_value(input)?;

        // SSRF guard (OC-04): http_request had none. Resolve the host and refuse
        // any URL that is, or resolves to, a loopback / private / link-local /
        // metadata / CGNAT address before a single byte goes out. Redirect hops
        // are re-validated by the shared policy below.
        if let Err(reason) = super::ssrf::validate_url_resolved(&input.url).await {
            return Ok(ToolResult::error(format!("http_request refused: {reason}")));
        }

        let method = parse_method(&input.method)?;

        // Build client with timeout. Always set a default User-Agent —
        // GitHub's API (and a long tail of other services that follow the
        // same convention) returns 403 Forbidden when the UA header is
        // missing, and reqwest ships with no UA by default. The error
        // body literally says "Request forbidden by administrative
        // rules. Please make sure your request has a User-Agent header."
        // The caller's `headers` map can still override this: reqwest's
        // per-request header setters win over client-level defaults, so
        // explicit `User-Agent` in the tool input replaces ours.
        let client = Client::builder()
            .timeout(StdDuration::from_secs(input.timeout_secs))
            .user_agent(concat!("opencrabs/", env!("CARGO_PKG_VERSION")))
            .redirect(if input.follow_redirects {
                // Re-validate every hop (OC-04): a public URL that 302s to an
                // internal address is stopped, not followed.
                super::ssrf::redirect_policy(10)
            } else {
                reqwest::redirect::Policy::none()
            })
            .build()
            .map_err(|e| ToolError::Execution(format!("Failed to build HTTP client: {}", e)))?;

        // Build request
        let mut request = client.request(method.clone(), &input.url);

        // Add headers
        if !input.headers.is_empty() {
            let mut header_map = HeaderMap::new();
            for (key, value) in &input.headers {
                let header_name: reqwest::header::HeaderName = key.parse().map_err(|e| {
                    ToolError::InvalidInput(format!("Invalid header name '{}': {}", key, e))
                })?;
                let header_value: reqwest::header::HeaderValue = value.parse().map_err(|e| {
                    ToolError::InvalidInput(format!("Invalid header value for '{}': {}", key, e))
                })?;
                header_map.insert(header_name, header_value);
            }
            request = request.headers(header_map);
        }

        // Add query parameters
        if !input.query.is_empty() {
            request = request.query(&input.query);
        }

        // Add body if present
        if let Some(body) = input.body {
            request = request.json(&body);
        }

        // Execute request
        let mut response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                ToolError::Timeout(input.timeout_secs)
            } else if e.is_connect() {
                ToolError::Execution(format!("Connection failed: {}", e))
            } else {
                ToolError::Execution(format!("Request failed: {}", e))
            }
        })?;

        // Extract response details
        let status = response.status();
        let status_code = status.as_u16();
        let is_success = status.is_success();

        // Extract response headers
        let mut response_headers = HashMap::new();
        for (key, value) in response.headers() {
            if let Ok(value_str) = value.to_str() {
                response_headers.insert(key.to_string(), value_str.to_string());
            }
        }

        // Get response body. Bounded at the read (#2104): a huge or hostile
        // response must never be fully buffered just to be cut afterwards.
        let declared_len = response.content_length();
        let (body_text, read_truncated) = read_body_capped(&mut response).await?;

        // Try to parse as JSON, fallback to text. A truncated body no longer
        // parses, so it takes the text arm and is rendered cut, which is the
        // honest result.
        let body_json: Option<Value> = serde_json::from_str(&body_text).ok();

        // Build output
        let mut output = format!(
            "HTTP {} {}\nStatus: {} {}\n\n",
            input.method.to_uppercase(),
            input.url,
            status_code,
            status.canonical_reason().unwrap_or("Unknown")
        );

        // Add response headers (limit to important ones)
        let important_headers = [
            "content-type",
            "content-length",
            "server",
            "date",
            "location",
            "cache-control",
        ];
        let mut has_headers = false;
        for header in important_headers {
            if let Some(value) = response_headers.get(header) {
                if !has_headers {
                    output.push_str("Headers:\n");
                    has_headers = true;
                }
                output.push_str(&format!("  {}: {}\n", header, value));
            }
        }
        if has_headers {
            output.push('\n');
        }

        // Add response body. One documented cap for every content type: JSON
        // used to skip it entirely (#2104).
        output.push_str("Response Body:\n");
        if let Some(json) = body_json {
            let pretty = serde_json::to_string_pretty(&json).unwrap_or(body_text.clone());
            push_body_capped(&mut output, &pretty, read_truncated, declared_len);
        } else if body_text.is_empty() {
            output.push_str("(empty)");
        } else {
            push_body_capped(&mut output, &body_text, read_truncated, declared_len);
        }

        // Tool success reflects whether the REQUEST completed, not the HTTP
        // status. A 3xx/4xx/5xx is a valid, informative response: the server
        // answered, and the model reads the status + body to decide what to do
        // next. Marking those as tool errors misreported reliability (a 404
        // existence probe, a 401/403 auth check, a 429 rate limit are expected,
        // not tool malfunctions) and could trip the agent's error-recovery path.
        // Only genuine transport failures (timeout, connect, DNS, body-read)
        // return Err above and count as failures (#574). Prepend a clear note on
        // non-2xx so the model still notices the status.
        if !is_success {
            output = format!(
                "⚠️ The server returned a non-2xx status ({status_code}). The request \
                 itself completed; treat the status and body below as the result, \
                 not as a tool failure.\n\n{output}"
            );
        }
        let mut tool_result = ToolResult::success(output);

        tool_result
            .metadata
            .insert("status_code".to_string(), status_code.to_string());
        tool_result
            .metadata
            .insert("method".to_string(), input.method.to_uppercase());
        tool_result.metadata.insert("url".to_string(), input.url);

        Ok(tool_result)
    }
}
