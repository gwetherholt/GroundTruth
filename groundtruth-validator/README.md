# groundtruth-validator — A streaming data validation framework in Rust

`groundtruth-validator` is a push-based validation framework for any
stream of timestamped numeric data: IoT sensors, server metrics,
financial ticks, A/B test outputs. It does three things, layered in
escalating order: classifies each reading as `Good`, `Suspect`, or
`Invalid` against rule-based checks (Tier 1); scores each source's
health 0–100 over time on five weighted signals (Tier 2); and runs a
hysteresis state machine that quarantines unhealthy sources and
recovers them once they're well again (Tier 3). It was extracted from
[GroundTruth](../README.md), a live soil-sensor IoT system that runs
continuously on a Raspberry Pi, to prove the validation logic
generalizes beyond the soil-sensor use case it was originally built
for.

## Quick start

```rust
use groundtruth_validator::{
    MetricConfig, Reading, StreamValidator, ValidatorConfig,
};
use chrono::Utc;

let config = ValidatorConfig::builder()
    .metric(
        "moisture",
        MetricConfig::new(0.0..=100.0)
            .with_raw_range(100..=3995)
            .with_max_rate_of_change(30.0)
            .with_stuck_count(6),
    )
    .build();

let mut validator = StreamValidator::new(config);

let now = Utc::now();
for (i, value) in [42.0, 42.5, 43.1, 999.0, 43.2].into_iter().enumerate() {
    let r = Reading::new("bed/1", "moisture", value, now + chrono::Duration::seconds(30 * i as i64));
    let result = validator.validate(r);
    println!("{} -> {} ({})", value, result.quality, result.reason);
}

// Periodically: refresh health scores and step the quarantine state machine.
let scores = validator.update_health();
let transitions = validator.update_quarantine();
```

Run that and you'll see `999.0` flagged as `Invalid` with reason
`moisture = 999.00 outside plausible range [0, 100]`; the rest stay
`Good`.

## Architecture

The framework is three independent tiers stacked on shared state. You
can run any subset; most users want all three.

### Tier 1 — Per-reading validation

Every reading runs through four rules in priority order:

| Rule           | Trigger                                                     | Severity |
|----------------|-------------------------------------------------------------|----------|
| value range    | Value outside the configured `valid_range`, or NaN          | Invalid  |
| raw range      | Raw transducer reading outside `raw_valid_range`            | Invalid  |
| stuck reading  | `stuck_count` consecutive readings within `stuck_threshold` | Suspect  |
| rate of change | Absolute delta > `max_rate_of_change` within `rate_window`  | Suspect  |

Each result carries a [`QualityLevel`], the rule that fired, and a
human-readable reason. Invalid readings are still emitted and recorded
in the validator's internal history — the philosophy is "classify, not
discard" so broken sources remain diagnosable.

### Tier 2 — Health scoring

While Tier 1 judges readings in isolation, Tier 2 judges *streams* over
time. Each `(source, metric)` gets a 0–100 composite score from five
weighted signals:

| Signal             | Weight | What it catches                                 |
|--------------------|--------|-------------------------------------------------|
| Quality rate       | 35%    | High proportion of Suspect/Invalid in window    |
| Reporting cadence  | 25%    | Source publishing slower than expected          |
| Drift detection    | 20%    | Recent mean moved > 3σ from long-term baseline  |
| Variance ratio     | 10%    | Recent stddev outside 0.5×–2× baseline stddev   |
| Recency            | 10%    | Time since last Good reading approaches horizon |

Baselines are built from **Good readings only**, so a source's
degradation doesn't infect the baseline used to detect that same
degradation. The window length is configurable (default 7 days).

### Tier 3 — Quarantine

A small state machine with hysteresis:

```
                    score < bad (N consecutive)
       ┌─────────────────────────────────────┐
       │                                     ▼
   ┌────────┐                          ┌──────────────┐
   │ Active │                          │ Quarantined  │
   └────────┘                          └──────────────┘
       ▲                                     │
       │                                     │
       └─────────────────────────────────────┘
                  score >= recovery (N consecutive)
```

Defaults: enter when 3 consecutive checks score below 40, recover when
3 consecutive checks score at or above 70. The gap between thresholds
prevents borderline sources from flapping in and out.

## Design principles

- **Push-based.** Feed readings in via `validate(reading)`. The
  validator pulls nothing from the outside.
- **Storage-agnostic.** The crate never touches a database, file, or
  network. Internal state lives in memory; persistence is a caller
  concern.
- **Framework-agnostic.** No opinion on metrics export, logging
  framework, async runtime, or transport.
- **Severity precedence.** When multiple rules fire on one reading,
  the highest severity wins: `Invalid > Suspect > Good`. The
  `QualityLevel` enum derives `Ord` so this is `max` over rule
  outcomes.
- **Classify, don't destroy.** Suspect and Invalid readings are still
  visible in the output stream and counted toward health. Downstream
  callers decide what to do with them.

## Configuration

Build a `ValidatorConfig` via the fluent builder:

```rust
use chrono::Duration;
use groundtruth_validator::{MetricConfig, ValidatorConfig};

let config = ValidatorConfig::builder()
    .metric(
        "moisture",
        MetricConfig::new(0.0..=100.0)
            .with_raw_range(100..=3995)
            .with_max_rate_of_change(30.0)
            .with_rate_window(Duration::seconds(600))
            .with_stuck_threshold(0.01)
            .with_stuck_count(6)
            .with_expected_cadence(Duration::seconds(60)),
    )
    .baseline_window(Duration::days(7))
    .health_check_interval(Duration::seconds(30))
    .quarantine_bad_threshold(40.0)
    .quarantine_recovery_threshold(70.0)
    .quarantine_consecutive_required(3)
    .build();
```

### `MetricConfig` fields

| Field                | Default          | Meaning                                           |
|----------------------|------------------|---------------------------------------------------|
| `valid_range`        | (required)       | Inclusive plausible range; outside → Invalid      |
| `raw_valid_range`    | `None`           | Inclusive plausible raw range; outside → Invalid  |
| `max_rate_of_change` | `f64::INFINITY`  | Max absolute delta between consecutive readings   |
| `rate_window`        | 600s             | Window in which rate-of-change applies            |
| `stuck_threshold`    | 0.01             | Per-reading match tolerance for stuck detection   |
| `stuck_count`        | 6                | Consecutive matches that trigger Suspect          |
| `expected_cadence`   | 60s              | Used by the Tier-2 cadence signal                 |

### `ValidatorConfig` global fields

| Field                              | Default | Meaning                                          |
|------------------------------------|---------|--------------------------------------------------|
| `baseline_window`                  | 7 days  | Tier-2 baseline horizon                          |
| `health_check_interval`            | 30s     | Advisory; informs the quarantine reason string   |
| `quarantine_bad_threshold`         | 40.0    | Score below this counts toward quarantine        |
| `quarantine_recovery_threshold`    | 70.0    | Score at-or-above this counts toward recovery    |
| `quarantine_consecutive_required`  | 3       | Consecutive checks required to transition        |

## API reference

The high-level entry point is `StreamValidator`:

| Method                                    | Purpose                                                    |
|-------------------------------------------|------------------------------------------------------------|
| `new(config)`                             | Construct from a `ValidatorConfig`.                        |
| `validate(reading)`                       | Run Tier-1; update history; return a `ValidationResult`.   |
| `update_health()`                         | Recompute Tier-2 scores anchored to `Utc::now()`.          |
| `update_health_at(now)`                   | Same, with a caller-provided anchor (useful for offline).  |
| `update_quarantine()`                     | Step Tier-3 from latest scores, anchored to `Utc::now()`.  |
| `update_quarantine_at(now)`               | Same, with a caller-provided anchor.                       |
| `health_score(source, metric)`            | Last computed `HealthScore`, if any.                       |
| `quarantine_status(source, metric)`       | Reference to the current `QuarantineState`.                |
| `is_quarantined(source, metric)`          | Convenience boolean.                                       |
| `sources()`                               | Sorted `(source, metric)` keys the validator has seen.     |
| `health_scores()`                         | Iterator over `((source, metric), &HealthScore)`.          |
| `quarantine_states()`                     | Iterator over `((source, metric), &QuarantineState)`.      |

Underlying rule functions (`tier1::run_tier1`, `tier1::check_stuck`,
`tier2::compute_health_score`, `quarantine::update_quarantine`) are
public, so callers who want their own state ownership model can skip
`StreamValidator` and compose the pieces directly.

## Testing

47 unit and doc tests cover all four Tier-1 rules (including edge
cases like NaN, short-circuit on Invalid, insufficient history),
Tier-2 health scoring (five signals, baseline insufficiency fallback,
multi-source isolation), the quarantine state machine (hysteresis,
counter resets on transitions, custom thresholds), config builder
shape, and `StreamValidator` integration paths including unknown
metric pass-through.

```bash
cargo test -p groundtruth-validator
```

## License

MIT. See [LICENSE](../LICENSE).
