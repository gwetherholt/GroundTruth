# GroundTruth

Full-stack IoT garden monitoring and automation platform. Monitors soil moisture, temperature, and humidity across 7 outdoor raised beds and a greenhouse using ESP32-C3 sensor nodes communicating over MQTT.

## Architecture

```
ESP32-C3 Nodes ──► MQTT (Mosquitto) ──► Rust Subscriber ──► SQLite
      │                                        │
      └── DHT22 + Soil Moisture               └── Web Dashboard
```

**Protocol:** MQTT via Mosquitto broker (contrast with [QuailSync V2](https://github.com/gwetherholt/QuailSyncV2)'s raw WebSocket approach — deliberate protocol choice per use case)

### Stack
- **Firmware:** ESP32-C3 Super Mini with DHT22 + I2C capacitive soil moisture sensors
- **Broker:** Eclipse Mosquitto 2 (Docker)
- **Backend:** Rust async service (rumqttc) → SQLite
- **Dashboard:** Real-time web UI with per-bed visualization
- **Infrastructure:** Docker Compose on Raspberry Pi 5, GitHub Actions CI

### MQTT Topics
```
groundtruth/bed/{bed_id}/moisture
groundtruth/bed/{bed_id}/temperature
groundtruth/bed/{bed_id}/humidity
groundtruth/greenhouse/temperature
groundtruth/greenhouse/humidity
```

## Setup

### Prerequisites
- Docker & Docker Compose
- Rust toolchain (for local development)

### Run
```bash
# Start Mosquitto broker + Rust backend
docker compose up -d

# Test with a manual MQTT publish
docker exec groundtruth-mosquitto mosquitto_pub \
  -t "groundtruth/bed/1/moisture" -m "45.2"
```

### Development
```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

## Roadmap
- [x] Phase 0: Single-bed prototype — validate MQTT pipeline end-to-end
- [ ] Phase 1: Monitoring MVP — dashboard, all 7 beds + greenhouse, PETG enclosures
- [ ] Phase 2: Alerts & automation — relay-controlled solenoid valves, scheduled watering
- [ ] Phase 3: Intelligence — trend analysis, weather API integration, watering optimization

## Hardware
- ESP32-C3 Super Mini boards
- DHT22 temperature/humidity sensors
- I2C capacitive soil moisture sensors (Adafruit STEMMA / DFRobot v2.0)
- 18650 lithium cells with TP4056 charge boards (deep sleep targeting 60-90 day battery life)
- 3D-printed PETG enclosures (OpenSCAD)

## License
MIT
