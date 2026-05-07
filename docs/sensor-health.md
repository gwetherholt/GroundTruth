# Sensor Health (Tier-2 validation)

This module monitors sensor STREAMS over time, complementing the
per-reading checks in `src/validation.rs`. Where Tier-1 asks "is THIS
reading plausible?", Tier-2 asks "is THIS SENSOR trustworthy right now?"

## Why this exists

Capacitive soil moisture sensors are well-known to fail in subtle ways
that escape per-reading validation. The hobbyist and research community
has documented several common failure modes:

- **Coating breach over months** — moisture seeps into the PCB, causing
  gradual upward drift in readings even in unchanged soil. Per-reading
  validation can't catch this because each reading individually looks
  plausible.
- **Production variance between sensors from the same batch** — two
  sensors of the same model can have different output ranges, requiring
  per-sensor calibration.
- **Component substitution** — some clones use NE555 instead of TLC555,
  producing different behavior at 3.3V.
- **Slow degradation** — sensors that progressively become noisier or
  more sluggish before failing outright.

The five signals in this module are designed to catch each of these
modes in the data after the fact, even when individual readings remain
nominally valid.

### Sources of inspiration

- [The Cave Pearl Project](https://thecavepearlproject.org/) — long-running
  embedded systems blog with rigorous capacitive soil sensor work,
  particularly around degradation over months.
- Daniel Robertson, "[Protecting Capacitive Soil Moisture Sensors](https://medium.com/@d.robertson/protecting-capacitive-soil-moisture-sensors-e1ab81f1c4fb)"
  (2021) — documents the gradual drift failure where readings climb
  over months as the PCB coating absorbs moisture.
- [Soil moisture sensors: problems & solutions](https://arduinodiy.wordpress.com/2020/08/24/soil-moisture-sensors/)
  (arduinodiy.wordpress.com) — community guide covering production
  variance, NE555 vs TLC555 issues, and PCB rotation failure modes.

## How the score is computed

Each sensor (uniquely identified by `zone × zone_id × metric`) gets a
0-100 health score updated every 30 seconds. The score combines five
weighted signals:

| Signal | Weight | What it catches |
|--------|--------|-----------------|
| **Quality rate** | 35% | High `suspect` or `invalid` rates from Tier-1 validation |
| **Reporting cadence** | 25% | Sensor stops reporting or reports far less frequently than expected |
| **Drift detection** | 20% | Recent mean has moved more than 3σ from the long-term baseline |
| **Variance ratio** | 10% | Recent stddev is more than 2x or less than 0.5x the baseline stddev |
| **Recency** | 10% | Time since last `good` reading approaches 10 minutes |

### Status tiers

| Score | Status | Visual |
|-------|--------|--------|
| 90-100 | Healthy | Green badge, calm |
| 70-89 | Degraded | Yellow badge, watch list |
| 40-69 | Suspect | Orange badge, "review me" |
| 0-39 | Bad | Red panel, large text, "primary concern: …" |

### Baseline construction

Drift and variance signals require a "what does this sensor normally
look like?" baseline. The baseline window is the **last 7 days of
readings flagged GOOD by Tier-1 validation**.

This is deliberate: excluding bad readings from the baseline prevents
sensor degradation from infecting the model used to detect that same
degradation. A sensor that's been broken for a week still has a clean
baseline from the week before.

A sensor with fewer than 30 baseline readings receives full credit for
drift and variance signals — those signals require enough history to be
meaningful.

## API

```http
GET /api/sensor-health
```

Returns an array of `HealthReport` objects, sorted by score ascending
(worst sensors first). Each report includes:

- `key` — `{zone, zone_id, metric}`
- `score` — 0-100
- `status` — `"healthy"`, `"degraded"`, `"suspect"`, or `"bad"`
- `primary_concern` — string identifying the worst-contributing signal,
  or null if all signals are healthy
- `quality_rate`, `cadence`, `drift`, `variance_ratio`, `recency` — raw
  signal values (0.0–1.0) for diagnostic display
- `readings_in_window`, `last_good_reading_at`, `computed_at`

## Prometheus metric

`groundtruth_sensor_health_score{zone, zone_id, metric}` (gauge, 0-100)

Updated by the background refresh task every 30 seconds. See
`docs/grafana-sensor-health-dashboard.json` for an importable dashboard.

## What this is NOT

- Not a replacement for Tier-1 validation. Both run together.
- Not predictive — it doesn't forecast failure, only detects current
  degradation. Predictive failure detection would be Tier-3 future work.
- Not real-time per-reading — operates on a 30s cache.
