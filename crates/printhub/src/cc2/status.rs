//! Printer status cache. The CC2 answers `GET_STATUS` with a full frame, then pushes deltas
//! carrying only the fields that changed.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::model::StatusView;

/// Merges `delta` into `base` in place. Objects merge key by key; anything else, arrays
/// included, replaces the previous value.
pub fn deep_merge(base: &mut Value, delta: Value) {
    match (base, delta) {
        (Value::Object(base), Value::Object(delta)) => {
            for (key, value) in delta {
                match base.get_mut(&key) {
                    Some(existing) => deep_merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, delta) => *base = delta,
    }
}

/// Consecutive out-of-order deltas tolerated before the cache is presumed to have drifted.
pub const MAX_CONSECUTIVE_GAPS: u32 = 5;

#[derive(Debug, Default)]
pub struct SequenceTracker {
    last: Option<u64>,
    consecutive_gaps: u32,
}

impl SequenceTracker {
    pub fn reset(&mut self, sequence: Option<u64>) {
        self.last = sequence;
        self.consecutive_gaps = 0;
    }

    /// Returns `true` when the caller should re-request a full status.
    pub fn observe(&mut self, sequence: u64) -> bool {
        let continuous = self.last.is_none_or(|last| sequence == last + 1);
        self.last = Some(sequence);
        if continuous {
            self.consecutive_gaps = 0;
            return false;
        }
        self.consecutive_gaps += 1;
        if self.consecutive_gaps >= MAX_CONSECUTIVE_GAPS {
            self.consecutive_gaps = 0;
            return true;
        }
        false
    }
}

/// The frame number of a status message. The Home Assistant client, which runs against real
/// printers, tracks `result.sequence`; the protocol doc describes the envelope `id` instead.
pub fn sequence_of(envelope_id: Option<u64>, result: &Value) -> Option<u64> {
    result
        .get("sequence")
        .and_then(Value::as_u64)
        .or(envelope_id)
}

#[derive(Debug)]
pub struct StatusCache {
    raw: Value,
    sequence: SequenceTracker,
    has_full_frame: bool,
}

impl Default for StatusCache {
    fn default() -> Self {
        Self {
            raw: Value::Object(Map::new()),
            sequence: SequenceTracker::default(),
            has_full_frame: false,
        }
    }
}

impl StatusCache {
    pub fn apply_full(&mut self, envelope_id: Option<u64>, result: Value) {
        self.sequence.reset(sequence_of(envelope_id, &result));
        self.raw = if result.is_object() {
            result
        } else {
            Value::Object(Map::new())
        };
        self.has_full_frame = true;
    }

    /// Returns `true` when the caller should re-request a full status: either no full frame
    /// has arrived yet, or deltas have been going missing.
    pub fn apply_delta(&mut self, envelope_id: Option<u64>, result: Value) -> bool {
        let gap = sequence_of(envelope_id, &result).is_some_and(|seq| self.sequence.observe(seq));
        deep_merge(&mut self.raw, result);
        gap || !self.has_full_frame
    }

    pub fn has_full_frame(&self) -> bool {
        self.has_full_frame
    }

    pub fn raw(&self) -> &Value {
        &self.raw
    }

    pub fn view(&self) -> Result<StatusView, serde_json::Error> {
        StatusView::deserialize(&self.raw)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merge_nested_objects_and_replace_leaves() {
        let mut base = json!({
            "machine_status": {"status": 2, "sub_status": 2075, "progress": 45},
            "extruder": {"temperature": 215.0, "target": 220},
            "machine_exceptions": [109, 1026]
        });
        deep_merge(
            &mut base,
            json!({
                "machine_status": {"progress": 46},
                "extruder": {"temperature": 219.5},
                "machine_exceptions": [],
                "new_block": {"x": 1}
            }),
        );
        assert_eq!(
            base,
            json!({
                "machine_status": {"status": 2, "sub_status": 2075, "progress": 46},
                "extruder": {"temperature": 219.5, "target": 220},
                "machine_exceptions": [],
                "new_block": {"x": 1}
            })
        );
    }

    #[test]
    fn merge_object_over_scalar_replaces() {
        let mut base = json!({"a": 1});
        deep_merge(&mut base, json!({"a": {"b": 2}}));
        assert_eq!(base, json!({"a": {"b": 2}}));
    }

    #[test]
    fn sequence_gaps_trigger_refresh_only_after_a_run() {
        let mut tracker = SequenceTracker::default();
        assert!(!tracker.observe(10));
        assert!(!tracker.observe(11));
        for seq in [13, 15, 17, 19] {
            assert!(!tracker.observe(seq), "gap at {seq} is not yet a run");
        }
        assert!(tracker.observe(21));
        assert!(
            !tracker.observe(23),
            "counter restarts after requesting a refresh"
        );
    }

    #[test]
    fn continuous_delta_resets_gap_count() {
        let mut tracker = SequenceTracker::default();
        tracker.observe(1);
        for seq in [3, 5, 7, 9] {
            tracker.observe(seq);
        }
        assert!(!tracker.observe(10));
        for seq in [12, 14, 16, 18] {
            assert!(!tracker.observe(seq));
        }
        assert!(tracker.observe(20));
    }

    #[test]
    fn sequence_prefers_result_field() {
        assert_eq!(sequence_of(Some(4), &json!({"sequence": 9})), Some(9));
        assert_eq!(sequence_of(Some(4), &json!({})), Some(4));
        assert_eq!(sequence_of(None, &json!({})), None);
    }

    #[test]
    fn cache_requests_full_frame_until_one_arrives() {
        let mut cache = StatusCache::default();
        assert!(cache.apply_delta(
            None,
            json!({"sequence": 1, "machine_status": {"progress": 3}})
        ));
        cache.apply_full(
            None,
            json!({"sequence": 5, "machine_status": {"status": 1}}),
        );
        assert!(!cache.apply_delta(
            None,
            json!({"sequence": 6, "machine_status": {"progress": 7}})
        ));
        let view = cache.view().unwrap();
        assert_eq!(view.machine_status.status, 1);
        assert_eq!(view.machine_status.progress, 7);
    }
}
