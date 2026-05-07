# GroundTruth

> **A self-validating IoT garden monitoring platform.** Capacitive soil
> moisture, temperature, and humidity sensors → MQTT → a Rust ingestion
> pipeline with a four-rule validation tier → SQLite, Prometheus, and
> three independent dashboards.

<img width="2537" height="1220" alt="image" src="https://github.com/user-attachments/assets/4b2eb17a-22c6-4b8e-8698-7666da82db64" />

---

## The story

I built GroundTruth's data validation pipeline before I trusted my
sensors. Good thing — within hours of the first plant being instrumented,
the pipeline had flagged **25,000 consecutive readings as suspect**.

The sensor was working. The values were "100% moisture" — and they
were also wrong.

The capacitive soil sensor I'd wired up was reading raw ADC values
around 750 in freshly watered soil. My firmware's calibration constants
assumed `WET = 1200`. Anything below 1200 calibrated above 100% and got
clamped. Every reading was reporting maximum saturation. The dashboard
would have shown a flat 100% line for as long as the system ran.

The validation tier caught this because of one of its four rules: the
**stuck-reading detector** flags any metric that produces six or more
consecutive readings within ±0.01 of each other. From the validation
tier's perspective, the sensor was clearly broken — even though the
underlying ADC readings were responsive and varied. The clamping had
hidden the variation. The validator surfaced it.

The fix was a one-line calibration update. The lesson — that the
validator should be built before the sensors are trusted — is the whole
reason GroundTruth exists.

---

## What it is

GroundTruth is a complete IoT monitoring platform for a garden setting.
Sensor nodes publish via MQTT, an ingestion server validates and
persists every reading, and three dashboards expose the data for
different audiences (operators, engineers, automation).

It's a portfolio project, but it runs continuously on real hardware and
watches a real plant. The data is real. The bugs are real.

## What it does

- **Ingests sensor readings over MQTT** from ESP32-C3 sensor nodes
- **Validates every reading** against four rules — range, raw ADC plausibility, stuck-reading detection, and rate-of-change
- **Persists everything** (including invalid readings, for diagnostics) to SQLite with a `quality` and `validation_reason`
- **Exposes JSON API endpoints** for application UIs (`/api/sensors`, `/api/readings/latest`, `/api/readings/history`)
- **Exposes Prometheus metrics** for ops dashboards (`/metrics`)
- **Renders a custom Next.js dashboard** with auto-refresh
- **Integrates with Grafana** via Prometheus for time-series visualization

## What it looks like running

Three observability surfaces, all backed by the same SQLite source of truth:

| Surface | Purpose |
|---------|---------|
| **Next.js dashboard** at `localhost:3000` | Custom React/TypeScript UI for daily monitoring |
| **Grafana** at `192.168.0.114:3001` | Production-style operational view via Prometheus |
| **Direct API** at `192.168.0.114:3002/api/sensors` | Raw JSON for any consumer |

The dashboard image at the top of this README shows ~24 hours of real
overnight data: soil moisture descending naturally as the plant dried,
temperature cycling diurnally between 65°F and 68°F, humidity inversely
correlating with temperature exactly as physics predicts.

---

## Architecture

```
┌─────────────────────┐
│  ESP32-C3 nodes     │
│  + SEN0308 (soil)   │  ─── MQTT ─→  ┌──────────────────┐
│  + DHT22 (climate)  │              │  Mosquitto       │
└─────────────────────┘              │  (Docker)        │
                                     └────────┬─────────┘
                                              │
                                     ┌────────▼─────────────────────────┐
                                     │  groundtruth-server (Rust)       │
                                     │  - parse topic                   │
                                     │  - 4-rule validation tier        │
                                     │  - persist to SQLite             │
                                     │  - update Prometheus metrics     │
                                     │  - serve JSON API on :3001       │
                                     └────┬───────┬──────────┬──────────┘
                                          │       │          │
                                  /api/*  │       │ /metrics │ SQLite
                                          │       │          │
                          ┌───────────────▼┐ ┌────▼─────┐    │
                          │ Next.js dash   │ │Prometheus│    │
                          │ (Tailwind +    │ │  scrape  │    │
                          │  Recharts)     │ └────┬─────┘    │
                          └────────────────┘      │          │
                                            ┌─────▼──────┐   │
                                            │  Grafana   │   │
                                            └────────────┘   │
                                                  ↑          │
                                                  │  query   │
                                                  └──────────┘
```

## The validation pipeline

Each reading is assigned `quality` ∈ `{good, suspect, invalid}` plus a
human-readable `validation_reason` if it's not good. Invalid readings
are still persisted (not dropped) so broken sensors remain diagnosable.
Dashboards filter on `quality = 'good'` to exclude them from analysis.

| Rule | What it detects | Severity |
|------|----------------|----------|
| Value range | Implausible values (moisture > 100%, temp > 200°F, etc.) | Invalid |
| Raw ADC range | ADC outside [100, 3995] — saturated or shorted sensor | Invalid |
| Stuck reading | 6+ consecutive readings within ±0.01 — sensor not responding | Suspect |
| Rate of change | Implausibly rapid deltas (e.g. 30%+ moisture change in 10 min) | Suspect |

Severity precedence is `Invalid > Suspect > Good`.

## The Rust server

Single binary, runs in Docker. Two long-running tasks:

- **MQTT subscriber loop** consumes `groundtruth/#`, parses topic and
  payload, validates, persists, updates metrics. Auto-resubscribes on
  broker reconnect (caught a Mosquitto restart in production once).
- **Axum HTTP server** on port 3001 serving JSON API and `/metrics`.

The binary is single-threaded async (`#[tokio::main]`). The SQLite
connection is wrapped in `Arc<Mutex<Connection>>` and shared between the
ingestion task and HTTP handlers.

Notable design choices:

- **`moisture_raw` and `moisture` are coalesced** by the ingestion
  layer. The C3 publishes both topics ~10ms apart on each wake; the
  server buffers `moisture_raw` for 30 seconds and joins it onto the
  matching `moisture` reading so each row in SQLite has both values.
- **Validation runs synchronously** inside the message handler. With
  ~6 readings/minute across 4 metrics × ~3 sensors, this is well within
  per-message budget and avoids the complexity of an async pipeline.
- **Metrics use lazy initialization** via `once_cell::Lazy`. A scrape
  before any reading has been ingested returns an empty (but valid)
  Prometheus response.

## The hardware

- **ESP32-C3 Super Mini** sensor nodes (multiple — meant to scale to 7+ raised beds and a greenhouse)
- **DFRobot SEN0308** capacitive soil moisture sensor (analog, 3.3V)
- **DHT22** temperature + humidity sensor (digital, one-wire)
- **Raspberry Pi 5** running Docker Compose with Mosquitto + groundtruth-server

The sensor node firmware ships in two variants:

- **`groundtruth_node_soil_only.ino`** — production, uses ESP-IDF deep
  sleep for ~5-minute wake cycles. Battery-friendly.
- **`groundtruth_node_dev.ino`** — bench debugging, no deep sleep, 10s
  loop, USB stays alive for live serial output during sensor calibration.

## Project layout

```
GroundTruth/
├── src/                      # Rust ingestion server
│   ├── main.rs              # MQTT subscriber + task wiring
│   ├── api.rs               # Axum HTTP routes
│   ├── db.rs                # SQLite schema + queries
│   ├── topics.rs            # MQTT topic parser
│   ├── validation.rs        # 4-rule validation tier
│   └── metrics.rs           # Prometheus instrumentation
├── web/                      # Next.js dashboard
│   ├── app/                 # App Router pages
│   ├── components/          # SensorCard, HistoryChart, QualityBadge
│   └── lib/                 # API client, types, formatters
├── firmware/
│   ├── groundtruth_node_soil_only.ino    # production firmware
│   └── groundtruth_node_dev.ino          # bench dev firmware
├── docs/
│   ├── grafana-setup.md
│   ├── grafana-dashboard.json            # importable
│   └── prometheus-snippet.yml
└── docker-compose.yml
```

## Running it

The full bring-up steps live in `docs/grafana-setup.md`. Short version:

```bash
# Backend (on the Pi)
docker compose up -d

# Verify the API
curl http://192.168.0.114:3002/api/sensors
curl http://192.168.0.114:3002/metrics

# Custom dashboard (anywhere)
cd web
npm install --legacy-peer-deps
npm run dev   # http://localhost:3000
```

## What's next

Roughly in priority order:

- **Multi-bed scaling** — physical hardware for 6 more sensor nodes; the backend already partitions by `(zone, zone_id)`
- **Greenhouse climate node** — a separate ESP32-C3 publishing on `groundtruth/greenhouse/*` topics; backend already recognizes that zone
- **Alert routing** — Prometheus alertmanager rules for "moisture below 30%" or "no readings in 30 minutes"
- **Tier 2 validation** — cross-sensor consistency checks (e.g. flag moisture readings inconsistent with rainfall in the same zone)
- **Permanent solder bring-up** — move sensor wiring from breadboard to soldered headers for greenhouse deployment

## License

MIT.
