# GroundTruth Grafana setup

Wire GroundTruth's Prometheus metrics into the existing QuailSync
Prometheus + Grafana stack on the Pi. One-time setup.

## Prerequisites

- GroundTruth Rust backend deployed with `/metrics` endpoint working
- QuailSync's Prometheus + Grafana already running on the Pi
- SSH access to the Pi

Verify the metrics endpoint responds before doing anything else:

```bash
curl http://192.168.0.114:3002/metrics | head -5
```

Expected output starts with `# HELP groundtruth_...`. If empty, fire a
test publish first to wake up the lazy metric initializers:

```bash
mosquitto_pub -h 192.168.0.114 -t 'groundtruth/bed/1/moisture' -m '50'
```

Then re-curl.

## 1. Add the GroundTruth scrape job to Prometheus

On the Pi:

```bash
cd /home/gwetherholt/QuailSyncV2
sudo cp prometheus.yml prometheus.yml.bak
```

Then edit `prometheus.yml` and add this entry under `scrape_configs:`
alongside the existing `quailsync` job:

```yaml
  - job_name: "groundtruth"
    static_configs:
      - targets: ["localhost:3002"]
    metrics_path: /metrics
    scrape_interval: 30s
```

Restart Prometheus to pick up the new config:

```bash
sudo docker compose restart prometheus
```

## 2. Verify the scrape target is healthy

Open in a browser:

```
http://192.168.0.114:9090/targets
```

You should see two jobs: `quailsync` (existing) and `groundtruth` with
state `UP`. If `groundtruth` shows `DOWN`, see Troubleshooting below.

You can also test directly in the Prometheus query UI:

```
http://192.168.0.114:9090/graph?g0.expr=groundtruth_readings_total
```

Should return at least one series.

## 3. Import the Grafana dashboard

Open Grafana at `http://192.168.0.114:3001` and sign in.

- Dashboards → New → Import
- Click "Upload JSON file"
- Select `docs/grafana-dashboard.json` from this repo
- When asked for a data source, select the existing Prometheus
- Click Import

The dashboard appears with six panels: moisture, temperature, humidity,
raw ADC, validation quality stats, and reading rate.

## 4. Generate demo data (optional)

If your C3 is currently publishing only stuck readings (or no readings),
the dashboard will look empty. Use the helper script to populate it
with realistic-looking moisture data:

```bash
./scripts/seed-demo-data.sh
```

Wait ~30 seconds after the script finishes — Prometheus needs one
scrape cycle to pick up the new metric values.

## Troubleshooting

### Target shows DOWN with "connection refused"

Prometheus reaches GroundTruth via `localhost:3002` from inside its
container. This works when:
- Prometheus uses host networking (the QuailSync setup), AND
- GroundTruth's Docker port mapping binds to `0.0.0.0:3002`, not
  `127.0.0.1:3002`.

Verify the bind address:

```bash
sudo ss -tlnp | grep 3002
```

Look for `0.0.0.0:3002` or `*:3002`. If you see `127.0.0.1:3002`, edit
GroundTruth's `docker-compose.yml` and explicitly set the host bind:

```yaml
ports:
  - "0.0.0.0:3002:3001"
```

### Empty `/metrics` output

Prometheus metrics use lazy initialization. If no readings have been
ingested since server start, the metric vectors don't exist yet — and
Prometheus's text encoder emits nothing for empty registries. Fix by
publishing one reading:

```bash
mosquitto_pub -h 192.168.0.114 -t 'groundtruth/bed/1/moisture' -m '50'
```

### Dashboard panels show "No data"

Two common causes:

1. The data source UID in the imported dashboard JSON is `prometheus`,
   but your Grafana might use a different UID (often a random string
   like `cd28bf9e-1a2c-...`). Edit any panel → change the data source
   in the dropdown → save the dashboard. Grafana will rewrite all panel
   queries to use the new UID.

2. Prometheus has scraped GroundTruth but the metric you're querying
   has never had a value. Check at
   `http://192.168.0.114:9090/graph?g0.expr=groundtruth_moisture_percent`
   — if empty, no `good`-quality moisture readings have been ingested
   yet. Run `./scripts/seed-demo-data.sh`.

### Reverting the Prometheus config

```bash
cd /home/gwetherholt/QuailSyncV2
sudo cp prometheus.yml.bak prometheus.yml
sudo docker compose restart prometheus
```
