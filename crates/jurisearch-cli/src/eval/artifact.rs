//! Shared eval metric/artifact helpers (mean, floor_metric).

pub(crate) fn mean(hits: usize, total: usize) -> f64 {
    if total > 0 {
        hits as f64 / total as f64
    } else {
        0.0
    }
}

/// Truncate (floor) a gate metric to 3 decimals for the artifact. Flooring, not rounding, so the
/// RECORDED metric can never exceed the raw value: the status gate re-derives pass from the recorded
/// 3-decimal `metric_value` against a 3-decimal floor, and `floor(raw*1000) >= floor*1000` holds iff
/// `raw >= floor`, so the recorded value passes exactly when the runner's raw decision passes (a
/// below-floor raw metric can never round up into a passing recorded value).
pub(crate) fn floor_metric(value: f64) -> f64 {
    (value * 1000.0).floor() / 1000.0
}
