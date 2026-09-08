# Characterization station

The station is a bench fixture for measuring SEN0308 units one at a
time in prepared batches — air, dry sand, sand at known water fractions
by mass, saturated sand, water. It publishes over the same MQTT
pipeline and into the same `readings` table as the garden, but it is a
lab instrument and the system treats it as one.

- [Topics](#topics)
- [Validation policy](#validation-policy)
- [Session log](#session-log)
- [Joining readings to conditions](#joining-readings-to-conditions)
- [Analyzing station data](#analyzing-station-data)
- [Running a session](#running-a-session)

## Topics

| Topic | Payload | Meaning |
|-------|---------|---------|
| `charstation/{unit_id}/raw_adc` | integer, e.g. `1487` | Mean of 16 ADC samples taken over ~100 ms |
| `charstation/{unit_id}/temperature` | float, e.g. `71.4` | Optional bench DHT22, same payload as a bed |
| `charstation/session` | JSON, retained | One session-context record (see below) |
| `charstation/control/unit` | string, retained | Which unit the station firmware is measuring |

`unit_id` runs `1`–`16`, one 16-channel mux worth of bench positions.
Anything else is rejected at the topic parser.

**There is no `charstation/{unit}/moisture`.** Calibrated moisture is
computed from a calibration curve, and the calibration curve is what
this station exists to produce. Feeding a calibrated value back into
the study would be circular, so the parser rejects `moisture` and
`moisture_raw` under `charstation` outright.

Readings land in `readings` with `zone = 'charstation'`, `zone_id` set
to the unit id, and `metric = 'raw_adc'`. The count is stored in both
`value` and the `raw_adc` column, so a query can read one column
regardless of which zone the row came from.

## Validation policy

Station streams get **Tier-1 only**. The validator applies this through
a per-source-group policy (`StreamPolicy::Tier1Only` on the
`charstation` group), not through string matching scattered across the
server — see `groundtruth-validator/src/config.rs`.

| Tier | Garden | Station | Why |
|------|--------|---------|-----|
| Tier-1 value + raw range | ✅ | ✅ | A shorted probe reads out of range on a bench exactly as it does in a bed |
| Tier-1 stuck detection | ✅ | ✅ | Count-based, as for garden raw ADC |
| Tier-1 rate of change | ✅ | ❌ | Moving a probe from sand to water between two readings is the experiment |
| Tier-2 health score | ✅ | ❌ | Reads silence as failure; a bench unit is *supposed* to be silent between sessions |
| Tier-3 quarantine | ✅ | ❌ | Would quarantine every unit in the drawer |

Concretely: station streams never appear in `/api/sensor-health`,
never appear in `/api/sensor-health/quarantine`, are not counted in
`total_active_sensors`, and never get
`groundtruth_sensor_health_score` or `groundtruth_sensor_quarantined`
gauges. No health or quarantine state is created for them at all.

Value gauges behave normally: `groundtruth_moisture_raw_adc{zone="charstation"}`
is exported while a session runs and drops out five minutes after the
last reading, and `groundtruth_last_reading_age_seconds` keeps climbing
through the weeks between sessions. That is the intended picture — the
series ends when the session does.

One thing to watch on the first run: stuck detection uses the inherited
`stuck_threshold` of 0.01, which against integer ADC counts is an
exact-repeat detector. Six identical rounded means in a row is unlikely
but not impossible in a very still batch, so a few `suspect` flags in
air or water are worth reading as data rather than as faults — the
noise floor those flags depend on is precisely what the station is
being built to measure.

## Session log

Each message published to `charstation/session` appends one row to
`charstation_sessions`:

```sql
CREATE TABLE charstation_sessions (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at       TEXT    NOT NULL,   -- RFC 3339, stamped at receipt
    unit_id          TEXT    NOT NULL,
    batch_label      TEXT    NOT NULL,   -- "sand-100gkg", "air", "water"
    insertion_number INTEGER,            -- nullable
    notes            TEXT                -- nullable
);
CREATE INDEX idx_charstation_sessions_started_at ON charstation_sessions (started_at);
```

Payload:

```json
{"unit_id": "3", "batch": "sand-100gkg", "insertion": 4, "notes": "reseated probe"}
```

`unit_id` and `batch` are required; `insertion` and `notes` are
optional. `unit_id` may be a string or a number. The row is timestamped
at the moment the server receives the message, not from anything in the
payload — the log records what the bench was doing when you said so.

Because the message is retained, a server restart re-delivers the last
one and appends a fresh row stamped at restart time. That is harmless
under the join rule below — it re-asserts the conditions currently in
force — but it is why you will occasionally see the same batch logged
twice with different timestamps.

The log is **append-only**: every message is a new row, and there is no
edit path. A mistake is corrected by publishing again, which is also
what makes the join below well-defined. A malformed payload is logged
and dropped; the ingest loop never stops for it. A `unit_id` outside
1–16 is recorded with a warning rather than discarded — the row is the
only record of what was on the bench — but no readings stream will ever
match it.

## Joining readings to conditions

There is no foreign key between `readings` and `charstation_sessions`.
The join is by time, and the rule is:

> **A reading belongs to the most recent session row for its unit whose
> `started_at` is at or before the reading's `timestamp`** — i.e.
> readings falling between two consecutive session rows belong to the
> earlier one.

```sql
SELECT
    r.timestamp,
    r.value       AS raw_adc,
    r.quality,
    s.batch_label,
    s.insertion_number,
    s.notes
FROM readings r
LEFT JOIN charstation_sessions s
  ON s.unit_id = r.zone_id
 AND s.started_at = (
       SELECT MAX(s2.started_at)
       FROM charstation_sessions s2
       WHERE s2.unit_id = r.zone_id
         AND s2.started_at <= r.timestamp
     )
WHERE r.zone = 'charstation'
  AND r.metric = 'raw_adc'
ORDER BY r.timestamp;
```

Two consequences worth knowing:

- Readings that arrive **before** the first session row for a unit join
  to `NULL`. Publish the session message before starting the unit, and
  they won't exist. If they do, they are readings whose conditions were
  never recorded, and `LEFT JOIN` keeps them visible rather than
  quietly dropping them.
- Timestamps are RFC 3339 strings in both tables and sort correctly as
  text, so `MAX()` and `<=` mean what they look like. Keep them in UTC
  — the server writes `Utc::now().to_rfc3339()` on both sides.

Summarizing a batch, which is what the t=0 baseline needs:

```sql
SELECT s.unit_id, s.batch_label, COUNT(*) AS n,
       AVG(r.value) AS mean_adc,
       MIN(r.value) AS min_adc,
       MAX(r.value) AS max_adc
FROM readings r
JOIN charstation_sessions s
  ON s.unit_id = r.zone_id
 AND s.started_at = (SELECT MAX(s2.started_at) FROM charstation_sessions s2
                     WHERE s2.unit_id = r.zone_id AND s2.started_at <= r.timestamp)
WHERE r.zone = 'charstation' AND r.metric = 'raw_adc'
  AND r.quality != 'invalid'   -- NOT quality = 'good' — see below
GROUP BY s.unit_id, s.batch_label, s.started_at
ORDER BY s.started_at;
```

## Analyzing station data

**Do not filter to `quality = 'good'` when computing noise floors from
station data.** Use this instead:

```sql
AND r.quality != 'invalid'
```

That keeps every reading in the session window and drops only the ones
Tier-1 judged physically impossible — a raw value outside 100–3995,
meaning a shorted, disconnected, or saturated probe. Those are genuinely
not measurements of the medium and are the only rows worth excluding.

The reason the obvious filter is wrong: in still media — air, water, a
settled sand batch — Tier-1's count-based stuck rule fires on six
consecutive readings within `stuck_threshold` of each other, and that
threshold is 0.01 against integer ADC means. On integers it is an
exact-repeat detector, and a genuinely quiet sensor in a genuinely
still medium *does* repeat its rounded mean. Those readings get marked
`suspect` while being perfectly healthy.

So the `suspect` flags are false positives of the very threshold this
station exists to replace. Excluding them would drop precisely the
quietest readings — the ones with the least spread — and bias the
measured noise floor **low**, which would then justify keeping the
threshold that produced the flags. The station would have measured its
own assumption.

One more thing that shapes what the numbers mean: **the station's ADC
is not the garden's ADC.** The bench runs an ESP32-C3 Super Mini, the
deployed nodes run LOLIN S2 Minis, and the same probe read through
different silicon does not have to produce the same counts. Everything
the station is built to measure — unit-to-unit spread, per-unit noise
floor, drift across monthly re-characterizations — goes through one
ADC for every unit, so those comparisons hold. Mapping a bench count
onto a deployed reading is the operation that needs a cross-board check
first.

Read those flags as data. A batch with many `suspect` rows is a batch
where the sensor was quiet, which is information about the noise floor,
not a reason to throw the rows away. Once the real noise floor is
measured, `stuck_threshold` gets set from it and the flags stop
appearing on healthy readings — which is the point of the exercise.

```sql
-- Noise floor for one session window: spread of the raw counts,
-- invalid rows excluded, suspect rows kept.
SELECT s.unit_id, s.batch_label, s.started_at,
       COUNT(*)                                   AS n,
       AVG(r.value)                               AS mean_adc,
       MAX(r.value) - MIN(r.value)                AS peak_to_peak,
       SUM(r.quality = 'suspect')                 AS suspect_rows
FROM readings r
JOIN charstation_sessions s
  ON s.unit_id = r.zone_id
 AND s.started_at = (SELECT MAX(s2.started_at) FROM charstation_sessions s2
                     WHERE s2.unit_id = r.zone_id AND s2.started_at <= r.timestamp)
WHERE r.zone = 'charstation'
  AND r.metric = 'raw_adc'
  AND r.quality != 'invalid'
GROUP BY s.unit_id, s.batch_label, s.started_at;
```

## Running a session

Everything is driven from the Pi with `mosquitto_pub`; there is no UI.

```bash
# 1. Tell the station which unit is on the bench (retained, so the
#    firmware picks it up whenever it reconnects).
mosquitto_pub -h 192.168.0.114 -r -t charstation/control/unit -m 3

# 2. Record what the unit is sitting in. One message per condition.
mosquitto_pub -h 192.168.0.114 -r -t charstation/session \
  -m '{"unit_id": "3", "batch": "sand-100gkg", "insertion": 4, "notes": "reseated probe"}'

# 3. Readings flow at 2 s until you move the unit. Then publish the
#    next session message and keep going.
mosquitto_pub -h 192.168.0.114 -r -t charstation/session \
  -m '{"unit_id": "3", "batch": "water", "insertion": 5}'
```

To end a session, unplug the unit or clear the control topic:

```bash
mosquitto_pub -h 192.168.0.114 -r -t charstation/control/unit -n
```

The station publishes nothing until a unit id has been received, so a
cleared control topic leaves the bench quiet rather than attributing
readings to whichever unit was measured last. Five minutes later the
value gauges drop out of `/metrics`, which is the correct picture of a
bench with nothing on it.
