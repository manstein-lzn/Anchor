//! One accounting fold for full observed usage; missing coverage is never zero.
use crate::ModelUsage;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Totals {
    pub model_requests: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub elapsed_ms: Option<u64>,
}

impl Default for Totals {
    fn default() -> Self {
        Self {
            model_requests: 0,
            input_tokens: Some(0),
            output_tokens: Some(0),
            cost_usd: Some(0.0),
            elapsed_ms: Some(0),
        }
    }
}

impl Totals {
    pub fn observe(&mut self, usage: &ModelUsage) -> Result<(), String> {
        self.merge(&Self {
            model_requests: usage.messages,
            input_tokens: usage
                .input_tokens
                .filter(|_| usage.complete && usage.reported_turns.input_tokens == usage.messages),
            output_tokens: usage
                .output_tokens
                .filter(|_| usage.complete && usage.reported_turns.output_tokens == usage.messages),
            cost_usd: usage
                .cost_usd
                .filter(|_| usage.complete && usage.reported_turns.cost_usd == usage.messages),
            elapsed_ms: usage
                .elapsed_ms
                .filter(|_| usage.complete && usage.reported_turns.elapsed_ms == usage.messages),
        })
    }

    pub fn merge(&mut self, other: &Self) -> Result<(), String> {
        self.model_requests = self
            .model_requests
            .checked_add(other.model_requests)
            .ok_or("usage message count overflow")?;
        let add = |left: Option<u64>, right: Option<u64>| {
            left.zip(right)
                .and_then(|(left, right)| left.checked_add(right))
        };
        self.input_tokens = add(self.input_tokens, other.input_tokens);
        self.output_tokens = add(self.output_tokens, other.output_tokens);
        self.elapsed_ms = add(self.elapsed_ms, other.elapsed_ms);
        self.cost_usd = self
            .cost_usd
            .zip(other.cost_usd)
            .map(|(left, right)| left + right)
            .filter(|value| value.is_finite());
        Ok(())
    }

    pub fn require_requests(&mut self, expected: u64) {
        if self.model_requests != expected {
            self.input_tokens = None;
            self.output_tokens = None;
            self.cost_usd = None;
            self.elapsed_ms = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_and_request_coverage_remain_unknown_after_merging() {
        let mut total = Totals::default();
        total
            .observe(&ModelUsage {
                complete: true,
                messages: 2,
                input_tokens: Some(30),
                output_tokens: Some(10),
                cost_usd: None,
                elapsed_ms: Some(4),
                reported_turns: crate::UsageReportedTurns {
                    input_tokens: 2,
                    output_tokens: 1,
                    elapsed_ms: 2,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total.model_requests, 2);
        assert_eq!(total.input_tokens, Some(30));
        assert_eq!(total.output_tokens, None);
        assert_eq!(total.cost_usd, None);
        assert_eq!(total.elapsed_ms, Some(4));
        total.merge(&Totals::default()).unwrap();
        total.require_requests(3);
        assert_eq!(total.model_requests, 2);
        assert_eq!(total.input_tokens, None);
        assert_eq!(total.elapsed_ms, None);
    }
}
