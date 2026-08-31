# GroundTruth

> **A two-tier validation framework for streaming numeric data, with a
> soil sensor IoT system as its reference implementation.** Real-time
> per-reading checks. Per-sensor health scoring over time. Three
> dashboards. Runs continuously on a Raspberry Pi watching a garden bed.

**Status (2026-08-30):** garden monitoring is live on a single node
(bed/1). A second node, bed/2, is built but not currently deployed. The
validator is in production and catching real faults. Multi-unit sensor
characterization is the next phase — see
[Where this is going](#where-this-is-going).

![GroundTruth dashboard with real overnight data](docs/screenshots/dashboard-overnight.png)
<img width="2520" height="1247" alt="image" src="https://github.com/user-attachments/assets/5480c0a0-e60d-4be9-be90-9310e4bff33c" />

---

## The story

GroundTruth started as an IoT garden monitor. The validator emerged as
the more interesting technical contribution somewhere around hour three.

Within hours of the first sensor coming online, the validation pipeline
had flagged **25,000 consecutive readings as suspect**. The sensor was
working. The values were "100% moisture" — and they were also wrong.

My calibration constants assumed the sensor reads ADC 1200 in wet soil.
Mine reads 750. Every reading was calibrating above 100%, clamping to
the upper bound, and the dashboard would have shown a flat line as
moisture data. The stuck-reading detector caught it: 25,000 identical
values is not a real sensor.

That moment made the validator more interesting than the IoT system that
hosts it. The IoT side is well-trodden — sensors, MQTT, time-series.
The validator is the thing that catches errors at the source, before
they pollute downstream analysis. It's the part of the project that
generalizes beyond gardening.

---

## What it is

GroundTruth is two things stacked:

**1. The validator** (the interesting part)
- A two-tier validation pipeline for streaming numeric data
- Tier-1: per-reading rules (range, raw plausibility, stuck detection, rate-of-change)
- Tier-2: per-sensor health scoring (quality rate, cadence, drift, variance, recency)
- Quality flags persisted alongside data so dashboards can filter or visualize broken sources
- Sensor health gauges exposed to Prometheus for ops-style alerting

**2. The IoT system** (the reference implementation)
- ESP32-C3 sensor nodes publishing soil moisture, temperature, humidity via MQTT
- Rust ingestion server (Axum + tokio + rumqttc) handling MQTT subscription and HTTP API
- SQLite persistence with quality flags inline on every row
- Three observability surfaces: Next.js dashboard, Grafana, raw JSON API
- Self-contained Docker Compose stack with bundled Prometheus
- Runs continuously on a Raspberry Pi 5 watching a garden bed

---

## Currently running

The system is live in production on the Raspberry Pi 5, processing
real data from one sensor node:

- **bed/1** — soil moisture, temperature, humidity (SEN0308 + DHT22) — **live**
- **bed/2** — soil moisture only (SEN0308) — built, not currently deployed

That's three independent `(source, metric)` streams flowing through the
validator today. Each gets its own Tier-1 history buffer, its own
Tier-2 health score with its own baseline, and its own quarantine state
machine — so bed/1's moisture sensor going stuck doesn't affect its
temperature stream, and temperature is judged separately from moisture
on the same node. The multi-source design is exercised for real, not
just in integration tests; bed/2's history is still in the database and
its streams reappear the moment it is plugged back in.

One consequence worth knowing when reading the API: bed/2 still shows
up as a sensor. `/api/sensors` lists every `(zone, zone_id, metric)`
that has *ever* reported, and `total_active_sensors` counts every
stream the validator has seen since process start. Neither asks whether
the stream reported recently, so "known" and "active" are the same
number today. See [What's next](#whats-next).

---

## Where this is going

The story so far is that the validator turned out to be the interesting
part. The next chapter is that the validator can't be trusted further
than the sensors it's judging are understood — and cheap capacitive
soil moisture sensors are not well understood.

The DFRobot SEN0308 and its many clones are everywhere in hobbyist
soil monitoring, and widely distrusted. But the distrust is anecdotal:
forum posts, one-off comparisons, "mine drifted." Nobody has published
a methodical multi-unit characterization — how far identical units
disagree out of the bag, how much of that spread per-unit calibration
actually removes, and, the question that matters for anyone leaving
one in the ground, how the sensors drift over months in soil and how
often they need recalibrating. GroundTruth is becoming the instrument
for answering that.

The plan is to build a characterization station: a fixture holding a
batch of sensors in the same medium, read through a 16-channel analog
mux on a single ESP32-C3 and stepped through defined moisture states —
air, dry sand, sand with water fractions measured by mass, saturated,
water. It will publish over the same MQTT pipeline as the garden nodes,
tagged by unit ID, so its readings flow through the same validator.
Acceptance tolerances get defined before the first run, not after
seeing the numbers.

The first number it has to produce is one the pipeline is already
using without having earned it. Tier-1 stuck detection on calibrated
moisture runs at a `stuck_threshold` of 0.01 % — an exact-repeat
detector in all but name. It has worked, but only because ADC noise
makes exact repeats rare; the threshold was inherited as a default, not
derived from anything the SEN0308 does. The honest version is the
calibrated-percent equivalent of the probe's measured noise floor: hold
a unit in a fixed medium, log the ADC spread at rest, push it through
the calibration curve, and take the width of the resulting band. That
single measurement replaces a number picked by accident, and the same
procedure — noise floor for analog parts, resolution for quantized
ones — is what every other Tier-1 threshold in the system should come
from.

From there: a t=0 baseline for about ten units — mean, standard
deviation, and coefficient of variation at each state, per unit, plus
linearity and wet→dry vs. dry→wet hysteresis. Then the batch gets
deployed in a split design: some units buried and continuously
powered, some buried and unpowered, some kept dry as controls. That
separates drift caused by moisture ingress from drift caused by
electrical stress, and sensor drift from rig drift. Monthly
re-characterization on the same station tracks per-unit offset and
span against t=0. The validator's Tier-2 drift signal makes a
prediction each month; the measurement grades it.

One possible extension is a load-cell node, so a pot's mass loss — the
gravimetric method, the actual reference for soil water content — can
be logged alongside the sensor ADC. That would turn a relative trend
into a real calibration curve with an error figure attached.

Two findings from a single afternoon of production data are why this
looks like a prerequisite rather than a side project. The first: the
stuck-reading rule, tuned for a noisy analog probe, turned out to be a
false-positive machine on a quantized DHT22, flagging a healthy sensor
most of the time simply because 0.1° resolution against a slow-moving
garage means repeated values are normal. Validation thresholds have to
be derived from measured sensor characteristics — resolution, noise
floor, expected dynamics — not chosen once and applied everywhere. The
second: a Prometheus gauge held a dead sensor's last value for 18
hours, and the raw chart showed a flat, plausible line the whole time.
The health-scoring layer noticed; the data did not. Both point the same
way. Knowing what the hardware actually does is what makes the layer
above it worth believing.

---

## Workspace structure

The repo is a Cargo workspace with three members:

| Crate | Purpose | README |
|-------|---------|--------|
| [`groundtruth-validator`](groundtruth-validator/README.md) | The validation framework as a standalone library. No I/O, no storage, no transport. Push readings in, get classifications, health scores, and quarantine state out. | [README](groundtruth-validator/README.md) |
| [`groundtruth-validator-cli`](groundtruth-validator-cli/README.md) | `gt-validate` command-line tool that runs the validator over arbitrary CSV files. Proves the library is domain-agnostic. | [README](groundtruth-validator-cli/README.md) |
| `server` | The GroundTruth Rust binary: MQTT ingestion, SQLite persistence, Prometheus metrics, Axum API. Consumes `groundtruth-validator` as a dependency. | documented here — see [Metrics](#metrics) |

The validator is the headline contribution; everything else exists to
prove it works on real data (the server) and on data that isn't soil
sensors (the CLI).

---

## The validation pipeline
<img width="1168" height="619" alt="image" src="https://github.com/user-attachments/assets/81d444f8-ecd0-4ec9-929b-4ee9a7493b29" />

The validator is structurally separate from the IoT bits. The Rust
modules `src/validation.rs` and `src/sensor_health.rs` take numbers
with metadata (zone, zone_id, metric, value, raw_adc, timestamp) and
return quality judgments. They don't know anything about soil.

### Tier-1: per-reading validation

Every reading is assigned `quality ∈ {good, suspect, invalid}` plus a
human-readable `validation_reason` when not good. Invalid readings are
persisted (not dropped) so broken sensors remain diagnosable.

| Rule | What it catches | Severity |
|------|----------------|----------|
| Value range | Implausible values (e.g. moisture > 100%) | Invalid |
| Raw ADC range | ADC outside [100, 3995] — saturated/shorted sensor | Invalid |
| Stuck reading | Per metric: 6+ consecutive readings within ±0.01 (noisy analog), or a value unchanged within its resolution for 60+ min (quantized) | Suspect |
| Rate of change | Implausibly rapid deltas (e.g. 30%+ moisture change in 10 min) | Suspect |

Severity precedence is `Invalid > Suspect > Good`.

### Tier-2: per-sensor health monitoring

While Tier-1 judges individual readings, Tier-2 judges sensor *streams*
over time. Each sensor gets a 0-100 health score updated every 30
seconds, combining five weighted signals:

| Signal | Weight | What it catches |
|--------|--------|-----------------|
| Quality rate | 35% | High suspect/invalid rates from Tier-1 |
| Reporting cadence | 25% | Sensor stopped reporting or fell behind expected frequency |
| Drift detection | 20% | Recent mean has moved >3σ from long-term baseline |
| Variance ratio | 10% | Recent stddev is outside 0.5x–2x the baseline stddev |
| Recency | 10% | Time since last good reading approaches 10 minutes |

Baselines are computed from the **last 7 days of good readings only**.
Excluding bad readings from the baseline prevents sensor degradation
from infecting the model used to detect that same degradation. A sensor
that's been broken for a week still has a clean baseline from the week
before.

The Tier-2 signals are designed around well-documented capacitive soil
sensor failure modes from the hobbyist embedded community: gradual
coating-leak drift over months (Cave Pearl Project, Daniel Robertson),
production variance between sensors from the same batch (arduinodiy),
and slow degradation patterns that escape per-reading validation.

---

## Architecture

```
┌──────────────────────┐
│  ESP32-C3 nodes      │  ───MQTT───┐
│  + SEN0308 (soil)    │            │
│  + DHT22 (climate)   │            │
└──────────────────────┘            │
                                    ▼
                          ┌──────────────────┐
                          │  Mosquitto       │
                          └──────────┬───────┘
                                     │
            ┌────────────────────────▼──────────────────────┐
            │  groundtruth-server (Rust)                    │
            │                                               │
            │  ┌─────────────────────────────────────────┐  │
            │  │  Ingestion: parse topic, parse payload  │  │
            │  └──────────────────────┬──────────────────┘  │
            │                         ▼                     │
            │  ┌─────────────────────────────────────────┐  │
            │  │  VALIDATION PIPELINE                    │  │
            │  │  ── Tier-1: per-reading rules           │  │
            │  │  ── Tier-2: per-sensor health (30s loop)│  │
            │  └──────────────────────┬──────────────────┘  │
            │                         ▼                     │
            │  ┌─────────────┐  ┌──────────────┐  ┌─────┐   │
            │  │   SQLite    │  │  Prometheus  │  │ API │   │
            │  └─────────────┘  └──────┬───────┘  └──┬──┘   │
            └──────────────────────────┼─────────────┼──────┘
                                       │             │
                              ┌────────▼─────┐  ┌────▼────────────┐
                              │   Grafana    │  │  Next.js + TS   │
                              │  (2 dashboards)│  │   dashboard     │
                              └──────────────┘  └─────────────────┘
```

The validation pipeline is the centerpiece. Everything upstream feeds
it; everything downstream consumes its judgments.

---

## How it generalizes

The validator is built around streaming numeric data with a small
metadata schema (a source identity, a metric name, a value, an optional
raw reading, a timestamp). Nothing in the rules themselves depends on
soil sensors specifically.

Realistically, today, the validator is coupled to GroundTruth in a few
ways:
- Its persistence layer assumes a specific SQLite schema
- Its baseline window is hardcoded at 7 days
- Its expected-cadence values are tuned for the firmware's publish rates

The rule shapes themselves — range checking, raw plausibility, stuck
detection, rate-of-change, drift, variance ratio, cadence, recency — are
domain-agnostic. The patterns would apply to financial time-series,
server metrics, A/B test outputs, or any other streaming numeric data
where the source can be untrustworthy.

Extracting the validator into a standalone crate with pluggable
persistence and configuration is a known follow-up. See "What's next".

---

## The IoT reference implementation

The IoT side is well-trodden territory, but the design choices are worth
calling out:

- **ESP32-C3 Super Mini** sensor nodes, deployed to a greenhouse
- **DFRobot SEN0308** capacitive soil moisture (analog, 3.3V)
- **DHT22** temperature and humidity (digital, one-wire)
- **Raspberry Pi 5** running the Docker Compose stack

The sensor node firmware ships in two variants:

- `groundtruth_node_soil_only.ino` — production, ESP-IDF deep sleep for
  ~5-minute wake cycles. Battery-friendly.
- `groundtruth_node_dev.ino` — bench debugging, no deep sleep, 10s loop,
  USB stays alive for live serial output during calibration.

The Rust ingestion server is single-threaded async (`#[tokio::main]`)
with the SQLite connection wrapped in `Arc<Mutex<Connection>>` and
shared between the ingestion task and HTTP handlers.

Notable design choices:

- **`moisture_raw` and `moisture` are coalesced** into one DB row by
  buffering raw values for 30 seconds and joining them onto the matching
  calibrated reading. Each row in SQLite has both values.
- **Self-contained observability stack.** GroundTruth ships with its
  own Prometheus instance for metrics. The shared Grafana
  (a separate service) connects to it as a data source.
- **MQTT subscriber auto-resubscribes on broker reconnect.** Caught a
  Mosquitto restart in production once and the system recovered
  without manual intervention.

---

## Project layout

```
GroundTruth/
├── src/                      # Rust ingestion server
│   ├── validation.rs        # Tier-1: per-reading rules
│   ├── sensor_health.rs     # Tier-2: per-sensor scoring
│   ├── main.rs              # MQTT subscriber + task wiring
│   ├── api.rs               # Axum HTTP routes
│   ├── db.rs                # SQLite schema + queries
│   ├── topics.rs            # MQTT topic parser
│   └── metrics.rs           # Prometheus instrumentation
├── web/                      # Next.js dashboard
├── firmware/                 # ESP32-C3 firmware (deep-sleep + dev variants)
├── prometheus/               # Bundled Prometheus config
├── docs/                     # Setup runbooks, dashboard JSONs, design docs
└── docker-compose.yml
```

The validation pipeline lives in two Rust modules at the top of `src/`
to keep it visible. Everything else in `src/` is the IoT plumbing
around it.

---

## Metrics

The server exposes Prometheus text format at `GET /metrics`.

| Metric                                   | Type    | Labels                                 | Meaning                                          |
|------------------------------------------|---------|----------------------------------------|--------------------------------------------------|
| `groundtruth_moisture_percent`           | gauge   | `zone`, `zone_id`                      | Calibrated soil moisture, 0-100                  |
| `groundtruth_moisture_raw_adc`           | gauge   | `zone`, `zone_id`                      | Raw ADC counts from the capacitive probe, 0-4095 |
| `groundtruth_temperature_fahrenheit`     | gauge   | `zone`, `zone_id`                      | Temperature in °F                                |
| `groundtruth_humidity_percent`           | gauge   | `zone`, `zone_id`                      | Relative humidity, 0-100                         |
| `groundtruth_last_reading_age_seconds`   | gauge   | `zone`, `zone_id`, `metric`            | Seconds since this stream last reported          |
| `groundtruth_sensor_health_score`        | gauge   | `zone`, `zone_id`, `metric`            | Tier-2 health score, 0-100                       |
| `groundtruth_sensor_quarantined`         | gauge   | `zone`, `zone_id`, `metric`            | 1 while the stream is quarantined, else 0        |
| `groundtruth_quarantine_events_total`    | counter | `zone`, `zone_id`, `metric`            | Times the stream has entered quarantine          |
| `groundtruth_readings_total`             | counter | `zone`, `zone_id`, `metric`, `quality` | Readings ingested, partitioned by Tier-1 verdict |

The four value gauges update on every Good reading. Everything else is
refreshed by the 30-second background tick.

### Stale streams break the line instead of flattening it

A Prometheus gauge holds its last value indefinitely, and Prometheus
records that value on every scrape. When `bed/1/temperature` went
silent for 18 hours, Grafana drew a flat 73 °F line for 18 hours — a
dead sensor and a perfectly stable one looked identical on the chart.

So the server tracks when each stream last reported and, once a stream
has been silent longer than `STALE_TIMEOUT_SECS` (default 300),
**removes that stream's label set from the value gauge families**. The
series disappears from `/metrics`, Prometheus records a gap rather than
a repeated stale sample, and Grafana breaks the line. Removing the
label set is preferred over exporting `NaN`: it produces a real
absence, not a value every consumer has to be told to ignore.

Alongside that, `groundtruth_last_reading_age_seconds` is updated both
on arrival and on the 30-second tick, so it keeps climbing while the
stream is quiet. That gauge is what an outage alert should watch:

```promql
groundtruth_last_reading_age_seconds > 900
```

Three deliberate details:

- **Health and quarantine gauges keep being exported during silence.**
  When a sensor dies, the score falling and the quarantine flag rising
  are exactly the series you want on the chart. Only the values the
  dead sensor is no longer producing go away.
- **Recovery is immediate.** The next Good reading re-creates the label
  set, so the series returns on the following scrape with no operator
  action.
- **Any reading marks a stream alive**, including Suspect and Invalid
  ones. A sensor emitting garbage is not silent and its age gauge
  should say so; the value gauges stay untouched because only Good
  readings ever set them.

### Server configuration

| Variable             | Default     | Meaning                                                     |
|----------------------|-------------|-------------------------------------------------------------|
| `MQTT_BROKER_HOST`   | `localhost` | Broker to subscribe to                                      |
| `MQTT_BROKER_PORT`   | `1883`      | Broker port                                                 |
| `API_PORT`           | `3001`      | Port for the JSON API and `/metrics`                        |
| `STALE_TIMEOUT_SECS` | `300`       | Silence after which a stream's value gauge stops exporting  |

---

## Running it

Full setup steps live in `docs/grafana-setup.md`. Short version:

```bash
# Backend (on the Pi)
docker compose up -d

# Verify
curl http://192.168.0.114:3002/api/sensors
curl http://192.168.0.114:3002/metrics | grep groundtruth_sensor_health

# Custom dashboard (anywhere)
cd web
npm install --legacy-peer-deps
npm run dev   # http://localhost:3000
```

---

## What's next

Roughly in priority order:

- **Decouple the validator from SQLite specifics** so it can be extracted
  into a standalone crate. Today the rules are pluggable but the
  persistence layer assumes a particular schema.
- **Per-metric tuning of Tier-1 and Tier-2 thresholds.** The variance
  signal currently flags rock-stable temperature readings as suspect.
  That's correct given the uniform threshold, but legitimately stable
  metrics should get more lenient bounds.
- **Separate "known" from "active" in the API.** `/api/sensors` and
  `total_active_sensors` both answer "has this stream ever reported",
  so an undeployed node like bed/2 counts as active indefinitely. Now
  that `groundtruth_last_reading_age_seconds` exists, the same
  staleness cutoff the metrics layer uses should decide this, and the
  two questions should get distinct fields.
- **Multi-bed scaling.** Redeploy bed/2 and scale beyond it to the full
  garden by wiring up more C3 nodes. Backend already partitions by
  `(zone, zone_id)`.
- **Greenhouse climate node.** A separate ESP32-C3 publishing on
  `groundtruth/greenhouse/*` topics; backend already recognizes that
  zone.
- **Tier-3 predictive failure detection.** Use historical sensor health
  trajectories to predict failures before they happen — the obvious
  successor to current Tier-2 monitoring.
- **Permanent solder-up.** Move sensor wiring from breadboard to soldered
  headers and weatherproof enclosure for long-term greenhouse deployment.

---

## License

MIT.
