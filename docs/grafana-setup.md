# GroundTruth Grafana setup

GroundTruth ships with its own Prometheus instance. Grafana (running
elsewhere — currently in the QuailSync stack) connects to GroundTruth's
Prometheus as a separate data source.

## Prerequisites

- GroundTruth running on the Pi (see root README)
- Grafana accessible somewhere (default: http://192.168.0.114:3001 from
  the QuailSync stack)

## 1. Verify GroundTruth's Prometheus is running

```bash
curl -s http://localhost:9091/-/healthy
```

Expected: `Prometheus Server is Healthy.`

If the endpoint is unreachable, check that the prometheus container is
running:

```bash
sudo docker compose -f ~/projects/GroundTruth/docker-compose.yml ps prometheus
```

## 2. Verify scrape targets

Open in browser: `http://192.168.0.114:9091/targets`

You should see the `groundtruth` job listed with state `UP`. If it's
`DOWN`, see Troubleshooting below.

## 3. Add the data source to Grafana

In Grafana (e.g. `http://192.168.0.114:3001`):

1. Connections → Data sources → Add new data source
2. Select **Prometheus**
3. **Name:** `Prometheus (GroundTruth)`
4. **UID** (Advanced section): `prometheus-groundtruth`
5. **Connection URL:** `http://localhost:9091`
6. Click **Save & test** — should report "Data source is working"

The UID setting is important: the bundled dashboard JSONs reference this
specific UID so re-imports pick the right source automatically.

## 4. Import the dashboards

For each of:
- `docs/grafana-dashboard.json` (main dashboard)
- `docs/grafana-sensor-health-dashboard.json` (Tier-2 sensor health)

In Grafana:
1. Dashboards → New → Import
2. Upload JSON file or paste contents
3. Grafana will auto-detect the `prometheus-groundtruth` UID requirement;
   confirm the data source is set correctly
4. Click Import

## Troubleshooting

### Target shows DOWN

Check that the server container is healthy:

```bash
sudo docker compose -f ~/projects/GroundTruth/docker-compose.yml ps server
curl http://localhost:3002/metrics | head -5
```

If the metrics endpoint is empty, fire a test publish to wake up the
lazy metric initializers:

```bash
mosquitto_pub -h localhost -t 'groundtruth/bed/1/moisture' -m '50'
```

Then retry the curl.

### Dashboard panels show "No data" after import

Most likely the data source UID in the imported dashboard doesn't match
your Grafana's actual UID for GroundTruth's Prometheus. Either:

- Edit each panel's data source dropdown and pick the right one manually
- Or delete the data source and re-add it ensuring UID is set to
  `prometheus-groundtruth`

### Reverting

To roll back to QuailSync's shared Prometheus:

1. Remove the `prometheus` service from `docker-compose.yml`
2. Add the GroundTruth scrape job back to QuailSync's prometheus.yml
3. Update dashboard JSONs to use UID `prometheus` (or whichever UID
   QuailSync's Prometheus uses in your Grafana)
