//! Bounded, single-request Responses observations for explicitly initiated
//! quality checks. This module never dispatches tools or resumes an agent task.
use crate::network::{Assembly, Snapshot, numeric_usage, strict_json};
use futures_util::StreamExt;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{sync::OnceLock, time::Instant};
use tokio_util::sync::CancellationToken;

const BODY_LIMIT: usize = 2 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 32 * 1024;
const SAMPLE_LIMIT: usize = 512;

type Failure = (&'static str, String);
fn failure(kind: &'static str, message: &str) -> Failure {
    (kind, message.to_owned())
}
fn milliseconds(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
fn bounded(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
fn clean_text(snapshot: &Snapshot, text: &str, limit: usize) -> String {
    static BEARER: OnceLock<Regex> = OnceLock::new();
    let redacted = snapshot.diagnostic_redact(text);
    let regex =
        BEARER.get_or_init(|| Regex::new(r"(?i)\bbearer[\t ]+[A-Za-z0-9._~+/=-]+").unwrap());
    bounded(&regex.replace_all(&redacted, "Bearer [redacted]"), limit)
}
fn clean_value(snapshot: &Snapshot, value: &Value, depth: usize) -> Value {
    if depth > 32 {
        return Value::Null;
    }
    match value {
        Value::String(text) => json!(clean_text(snapshot, text, OUTPUT_LIMIT)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(1024)
                .map(|item| clean_value(snapshot, item, depth + 1))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .take(1024)
                .filter(|(key, _)| {
                    !matches!(
                        key.to_ascii_lowercase().replace(['-', '_'], "").as_str(),
                        "encryptedcontent"
                            | "encryptedstate"
                            | "ciphertext"
                            | "authorization"
                            | "proxyauthorization"
                            | "setcookie"
                            | "cookie"
                            | "secret"
                            | "apikey"
                            | "accesstoken"
                            | "refreshtoken"
                            | "password"
                    )
                })
                .map(|(key, value)| {
                    (
                        clean_text(snapshot, key, 128),
                        clean_value(snapshot, value, depth + 1),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn sanitized_headers(snapshot: &Snapshot, headers: &reqwest::header::HeaderMap) -> Value {
    let names = [
        "date",
        "server",
        "content-type",
        "via",
        "x-cache",
        "x-served-by",
        "x-request-id",
        "request-id",
        "openai-request-id",
        "openai-model",
        "openai-version",
        "openai-processing-ms",
        "x-upstream-model",
        "x-model",
        "x-provider",
        "x-reported-model",
        "x-backend-model",
        "x-oneapi-request-id",
        "x-newapi-request-id",
        "x-rixapi-request-id",
        "x-veloera-request-id",
        "x-litellm-call-id",
        "x-litellm-response-duration-ms",
        "x-envoy-upstream-service-time",
        "x-kong-upstream-latency",
        "x-kong-proxy-latency",
        "cf-aig-cache-status",
        "server-timing",
        "x-powered-by",
        "x-upstream-latency",
        "x-response-time",
        "x-ratelimit-limit-requests",
        "x-ratelimit-remaining-requests",
        "x-ratelimit-limit-tokens",
        "x-ratelimit-remaining-tokens",
        "cf-ray",
        "x-amzn-requestid",
        "x-amzn-trace-id",
    ];
    Value::Object(
        names
            .into_iter()
            .filter_map(|name| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| (name.into(), json!(clean_text(snapshot, value, 256))))
            })
            .collect(),
    )
}
fn append_sample(record: &mut Value, field: &str, text: &str, snapshot: &Snapshot) {
    let previous = record[field].as_str().unwrap_or("");
    if previous.chars().count() >= SAMPLE_LIMIT {
        return;
    }
    record[field] = json!(clean_text(
        snapshot,
        &format!("{previous}{text}"),
        SAMPLE_LIMIT
    ));
}
fn safe_scalar(snapshot: &Snapshot, value: &Value) -> Value {
    value
        .as_str()
        .map(|text| json!(clean_text(snapshot, text, 256)))
        .unwrap_or(Value::Null)
}
fn capture_metadata(record: &mut Value, response: &Value, snapshot: &Snapshot) {
    for (field, key) in [
        ("response_model", "model"),
        ("reported_api", "api"),
        ("reported_provider", "provider"),
        ("system_fingerprint", "system_fingerprint"),
        ("response_id", "id"),
        ("service_tier", "service_tier"),
    ] {
        let val = safe_scalar(snapshot, &response[key]);
        if !val.is_null() {
            record[field] = val;
        }
    }
    let rc = safe_scalar(snapshot, &response["reasoning"]["context"]);
    if !rc.is_null() {
        record["reasoning_context"] = rc;
    }
}
fn capture_terminal(record: &mut Value, terminal: &Value, snapshot: &Snapshot) {
    record["terminal_status"] = safe_scalar(snapshot, &terminal["status"]);
    capture_metadata(record, terminal, snapshot);
    record["usage"] = if terminal["usage"].is_object() {
        clean_value(snapshot, &numeric_usage(&terminal["usage"]), 0)
    } else {
        Value::Null
    };
    if let Some(items) = terminal["output"].as_array() {
        let mut text = String::new();
        let mut refusal = String::new();
        for item in items {
            if item["type"] != "message" {
                continue;
            }
            if let Some(parts) = item["content"].as_array() {
                for part in parts {
                    if part["type"] == "output_text" && text.chars().count() < SAMPLE_LIMIT {
                        text.extend(
                            part["text"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(SAMPLE_LIMIT),
                        );
                    }
                    if part["type"] == "refusal" && refusal.chars().count() < SAMPLE_LIMIT {
                        refusal.extend(
                            part["refusal"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(SAMPLE_LIMIT),
                        );
                    }
                }
            }
        }
        if !text.is_empty() {
            record["text_sample"] = json!(clean_text(snapshot, &text, SAMPLE_LIMIT));
        }
        if !refusal.is_empty() {
            record["refusal_sample"] = json!(clean_text(snapshot, &refusal, SAMPLE_LIMIT));
        }
        let safe = clean_value(snapshot, &terminal["output"], 0);
        if serde_json::to_vec(&safe).is_ok_and(|bytes| bytes.len() <= OUTPUT_LIMIT) {
            record["output"] = safe;
        }
    }
}
fn error_code(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        !text.is_empty()
            && text.len() <= 80
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    })
}
fn http_error(status: u16, bytes: &[u8], snapshot: &Snapshot) -> Failure {
    let mut message = format!("Provider returned HTTP {status}");
    if let Ok(text) = std::str::from_utf8(bytes)
        && let Ok(payload) = strict_json(text)
    {
        let error = &payload["error"];
        let mut codes = Vec::new();
        for name in ["code", "type"] {
            if let Some(code) = error_code(&error[name]) {
                codes.push(format!("{name}={}", clean_text(snapshot, code, 80)));
            }
        }
        if !codes.is_empty() {
            message.push_str(&format!(" ({})", codes.join(", ")));
        }
    }
    ("http_error", message)
}
fn process_event(
    data: &str,
    assembly: &mut Assembly,
    record: &mut Value,
    snapshot: &Snapshot,
    start: Instant,
) -> Result<bool, Failure> {
    if data == "[DONE]" {
        return Err(failure(
            "protocol_error",
            "Stream ended with [DONE] before a terminal Responses event",
        ));
    }
    let event = strict_json(data)
        .map_err(|_| failure("protocol_error", "Invalid or duplicate-key SSE event JSON"))?;
    if record["first_event_ms"].is_null() {
        record["first_event_ms"] = json!(milliseconds(start));
    }
    let kind = event["type"]
        .as_str()
        .ok_or_else(|| failure("protocol_error", "SSE event is missing its type"))?;
    if matches!(
        kind,
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta"
    ) && event["delta"].as_str().is_some_and(|s| !s.is_empty())
    {
        if record["first_reasoning_ms"].is_null() {
            record["first_reasoning_ms"] = json!(milliseconds(start));
        }
        append_sample(
            record,
            "reasoning_sample",
            event["delta"].as_str().unwrap_or(""),
            snapshot,
        );
    }
    if matches!(
        kind,
        "response.output_text.delta" | "response.function_call_arguments.delta"
    ) && event["delta"].as_str().is_some_and(|s| !s.is_empty())
        && record["first_token_ms"].is_null()
    {
        record["first_token_ms"] = json!(milliseconds(start));
    }
    if kind == "response.output_text.delta" {
        append_sample(
            record,
            "text_sample",
            event["delta"].as_str().unwrap_or(""),
            snapshot,
        );
    }
    if matches!(kind, "response.refusal.delta" | "response.refusal.done") {
        let value = if kind.ends_with(".done") {
            &event["refusal"]
        } else {
            &event["delta"]
        };
        append_sample(
            record,
            "refusal_sample",
            value.as_str().unwrap_or(""),
            snapshot,
        );
    }
    if matches!(kind, "response.created" | "response.in_progress") && event["response"].is_object() {
        capture_metadata(record, &event["response"], snapshot);
    }
    if record["response_model"].is_null() && event["model"].is_string() {
        record["response_model"] = safe_scalar(snapshot, &event["model"]);
    }
    if matches!(
        kind,
        "response.completed" | "response.incomplete" | "response.failed"
    ) {
        capture_terminal(record, &event["response"], snapshot);
    }
    let completed = kind == "response.completed";
    assembly.event(event).map_err(|_| {
        failure(
            "protocol_error",
            "Responses event assembly rejected an inconsistent, failed or incomplete response",
        )
    })?;
    Ok(completed)
}

async fn collect(
    snapshot: &Snapshot,
    body: Value,
    record: &mut Value,
    hash: &mut Sha256,
    received: &mut bool,
    start: Instant,
) -> Result<(), Failure> {
    let builder = snapshot.diagnostic_request(&body).map_err(|_| {
        failure(
            "configuration_error",
            "Unable to construct configured diagnostic transport",
        )
    })?;
    let response=tokio::time::timeout(snapshot.diagnostic_timeout("headers_timeout_secs",15),builder.send()).await
        .map_err(|_|failure("header_timeout","Diagnostic response-header deadline exceeded"))?
        .map_err(|_|failure("transport_error","Connection failed under the configured proxy/TLS policy; no fallback or retry was attempted"))?;
    record["header_latency_ms"] = json!(milliseconds(start));
    record["http_status"] = json!(response.status().as_u16());
    record["headers"] = sanitized_headers(snapshot, response.headers());
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|s| s.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let sse = content_type == "text/event-stream";
    let json_response = content_type == "application/json" || content_type.ends_with("+json");
    record["protocol"] = json!(if sse {
        "responses_sse"
    } else if json_response {
        "responses_json"
    } else {
        "unknown"
    });
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut size = 0usize;
    let mut data_lines = Vec::<String>::new();
    let mut assembly = Assembly::default();
    let mut completed = false;
    let mut observation_error = None;
    loop {
        let next = tokio::time::timeout(
            snapshot.diagnostic_timeout("idle_timeout_secs", 20),
            stream.next(),
        )
        .await
        .map_err(|_| failure("idle_timeout", "Diagnostic stream-idle deadline exceeded"))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|_| {
            failure(
                "transport_error",
                "Diagnostic response body transport was interrupted",
            )
        })?;
        if !chunk.is_empty() {
            *received = true;
            hash.update(&chunk);
        }
        size = size.saturating_add(chunk.len());
        if size > BODY_LIMIT {
            return Err(failure(
                "response_limit",
                "Diagnostic response exceeded the 2 MiB body limit",
            ));
        }
        buffer.extend_from_slice(&chunk);
        if sse && status == 200 {
            let mut consumed = 0;
            for (position, byte) in buffer.iter().enumerate() {
                if *byte != b'\n' {
                    continue;
                }
                let line = std::str::from_utf8(&buffer[consumed..position])
                    .map_err(|_| failure("protocol_error", "SSE response contained invalid UTF-8"))?
                    .trim_end_matches('\r');
                consumed = position + 1;
                if line.is_empty() {
                    if !data_lines.is_empty() {
                        let data = data_lines.join("\n");
                        data_lines.clear();
                        match process_event(&data, &mut assembly, record, snapshot, start) {
                            Ok(true) => {
                                completed = true;
                                break;
                            }
                            Ok(false) => {}
                            Err(error) => {
                                observation_error.get_or_insert(error);
                                // Diagnostic-only: keep reading a bounded response
                                // after a semantic error to retain final usage and
                                // identity. The error never becomes success, and
                                // normal generation is entirely unaffected.
                                if data == "[DONE]" || !record["terminal_status"].is_null() {
                                    completed = true;
                                    break;
                                }
                            }
                        }
                    }
                } else if let Some(data) = line.strip_prefix("data:") {
                    data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_owned());
                }
            }
            buffer.drain(..consumed);
            if completed {
                break;
            }
        }
    }
    if status != 200 {
        // Some providers attach model/usage even to an HTTP error envelope.
        // Preserve those numeric/identity fields, never the original error message.
        if let Ok(text) = std::str::from_utf8(&buffer)
            && let Ok(value) = strict_json(text)
        {
            let terminal = if value["response"].is_object() {
                &value["response"]
            } else {
                &value
            };
            record["terminal_status"] = safe_scalar(snapshot, &terminal["status"]);
            record["response_model"] = safe_scalar(snapshot, &terminal["model"]);
            record["reported_api"] = safe_scalar(snapshot, &terminal["api"]);
            record["reported_provider"] = safe_scalar(snapshot, &terminal["provider"]);
            record["system_fingerprint"] = safe_scalar(snapshot, &terminal["system_fingerprint"]);
            record["response_id"] = safe_scalar(snapshot, &terminal["id"]);
            record["service_tier"] = safe_scalar(snapshot, &terminal["service_tier"]);
            record["reasoning_context"] = safe_scalar(snapshot, &terminal["reasoning"]["context"]);
            if terminal["usage"].is_object() {
                record["usage"] = clean_value(snapshot, &numeric_usage(&terminal["usage"]), 0);
            }
        }
        return Err(http_error(status, &buffer, snapshot));
    }
    if sse {
        if let Some(error) = observation_error {
            return Err(error);
        }
        if !completed {
            return Err(failure(
                "protocol_error",
                "SSE ended without a completed terminal Responses event",
            ));
        }
        assembly.finish().map_err(|_| {
            failure(
                "protocol_error",
                "Completed Responses output failed consistency or argument validation",
            )
        })?;
    } else if json_response {
        let text = std::str::from_utf8(&buffer)
            .map_err(|_| failure("protocol_error", "JSON response contained invalid UTF-8"))?;
        let terminal = strict_json(text)
            .map_err(|_| failure("protocol_error", "Invalid or duplicate-key JSON response"))?;
        capture_terminal(record, &terminal, snapshot);
        if terminal["status"] != "completed" {
            return Err(failure(
                "protocol_error",
                "JSON response was failed, incomplete or missing a completed status",
            ));
        }
        assembly
            .event(json!({"type":"response.completed","response":terminal}))
            .map_err(|_| failure("protocol_error", "Invalid JSON terminal Responses event"))?;
        assembly.finish().map_err(|_| {
            failure(
                "protocol_error",
                "Completed JSON Responses output failed consistency or argument validation",
            )
        })?;
    } else {
        return Err(failure(
            "protocol_error",
            "Endpoint did not return Responses SSE or JSON content",
        ));
    }
    Ok(())
}

/// Observe exactly one configured model HTTP request. The manager owns planning,
/// source metadata, pricing and analysis. This function never retries or executes.
pub async fn observe(snapshot: &Snapshot, body: Value, cancel: CancellationToken) -> Value {
    let start = Instant::now();
    let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
    let mut record = json!({"http_status":null,"header_latency_ms":null,"first_event_ms":null,"first_reasoning_ms":null,"first_token_ms":null,"total_ms":0,"protocol":"unknown","terminal_status":null,"response_model":null,"reported_api":null,"reported_provider":null,"system_fingerprint":null,"response_id":null,"service_tier":null,"reasoning_context":null,"usage":null,"text_sample":"","reasoning_sample":"","refusal_sample":null,"output":null,"headers":{},"error":null,"body_sha256":format!("{:x}",Sha256::digest(&body_bytes)),"response_sha256":null,"request_started_at":crate::models::now(),"request_finished_at":null});
    let mut hash = Sha256::new();
    let mut received = false;
    let result = if cancel.is_cancelled() {
        Err(failure(
            "cancelled",
            "Quality check was cancelled; partial usage may be unavailable",
        ))
    } else {
        let operation = collect(snapshot, body, &mut record, &mut hash, &mut received, start);
        tokio::pin!(operation);
        tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(failure("cancelled","Quality check was cancelled; partial usage may be unavailable")),
            result=tokio::time::timeout(snapshot.diagnostic_timeout("request_timeout_secs",90),&mut operation)=>result.unwrap_or_else(|_|Err(failure("overall_timeout","Diagnostic overall deadline exceeded; partial usage may be unavailable"))),
        }
    };
    if let Err((kind, message)) = result {
        record["error"] = json!({"kind":kind,"message":clean_text(snapshot,&message,512)});
    }
    record["total_ms"] = json!(milliseconds(start));
    record["request_finished_at"] = json!(crate::models::now());
    if received {
        record["response_sha256"] = json!(format!("{:x}", hash.finalize()));
    }
    // A second pass catches credentials split between streamed text fragments.
    let safe = clean_value(snapshot, &record, 0);
    // Preserve exact stable top-level fields; sanitization only touches values.
    Value::Object(safe.as_object().cloned().unwrap_or_else(Map::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{Request, Response, StatusCode},
        routing::post,
    };
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    const SECRET: &str = "private-fixture-key-not-runtime";
    struct Server {
        origin: String,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn server(router: Router) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server { origin, task }
    }
    fn snapshot(origin: &str, network: Value) -> Snapshot {
        let config:crate::models::AppConfig=serde_json::from_value(json!({"sites":[{"id":"s","name":"s","base_url":format!("{origin}/v1"),"keys":[{"id":"k","name":"k","secret":SECRET,"models":[{"id":"m","name":"Fixture model","model":"requested-model","max_output_tokens":256,"context_window":128000}]}]}],"network":network})).unwrap();
        Snapshot::resolve(
            &config,
            &crate::models::Selection {
                site_id: "s".into(),
                key_id: "k".into(),
                model_id: "m".into(),
            },
        )
        .unwrap()
    }
    fn terminal() -> Value {
        json!({"id":"response_fixture","status":"completed","model":"requested-model","service_tier":"default","reasoning":{"context":"all_turns"},"usage":{"input_tokens":100,"output_tokens":3,"input_tokens_details":{"cached_tokens":80}},"output":[{"type":"message","id":"msg","status":"completed","role":"assistant","content":[{"type":"output_text","text":"323","annotations":[]}]}]})
    }
    fn sse(event: Value) -> String {
        format!("data: {}\n\n", serde_json::to_string(&event).unwrap())
    }
    fn sse_response(text: String) -> Response<Body> {
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(text))
            .unwrap()
    }
    async fn observe_at(router: Router, body: Value) -> Value {
        let local = server(router).await;
        observe(
            &snapshot(&local.origin, json!({"proxy_mode":"direct"})),
            body,
            CancellationToken::new(),
        )
        .await
    }

    #[tokio::test]
    async fn sse_records_real_event_token_latencies_model_mismatch_and_hashes() {
        let mut response = terminal();
        response["model"] = json!("reported-other-model");
        response["api"] = json!("upstream-self-report");
        response["provider"] = json!("synthetic-provider");
        response["system_fingerprint"] = json!("fixture-fingerprint");
        let pieces = vec![
            sse(json!({"type":"response.created","response":{"status":"in_progress"}})),
            sse(json!({"type":"response.output_text.delta","output_index":0,"delta":"323"})),
            sse(json!({"type":"response.completed","response":response})),
        ];
        let expected_hash = format!("{:x}", Sha256::digest(pieces.join("").as_bytes()));
        let router = Router::new().route(
            "/v1/responses",
            post(move |req: Request<Body>| {
                let pieces = pieces.clone();
                async move {
                    assert_eq!(req.headers()["authorization"], format!("Bearer {SECRET}"));
                    assert!(req.headers().get("cookie").is_none());
                    let chunks = futures_util::stream::iter(pieces).then(|piece| async move {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        Ok::<_, Infallible>(Bytes::from(piece))
                    });
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("x-request-id", "fixture-request")
                        .header("set-cookie", "must-not-store=private")
                        .body(Body::from_stream(chunks))
                        .unwrap()
                }
            }),
        );
        let body = json!({"model":"requested-model","input":[],"stream":true});
        let expected_body = format!("{:x}", Sha256::digest(serde_json::to_vec(&body).unwrap()));
        let observed = observe_at(router, body).await;
        assert_eq!(observed["http_status"], 200);
        assert_eq!(observed["protocol"], "responses_sse");
        assert_eq!(observed["response_model"], "reported-other-model");
        assert_eq!(observed["reported_api"], "upstream-self-report");
        assert_eq!(observed["reported_provider"], "synthetic-provider");
        assert_eq!(observed["system_fingerprint"], "fixture-fingerprint");
        assert_eq!(observed["terminal_status"], "completed");
        assert_eq!(observed["text_sample"], "323");
        assert!(observed["error"].is_null());
        let header = observed["header_latency_ms"].as_u64().unwrap();
        let event = observed["first_event_ms"].as_u64().unwrap();
        let token = observed["first_token_ms"].as_u64().unwrap();
        assert!(event >= header + 10);
        assert!(token >= event + 10);
        assert!(observed["total_ms"].as_u64().unwrap() >= token);
        assert_eq!(observed["body_sha256"], expected_body);
        assert_eq!(observed["response_sha256"], expected_hash);
        assert_eq!(observed["headers"]["x-request-id"], "fixture-request");
        assert!(observed["headers"].get("set-cookie").is_none());
        assert!(observed["request_finished_at"].as_str().is_some());
    }
    #[tokio::test]
    async fn json_fallback_keeps_missing_cached_usage_unknown_and_strips_ciphertext() {
        let mut value = terminal();
        value["usage"] = json!({"input_tokens":100,"output_tokens":3,"details":{"cached_tokens":"private","numeric":2},"ignored":"secret text"});
        value["output"].as_array_mut().unwrap().insert(0,json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":"OPAQUE_MUST_NOT_STORE"}));
        let observed = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let value = value.clone();
                    async move { axum::Json(value) }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(observed["protocol"], "responses_json");
        assert!(observed["first_event_ms"].is_null() && observed["first_token_ms"].is_null());
        assert!(observed["usage"]["input_tokens_details"]["cached_tokens"].is_null());
        assert_eq!(observed["usage"]["details"]["numeric"], 2);
        assert!(observed["usage"]["details"].get("cached_tokens").is_none());
        assert!(!observed.to_string().contains("OPAQUE_MUST_NOT_STORE"));
        assert!(!observed.to_string().contains("secret text"));
        assert!(observed["error"].is_null());
    }
    #[tokio::test]
    async fn incomplete_and_failed_keep_terminal_model_status_and_numeric_usage() {
        for status in ["incomplete", "failed"] {
            let mut value = terminal();
            value["status"] = json!(status);
            value["model"] = json!("actual-model");
            value["incomplete_details"] = json!({"reason":"max_output_tokens"});
            let event = sse(json!({"type":format!("response.{status}"),"response":value}));
            let observed = observe_at(
                Router::new().route(
                    "/v1/responses",
                    post(move || {
                        let event = event.clone();
                        async move { sse_response(event) }
                    }),
                ),
                json!({}),
            )
            .await;
            assert_eq!(observed["terminal_status"], status);
            assert_eq!(observed["response_model"], "actual-model");
            assert_eq!(observed["usage"]["input_tokens"], 100);
            assert_eq!(
                observed["usage"]["input_tokens_details"]["cached_tokens"],
                80
            );
            assert_eq!(observed["error"]["kind"], "protocol_error");
        }
    }
    #[tokio::test]
    async fn http_errors_store_safe_codes_not_messages_keys_cookies_or_bearers() {
        let error=json!({"error":{"type":"authentication_error","code":"invalid_api_key","message":format!("Echo {SECRET} Bearer another-secret credential")}}).to_string();
        let observed = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let error = error.clone();
                    async move {
                        Response::builder()
                            .status(401)
                            .header("content-type", "application/json")
                            .header("server", format!("{SECRET} Bearer another-secret"))
                            .header("x-request-id", SECRET)
                            .header("set-cookie", "private-cookie")
                            .body(Body::from(error))
                            .unwrap()
                    }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(observed["http_status"], 401);
        assert_eq!(observed["error"]["kind"], "http_error");
        assert!(
            observed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("invalid_api_key")
        );
        let encoded = observed.to_string();
        for forbidden in [SECRET, "another-secret", "private-cookie", "Echo"] {
            assert!(!encoded.contains(forbidden), "leaked {forbidden}");
        }
        assert!(observed["output"].is_null());
        assert!(observed["usage"].is_null());
    }
    #[tokio::test]
    async fn stream_eof_preserves_partial_text_but_is_not_success() {
        let event =
            sse(json!({"type":"response.output_text.delta","output_index":0,"delta":"unfinished"}));
        let observed = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let event = event.clone();
                    async move { sse_response(event) }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(observed["error"]["kind"], "protocol_error");
        assert_eq!(observed["text_sample"], "unfinished");
        assert!(observed["terminal_status"].is_null());
        assert!(observed["first_token_ms"].is_number());
        assert!(observed["response_sha256"].is_string());
    }
    #[tokio::test]
    async fn malformed_stream_semantics_use_the_same_assembly_guard_without_tool_dispatch() {
        let added = json!({"type":"function_call","id":"fc","call_id":"call","name":"quality_echo","arguments":"{}","status":"in_progress"});
        let mut done = added.clone();
        done["status"] = json!("completed");
        done["arguments"] = json!("{\"answer\":323}");
        let text = sse(json!({"type":"response.output_item.added","output_index":0,"item":added}))
            + &sse(
                json!({"type":"response.completed","response":{"status":"completed","model":"observed-model","usage":{"output_tokens":5},"output":[done]}}),
            );
        let observed = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let text = text.clone();
                    async move { sse_response(text) }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(observed["error"]["kind"], "protocol_error");
        assert_eq!(observed["response_model"], "observed-model");
        assert_eq!(observed["usage"]["output_tokens"], 5);
        assert_eq!(observed["output"][0]["name"], "quality_echo");
    }
    #[tokio::test]
    async fn early_argument_conflict_remains_error_but_later_terminal_usage_is_retained() {
        let mut final_response = terminal();
        final_response["output"] = json!([{"type":"function_call","call_id":"call","name":"quality_echo","arguments":"{\"answer\":323}","status":"completed"}]);
        let text = sse(
            json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"answer\":0}"}),
        ) + &sse(
            json!({"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"answer\":323}"}),
        ) + &sse(json!({"type":"response.completed","response":final_response}));
        let record = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let text = text.clone();
                    async move { sse_response(text) }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(record["error"]["kind"], "protocol_error");
        assert_eq!(record["terminal_status"], "completed");
        assert_eq!(record["response_model"], "requested-model");
        assert_eq!(record["usage"]["input_tokens"], 100);
        assert_eq!(record["output"][0]["name"], "quality_echo");
        assert!(record["first_token_ms"].is_number());
    }
    #[tokio::test]
    async fn cancellation_before_send_and_during_pending_headers_is_structured() {
        let count = Arc::new(AtomicUsize::new(0));
        let local = server(
            Router::new()
                .route(
                    "/v1/responses",
                    post(|State(count): State<Arc<AtomicUsize>>| async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        axum::Json(terminal())
                    }),
                )
                .with_state(count.clone()),
        )
        .await;
        let snapshot = snapshot(&local.origin, json!({"proxy_mode":"direct"}));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = observe(&snapshot, json!({}), cancel).await;
        assert_eq!(result["error"]["kind"], "cancelled");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            trigger.cancel();
        });
        let result = observe(&snapshot, json!({}), cancel).await;
        task.await.unwrap();
        assert_eq!(result["error"]["kind"], "cancelled");
        assert!(result["total_ms"].as_u64().unwrap() < 1000);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn configured_header_idle_and_overall_deadlines_are_bounded() {
        let header_server = server(Router::new().route(
            "/v1/responses",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                axum::Json(terminal())
            }),
        ))
        .await;
        let header = observe(
            &snapshot(
                &header_server.origin,
                json!({"proxy_mode":"direct","headers_timeout_secs":1}),
            ),
            json!({}),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(header["error"]["kind"], "header_timeout");
        assert!(header["total_ms"].as_u64().unwrap() < 2200);
        let idle_server = server(Router::new().route(
            "/v1/responses",
            post(|| async {
                let chunks = futures_util::stream::once(async {
                    Ok::<_, Infallible>(Bytes::from(": heartbeat\n\n"))
                })
                .chain(futures_util::stream::pending::<Result<Bytes, Infallible>>());
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .unwrap()
            }),
        ))
        .await;
        let idle = observe(
            &snapshot(
                &idle_server.origin,
                json!({"proxy_mode":"direct","idle_timeout_secs":1}),
            ),
            json!({}),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(idle["error"]["kind"], "idle_timeout");
        assert!(idle["header_latency_ms"].is_number());
        assert!(idle["first_token_ms"].is_null());
        let total = observe(
            &snapshot(
                &idle_server.origin,
                json!({"proxy_mode":"direct","request_timeout_secs":1}),
            ),
            json!({}),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(total["error"]["kind"], "overall_timeout");
        assert!(total["total_ms"].as_u64().unwrap() < 2200);
    }
    #[tokio::test]
    async fn proxy_failure_never_falls_back_to_target_and_credentials_are_redacted() {
        let reached = Arc::new(AtomicUsize::new(0));
        let local = server(
            Router::new()
                .route(
                    "/v1/responses",
                    post(|State(reached): State<Arc<AtomicUsize>>| async move {
                        reached.fetch_add(1, Ordering::SeqCst);
                        axum::Json(terminal())
                    }),
                )
                .with_state(reached.clone()),
        )
        .await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let snapshot = snapshot(
            &local.origin,
            json!({"proxy_mode":"manual","proxy_url":format!("http://proxy-user:proxy-password@127.0.0.1:{port}")}),
        );
        let observed = observe(&snapshot, json!({}), CancellationToken::new()).await;
        assert_eq!(observed["error"]["kind"], "transport_error");
        assert_eq!(reached.load(Ordering::SeqCst), 0);
        let encoded = observed.to_string();
        assert!(!encoded.contains("proxy-password") && !encoded.contains("proxy-user"));
        assert_eq!(
            clean_text(
                &snapshot,
                "proxy-user proxy-password Bearer unknown-token",
                256
            ),
            "[redacted-proxy] [redacted-proxy] Bearer [redacted]"
        );
    }
    #[tokio::test]
    async fn response_and_stored_output_are_bounded_and_html_is_not_responses() {
        let mut value = terminal();
        value["output"][0]["content"][0]["text"] = json!("x".repeat(OUTPUT_LIMIT + 1024));
        let observed = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let value = value.clone();
                    async move { axum::Json(value) }
                }),
            ),
            json!({}),
        )
        .await;
        assert!(observed["output"].is_null());
        assert_eq!(
            observed["text_sample"].as_str().unwrap().chars().count(),
            SAMPLE_LIMIT
        );
        assert!(observed["error"].is_null());
        let too_large = "x".repeat(BODY_LIMIT + 1);
        let limited = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let text = too_large.clone();
                    async move {
                        Response::builder()
                            .header("content-type", "application/json")
                            .body(Body::from(text))
                            .unwrap()
                    }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(limited["error"]["kind"], "response_limit");
        let html = observe_at(
            Router::new().route(
                "/v1/responses",
                post(|| async {
                    Response::builder()
                        .header("content-type", "text/html")
                        .body(Body::from(format!("<html>{SECRET}</html>")))
                        .unwrap()
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(html["protocol"], "unknown");
        assert_eq!(html["error"]["kind"], "protocol_error");
        assert!(!html.to_string().contains(SECRET));
    }
    #[test]
    fn forbidden_metadata_field_aliases_are_removed_before_storage() {
        let snapshot = snapshot("http://127.0.0.1:1", json!({"proxy_mode":"direct"}));
        let value = json!({"metadata":{"apiKey":"dummy","API_KEY":"dummy","Proxy_Authorization":"dummy","Cookie":"dummy","set-cookie":"dummy","accessToken":"dummy","refresh_token":"dummy","password":"dummy","secret":"dummy","encryptedContent":"opaque","valid":323}});
        assert_eq!(
            clean_value(&snapshot, &value, 0),
            json!({"metadata":{"valid":323}})
        );
    }
    #[tokio::test]
    async fn safe_output_refusals_and_arbitrary_nested_strings_never_echo_credentials() {
        let mut value = terminal();
        value["model"] = json!(format!("Bearer unrelated-secret {SECRET}"));
        value["output"][0]["content"] = json!([{"type":"output_text","text":format!("{SECRET} Bearer unconfigured-token {}","尾".repeat(600))},{"type":"refusal","refusal":format!("Cannot comply {SECRET} Bearer refusal-token")}]);
        value["output"][0]["metadata"] = json!({SECRET:SECRET,"nested":["Bearer nested-token"]});
        let observed = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let value = value.clone();
                    async move { axum::Json(value) }
                }),
            ),
            json!({}),
        )
        .await;
        let text = observed.to_string();
        for forbidden in [
            SECRET,
            "unrelated-secret",
            "unconfigured-token",
            "refusal-token",
            "nested-token",
        ] {
            assert!(!text.contains(forbidden), "leaked {forbidden}");
        }
        assert!(
            observed["refusal_sample"]
                .as_str()
                .unwrap()
                .contains("Cannot comply")
        );
        assert!(observed["text_sample"].as_str().unwrap().chars().count() <= 512);
    }
    #[tokio::test]
    async fn redirects_are_not_followed_and_tool_argument_delta_is_a_token() {
        let count = Arc::new(AtomicUsize::new(0));
        let destination = server(
            Router::new()
                .route(
                    "/v1/responses",
                    post(|State(count): State<Arc<AtomicUsize>>| async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        axum::Json(terminal())
                    }),
                )
                .with_state(count.clone()),
        )
        .await;
        let location = format!("{}/v1/responses", destination.origin);
        let redirect = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let location = location.clone();
                    async move {
                        Response::builder()
                            .status(StatusCode::TEMPORARY_REDIRECT)
                            .header("location", location)
                            .body(Body::empty())
                            .unwrap()
                    }
                }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(redirect["http_status"], 307);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let call = json!({"type":"function_call","status":"completed","id":"fc","call_id":"c","name":"quality_echo","arguments":"{\"answer\":323}"});
        let text = sse(
            json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"answer\":323}"}),
        ) + &sse(
            json!({"type":"response.completed","response":{"status":"completed","model":"requested-model","output":[call]}}),
        );
        let result = observe_at(
            Router::new().route(
                "/v1/responses",
                post(move || {
                    let text = text.clone();
                    async move { sse_response(text) }
                }),
            ),
            json!({}),
        )
        .await;
        assert!(result["first_token_ms"].is_number());
        assert!(result["error"].is_null());
        assert_eq!(result["output"][0]["arguments"], "{\"answer\":323}");
        assert!(result["usage"].is_null());
    }
}
