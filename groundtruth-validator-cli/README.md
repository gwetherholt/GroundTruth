# gt-validate — Validate streaming data from the command line

`gt-validate` is a CLI that runs the
[`groundtruth-validator`](../groundtruth-validator/README.md) framework
over a CSV. Point it at any CSV of timestamped numeric data — IoT
exports, server-metric dumps, an A/B test pull, whatever — and it
reports a quality breakdown, the Tier-2 health score with all five
component signals, and the quarantine state. It auto-detects which
columns are timestamps, sources, values, or raw transducer readings,
so most CSVs work without any flags.

## Install

The CLI lives in the GroundTruth workspace. From the repo root:

```bash
cargo build --release -p groundtruth-validator-cli
# binary at: ./target/release/gt-validate
```

Or install to `~/.cargo/bin`:

```bash
cargo install --path groundtruth-validator-cli
```

## Usage

```
Run groundtruth-validator over a CSV file

Usage: gt-validate [OPTIONS] <FILE>

Arguments:
  <FILE>  Path to CSV file. Use "-" for stdin

Options:
      --metric <METRIC>            Metric name to assign. If omitted and the CSV has
                                   multiple numeric value columns, each is processed
                                   as a separate metric
      --source <SOURCE>            Source name to assign if the CSV has no source
                                   column [default: default]
      --range <RANGE>              Valid value range, e.g. "0:100" or "-40:200"
      --raw-range <RAW_RANGE>      Valid raw transducer range, e.g. "100:3995"
      --cadence <CADENCE>          Expected reporting cadence, e.g. "30s" or "5m" or
                                   "1h" [default: 60s]
      --stuck-count <STUCK_COUNT>  Consecutive readings within stuck-threshold to
                                   flag as stuck [default: 6]
      --max-rate <MAX_RATE>        Max absolute change between consecutive readings
                                   [default: 10]
      --format <FORMAT>            Output format [default: summary]
                                   [possible values: summary, full, json]
      --no-color                   Disable colored output (also respected via
                                   NO_COLOR env var)
  -h, --help                       Print help
  -V, --version                    Print version
```

**Note on negative ranges:** clap interprets a leading `-` as a flag,
so use the `=` form for negative-bound ranges:
`--range=-40:200`, not `--range "-40:200"`.

## Examples

### 1. Clean sensor data

```bash
gt-validate examples/clean_sensor.csv
```

```
── gt-validate report: value ───────────────────────────
File:       examples/clean_sensor.csv
Readings:   100
Metric:     value
Source(s):  bed/1
Duration:   49m 30s (2026-05-16 10:00 → 10:49)
── Quality breakdown ───────────────────────────────────
Good:          98  (98.0%)  █████████████████████░
Suspect:        2  ( 2.0%)  ░░░░░░░░░░░░░░░░░░░░░
Invalid:        0  ( 0.0%)  ░░░░░░░░░░░░░░░░░░░░░
── Flagged readings ────────────────────────────────────
stuck_reading:         2  readings
── Health score ────────────────────────────────────────
Overall:            99.3 / 100  [Healthy]
Quality rate:       98.0%  (weight: 35%)
Reporting cadence: 100.0%  (weight: 25%)
Drift:             100.0%  (weight: 20%)
Variance ratio:    100.0%  (weight: 10%)
Recency:           100.0%  (weight: 10%)
── Quarantine ──────────────────────────────────────────
Status: Active
Quarantine events: 0
────────────────────────────────────────────────────────
```

### 2. Noisy sensor data, full output with flagged-reading detail

```bash
gt-validate examples/noisy_sensor.csv --range=-40:200 --format=full
```

The trailing section appears only in `--format=full`:

```
── Flagged readings (detail) ───────────────────────────
2026-05-16 10:27:30  value=72.000    SUSPECT  stuck_reading: 6 consecutive readings within 0.01 of 72.00 — source may be stuck
2026-05-16 10:28:00  value=72.000    SUSPECT  stuck_reading: 6 consecutive readings within 0.01 of 72.00 — source may be stuck
2026-05-16 10:28:30  value=72.000    SUSPECT  stuck_reading: 6 consecutive readings within 0.01 of 72.00 — source may be stuck
2026-05-16 11:05:00  value=999.000   INVALID  value_range: value = 999.00 outside plausible range [-40, 200]
2026-05-16 11:05:30  value=70.800    SUSPECT  rate_of_change: value changed by 928.20 in 30s (threshold 10.00/600s)
────────────────────────────────────────────────────────
```

Tier-1 caught the stuck block, the out-of-range spike, and the
rate-of-change snap-back into normal values. The Drift signal in
Tier-2 dropped to 90% from the gradual upward drift in the last 50
rows.

### 3. JSON output piped to jq

```bash
gt-validate examples/clean_sensor.csv --format=json \
    | jq '{metric, good_pct: .quality.good_percent, health: .health.overall, status: .health.status}'
```

```json
{
  "metric": "value",
  "good_pct": 98.0,
  "health": 99.3,
  "status": "healthy"
}
```

You can also pipe a CSV in on stdin:

```bash
cat readings.csv | gt-validate - --format=json | jq '.flagged_rules'
```

## CSV format

Column detection is case-insensitive and runs against the header row.

| Role          | Header aliases                                       |
|---------------|------------------------------------------------------|
| Timestamp     | `timestamp`, `time`, `datetime`, `date`, `ts`        |
| Source        | `source`, `sensor`, `device`, `node`, `id`           |
| Value (named) | `value`, `reading`, `measurement`, `data`            |
| Raw value     | `raw`, `raw_value`, `adc`                            |

**Numeric promotion.** Any unclassified column whose data parses as
numeric is promoted to a value column, with the metric name taken from
the header. This means a CSV like `timestamp,temperature,humidity`
gets two value columns auto-detected.

**Multiple value columns.** Without `--metric`, each value column is
processed independently and one report is emitted per metric. With
`--metric NAME`, only the matching column (by header) is processed,
falling back to the first numeric column tagged with the user's name
if no header matches.

**Missing timestamp column.** Synthetic timestamps are generated
starting at "now" and advancing by `--cadence` per row.

**Missing source column.** All readings are assigned the `--source`
value (default `"default"`).

**Timestamp formats supported.** RFC 3339, RFC 2822, common
ISO-without-timezone variants (`YYYY-MM-DD HH:MM:SS[.frac]`), Unix
epoch seconds, Unix epoch milliseconds.

## Sample data

The crate ships two CSVs under `examples/`:

**`clean_sensor.csv`** — 100 rows of synthetic soil-moisture data
on a 30s cadence (50 minutes of data). Header is
`timestamp,sensor,value,raw`. Values trend smoothly from ~60% down to
~45% with mild sinusoidal jitter, plus a deliberate 7-row block of
identical values around row 40 to trigger 2 stuck-reading flags.
Expected: 98% Good, 2% Suspect, Healthy.

**`noisy_sensor.csv`** — 200 rows of synthetic temperature data on
a 30s cadence (~1h 44m of data, including a deliberate 5-minute gap
near row 80). Header is `timestamp,sensor,value`. Designed to exercise
every Tier-1 rule and degrade Tier-2 drift:

- Rows 50–57: stuck block at 72.0°F (8 identical readings → 3 Suspect)
- Row 80: 5-minute timestamp gap
- Row 120: spike to 999°F (Invalid via `value_range` when
  `--range=-40:200`; the snap-back at row 121 also fires `rate_of_change`)
- Rows 150–199: gradual upward drift (+0.5°F/reading) — Tier-1 doesn't
  flag this (per-step delta is small), but Tier-2 drift drops to ~90%

Expected: 97.5% Good, 2% Suspect, 0.5% Invalid, Healthy.

## Output formats

- **`summary`** *(default)* — Compact human-readable report with
  box-drawing section headers, a colored quality breakdown with ASCII
  bars, the five-signal health breakdown, and quarantine status.
  Colors disabled with `--no-color` or `NO_COLOR=1`.
- **`full`** — Everything in `summary`, plus a "Flagged readings
  (detail)" section listing every non-Good reading with timestamp,
  value, quality, rule, and reason. Useful for triage.
- **`json`** — Single JSON object (or array, if multiple metrics) with
  the entire report including `flagged_readings`. Stable shape; pipe
  through `jq` or feed to a notebook.

## License

MIT. See [LICENSE](../LICENSE).
