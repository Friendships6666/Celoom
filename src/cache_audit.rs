//! Metadata-only request-prefix diagnostics; raw values are never retained.
//! The caller supplies a private random installation key and calls `completed`
//! only after committing a successful normal response. Equality diagnostics do
//! not establish whether a provider granted a cache hit.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

type Digest = [u8; 32];
type HmacSha256 = Hmac<Sha256>;

const MAX_SESSIONS: usize = 256;
// Bound the combined input/output digest vectors.
const MAX_BASELINE_ITEMS: usize = 100_000;
const MAX_PENDING_ATTEMPTS: usize = 32;

#[derive(Clone)]
pub struct CacheAudit {
    inner: Arc<Inner>,
}

struct Inner {
    key: [u8; 32],
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<Digest, Session>,
    // Deterministic FIFO eviction, independent of wall-clock time.
    insertion_order: VecDeque<Digest>,
}

#[derive(Default)]
struct Session {
    complete: Option<Baseline>,
    attempts: VecDeque<Attempt>,
}

struct Attempt {
    request: Digest,
    epoch: u32,
}

struct Baseline {
    epoch: u32,
    instructions: Digest,
    tools: Digest,
    settings: Digest,
    input: Vec<Digest>,
    output: Vec<Digest>,
    output_bytes: usize,
}

struct Fingerprints {
    request: Digest,
    instructions: Digest,
    tools: Digest,
    settings: Digest,
    input: Digest,
    request_bytes: usize,
    input_bytes: Option<usize>,
}

#[derive(Default)]
struct Comparison {
    prior_epoch: Option<u32>,
    same_epoch: Option<bool>,
    instructions_unchanged: Option<bool>,
    tools_unchanged: Option<bool>,
    settings_unchanged: Option<bool>,
    prior_input_items: Option<usize>,
    prior_output_items: Option<usize>,
    prior_output_bytes: Option<usize>,
    expected_prefix_items: Option<usize>,
    common_prefix_items: Option<usize>,
    prior_input_prefix_preserved: Option<bool>,
    native_output_replayed: Option<bool>,
    expected_prefix_preserved: Option<bool>,
    first_difference_item: Option<usize>,
}

impl CacheAudit {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            inner: Arc::new(Inner {
                key,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Only literal `normal` participates in session state. Other purposes
    /// have null comparisons and cannot evict or overwrite normal sessions.
    /// A request never replaces a successful complete baseline.
    pub fn request(&self, session_id: &str, epoch: u32, purpose: &str, body: &Value) -> Value {
        let fingerprints = self.fingerprints(body);
        let input = body.get("input").and_then(Value::as_array);
        let normal = purpose == "normal";
        let mut comparison = Comparison::default();
        let mut change_kind = "first_request";

        if normal {
            let session_key = self.session_digest(session_id);
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = state.session_mut(session_key);
            if let Some(prior) = &session.complete {
                comparison = self.compare(prior, epoch, body, &fingerprints);
                change_kind = if comparison.same_epoch == Some(false) {
                    "epoch_changed"
                } else if comparison.instructions_unchanged == Some(false) {
                    "instructions_changed"
                } else if comparison.tools_unchanged == Some(false) {
                    "tools_changed"
                } else if comparison.settings_unchanged == Some(false) {
                    "settings_changed"
                } else if comparison.prior_input_prefix_preserved == Some(false) {
                    "history_changed"
                } else if comparison.native_output_replayed == Some(false) {
                    "native_replay_changed"
                } else {
                    "append_only"
                };
            }
            // Exact body hashes associate completion with its own epoch, not
            // the epoch of a later failed attempt. Never retain attempt bodies.
            if let Some(index) = session.attempts.iter().position(|attempt| {
                attempt.request == fingerprints.request && attempt.epoch == epoch
            }) {
                session.attempts.remove(index);
            }
            if session.attempts.len() == MAX_PENDING_ATTEMPTS {
                session.attempts.pop_front();
            }
            session.attempts.push_back(Attempt {
                request: fingerprints.request,
                epoch,
            });
        }

        json!({
            // Never echo an unrecognized caller-supplied purpose.
            "purpose": match purpose {
                "normal" => Some("normal"),
                "compact" => Some("compact"),
                "probe" => Some("probe"),
                _ => None,
            },
            "epoch": epoch,
            "normal_baseline_used": normal && comparison.prior_epoch.is_some(),
            "baseline_eligible": normal && input.is_some_and(|items| items.len() <= MAX_BASELINE_ITEMS),
            "prior_complete": comparison.prior_epoch.is_some(),
            "prior_epoch": comparison.prior_epoch,
            "same_epoch": comparison.same_epoch,
            "instructions_unchanged": comparison.instructions_unchanged,
            "tools_unchanged": comparison.tools_unchanged,
            "settings_unchanged": comparison.settings_unchanged,
            "input_items": input.map(Vec::len),
            "prior_input_items": comparison.prior_input_items,
            "prior_output_items": comparison.prior_output_items,
            "prior_output_bytes": comparison.prior_output_bytes,
            "expected_prefix_items": comparison.expected_prefix_items,
            // Compare against the complete expected prefix: old input + output.
            "common_prefix_items": comparison.common_prefix_items,
            "prior_input_prefix_preserved": comparison.prior_input_prefix_preserved,
            "native_output_replayed": comparison.native_output_replayed,
            "expected_prefix_preserved": comparison.expected_prefix_preserved,
            "first_difference_item": comparison.first_difference_item,
            "change_kind": change_kind,
            "request_fingerprint": hex(&fingerprints.request),
            "instructions_fingerprint": hex(&fingerprints.instructions),
            "tools_fingerprint": hex(&fingerprints.tools),
            "settings_fingerprint": hex(&fingerprints.settings),
            "input_fingerprint": hex(&fingerprints.input),
            "request_bytes": fingerprints.request_bytes,
            "input_bytes": fingerprints.input_bytes,
            "input_present": body.get("input").is_some(),
            "instructions_present": body.get("instructions").is_some(),
            "tools_present": body.get("tools").is_some(),
        })
    }

    /// Call only after committing a successful normal response. Unmatched
    /// attempts (including compact/probe-only bodies) cannot change a baseline.
    /// Normal calls for a session must be serialized if identical bodies can
    /// be in flight across epochs: this API has no separate attempt identifier.
    pub fn completed(&self, session_id: &str, body: &Value, output: &[Value]) {
        let request = self.digest("request", body).0;
        let session_key = self.session_digest(session_id);
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = state.sessions.get_mut(&session_key) else {
            return;
        };
        let Some(index) = session
            .attempts
            .iter()
            .rposition(|attempt| attempt.request == request)
        else {
            return;
        };
        let attempt = session.attempts.remove(index).expect("matched attempt");
        // An unrepresentable successful baseline invalidates the old baseline;
        // do not report stale state as the most recent completion.
        session.complete = self.baseline(attempt.epoch, body, output);
    }

    pub(crate) fn digest_value(&self, domain: &str, value: &Value) -> String {
        hex(&self.digest(domain, value).0)
    }

    fn fingerprints(&self, body: &Value) -> Fingerprints {
        let (request, request_bytes) = self.digest("request", body);
        let (input, input_bytes) = self.field_digest("input", body.get("input"));
        Fingerprints {
            request,
            instructions: self
                .field_digest("instructions", body.get("instructions"))
                .0,
            tools: self.field_digest("tools", body.get("tools")).0,
            settings: self.settings_digest(body),
            input,
            request_bytes,
            input_bytes,
        }
    }

    fn compare(
        &self,
        prior: &Baseline,
        epoch: u32,
        body: &Value,
        facts: &Fingerprints,
    ) -> Comparison {
        let input = body.get("input").and_then(Value::as_array);
        let expected_items = prior.input.len() + prior.output.len();
        let current: Vec<Digest> = input
            .into_iter()
            .flatten()
            .take(expected_items)
            .map(|item| self.digest("native_item", item).0)
            .collect();
        let common = prior
            .input
            .iter()
            .chain(&prior.output)
            .zip(&current)
            .take_while(|(expected, actual)| expected == actual)
            .count();
        let input_preserved = input.is_some() && common >= prior.input.len();
        let output_replayed = input.is_some()
            && current.len() == expected_items
            && current[prior.input.len()..] == prior.output;
        Comparison {
            prior_epoch: Some(prior.epoch),
            same_epoch: Some(prior.epoch == epoch),
            instructions_unchanged: Some(prior.instructions == facts.instructions),
            tools_unchanged: Some(prior.tools == facts.tools),
            settings_unchanged: Some(prior.settings == facts.settings),
            prior_input_items: Some(prior.input.len()),
            prior_output_items: Some(prior.output.len()),
            prior_output_bytes: Some(prior.output_bytes),
            expected_prefix_items: Some(expected_items),
            common_prefix_items: Some(common),
            prior_input_prefix_preserved: Some(input_preserved),
            native_output_replayed: Some(output_replayed),
            expected_prefix_preserved: Some(input_preserved && output_replayed),
            first_difference_item: if input.is_none() {
                Some(0)
            } else if common < expected_items {
                Some(common)
            } else {
                None
            },
        }
    }

    fn baseline(&self, epoch: u32, body: &Value, output: &[Value]) -> Option<Baseline> {
        let input = body.get("input")?.as_array()?;
        if input.len().checked_add(output.len())? > MAX_BASELINE_ITEMS {
            return None;
        }
        let mut output_bytes = 0_usize;
        let output = output
            .iter()
            .map(|item| {
                let (digest, bytes) = self.digest("native_item", item);
                output_bytes = output_bytes.saturating_add(bytes);
                digest
            })
            .collect();
        Some(Baseline {
            epoch,
            instructions: self
                .field_digest("instructions", body.get("instructions"))
                .0,
            tools: self.field_digest("tools", body.get("tools")).0,
            settings: self.settings_digest(body),
            input: input
                .iter()
                .map(|item| self.digest("native_item", item).0)
                .collect(),
            output,
            output_bytes,
        })
    }

    fn mac(&self, domain: &str) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.inner.key).expect("32-byte HMAC key");
        mac.update(b"JustAIAgent.cache_audit.v1\0");
        mac.update(&(domain.len() as u64).to_be_bytes());
        mac.update(domain.as_bytes());
        mac
    }

    fn session_digest(&self, session_id: &str) -> Digest {
        let mut mac = self.mac("session");
        mac.update(session_id.as_bytes());
        mac.finalize().into_bytes().into()
    }

    fn digest(&self, domain: &str, value: &Value) -> (Digest, usize) {
        let mut mac = self.mac(domain);
        let bytes = canonical_into(&mut mac, value);
        (mac.finalize().into_bytes().into(), bytes)
    }

    fn field_digest(&self, domain: &str, value: Option<&Value>) -> (Digest, Option<usize>) {
        let mut mac = self.mac(domain);
        mac.update(&[u8::from(value.is_some())]);
        let bytes = value.map(|value| canonical_into(&mut mac, value));
        (mac.finalize().into_bytes().into(), bytes)
    }

    fn settings_digest(&self, body: &Value) -> Digest {
        let mut mac = self.mac("settings");
        if let Some(object) = body.as_object() {
            mac.update(&[1]);
            let mut writer = DigestWriter {
                mac: &mut mac,
                bytes: 0,
            };
            let mut fields: Vec<_> = object
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "input" | "instructions" | "tools"))
                .collect();
            fields.sort_unstable_by_key(|(key, _)| *key);
            canonical_object(&mut writer, fields).expect("infallible HMAC writer");
        } else {
            mac.update(&[0]);
            canonical_into(&mut mac, body);
        }
        mac.finalize().into_bytes().into()
    }
}

impl State {
    fn session_mut(&mut self, key: Digest) -> &mut Session {
        if !self.sessions.contains_key(&key) {
            if self.sessions.len() == MAX_SESSIONS
                && let Some(oldest) = self.insertion_order.pop_front()
            {
                self.sessions.remove(&oldest);
            }
            self.insertion_order.push_back(key);
            self.sessions.insert(key, Session::default());
        }
        self.sessions.get_mut(&key).expect("registered session")
    }
}

struct DigestWriter<'a> {
    mac: &'a mut HmacSha256,
    bytes: usize,
}

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.mac.update(bytes);
        self.bytes = self.bytes.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn canonical_into(mac: &mut HmacSha256, value: &Value) -> usize {
    let mut writer = DigestWriter { mac, bytes: 0 };
    canonical(&mut writer, value).expect("infallible HMAC writer");
    writer.bytes
}

// Explicitly sort recursively even with serde_json preserve_order enabled.
// Arrays, native types, strings, nulls and unknown fields stay unchanged.
fn canonical(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    match value {
        Value::Object(object) => {
            let mut fields: Vec<_> = object.iter().collect();
            fields.sort_unstable_by_key(|(key, _)| *key);
            canonical_object(writer, fields)
        }
        Value::Array(values) => {
            writer.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    writer.write_all(b",")?;
                }
                canonical(writer, value)?;
            }
            writer.write_all(b"]")
        }
        _ => serde_json::to_writer(writer, value).map_err(io::Error::other),
    }
}

fn canonical_object(writer: &mut impl Write, fields: Vec<(&String, &Value)>) -> io::Result<()> {
    writer.write_all(b"{")?;
    for (index, (key, value)) in fields.into_iter().enumerate() {
        if index != 0 {
            writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut *writer, key).map_err(io::Error::other)?;
        writer.write_all(b":")?;
        canonical(writer, value)?;
    }
    writer.write_all(b"}")
}

fn hex(digest: &Digest) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in digest {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit() -> CacheAudit {
        CacheAudit::new([7; 32])
    }

    fn initial() -> Value {
        json!({
            "model": "private-model-name",
            "stream": true,
            "store": false,
            "reasoning": {"effort": "high"},
            "max_output_tokens": 1024,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "private-cache-key",
            "instructions": "private instructions",
            "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}],
            "input": [{"role": "user", "content": "original prompt"}],
        })
    }

    fn native_output() -> Vec<Value> {
        vec![
            json!({
                "id": "rs_private", "type": "reasoning", "status": "completed",
                "encrypted_content": "private-encrypted-base64-AQID==",
                "summary": [{"type": "summary_text", "text": "private reasoning summary"}],
                "unknown_native_field": {"nullable": null, "ordered": [2, 1]},
            }),
            json!({
                "id": "msg_private", "type": "message", "role": "assistant",
                "status": "completed", "phase": "commentary",
                "content": [{"type": "output_text", "text": "working", "annotations": [], "logprobs": []}],
            }),
            json!({
                "id": "fc_private", "type": "function_call", "status": "completed",
                "call_id": "call_private", "name": "shell",
                "arguments": "{  \"cmd\" : \"printf hello\", \"n\": 1 }",
            }),
        ]
    }

    fn replay(body: &Value, output: &[Value]) -> Value {
        let mut next = body.clone();
        let input = next["input"].as_array_mut().unwrap();
        input.extend_from_slice(output);
        input.push(
            json!({"type": "function_call_output", "call_id": "call_private", "output": "result"}),
        );
        input.push(json!({"role": "user", "content": "continue"}));
        next
    }

    fn commit(audit: &CacheAudit, session: &str, epoch: u32, body: &Value, output: &[Value]) {
        audit.request(session, epoch, "normal", body);
        audit.completed(session, body, output);
    }

    #[test]
    fn first_request_and_restart_have_null_comparisons() {
        let body = initial();
        for fresh in [audit(), audit()] {
            let result = fresh.request("s", 0, "normal", &body);
            assert_eq!(result["change_kind"], "first_request");
            assert_eq!(result["prior_complete"], false);
            assert_eq!(result["input_items"], 1);
            for field in [
                "prior_epoch",
                "same_epoch",
                "instructions_unchanged",
                "tools_unchanged",
                "settings_unchanged",
                "prior_input_items",
                "common_prefix_items",
                "prior_input_prefix_preserved",
                "native_output_replayed",
                "expected_prefix_preserved",
                "first_difference_item",
                "prior_output_items",
                "prior_output_bytes",
            ] {
                assert!(result[field].is_null(), "{field}");
            }
        }
    }

    #[test]
    fn complete_native_roundtrip_preserves_reasoning_phase_and_original_arguments() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let native_replay = replay(&body, &output);
        let next: Value =
            serde_json::from_slice(&serde_json::to_vec(&native_replay).unwrap()).unwrap();
        let result = audit.request("s", 0, "normal", &next);
        assert_eq!(result["change_kind"], "append_only");
        assert_ne!(
            result["request_fingerprint"],
            audit.request("unrelated", 0, "probe", &body)["request_fingerprint"]
        );
        assert_eq!(result["prior_complete"], true);
        assert_eq!(result["prior_input_items"], 1);
        assert_eq!(result["prior_output_items"], 3);
        assert_eq!(result["common_prefix_items"], 4);
        assert_eq!(result["expected_prefix_items"], 4);
        assert_eq!(result["prior_input_prefix_preserved"], true);
        assert_eq!(result["native_output_replayed"], true);
        assert_eq!(result["expected_prefix_preserved"], true);
        assert!(result["first_difference_item"].is_null());
        assert!(result["prior_output_bytes"].as_u64().unwrap() > 0);

        let next_output = vec![json!({
            "type": "message", "role": "assistant", "phase": "final_answer",
            "content": [{"type": "output_text", "text": "done"}],
        })];
        audit.completed("s", &next, &next_output);
        let result = audit.request("s", 0, "normal", &replay(&next, &next_output));
        assert_eq!(result["change_kind"], "append_only");
        assert_eq!(result["prior_input_items"], 6);
        assert_eq!(result["prior_output_items"], 1);
        assert_eq!(result["common_prefix_items"], 7);
    }

    #[test]
    fn old_input_string_mutation_is_history_change_even_with_exact_native_output() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let mut next = replay(&body, &output);
        next["input"][0]["content"] = json!("original prompt ");
        let result = audit.request("s", 0, "normal", &next);
        assert_eq!(result["change_kind"], "history_changed");
        assert_eq!(result["prior_input_prefix_preserved"], false);
        assert_eq!(result["native_output_replayed"], true);
        assert_eq!(result["first_difference_item"], 0);
    }

    #[test]
    fn omitted_reasoning_is_native_replay_change() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let next = replay(&body, &output[1..]);
        let result = audit.request("s", 0, "normal", &next);
        assert_eq!(result["change_kind"], "native_replay_changed");
        assert_eq!(result["prior_input_prefix_preserved"], true);
        assert_eq!(result["native_output_replayed"], false);
        assert_eq!(result["expected_prefix_preserved"], false);
        assert_eq!(result["common_prefix_items"], 1);
        assert_eq!(result["first_difference_item"], 1);
    }

    #[test]
    fn phase_null_and_absent_differ_in_both_directions() {
        let body = initial();
        for original_has_null in [true, false] {
            let audit = audit();
            let mut output = native_output();
            output[1].as_object_mut().unwrap().remove("phase");
            if original_has_null {
                output[1]["phase"] = Value::Null;
            }
            commit(&audit, "s", 0, &body, &output);
            let mut next = replay(&body, &output);
            if original_has_null {
                next["input"][2].as_object_mut().unwrap().remove("phase");
            } else {
                next["input"][2]["phase"] = Value::Null;
            }
            let result = audit.request("s", 0, "normal", &next);
            assert_eq!(result["change_kind"], "native_replay_changed");
            assert_eq!(result["first_difference_item"], 2);
        }
    }

    #[test]
    fn native_ciphertext_summary_arguments_and_unknown_fields_are_not_normalized() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let exact = replay(&body, &output);
        let mutations = [
            (
                "/input/1/encrypted_content",
                json!("different encrypted bytes"),
            ),
            (
                "/input/1/summary/0/text",
                json!("different reasoning summary"),
            ),
            ("/input/1/unknown_native_field/ordered", json!([1, 2])),
            ("/input/2/phase", json!("final_answer")),
            // Semantically equivalent arguments JSON still has different original bytes.
            (
                "/input/3/arguments",
                json!("{\"cmd\":\"printf hello\",\"n\":1}"),
            ),
        ];
        for (pointer, value) in mutations {
            let mut next = exact.clone();
            *next.pointer_mut(pointer).unwrap() = value;
            let result = audit.request("s", 0, "normal", &next);
            assert_eq!(result["change_kind"], "native_replay_changed");
            assert_eq!(result["prior_input_prefix_preserved"], true);
            assert_eq!(result["native_output_replayed"], false);
        }
    }

    #[test]
    fn tools_instructions_and_every_top_level_setting_have_separate_causes() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let exact = replay(&body, &output);
        for (field, value, expected) in [
            (
                "instructions",
                json!("other instructions"),
                "instructions_changed",
            ),
            ("tools", json!([]), "tools_changed"),
            ("model", json!("other model"), "settings_changed"),
            ("stream", json!(false), "settings_changed"),
            ("store", json!(true), "settings_changed"),
            ("reasoning", json!({"effort": "low"}), "settings_changed"),
            ("max_output_tokens", json!(1025), "settings_changed"),
            ("include", json!([]), "settings_changed"),
            ("prompt_cache_key", json!("other key"), "settings_changed"),
            ("future_unknown_setting", Value::Null, "settings_changed"),
        ] {
            let mut next = exact.clone();
            next[field] = value;
            let result = audit.request("s", 0, "normal", &next);
            assert_eq!(result["change_kind"], expected);
            assert_eq!(result["expected_prefix_preserved"], true);
        }
    }

    #[test]
    fn top_level_null_is_distinct_from_absent() {
        for (field, cause) in [
            ("instructions", "instructions_changed"),
            ("tools", "tools_changed"),
            ("unknown_setting", "settings_changed"),
        ] {
            for original_has_null in [true, false] {
                let audit = audit();
                let mut body = initial();
                body.as_object_mut().unwrap().remove(field);
                if original_has_null {
                    body[field] = Value::Null;
                }
                commit(&audit, "s", 0, &body, &[]);
                let mut next = body.clone();
                if original_has_null {
                    next.as_object_mut().unwrap().remove(field);
                } else {
                    next[field] = Value::Null;
                }
                let result = audit.request("s", 0, "normal", &next);
                assert_eq!(result["change_kind"], cause);
            }
        }
    }

    #[test]
    fn epoch_change_is_reported_even_if_prefix_is_preserved() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let result = audit.request("s", 1, "normal", &replay(&body, &output));
        assert_eq!(result["change_kind"], "epoch_changed");
        assert_eq!(result["prior_epoch"], 0);
        assert_eq!(result["same_epoch"], false);
        assert_eq!(result["expected_prefix_preserved"], true);
    }

    #[test]
    fn failed_attempts_do_not_replace_complete_baseline() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let mut failed = initial();
        failed["input"] = json!([{"role": "user", "content": "uncommitted failed attempt"}]);
        for epoch in 1..=40 {
            audit.request("s", epoch, "normal", &failed);
        }
        let result = audit.request("s", 0, "normal", &replay(&body, &output));
        assert_eq!(result["change_kind"], "append_only");
        assert_eq!(result["prior_epoch"], 0);
        assert_eq!(result["prior_input_items"], 1);
        assert_eq!(result["native_output_replayed"], true);
    }

    #[test]
    fn completed_uses_exact_attempt_body_instead_of_latest_failed_epoch() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        audit.request("s", 2, "normal", &body);
        let mut later_failed = body.clone();
        later_failed["instructions"] = json!("later failed attempt");
        audit.request("s", 9, "normal", &later_failed);
        audit.completed("s", &body, &output);
        let result = audit.request("s", 2, "normal", &replay(&body, &output));
        assert_eq!(result["prior_epoch"], 2);
        assert_eq!(result["change_kind"], "append_only");
    }

    #[test]
    fn compact_probe_and_unknown_purposes_neither_compare_nor_replace_normal() {
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "s", 0, &body, &output);
        let mut compact = initial();
        compact["input"] = json!([{"role": "user", "content": "private compact input"}]);
        for purpose in ["compact", "probe", "secret-invalid-purpose"] {
            let result = audit.request("s", 1, purpose, &compact);
            assert_eq!(result["prior_complete"], false);
            assert_eq!(result["normal_baseline_used"], false);
            assert!(result["prior_epoch"].is_null());
            assert!(result["expected_prefix_preserved"].is_null());
            if purpose == "secret-invalid-purpose" {
                assert!(result["purpose"].is_null());
            }
            audit.completed("s", &compact, &[]);
        }
        let next = replay(&body, &output);
        let result = audit.request("s", 0, "normal", &next);
        assert_eq!(result["change_kind"], "append_only");
        let changed = audit.request("s", 1, "normal", &next);
        assert_eq!(changed["change_kind"], "epoch_changed");
        assert_eq!(changed["prior_epoch"], 0);
    }

    #[test]
    fn excluded_and_unmatched_completions_do_not_create_sessions() {
        let audit = audit();
        let body = initial();
        for purpose in ["compact", "probe"] {
            audit.request("s", 0, purpose, &body);
            audit.completed("s", &body, &native_output());
        }
        audit.completed("unrequested", &body, &[]);
        assert!(audit.inner.state.lock().unwrap().sessions.is_empty());
        assert_eq!(
            audit.request("s", 0, "normal", &body)["change_kind"],
            "first_request"
        );
    }

    #[test]
    fn session_baselines_are_isolated_and_clones_share_thread_safe_state() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CacheAudit>();
        let audit = audit();
        let body = initial();
        let output = native_output();
        commit(&audit, "parent", 0, &body, &output);
        let clone = audit.clone();
        let child_body = body.clone();
        let thread = std::thread::spawn(move || {
            assert_eq!(
                clone.request("child", 4, "normal", &child_body)["change_kind"],
                "first_request"
            );
            clone.completed("child", &child_body, &[]);
        });
        thread.join().unwrap();
        let parent = audit.request("parent", 0, "normal", &replay(&body, &output));
        assert_eq!(parent["change_kind"], "append_only");
        assert_eq!(parent["prior_epoch"], 0);
        let child = audit.request("child", 4, "normal", &body);
        assert_eq!(child["change_kind"], "append_only");
        assert_eq!(child["prior_output_items"], 0);
        assert_eq!(child["prior_epoch"], 4);
    }

    #[test]
    fn metadata_excludes_values_session_ids_and_key_material() {
        let audit = CacheAudit::new([b'K'; 32]);
        let mut body = initial();
        body["input"][0]["content"] = json!([
            {"type": "input_text", "text": "low-entropy-yes"},
            {"type": "input_image", "image_url": "data:image/png;base64,PRIVATE_BASE64=="},
        ]);
        body["private_unknown_field"] = json!({"token": "PRIVATE_TOKEN"});
        let output = native_output();
        commit(&audit, "private-session-id", 0, &body, &output);
        let next = replay(&body, &output);
        let metadata = audit
            .request("private-session-id", 0, "normal", &next)
            .to_string();
        for secret in [
            "private-session-id",
            "private-model-name",
            "private-cache-key",
            "private instructions",
            "low-entropy-yes",
            "PRIVATE_BASE64",
            "data:image",
            "PRIVATE_TOKEN",
            "private_unknown_field",
            "private-encrypted-base64",
            "private reasoning summary",
            "printf hello",
            "call_private",
            "KKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKK",
        ] {
            assert!(!metadata.contains(secret));
        }
    }

    #[test]
    fn hmacs_are_stable_keyed_domain_separated_and_canonical() {
        let audit = audit();
        let other = CacheAudit::new([8; 32]);
        let one: Value = serde_json::from_str(r#"{"z":[{"b":2,"a":1}],"a":"text"}"#).unwrap();
        let reordered: Value = serde_json::from_str(r#"{"a":"text","z":[{"a":1,"b":2}]}"#).unwrap();
        let digest = audit.digest_value("test", &one);
        assert_eq!(digest, audit.digest_value("test", &reordered));
        assert_ne!(digest, other.digest_value("test", &one));
        assert_ne!(digest, audit.digest_value("other-domain", &one));
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(
            audit.digest_value("test", &json!([1, 2])),
            audit.digest_value("test", &json!([2, 1]))
        );
        assert_ne!(
            audit.digest_value("test", &json!(1)),
            audit.digest_value("test", &json!("1"))
        );
        assert_ne!(
            audit.digest_value("test", &json!("text")),
            audit.digest_value("test", &json!("text "))
        );
    }

    #[test]
    fn input_shape_and_truncated_prefix_are_not_mistaken_for_append_only() {
        let audit = audit();
        let body = initial();
        commit(&audit, "s", 0, &body, &native_output());
        for input in [Value::Null, json!("original prompt"), json!([])] {
            let mut next = body.clone();
            next["input"] = input;
            let result = audit.request("s", 0, "normal", &next);
            assert_eq!(result["change_kind"], "history_changed");
            assert_eq!(result["first_difference_item"], 0);
        }
        let result = audit.request("s", 0, "normal", &body);
        assert_eq!(result["change_kind"], "native_replay_changed");
        assert_eq!(result["first_difference_item"], 1);
    }

    #[test]
    fn map_and_attempts_are_bounded_with_deterministic_fifo_eviction() {
        let audit = audit();
        let body = initial();
        for id in 0..MAX_SESSIONS {
            commit(&audit, &format!("s{id}"), 0, &body, &[]);
        }
        for epoch in 0..100 {
            audit.request("s1", epoch, "normal", &body);
        }
        // Excluded requests cannot populate the session map or evict baselines.
        for id in 0..300 {
            audit.request(&format!("probe{id}"), 0, "probe", &body);
        }
        {
            let state = audit.inner.state.lock().unwrap();
            assert_eq!(state.sessions.len(), MAX_SESSIONS);
            assert_eq!(state.insertion_order.len(), MAX_SESSIONS);
            assert_eq!(
                state.sessions[&audit.session_digest("s1")].attempts.len(),
                MAX_PENDING_ATTEMPTS
            );
        }
        assert_eq!(
            audit.request("s1", 0, "normal", &body)["change_kind"],
            "append_only"
        );
        audit.request("overflow", 0, "normal", &body);
        {
            let state = audit.inner.state.lock().unwrap();
            assert_eq!(state.sessions.len(), MAX_SESSIONS);
            assert!(!state.sessions.contains_key(&audit.session_digest("s0")));
            assert!(state.sessions.contains_key(&audit.session_digest("s1")));
        }
        assert_eq!(
            audit.request("s0", 0, "normal", &body)["change_kind"],
            "first_request"
        );
    }

    #[test]
    fn oversized_success_drops_stale_baseline_without_retaining_raw_history() {
        let audit = audit();
        let body = initial();
        commit(&audit, "s", 0, &body, &native_output());
        let mut huge = initial();
        huge["input"] = Value::Array(vec![Value::Null; MAX_BASELINE_ITEMS + 1]);
        let result = audit.request("s", 0, "normal", &huge);
        assert_eq!(result["baseline_eligible"], false);
        audit.completed("s", &huge, &[]);
        let result = audit.request("s", 0, "normal", &body);
        assert_eq!(result["change_kind"], "first_request");
        assert!(!result["prior_complete"].as_bool().unwrap());

        // The cap applies to input + native output together.
        let too_many_outputs = vec![Value::Null; MAX_BASELINE_ITEMS];
        audit.completed("s", &body, &too_many_outputs);
        assert!(
            audit.inner.state.lock().unwrap().sessions[&audit.session_digest("s")]
                .complete
                .is_none()
        );
    }
}
