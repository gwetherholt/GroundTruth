# GroundTruth Grafana setup

These steps integrate GroundTruth metrics into the existing QuailSync
Prometheus + Grafana stack on the Pi. They run once after the Rust
backend is updated to expose `/metrics`.

## 1. Verify the metrics endpoint

After deploying the latest Rust backend:

```bash
curl http://192.168.0.114:3002/metrics
```

You should see Prometheus text-format output including
`groundtruth_readings_total`, `groundtruth_moisture_percent`, etc.

If the output is empty or missing the expected metric names, the backend
hasn't yet ingested any readings — flash a sensor or use mosquitto_pub
to inject one first, then re-curl.

## 2. Add the GroundTruth scrape job to Prometheus

Edit `/home/gwetherholt/QuailSyncV2/prometheus.yml` on the Pi. Add the
new job under `scrape_configs:` (alongside the existing `quailsync` job):

```yaml
  - job_name: "groundtruth"
    static_configs:
      - targets: ["localhost:3002"]
    metrics_path: /metrics
    scrape_interval: 30s
```

The full file should now contain two jobs.

Reload Prometheus by restarting its container:

```bash
sudo docker compose -f /home/gwetherholt/QuailSyncV2/docker-compose.yml restart prometheus
```

(Adjust the path if the QuailSync compose file is elsewhere.)

Verify the scrape target is healthy:

```
http://192.168.0.114:9090/targets
```

You should see `groundtruth` listed with state `UP`.

## 3. Import the dashboard into Grafana

Open Grafana at `http://192.168.0.114:3001` and log in.

- Dashboards → New → Import
- Upload JSON file → select `docs/grafana-dashboard.json` from this repo,
  OR paste its contents directly
- When prompted for the data source, select the existing Prometheus
- Click Import

The dashboard appears with six panels: moisture, temperature, humidity,
raw ADC, validation quality stats, and reading rate.

## Troubleshooting

**Targets page shows "Connection refused"**: Prometheus is reaching
GroundTruth via `localhost:3002` from inside the Prometheus container.
This works only when the Prometheus container shares the host network or
when the GroundTruth Docker port mapping binds to all interfaces. Verify:

```bash
sudo ss -tlnp | grep 3002
```

Should show `0.0.0.0:3002` or `*:3002`. If it shows `127.0.0.1:3002`,
update the GroundTruth `docker-compose.yml` ports section accordingly.

**No data in dashboard panels**: confirm metrics are being scraped:

```
http://192.168.0.114:9090/graph?g0.expr=groundtruth_readings_total
```

If the query returns results, the dashboard's Prometheus data source UID
likely doesn't match the imported JSON's `"uid": "prometheus"`. Edit the
panel data source manually after import to point at your real instance.
