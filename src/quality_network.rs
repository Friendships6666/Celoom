//! Anonymous network observations. No model generation, auth, tool dispatch,
//! proxy bypass, public lookup service, privilege escalation or package install.
use crate::network::Snapshot;
use base64::Engine;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    net::IpAddr,
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use url::Url;

const MAX_HOPS: usize = 30;
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;
const TRACE_OUTPUT_LIMIT: usize = 16 * 1024;
const TRACE_WALL_BUDGET: Duration = Duration::from_secs(12);
const PROCESS_REAP_BUDGET: Duration = Duration::from_millis(100);
const HEADER_SEPARATOR: &str = "\nJAA_NETWORK_HEADERS\n";
const CERTS_SEPARATOR: &str = "\nJAA_NETWORK_CERTS\n";
const HEADER_NAMES: &[&str] = &[
    "date",
    "server",
    "content-type",
    "content-length",
    "via",
    "cf-ray",
    "cf-cache-status",
    "x-cache",
    "x-cache-hits",
    "x-served-by",
    "x-amz-cf-id",
    "x-amz-cf-pop",
    "x-cdn",
    "x-akamai-transformed",
    "akamai-grn",
    "x-request-id",
    "request-id",
    "openai-request-id",
    "x-amzn-requestid",
    "x-oneapi-request-id",
    "x-newapi-request-id",
    "x-rixapi-request-id",
    "x-veloera-request-id",
    "x-litellm-call-id",
    "x-envoy-upstream-service-time",
    "x-kong-upstream-latency",
    "x-powered-by",
    "server-timing",
];

fn ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
fn safe(snapshot: &Snapshot, text: &str, limit: usize) -> String {
    static BEARER: OnceLock<Regex> = OnceLock::new();
    let redacted = snapshot.diagnostic_redact(text);
    let regex =
        BEARER.get_or_init(|| Regex::new(r"(?i)\bbearer[\t ]+[A-Za-z0-9._~+/=-]+").unwrap());
    regex
        .replace_all(&redacted, "Bearer [redacted]")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(limit)
        .collect()
}
fn headers_from_json(snapshot: &Snapshot, raw: &Value) -> Value {
    let mut headers = Map::new();
    if let Some(fields) = raw.as_object() {
        for name in HEADER_NAMES {
            if let Some(value) = fields
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
            {
                let text = if let Some(s) = value.as_str() {
                    Some(s.to_owned())
                } else {
                    value.as_array().map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .take(4)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                };
                if let Some(text) = text {
                    headers.insert((*name).into(), json!(safe(snapshot, &text, 256)));
                }
            }
        }
    }
    Value::Object(headers)
}
fn reqwest_headers(snapshot: &Snapshot, raw: &reqwest::header::HeaderMap) -> Value {
    let mut map = Map::new();
    for name in HEADER_NAMES {
        if let Some(text) = raw.get(*name).and_then(|v| v.to_str().ok()) {
            map.insert((*name).into(), json!(safe(snapshot, text, 256)));
        }
    }
    Value::Object(map)
}
fn safe_location(snapshot: &Snapshot, base: &Url, raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    let Ok(mut url) = base.join(raw) else {
        return json!("[invalid redirect location]");
    };
    if !matches!(url.scheme(), "http" | "https") {
        return json!("[non-HTTP redirect location]");
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    json!(safe(snapshot, url.as_str(), 512))
}
fn cdn(headers: &Value) -> Value {
    let fields = headers.as_object();
    let mut candidates = Vec::new();
    let field =
        |key: &str| -> Option<&str> { fields.and_then(|f| f.get(key)).and_then(Value::as_str) };
    for (name, keys, server_word) in [
        (
            "Cloudflare",
            vec!["cf-ray", "cf-cache-status"],
            "cloudflare",
        ),
        (
            "CloudFront",
            vec!["x-amz-cf-id", "x-amz-cf-pop"],
            "cloudfront",
        ),
        ("Fastly", vec!["x-served-by", "x-cache-hits"], "fastly"),
        (
            "Akamai",
            vec!["akamai-grn", "x-akamai-transformed"],
            "akamai",
        ),
    ] {
        let mut evidence: Vec<String> = keys
            .into_iter()
            .filter(|key| field(key).is_some())
            .map(|key| format!("header {key} present"))
            .collect();
        for key in ["server", "via", "x-cdn"] {
            if field(key).is_some_and(|v| v.to_ascii_lowercase().contains(server_word)) {
                evidence.push(format!("{key} contains {server_word}"));
            }
        }
        if !evidence.is_empty() {
            candidates.push(json!({"name":name,"evidence":evidence}));
        }
    }
    if field("server").is_some_and(|v| v.to_ascii_lowercase().contains("nginx")) {
        candidates.push(json!({"name":"nginx proxy","evidence":["server contains nginx; reverse proxy/server clue, not a CDN identity proof"]}));
    }
    json!({"candidates":candidates,"evidence_only":true,"notes":["Header branding can be inserted or changed by a proxy. These are clues, not verified CDN ownership or origin identity."]})
}
fn record(snapshot: &Snapshot, variant: &str, url: &Url) -> Value {
    let host = match url.host() {
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        Some(h) => h.to_string(),
        None => String::new(),
    };
    json!({"http_status":null,"header_latency_ms":null,"first_event_ms":null,"first_token_ms":null,"total_ms":0,"protocol":"network","terminal_status":null,"response_model":null,"response_id":null,"service_tier":null,"reasoning_context":null,"usage":null,"text_sample":"","refusal_sample":null,"output":null,"headers":{},"error":null,"request_started_at":crate::models::now(),"request_finished_at":null,"body_sha256":null,"response_sha256":null,"network":{"kind":variant,"target":{"host":host,"port":url.port_or_known_default(),"scheme":url.scheme()},"policy":safe(snapshot,&snapshot.transport_description(),512),"path":"unknown","dns":null,"connection":null,"http":null,"cdn":null,"route":null}})
}
fn set_error(result: &mut Value, kind: &str, message: &str) {
    result["error"] = json!({"kind":kind,"message":message});
}
fn dns_unknown(status: &str, note: &str) -> Value {
    json!({"status":status,"elapsed_ms":null,"addresses":[],"notes":[note]})
}
fn connection_unknown(status: &str, note: &str) -> Value {
    json!({"status":status,"dns_ms":null,"tcp_ms":null,"tls_ms":null,"http_header_ms":null,"remote_ip":null,"http_version":null,"tls":{"status":"unknown","verified":null,"subject":null,"issuer":null,"expires_at":null,"certificate_sha256":null,"notes":[note]}})
}
fn route_unknown(status: &str, tool: Option<&str>, note: &str) -> Value {
    json!({"status":status,"tool":tool,"reached_target":false,"hop_count":null,"max_hops":MAX_HOPS,"hops":[],"notes":[note]})
}
async fn resolve(url: &Url, cancel: &CancellationToken) -> Value {
    resolve_with_budget(url, cancel, Duration::from_secs(3)).await
}
async fn resolve_with_budget(url: &Url, cancel: &CancellationToken, budget: Duration) -> Value {
    let start = Instant::now();
    let host = match url.host() {
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        Some(h) => h.to_string(),
        None => return dns_unknown("error", "Target host is missing"),
    };
    if cancel.is_cancelled() {
        return dns_unknown("cancelled", "Local resolver wait cancelled");
    }
    if budget.is_zero() {
        return dns_unknown(
            "timeout",
            "Overall observation deadline expired before local resolution",
        );
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return json!({"status":"literal","elapsed_ms":ms(start),"addresses":[ip.to_string()],"notes":["IP literal: no DNS query was made."]});
    }
    let answer = tokio::select! {biased;_=cancel.cancelled()=>return dns_unknown("cancelled","Local resolver wait cancelled"),r=tokio::time::timeout(budget.min(Duration::from_secs(3)),tokio::net::lookup_host((host.as_str(),url.port_or_known_default().unwrap_or(443))))=>r};
    match answer {
        Ok(Ok(addresses)) => {
            let mut seen = HashSet::new();
            let values: Vec<_> = addresses
                .map(|a| a.ip().to_string())
                .filter(|a| seen.insert(a.clone()))
                .take(8)
                .collect();
            json!({"status":if values.is_empty(){"error"}else{"ok"},"elapsed_ms":ms(start),"addresses":values,"notes":["Bounded local OS resolver sample, separate from curl's DNS stage; cache and address order may differ."]})
        }
        Ok(Err(_)) => {
            json!({"status":"error","elapsed_ms":ms(start),"addresses":[],"notes":["Local resolver failed; no external DNS lookup service was contacted."]})
        }
        Err(_) => {
            json!({"status":"timeout","elapsed_ms":ms(start),"addresses":[],"notes":["Local resolver exceeded the three-second observation deadline."]})
        }
    }
}

#[derive(Debug)]
enum ProcessFailure {
    Unavailable,
    Cancelled,
    Timeout,
    TooLarge,
    Io,
}
struct ProcessOutput {
    success: bool,
    stdout: Vec<u8>,
}
fn clean_command(program: &str, args: &[String]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // Preserve only explicit system-trust paths; never a proxy/auth environment.
    for key in ["SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if program == "curl" {
        command.env("CURL_HOME", "/dev/null");
    }
    command
}
async fn run_command(
    program: &str,
    args: &[String],
    limit: usize,
    budget: Duration,
    cancel: &CancellationToken,
) -> Result<ProcessOutput, ProcessFailure> {
    if cancel.is_cancelled() {
        return Err(ProcessFailure::Cancelled);
    }
    let deadline = tokio::time::Instant::now() + budget;
    // Reaping is part of the caller's wall budget, never an extra second after it.
    if budget <= PROCESS_REAP_BUDGET {
        return Err(ProcessFailure::Timeout);
    }
    let work_deadline = deadline - PROCESS_REAP_BUDGET;
    let mut child = clean_command(program, args).spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ProcessFailure::Unavailable
        } else {
            ProcessFailure::Io
        }
    })?;
    let mut stdout = child.stdout.take().ok_or(ProcessFailure::Io)?;
    let mut bytes = Vec::new();
    let operation = async {
        let mut chunk = [0u8; 2048];
        loop {
            let n = stdout
                .read(&mut chunk)
                .await
                .map_err(|_| ProcessFailure::Io)?;
            if n == 0 {
                break;
            }
            if bytes.len() + n > limit {
                return Err(ProcessFailure::TooLarge);
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        let status = child.wait().await.map_err(|_| ProcessFailure::Io)?;
        Ok(status.success())
    };
    let outcome = tokio::select! {biased;_=cancel.cancelled()=>Err(ProcessFailure::Cancelled),r=tokio::time::timeout_at(work_deadline,operation)=>r.unwrap_or(Err(ProcessFailure::Timeout))};
    match outcome {
        Ok(success) => Ok(ProcessOutput {
            success,
            stdout: bytes,
        }),
        Err(error) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout_at(
                deadline.min(tokio::time::Instant::now() + PROCESS_REAP_BUDGET),
                child.wait(),
            )
            .await;
            Err(error)
        }
    }
}
fn process_error(result: &mut Value, error: ProcessFailure, tool: &str) {
    match error {
        ProcessFailure::Unavailable => set_error(
            result,
            "unavailable",
            &format!("{tool} is not installed or not available; no dependency was installed"),
        ),
        ProcessFailure::Cancelled => {
            set_error(result, "cancelled", "Network observation cancelled")
        }
        ProcessFailure::Timeout => set_error(
            result,
            "timeout",
            "Network tool exceeded its bounded wall-clock deadline",
        ),
        ProcessFailure::TooLarge => set_error(
            result,
            "output_limit",
            "Network tool output exceeded its bounded size limit",
        ),
        ProcessFailure::Io => {
            set_error(result, "tool_error", "Network tool could not be run safely")
        }
    }
}
fn curl_args(url: &Url, snapshot: &Snapshot) -> Vec<String> {
    vec![
        "-q".into(),
        "--http1.1".into(),
        "--head".into(),
        "--proxy".into(),
        "".into(),
        "--noproxy".into(),
        "*".into(),
        "--max-redirs".into(),
        "0".into(),
        "--connect-timeout".into(),
        snapshot
            .diagnostic_timeout("connect_timeout_secs", 5)
            .as_secs_f64()
            .to_string(),
        "--max-time".into(),
        snapshot
            .diagnostic_timeout("request_timeout_secs", 10)
            .as_secs_f64()
            .to_string(),
        "--silent".into(),
        "--show-error".into(),
        "--output".into(),
        "/dev/null".into(),
        "--write-out".into(),
        format!("%{{json}}{HEADER_SEPARATOR}%{{header_json}}{CERTS_SEPARATOR}%{{certs}}"),
        "--url".into(),
        url.as_str().into(),
    ]
}
fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= 0.0)
}
fn millis_seconds(value: &Value) -> Value {
    finite(value)
        .map(|v| json!(v * 1000.0))
        .unwrap_or(Value::Null)
}
fn stage(total: &Value, previous: &Value) -> Value {
    finite(total)
        .zip(finite(previous))
        .filter(|(a, b)| a >= b)
        .map(|(a, b)| json!((a - b) * 1000.0))
        .unwrap_or(Value::Null)
}
fn curl_tls(
    snapshot: &Snapshot,
    url: &Url,
    m: &Value,
    extra_certs: Option<&str>,
    success: bool,
) -> Value {
    if url.scheme() != "https" {
        return json!({"status":"not_applicable","verified":null,"subject":null,"issuer":null,"starts_at":null,"expires_at":null,"certificate_sha256":null,"notes":["Plain HTTP has no TLS handshake or certificate."]});
    }
    let verified = success && m["ssl_verify_result"].as_i64() == Some(0);
    let mut tls = json!({"status":if verified{"verified"}else{"unknown"},"verified":if verified{Some(true)}else{None},"subject":null,"issuer":null,"starts_at":null,"expires_at":null,"certificate_sha256":null,"notes":["Certificate metadata is unavailable in this curl build/capture; no extra TLS handshake was attempted."]});
    // curl >=7.88 can include certs via %{json} or %{certs} write-out. Parse only
    // bounded first-certificate metadata; never persist raw PEM or chain text.
    let certs_raw = m["certs"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| extra_certs.filter(|s| !s.trim().is_empty()));
    if let Some(certs) = certs_raw.filter(|s| s.len() <= 32 * 1024) {
        let mut pem = String::new();
        let mut in_pem = false;
        let mut found = false;
        for line in certs.lines() {
            let line = line
                .trim()
                .strip_prefix("Cert:")
                .unwrap_or(line.trim())
                .trim();
            if line == "-----END CERTIFICATE-----" {
                break;
            }
            if line == "-----BEGIN CERTIFICATE-----" {
                in_pem = true;
                continue;
            }
            if in_pem {
                pem.push_str(line);
                continue;
            }
            for (prefix, key) in [
                ("Subject:", "subject"),
                ("Issuer:", "issuer"),
                ("Start date:", "starts_at"),
                ("Expire date:", "expires_at"),
            ] {
                if let Some(value) = line
                    .strip_prefix(prefix)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if !tls[key].is_null() {
                        break;
                    }
                    tls[key] = json!(safe(snapshot, value, 512));
                    found = true;
                }
            }
        }
        if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(pem.as_bytes())
            && !der.is_empty()
        {
            tls["certificate_sha256"] = json!(format!("{:x}", Sha256::digest(&der)));
            found = true;
        }
        if found {
            tls["notes"] = json!([
                "Certificate metadata was parsed from this same curl HEAD capture; metadata alone does not prove successful verification. No extra TLS handshake was sent."
            ]);
        }
    }
    tls
}
fn curl_observation(
    result: &mut Value,
    snapshot: &Snapshot,
    url: &Url,
    raw: &[u8],
    success: bool,
) -> Result<(), ()> {
    let text = std::str::from_utf8(raw).map_err(|_| ())?;
    let (metrics, rest) = text.split_once(HEADER_SEPARATOR).unwrap_or((text, ""));
    let (headers, extra_certs) = match rest.split_once(CERTS_SEPARATOR) {
        Some((h, c)) => (h, Some(c)),
        None => (rest, None),
    };
    let m: Value = serde_json::from_str(metrics.trim()).map_err(|_| ())?;
    let status = m["http_code"]
        .as_u64()
        .or_else(|| m["response_code"].as_u64())
        .filter(|n| (100..=599).contains(n));
    let parsed_headers = serde_json::from_str::<Value>(headers).unwrap_or(Value::Null);
    let safe_headers = headers_from_json(snapshot, &parsed_headers);
    let location = parsed_headers
        .as_object()
        .and_then(|v| v.iter().find(|(k, _)| k.eq_ignore_ascii_case("location")))
        .map(|(_, v)| v)
        .and_then(|v| {
            v.as_str()
                .or_else(|| v.as_array().and_then(|v| v.first()).and_then(Value::as_str))
        })
        .or_else(|| m["redirect_url"].as_str().filter(|s| !s.is_empty()));
    let version = m["http_version"].as_str().map(|v| safe(snapshot, v, 16));
    let tls = curl_tls(snapshot, url, &m, extra_certs, success);
    // Zero connect/appconnect timestamps on a failed operation do not prove a
    // zero-duration TCP/TLS stage: report unknown until that stage completed.
    let tcp = if finite(&m["time_connect"]).is_some_and(|v| v > 0.0) {
        stage(&m["time_connect"], &m["time_namelookup"])
    } else {
        Value::Null
    };
    let tls_ms = if url.scheme() == "https"
        && finite(&m["time_connect"]).is_some_and(|v| v > 0.0)
        && finite(&m["time_appconnect"]).is_some_and(|v| v > 0.0)
    {
        stage(&m["time_appconnect"], &m["time_connect"])
    } else {
        Value::Null
    };
    let handshake = if url.scheme() == "https" {
        &m["time_appconnect"]
    } else {
        &m["time_connect"]
    };
    let header_stage = if status.is_some() && finite(handshake).is_some_and(|v| v > 0.0) {
        stage(&m["time_starttransfer"], handshake)
    } else {
        Value::Null
    };
    let dns_ms =
        if success && status.is_some() || finite(&m["time_namelookup"]).is_some_and(|v| v > 0.0) {
            millis_seconds(&m["time_namelookup"])
        } else {
            Value::Null
        };
    result["http_status"] = json!(status);
    result["header_latency_ms"] =
        if status.is_some() && finite(&m["time_starttransfer"]).is_some_and(|v| v > 0.0) {
            millis_seconds(&m["time_starttransfer"])
        } else {
            Value::Null
        };
    result["headers"] = safe_headers.clone();
    result["network"]["connection"] = json!({"status":if success&&status.is_some(){"ok"}else{"error"},"dns_ms":dns_ms,"tcp_ms":tcp,"tls_ms":tls_ms,"http_header_ms":header_stage,"remote_ip":m["remote_ip"].as_str().and_then(|s|s.parse::<IpAddr>().ok()).map(|ip|ip.to_string()),"http_version":version,"tls":tls});
    result["network"]["http"] = json!({"method":"HEAD","status":status,"version":version,"headers":safe_headers,"redirect_location":safe_location(snapshot,url,location),"redirect_followed":false});
    result["network"]["cdn"] = cdn(&result["headers"]);
    if success && status.is_some() {
        result["terminal_status"] = json!("completed");
    } else {
        let code = m["exitcode"].as_i64();
        set_error(
            result,
            if matches!(code, Some(51 | 60 | 77 | 83 | 90 | 91)) {
                "tls_verification"
            } else if code == Some(28) {
                "timeout"
            } else {
                "connection_error"
            },
            "Anonymous curl HEAD did not complete; stderr and URL-bearing error details were not persisted",
        );
    }
    Ok(())
}
async fn anonymous_head(
    snapshot: &Snapshot,
    url: &Url,
    cancel: &CancellationToken,
    result: &mut Value,
) {
    let start = Instant::now();
    let request = match snapshot.diagnostic_anonymous_head() {
        Ok(r) => r,
        Err(_) => {
            set_error(
                result,
                "configuration_error",
                "Invalid configured proxy/TLS policy",
            );
            return;
        }
    };
    let deadline = snapshot
        .diagnostic_timeout("headers_timeout_secs", 15)
        .min(snapshot.diagnostic_timeout("request_timeout_secs", 15));
    let response = tokio::select! {_=cancel.cancelled()=>{set_error(result,"cancelled","Anonymous HEAD cancelled");return;},r=tokio::time::timeout(deadline,request.send())=>r};
    match response {
        Ok(Ok(response)) => {
            let headers = reqwest_headers(snapshot, response.headers());
            let version = format!("{:?}", response.version());
            let status = response.status().as_u16();
            let elapsed = ms(start);
            let location = safe_location(
                snapshot,
                url,
                response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok()),
            );
            result["http_status"] = json!(status);
            result["header_latency_ms"] = json!(elapsed);
            result["headers"] = headers.clone();
            result["terminal_status"] = json!("completed");
            result["network"]["http"] = json!({"method":"HEAD","status":status,"version":version,"headers":headers,"redirect_location":location,"redirect_followed":false});
            result["network"]["cdn"] = cdn(&result["headers"]);
            if result["network"]["connection"].is_object() {
                result["network"]["connection"]["status"] = json!("http_reachable");
                result["network"]["connection"]["http_version"] = json!(version);
                result["network"]["connection"]["remote_ip"] =
                    json!(response.remote_addr().map(|a| a.ip().to_string()));
            }
        }
        Ok(Err(_)) => set_error(
            result,
            "connection_error",
            "Anonymous HEAD failed under the configured proxy/TLS policy; no direct fallback was attempted",
        ),
        Err(_) => set_error(
            result,
            "timeout",
            "Anonymous HEAD exceeded its configured bounded response-header deadline",
        ),
    }
}

fn parse_trace(text: &str, addresses: &HashSet<IpAddr>) -> Value {
    parse_trace_with_tool(text, addresses, "traceroute")
}

fn parse_trace_with_tool(text: &str, addresses: &HashSet<IpAddr>, tool: &str) -> Value {
    let mut hops = Vec::new();
    let mut reached = false;
    let mut hop_count = None;
    for line in text.lines() {
        let pieces: Vec<_> = line.split_whitespace().collect();
        let Some(first) = pieces.first() else {
            continue;
        };
        if first.ends_with("?:") || line.contains("[LOCALHOST]") {
            continue;
        }
        let ttl_token = first
            .trim_end_matches(':')
            .trim_end_matches('?')
            .split(".|--")
            .next()
            .unwrap_or("");
        let Some(ttl) = ttl_token
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_HOPS).contains(n))
        else {
            continue;
        };
        if hops.iter().any(|h: &Value| h["ttl"] == ttl) {
            continue;
        }
        let ip = pieces
            .iter()
            .skip(1)
            .find_map(|word| word.trim_matches(['(', ')', ',']).parse::<IpAddr>().ok());
        let rtt = pieces
            .windows(2)
            .find_map(|pair| {
                if pair[1] == "ms" {
                    pair[0]
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite() && *n >= 0.0)
                } else {
                    None
                }
            })
            .or_else(|| {
                pieces.iter().skip(1).find_map(|word| {
                    word.strip_suffix("ms")
                        .and_then(|num| num.parse::<f64>().ok())
                        .filter(|n| n.is_finite() && *n >= 0.0)
                })
            })
            .or_else(|| {
                if tool == "mtr" && ip.is_some() {
                    pieces
                        .get(4)
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|n| n.is_finite() && *n >= 0.0)
                } else {
                    None
                }
            });
        let is_target = ip.is_some_and(|ip| addresses.contains(&ip))
            || (tool == "tracepath" && ip.is_some() && pieces.iter().any(|w| *w == "reached"));
        if is_target {
            reached = true;
            hop_count = Some(ttl);
        }
        hops.push(json!({"ttl":ttl,"ip":ip.map(|ip|ip.to_string()),"rtt_ms":rtt,"status":if ip.is_some(){"observed"}else{"no_reply"}}));
        if is_target {
            break;
        }
    }
    let observed_hops = hops.len();
    let responding_hops = hops.iter().filter(|h| !h["ip"].is_null()).count();
    json!({"status":if reached{"ok"}else{"partial"},"tool":tool,"reached_target":reached,"hop_count":hop_count,"observed_hops":observed_hops,"responding_hops":responding_hops,"max_hops":MAX_HOPS,"hops":hops,"notes":if reached{vec!["Target matched an address from the bounded local DNS sample; hop count is the observed destination TTL."]}else{vec!["Destination not confirmed within the bounded observation; total route hop count is unknown. Asterisks do not by themselves prove packet loss."]}})
}
fn trace_args(url: &Url) -> Option<Vec<String>> {
    trace_tool_args("traceroute", url)
}
fn trace_tool_args(tool: &str, url: &Url) -> Option<Vec<String>> {
    let host = match url.host() {
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        Some(h) => h.to_string(),
        None => return None,
    };
    if host.starts_with('-')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
    {
        return None;
    }
    Some(match tool {
        "tracepath" => vec!["-n".into(), "-m".into(), MAX_HOPS.to_string(), host],
        "mtr" => vec![
            "-rn".into(),
            "-c".into(),
            "1".into(),
            "-m".into(),
            MAX_HOPS.to_string(),
            host,
        ],
        _ => vec![
            "-n".into(),
            "-m".into(),
            MAX_HOPS.to_string(),
            "-q".into(),
            "1".into(),
            "-w".into(),
            "0.5".into(),
            host,
        ],
    })
}
async fn route_trace(url: &Url, cancel: &CancellationToken, result: &mut Value) {
    let deadline = tokio::time::Instant::now() + TRACE_WALL_BUDGET;
    trace_with_lookup_chain(
        url,
        cancel,
        result,
        &["traceroute", "tracepath", "mtr"],
        deadline,
        resolve_with_budget(url, cancel, TRACE_WALL_BUDGET),
    )
    .await;
}
async fn trace_with_lookup<F: std::future::Future<Output = Value>>(
    url: &Url,
    cancel: &CancellationToken,
    result: &mut Value,
    tool: &str,
    deadline: tokio::time::Instant,
    lookup: F,
) {
    trace_with_lookup_chain(url, cancel, result, &[tool], deadline, lookup).await;
}
async fn trace_with_lookup_chain<F: std::future::Future<Output = Value>>(
    url: &Url,
    cancel: &CancellationToken,
    result: &mut Value,
    tools: &[&str],
    deadline: tokio::time::Instant,
    lookup: F,
) {
    let dns = tokio::select! {biased;_=cancel.cancelled()=>dns_unknown("cancelled","Route trace cancelled during local resolution"),r=tokio::time::timeout_at(deadline,lookup)=>r.unwrap_or_else(|_|dns_unknown("timeout","Overall route deadline expired during DNS"))};
    let addresses: HashSet<IpAddr> = dns["addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|s| s.parse().ok())
        .collect();
    result["network"]["dns"] = dns;
    if cancel.is_cancelled() {
        result["network"]["route"] = route_unknown(
            "error",
            tools.first().copied(),
            "Cancelled before trace invocation",
        );
        set_error(result, "cancelled", "Route trace cancelled");
        return;
    }
    if trace_args(url).is_none() {
        result["network"]["route"] = route_unknown(
            "unavailable",
            None,
            "Target host cannot be passed safely to route trace",
        );
        set_error(
            result,
            "unavailable",
            "Route trace unavailable for this target",
        );
        return;
    }
    // Prefer IPv4 when available so IPv4-only Linux hosts don't fail on dual-stack DNS.
    let resolved_list: Vec<&str> = result["network"]["dns"]["addresses"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(address) = resolved_list
        .iter()
        .copied()
        .find(|s| s.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv4()))
        .or_else(|| resolved_list.first().copied())
    else {
        result["network"]["route"] = route_unknown(
            "unavailable",
            tools.first().copied(),
            "No target address resolved within the bounded resolver deadline; trace not sent",
        );
        set_error(
            result,
            "unavailable",
            "No resolved address for bounded route trace",
        );
        return;
    };
    let mut last_error: Option<(String, ProcessFailure)> = None;
    for &tool in tools {
        let Some(mut args) = trace_tool_args(tool, url) else {
            continue;
        };
        *args.last_mut().unwrap() = address.to_owned();
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match run_command(tool, &args, TRACE_OUTPUT_LIMIT, remaining, cancel).await {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                let route = parse_trace_with_tool(&text, &addresses, tool);
                let has_hops = route["hops"].as_array().is_some_and(|h| !h.is_empty());
                if !output.success && !has_hops {
                    last_error = Some((tool.to_owned(), ProcessFailure::Io));
                    continue;
                }
                result["network"]["route"] = route;
                result["error"] = Value::Null;
                result["terminal_status"] = json!("completed");
                return;
            }
            Err(ProcessFailure::Unavailable) => {
                last_error = Some((tool.to_owned(), ProcessFailure::Unavailable));
                continue;
            }
            Err(error) => {
                let tool_name = tool.to_owned();
                result["network"]["route"] = route_unknown(
                    "error",
                    Some(&tool_name),
                    "Trace interrupted or timed out within bounded deadline",
                );
                process_error(result, error, &tool_name);
                return;
            }
        }
    }
    let (tool_name, failure) = last_error.unwrap_or_else(|| {
        (
            tools.first().copied().unwrap_or("traceroute").to_owned(),
            ProcessFailure::Unavailable,
        )
    });
    let status = if matches!(failure, ProcessFailure::Unavailable) {
        "unavailable"
    } else {
        "error"
    };
    result["network"]["route"] = route_unknown(
        status,
        Some(&tool_name),
        "Route trace tool unavailable or produced no hop observations",
    );
    if matches!(failure, ProcessFailure::Unavailable) {
        process_error(result, failure, &tool_name);
    } else {
        set_error(
            result,
            "trace_error",
            "Route trace exited without usable hop observations",
        );
    }
}

pub async fn observe(snapshot: &Snapshot, variant: &str, cancel: CancellationToken) -> Value {
    let start = Instant::now();
    let endpoint = snapshot.diagnostic_endpoint();
    let url = match Url::parse(&endpoint) {
        Ok(url) => url,
        Err(_) => {
            return json!({"protocol":"network","error":{"kind":"configuration_error","message":"Invalid configured endpoint"}});
        }
    };
    let mut result = record(snapshot, variant, &url);
    if cancel.is_cancelled() {
        set_error(
            &mut result,
            "cancelled",
            "Network observation cancelled before any request",
        );
    } else if !matches!(variant, "connection" | "http_head" | "route_trace") {
        set_error(
            &mut result,
            "unsupported_variant",
            "Unknown network diagnostic variant",
        );
    } else {
        match snapshot.diagnostic_has_proxy() {
            Err(_) => set_error(
                &mut result,
                "configuration_error",
                "Invalid configured proxy policy; no direct observation attempted",
            ),
            Ok(proxy) => {
                result["network"]["path"] =
                    json!(if proxy { "configured_proxy" } else { "direct" });
                match variant {
                    "http_head" => anonymous_head(snapshot, &url, &cancel, &mut result).await,
                    "connection" if proxy => {
                        result["network"]["dns"] = dns_unknown(
                            "skipped",
                            "A configured proxy may resolve the origin remotely or be bypassed by NO_PROXY. Direct DNS was skipped conservatively to avoid origin DNS leakage.",
                        );
                        result["network"]["connection"] = connection_unknown(
                            "skipped",
                            "Per-stage DNS/TCP/TLS metrics are unknown for the configured proxy path; no direct curl connection was made.",
                        );
                        anonymous_head(snapshot, &url, &cancel, &mut result).await;
                    }
                    "connection" => {
                        result["network"]["dns"] = resolve(&url, &cancel).await;
                        result["network"]["connection"] =
                            connection_unknown("unknown", "No timing observation yet");
                        if cancel.is_cancelled() {
                            set_error(
                                &mut result,
                                "cancelled",
                                "Connection observation cancelled before curl",
                            );
                        } else {
                            let args = curl_args(&url, snapshot);
                            let deadline = snapshot.diagnostic_timeout("request_timeout_secs", 10);
                            match run_command(
                                "curl",
                                &args,
                                COMMAND_OUTPUT_LIMIT,
                                deadline,
                                &cancel,
                            )
                            .await
                            {
                                Ok(output) => {
                                    if curl_observation(
                                        &mut result,
                                        snapshot,
                                        &url,
                                        &output.stdout,
                                        output.success,
                                    )
                                    .is_err()
                                    {
                                        set_error(
                                            &mut result,
                                            "unavailable",
                                            "Installed curl did not provide supported JSON timing output; no additional HEAD was sent",
                                        );
                                    } else if !cancel.is_cancelled() {
                                        let resolved_ip = result["network"]["connection"]
                                            ["remote_ip"]
                                            .as_str()
                                            .and_then(|s| s.parse::<IpAddr>().ok())
                                            .or_else(|| {
                                                result["network"]["dns"]["addresses"]
                                                    .as_array()
                                                    .and_then(|a| a.first())
                                                    .and_then(Value::as_str)
                                                    .and_then(|s| s.parse::<IpAddr>().ok())
                                            });
                                        if let Some(ip) = resolved_ip {
                                            let tls_sub = result["network"]["connection"]["tls"]
                                                ["subject"]
                                                .as_str()
                                                .map(str::to_string);
                                            let is_local = crate::quality_ip_report::is_bogon(ip);
                                            let origin = format!(
                                                "{}://{}{}",
                                                url.scheme(),
                                                url.host_str().unwrap_or(""),
                                                url.port()
                                                    .map(|p| format!(":{p}"))
                                                    .unwrap_or_default()
                                            );
                                            let (report, gw) = tokio::join!(
                                                crate::quality_ip_report::build_ip_report(
                                                    ip,
                                                    url.host_str(),
                                                    tls_sub.as_deref(),
                                                    &cancel,
                                                ),
                                                async {
                                                    if is_local {
                                                        Value::Null
                                                    } else {
                                                        crate::quality_ip_report::probe_gateway(
                                                            &origin, &cancel,
                                                        )
                                                        .await
                                                    }
                                                }
                                            );
                                            result["network"]["ip_report"] = report;
                                            if !gw.is_null() {
                                                result["network"]["gateway_probe"] = gw;
                                            }
                                        }
                                    }
                                }
                                Err(error) => process_error(&mut result, error, "curl"),
                            }
                        }
                    }
                    "route_trace" if proxy => {
                        result["network"]["dns"] =
                            dns_unknown("skipped", "Configured proxy: no direct DNS lookup made");
                        result["network"]["route"] = route_unknown(
                            "skipped",
                            None,
                            "Route tracing would bypass the configured proxy; skipped, including when NO_PROXY might apply",
                        );
                        result["terminal_status"] = json!("completed");
                    }
                    "route_trace" => route_trace(&url, &cancel, &mut result).await,
                    _ => unreachable!(),
                }
            }
        }
    }
    result["total_ms"] = json!(ms(start));
    result["request_finished_at"] = json!(crate::models::now());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AppConfig, Selection};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::any,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::mpsc;

    const SECRET: &str = "fixture-model-secret";
    fn snapshot(origin: &str, network: Value) -> Snapshot {
        let config:AppConfig=serde_json::from_value(json!({"sites":[{"id":"s","name":"s","base_url":format!("{origin}/nested/v1"),"keys":[{"id":"k","name":"k","secret":SECRET,"models":[{"id":"m","name":"m","model":"fixture"}]}]}],"network":network})).unwrap();
        Snapshot::resolve(
            &config,
            &Selection {
                site_id: "s".into(),
                key_id: "k".into(),
                model_id: "m".into(),
            },
        )
        .unwrap()
    }
    fn direct(origin: &str) -> Snapshot {
        snapshot(origin, json!({"proxy_mode":"direct"}))
    }
    struct Server {
        origin: String,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn server(router: axum::Router) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server { origin, task }
    }
    fn parsed(metrics: Value, headers: Value, https: bool, success: bool) -> Value {
        let s = direct(if https {
            "https://127.0.0.1:1"
        } else {
            "http://127.0.0.1:1"
        });
        let url = Url::parse(&s.diagnostic_endpoint()).unwrap();
        let mut result = record(&s, "connection", &url);
        let raw = format!("{metrics}{HEADER_SEPARATOR}{headers}");
        curl_observation(&mut result, &s, &url, raw.as_bytes(), success).unwrap();
        result
    }
    fn timing() -> Value {
        json!({"http_code":401,"http_version":"1.1","time_namelookup":0.001,"time_connect":0.004,"time_appconnect":0.009,"time_starttransfer":0.014,"remote_ip":"127.0.0.1","ssl_verify_result":0,"exitcode":0})
    }

    #[tokio::test]
    async fn head_exact_endpoint_no_auth_body_cookie_and_http_errors_reachable() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server = server(axum::Router::new().fallback(any(move |r: Request<Body>| {
            let tx = tx.clone();
            async move {
                let (parts, body) = r.into_parts();
                tx.send((parts, to_bytes(body, 1024).await.unwrap()))
                    .unwrap();
                (
                    StatusCode::METHOD_NOT_ALLOWED,
                    [("set-cookie", "private=never-retain"), ("server", "nginx")],
                )
            }
        })))
        .await;
        let s = direct(&server.origin);
        for _ in 0..2 {
            let result = observe(&s, "http_head", CancellationToken::new()).await;
            assert_eq!(result["terminal_status"], "completed");
            assert_eq!(result["http_status"], 405);
            assert!(result["error"].is_null());
            assert!(result["headers"].get("set-cookie").is_none());
            assert!(result["network"]["dns"].is_null());
            let (parts, body) = rx.recv().await.unwrap();
            assert_eq!(parts.method, "HEAD");
            assert_eq!(parts.uri.path(), "/nested/v1/responses");
            assert!(body.is_empty());
            for key in ["authorization", "proxy-authorization", "cookie"] {
                assert!(!parts.headers.contains_key(key));
            }
        }
    }
    #[tokio::test]
    async fn head_redirect_is_not_followed_and_location_is_sanitized() {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let target = server(axum::Router::new().fallback(any(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async { StatusCode::OK }
        })))
        .await;
        let location = format!(
            "http://user:password@{}/destination?token={SECRET}#private",
            target.origin.trim_start_matches("http://")
        );
        let source = server(axum::Router::new().fallback(any(move || {
            let location = location.clone();
            async move { (StatusCode::FOUND, [("location", location)]) }
        })))
        .await;
        let result = observe(
            &direct(&source.origin),
            "http_head",
            CancellationToken::new(),
        )
        .await;
        assert_eq!(result["http_status"], 302);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(
            result["network"]["http"]["redirect_location"],
            format!("{}/destination", target.origin)
        );
        assert_eq!(result["network"]["http"]["redirect_followed"], false);
        assert!(!result.to_string().contains(SECRET));
    }
    #[tokio::test]
    async fn configured_proxy_connection_skips_origin_dns_and_curl() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let proxy = server(axum::Router::new().fallback(any(move |r: Request<Body>| {
            tx.send(r.uri().to_string()).unwrap();
            async { StatusCode::UNAUTHORIZED }
        })))
        .await;
        // .invalid is not resolved: the HTTP proxy receives the absolute URI.
        let s = snapshot(
            "http://must-never-resolve.invalid",
            json!({"proxy_mode":"manual","proxy_url":proxy.origin}),
        );
        let result = observe(&s, "connection", CancellationToken::new()).await;
        assert_eq!(result["network"]["path"], "configured_proxy");
        assert_eq!(result["network"]["dns"]["status"], "skipped");
        for key in ["dns_ms", "tcp_ms", "tls_ms", "http_header_ms"] {
            assert!(result["network"]["connection"][key].is_null());
        }
        assert_eq!(result["http_status"], 401);
        assert!(
            rx.recv()
                .await
                .unwrap()
                .starts_with("http://must-never-resolve.invalid/")
        );
    }
    #[tokio::test]
    async fn proxy_trace_skips_when_proxy_applies_and_respects_no_proxy() {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let proxy = server(axum::Router::new().fallback(any(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async { StatusCode::OK }
        })))
        .await;
        let proxied = snapshot(
            "http://must-never-resolve.invalid",
            json!({"proxy_mode":"manual","proxy_url":proxy.origin}),
        );
        let result = observe(&proxied, "route_trace", CancellationToken::new()).await;
        assert_eq!(result["network"]["dns"]["status"], "skipped");
        assert_eq!(result["network"]["route"]["status"], "skipped");
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        let bypassed = snapshot(
            "http://127.0.0.1:1/v1",
            json!({"proxy_mode":"manual","proxy_url":proxy.origin,"no_proxy":"127.0.0.1,localhost"}),
        );
        assert!(!bypassed.diagnostic_has_proxy().unwrap());
    }
    #[tokio::test]
    async fn failed_proxy_never_retries_direct_origin() {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let origin = server(axum::Router::new().fallback(any(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async { StatusCode::OK }
        })))
        .await;
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let s = snapshot(
            &origin.origin,
            json!({"proxy_mode":"manual","proxy_url":proxy}),
        );
        let result = observe(&s, "connection", CancellationToken::new()).await;
        assert_eq!(result["error"]["kind"], "connection_error");
        assert_eq!(result["network"]["dns"]["status"], "skipped");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn curl_args_are_atomic_anonymous_and_disable_configuration() {
        let s = direct("https://127.0.0.1:1");
        let url = Url::parse(&s.diagnostic_endpoint()).unwrap();
        let args = curl_args(&url, &s);
        assert_eq!(args[0], "-q");
        assert_eq!(args.last().unwrap(), url.as_str());
        for forbidden in [
            SECRET,
            "--location",
            "-L",
            "--insecure",
            "-k",
            "--user",
            "--cookie",
            "--data",
            "--header",
        ] {
            assert!(!args.iter().any(|arg| arg.contains(forbidden)));
        }
        assert!(args.windows(2).any(|a| a == ["--proxy", ""]));
        assert!(args.windows(2).any(|a| a == ["--noproxy", "*"]));
        let command = clean_command("curl", &args);
        let env: Vec<_> = command.as_std().get_envs().collect();
        assert!(
            env.iter()
                .any(|(k, v)| *k == "CURL_HOME" && v.is_some_and(|v| v == "/dev/null"))
        );
        assert!(env.iter().all(|(k, _)| {
            [
                "PATH",
                "LANG",
                "LC_ALL",
                "CURL_HOME",
                "SSL_CERT_FILE",
                "SSL_CERT_DIR",
                "CURL_CA_BUNDLE",
            ]
            .iter()
            .any(|allowed| k == allowed)
        }));
    }
    #[test]
    fn curl_timing_uses_measured_deltas_and_http_has_no_tls() {
        let r = parsed(timing(), json!({}), false, true);
        let c = &r["network"]["connection"];
        assert!((c["dns_ms"].as_f64().unwrap() - 1.0).abs() < 1e-8);
        assert!((c["tcp_ms"].as_f64().unwrap() - 3.0).abs() < 1e-8);
        assert!((c["http_header_ms"].as_f64().unwrap() - 10.0).abs() < 1e-8);
        assert!(c["tls_ms"].is_null());
        assert_eq!(c["tls"]["status"], "not_applicable");
        let r = parsed(timing(), json!({}), true, true);
        assert!((r["network"]["connection"]["tls_ms"].as_f64().unwrap() - 5.0).abs() < 1e-8);
    }
    #[test]
    fn curl_failed_zero_and_invalid_stages_stay_unknown() {
        let r = parsed(
            json!({"time_namelookup":0,"time_connect":0,"time_appconnect":0,"time_starttransfer":0,"http_code":0,"exitcode":6}),
            json!({}),
            true,
            false,
        );
        for key in ["dns_ms", "tcp_ms", "tls_ms", "http_header_ms"] {
            assert!(r["network"]["connection"][key].is_null(), "{key}");
        }
        assert!(r["header_latency_ms"].is_null());
        assert!(r["terminal_status"].is_null());
        let mut m = timing();
        m["time_connect"] = json!(0.0001);
        let r = parsed(m, json!({}), true, true);
        assert!(r["network"]["connection"]["tcp_ms"].is_null());
        assert!(millis_seconds(&json!(-1)).is_null());
        assert!(stage(&json!(0.1), &json!(0.2)).is_null());
    }
    #[test]
    fn curl_certificates_are_first_leaf_bounded_redacted_and_no_raw_pem() {
        let mut m = timing();
        m["certs"] = json!(format!(
            "Subject: CN={SECRET}\nIssuer: CN=fixture CA\nStart date: Jan 1 00:00:00 2026 GMT\nExpire date: Jan 1 00:00:00 2027 GMT\nCert:-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\nSubject: CN=chain\nIssuer: CN=other"
        ));
        let r = parsed(m, json!({}), true, true);
        let tls = &r["network"]["connection"]["tls"];
        assert_eq!(tls["subject"], "CN=[redacted]");
        assert_eq!(tls["issuer"], "CN=fixture CA");
        assert_eq!(tls["verified"], true);
        assert_eq!(
            tls["certificate_sha256"],
            format!("{:x}", Sha256::digest(b"abc"))
        );
        assert_eq!(tls["expires_at"], "Jan 1 00:00:00 2027 GMT");
        assert!(!r.to_string().contains("BEGIN CERTIFICATE"));
        assert!(!r.to_string().contains(SECRET));
        let r = parsed(timing(), json!({}), true, true);
        assert!(r["network"]["connection"]["tls"]["issuer"].is_null());
    }
    #[test]
    fn curl_tls_failure_does_not_claim_verification() {
        let mut m = timing();
        m["ssl_verify_result"] = json!(20);
        m["exitcode"] = json!(60);
        let r = parsed(m, json!({}), true, false);
        assert_eq!(r["error"]["kind"], "tls_verification");
        assert!(r["network"]["connection"]["tls"]["verified"].is_null());
    }
    #[test]
    fn cdn_and_header_allowlist_remain_only_evidence() {
        let s = direct("http://127.0.0.1:1");
        let headers = headers_from_json(
            &s,
            &json!({"CF-Ray":["fake"],"server":["nginx"],"authorization":[SECRET],"set-cookie":[SECRET],"x-request-id":[format!("Bearer {SECRET}")]}),
        );
        assert!(headers.get("authorization").is_none());
        assert!(headers.get("set-cookie").is_none());
        assert!(!headers.to_string().contains(SECRET));
        let evidence = cdn(&headers);
        assert_eq!(evidence["evidence_only"], true);
        assert_eq!(evidence["candidates"][0]["name"], "Cloudflare");
        assert_eq!(evidence["candidates"][1]["name"], "nginx proxy");
    }
    #[test]
    fn route_parser_ipv6_no_reply_partial_and_unknown_hop_count() {
        let addresses = HashSet::from(["2001:db8::8".parse().unwrap()]);
        let route = parse_trace(
            "traceroute to 2001:db8::8\n 1 2001:db8::1 0.123 ms\n 2 *\n 3 2001:db8::8 1.250 ms\n",
            &addresses,
        );
        assert_eq!(route["status"], "ok");
        assert_eq!(route["hop_count"], 3);
        assert_eq!(route["hops"][1]["status"], "no_reply");
        assert!(route["hops"][1]["rtt_ms"].is_null());
        let partial = parse_trace(
            "1 2001:db8::1 0.1 ms\n2 * * *\n31 2001:db8::8 1 ms",
            &addresses,
        );
        assert_eq!(partial["status"], "partial");
        assert!(partial["hop_count"].is_null());
        assert_eq!(partial["reached_target"], false);
        assert_eq!(partial["hops"].as_array().unwrap().len(), 2);
        let empty = parse_trace("unrecognized diagnostic", &addresses);
        assert!(empty["hop_count"].is_null());
        assert!(empty["hops"].as_array().unwrap().is_empty());

        let tp = parse_trace_with_tool(
            " 1?: [LOCALHOST] pmtu 1500\n 1: 2001:db8::1 0.42ms\n 2: no reply\n 3: 2001:db8::8 1.50ms reached\n",
            &addresses,
            "tracepath",
        );
        assert_eq!(tp["status"], "ok");
        assert_eq!(tp["hop_count"], 3);
        assert_eq!(tp["responding_hops"], 2);

        let mtr = parse_trace_with_tool(
            "HOST: box Loss% Snt Last Avg Best Wrst StDev\n 1.|-- 2001:db8::1 0.0% 1 0.5 0.5 0.5 0.5 0.0\n 2.|-- ??? 100.0% 1 0.0 0.0 0.0 0.0 0.0\n 3.|-- 2001:db8::8 0.0% 1 2.1 2.1 2.1 2.1 0.0\n",
            &addresses,
            "mtr",
        );
        assert_eq!(mtr["status"], "ok");
        assert_eq!(mtr["hop_count"], 3);
        assert_eq!(mtr["hops"][2]["rtt_ms"], 2.1);
    }
    #[test]
    fn route_args_reject_options_and_preserve_numeric_ipv6() {
        assert!(trace_args(&Url::parse("http://-bad.invalid/").unwrap()).is_none());
        let args = trace_args(&Url::parse("http://[::1]/").unwrap()).unwrap();
        assert_eq!(args.last().unwrap(), "::1");
        assert!(args.windows(2).any(|p| p == ["-q", "1"]));
        assert!(args.windows(2).any(|p| p == ["-m", "30"]));
    }
    #[tokio::test]
    async fn missing_binary_is_honest_unavailable() {
        let error = run_command(
            "/nonexistent/jaa-network-fixture",
            &[],
            1024,
            Duration::from_secs(1),
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(error, ProcessFailure::Unavailable));
        let mut result = json!({"terminal_status":null});
        process_error(&mut result, error, "curl");
        assert_eq!(result["error"]["kind"], "unavailable");
        assert!(result["terminal_status"].is_null());
    }
    #[tokio::test]
    async fn process_output_is_bounded_and_timeout_is_reaped() {
        let error = run_command(
            "/usr/bin/yes",
            &[],
            100,
            Duration::from_secs(1),
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(error, ProcessFailure::TooLarge));
        let start = Instant::now();
        let error = run_command(
            "/bin/sleep",
            &["10".into()],
            100,
            Duration::from_millis(200),
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(error, ProcessFailure::Timeout));
        assert!(start.elapsed() < Duration::from_millis(500));
    }
    #[tokio::test]
    async fn running_process_cancellation_is_prompt() {
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let action = async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            trigger.cancel();
        };
        let start = Instant::now();
        let args = ["10".into()];
        let (result, ()) = tokio::join!(
            run_command("/bin/sleep", &args, 100, Duration::from_secs(5), &cancel),
            action
        );
        assert!(matches!(result, Err(ProcessFailure::Cancelled)));
        assert!(start.elapsed() < Duration::from_millis(500));
    }
    #[tokio::test]
    async fn trace_dns_and_tool_share_one_deadline() {
        let s = direct("http://127.0.0.1:1");
        let url = Url::parse(&s.diagnostic_endpoint()).unwrap();
        let mut result = record(&s, "route_trace", &url);
        let cancel = CancellationToken::new();
        let lookup = async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            json!({"status":"literal","addresses":["127.0.0.1"]})
        };
        let start = Instant::now();
        trace_with_lookup(
            &url,
            &cancel,
            &mut result,
            "/bin/sleep",
            tokio::time::Instant::now() + Duration::from_millis(200),
            lookup,
        )
        .await;
        assert_eq!(result["error"]["kind"], "timeout");
        assert!(start.elapsed() < Duration::from_millis(400));
        let mut result = record(&s, "route_trace", &url);
        trace_with_lookup(
            &url,
            &cancel,
            &mut result,
            "/nonexistent/must-not-start",
            tokio::time::Instant::now() + Duration::from_millis(10),
            std::future::pending(),
        )
        .await;
        assert_eq!(result["network"]["dns"]["status"], "timeout");
        assert_eq!(result["network"]["route"]["status"], "unavailable");
    }
    #[tokio::test]
    async fn head_cancel_and_precancel_send_no_extra_request() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let source = server(axum::Router::new().fallback(any(move || {
            tx.send(()).unwrap();
            async { std::future::pending::<StatusCode>().await }
        })))
        .await;
        let s = direct(&source.origin);
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let action = async move {
            rx.recv().await.unwrap();
            trigger.cancel();
        };
        let start = Instant::now();
        let (result, ()) = tokio::join!(observe(&s, "http_head", cancel.clone()), action);
        assert_eq!(result["error"]["kind"], "cancelled");
        assert!(start.elapsed() < Duration::from_secs(1));
        let result = observe(&s, "connection", cancel).await;
        assert_eq!(result["error"]["kind"], "cancelled");
        assert!(result["network"]["dns"].is_null());
    }
    #[tokio::test]
    async fn direct_curl_loopback_head_is_anonymous_measured_and_no_redirect() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let source = server(axum::Router::new().fallback(any(move |r: Request<Body>| {
            let tx = tx.clone();
            async move {
                let (parts, body) = r.into_parts();
                tx.send((parts, to_bytes(body, 1024).await.unwrap()))
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(25)).await;
                (
                    StatusCode::FOUND,
                    [
                        ("location", "http://127.0.0.1:1/never?private=secret"),
                        ("set-cookie", "private=never"),
                    ],
                )
            }
        })))
        .await;
        let result = observe(
            &direct(&source.origin),
            "connection",
            CancellationToken::new(),
        )
        .await;
        assert_eq!(result["terminal_status"], "completed", "{result}");
        assert_eq!(result["http_status"], 302);
        assert_eq!(result["network"]["dns"]["status"], "literal");
        assert!(result["header_latency_ms"].as_f64().unwrap() >= 20.0);
        assert!(result["network"]["connection"]["tcp_ms"].as_f64().is_some());
        assert!(result["network"]["connection"]["tls_ms"].is_null());
        assert_eq!(
            result["network"]["http"]["redirect_location"],
            "http://127.0.0.1:1/never"
        );
        let (parts, body) = rx.recv().await.unwrap();
        assert_eq!(parts.method, "HEAD");
        assert_eq!(parts.uri.path(), "/nested/v1/responses");
        assert!(body.is_empty());
        for key in ["authorization", "proxy-authorization", "cookie"] {
            assert!(!parts.headers.contains_key(key));
        }
        assert!(rx.try_recv().is_err());
    }
}
