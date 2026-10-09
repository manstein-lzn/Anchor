//! Structured metrics from Goose's custom `_goose/unstable/session/update`
//! notifications.
//!
//! Goose only emits these when the client declared
//! `clientCapabilities._meta.goose.customNotifications`, which this Host does.
//! They carry the per-message token/cost/timing accounting, the accumulated
//! totals, the context-window meter and the only visible compaction signal.
//!
//! The transport keeps just a bounded tail of raw notifications
//! (`MAX_NOTIFICATIONS` / `MAX_NOTIFICATION_BYTES`), so counting events after
//! the prompt returns undercounts long invocations. The tally observes every
//! notification as it arrives and keeps aggregates plus a bounded sample of
//! per-message records. Nothing here changes scheduling or completion facts: it
//! only records what the agent reported.

use serde_json::{Value, json};
use std::sync::Mutex;

/// Per-message samples kept for evidence; aggregates are unbounded counts.
const MAX_SAMPLES: usize = 64;
/// Status/notice text is truncated so evidence cannot grow without bound.
const MAX_STATUS_CHARS: usize = 200;

#[derive(Default)]
pub(crate) struct UsageTally {
    inner: Mutex<Usage>,
}

#[derive(Default)]
struct Usage {
    messages: u64,
    compaction_messages: u64,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_micros: u64,
    cost_reported: bool,
    elapsed_ms: u64,
    max_time_to_first_token_ms: u64,
    time_to_first_token_ms_reported: bool,
    context_used: Option<u64>,
    context_limit: Option<u64>,
    accumulated_input_tokens: Option<u64>,
    accumulated_output_tokens: Option<u64>,
    accumulated_cost_micros: Option<u64>,
    notices: u64,
    progress: u64,
    last_status: Option<String>,
    samples: Vec<Value>,
}

impl UsageTally {
    /// Record one server notification. Unknown methods and unknown variants are
    /// ignored so a newer Goose release cannot break an invocation.
    pub(crate) fn observe(&self, event: &Value) {
        if event["method"] != "_goose/unstable/session/update" {
            return;
        }
        let params = &event["params"];
        // Goose has shipped both `params.sessionUpdate` and
        // `params.update.sessionUpdate`; accept either shape.
        let (variant, body) = match params["update"]["sessionUpdate"].as_str() {
            Some(variant) => (variant, &params["update"]),
            None => match params["sessionUpdate"].as_str() {
                Some(variant) => (variant, params),
                None => return,
            },
        };
        let Ok(mut usage) = self.inner.lock() else {
            return;
        };
        match variant {
            "message_usage" => usage.message_usage(body),
            "usage_update" => usage.usage_update(body),
            "status_message" => usage.status_message(body),
            _ => {}
        }
    }

    /// Aggregated metrics for node evidence, plus the bounded per-message sample.
    pub(crate) fn metrics(&self) -> Value {
        let Ok(usage) = self.inner.lock() else {
            return json!({"unavailable": "usage tally lock poisoned"});
        };
        let mut metrics = json!({
            "messages": usage.messages,
            "compaction_messages": usage.compaction_messages,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
            "cache_read_tokens": usage.cache_read_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
            "elapsed_ms": usage.elapsed_ms,
            "max_time_to_first_token_ms": usage.max_time_to_first_token_ms,
            "notices": usage.notices,
            "progress": usage.progress,
            "samples": usage.samples,
        });
        if usage.cost_reported {
            metrics["cost_usd"] = json!(micros_to_usd(usage.cost_micros));
        }
        if usage.time_to_first_token_ms_reported {
            metrics["time_to_first_token_ms_reported"] = json!(true);
        }
        if let Some(used) = usage.context_used {
            metrics["context_used"] = json!(used);
        }
        if let Some(limit) = usage.context_limit {
            metrics["context_limit"] = json!(limit);
        }
        if let Some(tokens) = usage.accumulated_input_tokens {
            metrics["accumulated_input_tokens"] = json!(tokens);
        }
        if let Some(tokens) = usage.accumulated_output_tokens {
            metrics["accumulated_output_tokens"] = json!(tokens);
        }
        if let Some(cost) = usage.accumulated_cost_micros {
            metrics["accumulated_cost_usd"] = json!(micros_to_usd(cost));
        }
        if let Some(status) = &usage.last_status {
            metrics["last_status"] = json!(status);
        }
        metrics
    }
}

impl Usage {
    fn message_usage(&mut self, body: &Value) {
        // Older shapes put the accounting at the top level; current ones nest it
        // under `usage`. Read both.
        let usage = if body["usage"].is_object() {
            &body["usage"]
        } else {
            body
        };
        let is_compaction = usage["isCompaction"].as_bool().unwrap_or(false)
            || body["isCompaction"].as_bool().unwrap_or(false);
        self.messages += 1;
        if is_compaction {
            self.compaction_messages += 1;
        }
        let input = number(usage, "inputTokens");
        let output = number(usage, "outputTokens");
        let total = number(usage, "totalTokens");
        self.input_tokens += input;
        self.output_tokens += output;
        self.total_tokens += total;
        self.cache_read_tokens += number(usage, "cacheReadTokens");
        self.cache_write_tokens += number(usage, "cacheWriteTokens");
        if let Some(cost) = usage["cost"].as_f64() {
            self.cost_micros += usd_to_micros(cost);
            self.cost_reported = true;
        }
        self.elapsed_ms += number(usage, "elapsedMs");
        if let Some(ttft) = usage["timeToFirstTokenMs"].as_u64() {
            self.max_time_to_first_token_ms = self.max_time_to_first_token_ms.max(ttft);
            self.time_to_first_token_ms_reported = true;
        }
        if self.samples.len() < MAX_SAMPLES {
            let mut sample = json!({
                "input_tokens": input,
                "output_tokens": output,
                "total_tokens": total,
                "is_compaction": is_compaction,
            });
            if let Some(id) = body["messageId"].as_str() {
                sample["message_id"] = json!(id);
            }
            if let Some(cost) = usage["cost"].as_f64() {
                sample["cost_usd"] = json!(cost);
            }
            if let Some(source) = usage["costSource"].as_str() {
                sample["cost_source"] = json!(source);
            }
            if let Some(elapsed) = usage["elapsedMs"].as_u64() {
                sample["elapsed_ms"] = json!(elapsed);
            }
            if let Some(ttft) = usage["timeToFirstTokenMs"].as_u64() {
                sample["time_to_first_token_ms"] = json!(ttft);
            }
            self.samples.push(sample);
        }
    }

    fn usage_update(&mut self, body: &Value) {
        if let Some(used) = number_opt(body, "used") {
            self.context_used = Some(used);
        }
        if let Some(limit) = number_opt(body, "size").or_else(|| number_opt(body, "contextLimit")) {
            self.context_limit = Some(limit);
        }
        if let Some(tokens) = number_opt(body, "accumulatedInputTokens") {
            self.accumulated_input_tokens = Some(tokens);
        }
        if let Some(tokens) = number_opt(body, "accumulatedOutputTokens") {
            self.accumulated_output_tokens = Some(tokens);
        }
        if let Some(cost) = body["accumulatedCost"].as_f64() {
            self.accumulated_cost_micros = Some(usd_to_micros(cost));
        }
    }

    fn status_message(&mut self, body: &Value) {
        let status = &body["status"];
        match status["type"].as_str() {
            Some("notice") => self.notices += 1,
            Some("progress") => self.progress += 1,
            _ => {}
        }
        if let Some(message) = status["message"].as_str() {
            self.last_status = Some(truncate(message));
        }
    }
}

fn number(value: &Value, key: &str) -> u64 {
    number_opt(value, key).unwrap_or(0)
}

fn number_opt(value: &Value, key: &str) -> Option<u64> {
    value[key].as_u64()
}

fn usd_to_micros(cost: f64) -> u64 {
    (cost.max(0.0) * 1_000_000.0).round() as u64
}

fn micros_to_usd(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

fn truncate(text: &str) -> String {
    let mut out = String::new();
    for character in text.chars().take(MAX_STATUS_CHARS) {
        out.push(character);
    }
    if text.chars().count() > MAX_STATUS_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(variant: &str, body: Value) -> Value {
        let mut update = body;
        update["sessionUpdate"] = json!(variant);
        json!({"method":"_goose/unstable/session/update",
            "params":{"sessionId":"s","update":update}})
    }

    #[test]
    fn aggregates_message_usage_tokens_cost_and_timing() {
        let tally = UsageTally::default();
        tally.observe(&custom(
            "message_usage",
            json!({"messageId":"m1","usage":{"inputTokens":100,"outputTokens":20,
                "totalTokens":120,"cacheReadTokens":30,"cacheWriteTokens":5,
                "cost":0.0015,"costSource":"provider","elapsedMs":900,
                "timeToFirstTokenMs":250,"isCompaction":false}}),
        ));
        tally.observe(&custom(
            "message_usage",
            json!({"messageId":"m2","usage":{"inputTokens":50,"outputTokens":10,
                "totalTokens":60,"elapsedMs":300,"timeToFirstTokenMs":400,
                "isCompaction":true}}),
        ));
        let metrics = tally.metrics();
        assert_eq!(metrics["messages"], json!(2));
        assert_eq!(metrics["compaction_messages"], json!(1));
        assert_eq!(metrics["input_tokens"], json!(150));
        assert_eq!(metrics["output_tokens"], json!(30));
        assert_eq!(metrics["total_tokens"], json!(180));
        assert_eq!(metrics["cache_read_tokens"], json!(30));
        assert_eq!(metrics["cache_write_tokens"], json!(5));
        assert_eq!(metrics["elapsed_ms"], json!(1200));
        assert_eq!(metrics["max_time_to_first_token_ms"], json!(400));
        assert_eq!(metrics["cost_usd"], json!(0.0015));
        assert_eq!(metrics["samples"].as_array().unwrap().len(), 2);
        assert_eq!(metrics["samples"][1]["is_compaction"], json!(true));
    }

    #[test]
    fn accepts_the_flat_shape_and_reads_the_context_meter() {
        let tally = UsageTally::default();
        tally.observe(&json!({"method":"_goose/unstable/session/update",
            "params":{"sessionUpdate":"message_usage","messageId":"m1",
                "inputTokens":10,"outputTokens":2,"totalTokens":12}}));
        tally.observe(&json!({"method":"_goose/unstable/session/update",
            "params":{"sessionUpdate":"usage_update","used":4096,"size":128000,
                "accumulatedInputTokens":900,"accumulatedOutputTokens":120,
                "accumulatedCost":0.25}}));
        let metrics = tally.metrics();
        assert_eq!(metrics["messages"], json!(1));
        assert_eq!(metrics["total_tokens"], json!(12));
        assert_eq!(metrics["context_used"], json!(4096));
        assert_eq!(metrics["context_limit"], json!(128000));
        assert_eq!(metrics["accumulated_input_tokens"], json!(900));
        assert_eq!(metrics["accumulated_output_tokens"], json!(120));
        assert_eq!(metrics["accumulated_cost_usd"], json!(0.25));
    }

    #[test]
    fn records_compaction_status_messages_with_bounded_text() {
        let tally = UsageTally::default();
        tally.observe(&custom(
            "status_message",
            json!({"status":{"type":"progress","message":"goose is compacting the conversation…"}}),
        ));
        tally.observe(&custom(
            "status_message",
            json!({"status":{"type":"notice","message":"x".repeat(4096)}}),
        ));
        let metrics = tally.metrics();
        assert_eq!(metrics["progress"], json!(1));
        assert_eq!(metrics["notices"], json!(1));
        let status = metrics["last_status"].as_str().unwrap();
        assert!(status.chars().count() <= MAX_STATUS_CHARS + 1, "{status}");
    }

    #[test]
    fn ignores_unrelated_methods_and_variants() {
        let tally = UsageTally::default();
        tally.observe(&json!({"method":"session/update",
            "params":{"update":{"sessionUpdate":"message_usage","usage":{"totalTokens":99}}}}));
        tally.observe(&custom("unknown_variant", json!({"whatever":1})));
        assert_eq!(tally.metrics()["messages"], json!(0));
        assert_eq!(tally.metrics()["total_tokens"], json!(0));
    }
}
