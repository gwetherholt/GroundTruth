/*
 * GroundTruth — Sensor Characterization Station Firmware
 *
 * A bench fixture, not a garden node. It sits on a desk with one
 * SEN0308 in a prepared sand batch and reports raw ADC counts every
 * two seconds while a session is running. There is no deep sleep and
 * no calibration: the whole point of the station is to *produce* the
 * calibration curve, so publishing a calibrated moisture percentage
 * here would be circular. The server rejects it if you try.
 *
 * ─── Board target ───────────────────────────────────────────────
 * ESP32-C3 Super Mini (the Suuoo 3-pack). This is BENCH hardware,
 * bought for the station and separate from the garden.
 *
 * The deployed garden nodes are LOLIN S2 Minis (ESP32-S2FN4R2) and
 * stay that way — see groundtruth_node.ino. Do not copy pin choices
 * between the two sketches: the C3 and the S2 have different ADC pin
 * maps, different strapping pins, and different USB pins. Nearly
 * every pin number below differs from the garden node's.
 *
 *   Arduino IDE:  Tools > Board > esp32 > "ESP32C3 Dev Module"
 *                 Tools > USB CDC On Boot > "Enabled"
 *                 (without CDC on boot there is no Serial over USB on
 *                  a C3 Super Mini, and this sketch is driven by its
 *                  log)
 *   arduino-cli:  --fqbn esp32:esp32:esp32c3:CDCOnBoot=cdc
 *                 Confirm the id on your install first:
 *                   arduino-cli board listall | grep -i c3
 *                 The Super Mini is a generic clone, so the Dev Module
 *                 profile is the safe pick; lolin_c3_mini exists but
 *                 targets a different board.
 *
 * Wiring (C3 Super Mini):
 *   SEN0308:  VCC -> 3.3V, GND -> GND, AOUT -> GPIO3 (ADC1_CH3)
 *   DHT22 (optional bench ambient, see ENABLE_DHT22):
 *             VCC -> 3.3V, GND -> GND, DATA -> GPIO4
 *             (10kΩ pull-up between DATA and 3.3V — REQUIRED)
 *
 * Why GPIO3: it is on ADC1, which keeps working while WiFi is active.
 * On the C3, ADC1 is GPIO0–GPIO4 and ADC2 is GPIO5 alone — and ADC2
 * is unusable while the radio is up, exactly as on the S2. So GPIO0–4
 * are the only candidates for the analog input, and GPIO3 is one of
 * them: the pin number carries over from the garden node, the channel
 * behind it does not (ADC1_CH2 there, ADC1_CH3 here).
 *
 * ─── Pins to leave alone on this board ──────────────────────────
 *   GPIO8   strapping — sampled at reset; also the onboard LED on most
 *           Super Mini clones, so it is already loaded
 *   GPIO9   strapping — BOOT; low at reset = download mode
 *   GPIO2   strapping as well; must not be pulled low at reset
 *   GPIO18  USB D-   ) native USB Serial/JTAG, which is the console
 *   GPIO19  USB D+   ) this sketch logs to. Wiring here kills it.
 *   GPIO11–17  SPI flash. Not broken out on the Super Mini header,
 *           and driving them bricks flash access mid-run. This is
 *           where the garden node's DHT pin (GPIO16) lived — it does
 *           not exist here, which is why the DHT moves to GPIO4.
 *
 * ─── Mux select lines, when the seam gets filled in ─────────────
 * A CD74HC4067 needs four select lines (S0–S3) plus EN. Use GPIO5, 6,
 * 7 and 10 for S0–S3. All four are plain digital pins on the Super
 * Mini header, none is strapping, USB, or flash, and GPIO5 is ADC2 —
 * unusable for analog while WiFi is up, so spending it as a digital
 * output costs nothing. Tie EN to GND to keep the mux always on, or
 * drive it from a spare (GPIO0 or GPIO1) to float the input between
 * reads. The mux COM/SIG output goes to GPIO3, where the single probe
 * is wired today — so the analog path does not change when the mux
 * arrives.
 *
 * ─── One caveat this board introduces ───────────────────────────
 * The station's ADC is NOT the ADC the garden runs on. An S2 and a C3
 * read the same probe through different silicon, so absolute counts
 * from the bench do not transfer 1:1 to a bed. What the station is
 * built to measure — unit-to-unit spread, per-unit noise floor, drift
 * over months — is measured through ONE ADC for every unit, so those
 * comparisons stay valid. Anything that maps a bench count onto a
 * deployed reading needs a cross-board check first.
 *
 * Topics:
 *   subscribes  charstation/control/unit   (retained) — unit under test
 *   publishes   charstation/{unit}/raw_adc            — mean ADC counts
 *   publishes   charstation/{unit}/temperature        — if ENABLE_DHT22
 *
 * The unit under test is set at runtime, not at flash time. Publish
 * the retained control message from the Pi:
 *
 *   mosquitto_pub -h 192.168.0.114 -r -t charstation/control/unit -m 3
 *
 * Until that message has been received the station publishes nothing:
 * an unlabeled reading is worse than no reading, because it would be
 * attributed to whichever unit was measured last.
 */

#include <WiFi.h>
#include <PubSubClient.h>

// ─── Secrets ─────────────────────────────────────────────────────
// WiFi credentials live in firmware/secrets.h, which is gitignored.
// Copy firmware/secrets.h.example to firmware/secrets.h and fill it
// in before compiling. This repo is public; credentials committed
// here would stay in git history forever.
#include "secrets.h"
#if !defined(WIFI_SSID) || !defined(WIFI_PASSWORD)
#error "secrets.h must define WIFI_SSID and WIFI_PASSWORD - see secrets.h.example"
#endif

// ─── Configuration ───────────────────────────────────────────────
const char* MQTT_BROKER   = "192.168.0.114";   // quailsync Pi LAN IP
const int   MQTT_PORT     = 1883;

#define SOIL_PIN            3      // ADC1_CH3 on the C3 — WiFi-safe
#define PUBLISH_INTERVAL_MS 2000   // bench cadence: 2 s
#define ADC_SAMPLES         16     // averaged per published reading
#define ADC_SAMPLE_GAP_MS   6      // 16 × 6 ms ≈ 100 ms per reading

// Optional bench DHT22. Set to 1 and install the DHT library to log
// ambient temperature alongside the ADC. Absent hardware must never
// stall the loop, so a failed read is skipped, not retried.
#define ENABLE_DHT22        0
#define DHT_PIN             4      // GPIO16 is SPI flash on the C3
#define DHT_TYPE            DHT22
#define DHT_INTERVAL_MS     30000  // ambient moves slowly; 2 s is waste

#if ENABLE_DHT22
#include <DHT.h>
DHT dht(DHT_PIN, DHT_TYPE);
#endif

const char* CONTROL_TOPIC = "charstation/control/unit";

// ─── Globals ─────────────────────────────────────────────────────
WiFiClient wifiClient;
PubSubClient mqtt(wifiClient);

char unitId[8]   = "";     // empty until the control message arrives
char topicBuf[64];
char payloadBuf[16];

unsigned long lastPublishMs = 0;
#if ENABLE_DHT22
unsigned long lastDhtMs = 0;
#endif

// ─── Mux seam ────────────────────────────────────────────────────
// The pre-test is single-sensor: one probe, one ADC pin, moved by
// hand between batches. The 16-channel mux (CD74HC4067) lands HERE —
// selectChannel() gains the S0..S3 digital writes plus a settling
// delay before the ADC is sampled, and the caller starts sweeping
// channels instead of passing a constant. Nothing else in this file
// should need to change: the unit id already arrives at runtime, so a
// mux sweep is a loop over channels publishing under different ids.
//
// Suggested select pins on the C3 Super Mini (see the header block):
//   #define MUX_S0 5
//   #define MUX_S1 6
//   #define MUX_S2 7
//   #define MUX_S3 10
// ...pinMode(OUTPUT) them in setup(), write the four bits of `channel`
// here, then delay a few ms for the analog input to settle before
// readMeanAdc() samples it.
#define MUX_CHANNEL 0

static void selectChannel(uint8_t channel) {
    (void)channel;  // no mux wired yet — single sensor on SOIL_PIN
}

// ─── WiFi ────────────────────────────────────────────────────────
void connectWiFi() {
    if (WiFi.status() == WL_CONNECTED) return;

    WiFi.mode(WIFI_STA);
    WiFi.begin(WIFI_SSID, WIFI_PASSWORD);
    Serial.print("WiFi connecting");

    int attempts = 0;
    while (WiFi.status() != WL_CONNECTED && attempts < 40) {
        delay(250);
        Serial.print(".");
        attempts++;
    }
    Serial.println();

    if (WiFi.status() == WL_CONNECTED) {
        Serial.printf("WiFi connected: %s\n", WiFi.localIP().toString().c_str());
    } else {
        // No sleeping here — the bench node just keeps trying.
        Serial.println("WiFi not connected yet, will retry");
    }
}

// ─── MQTT ────────────────────────────────────────────────────────
// The retained control message is only redelivered on subscribe, so
// the subscription has to be re-issued on every successful connect —
// not just the first. A broker restart drops subscriptions silently;
// without this the station would keep running with a stale unit id,
// or with none at all, and never say so. (The Rust server re-subscribes
// on ConnAck for the same reason.)
void connectMQTT() {
    if (mqtt.connected()) return;

    char clientId[32];
    snprintf(clientId, sizeof(clientId), "gt-charstation-%04X",
             (uint16_t)(ESP.getEfuseMac() & 0xFFFF));

    Serial.printf("MQTT connecting as %s...\n", clientId);
    if (mqtt.connect(clientId)) {
        Serial.println("MQTT connected");
        if (mqtt.subscribe(CONTROL_TOPIC)) {
            Serial.printf("Subscribed to %s\n", CONTROL_TOPIC);
        } else {
            Serial.printf("Subscribe to %s FAILED\n", CONTROL_TOPIC);
        }
    } else {
        Serial.printf("MQTT connect failed rc=%d\n", mqtt.state());
    }
}

// The payload is the unit id as a string: "3", "12". Anything that
// isn't 1..16 is refused loudly rather than published under, because
// a mislabeled unit silently corrupts the whole characterization run.
//
// An empty payload clears the unit and stops publishing — that is what
// `mosquitto_pub -r -t charstation/control/unit -n` does, and it is
// how a session is ended from the Pi.
void onControlMessage(char* topic, byte* payload, unsigned int length) {
    if (strcmp(topic, CONTROL_TOPIC) != 0) return;

    char incoming[8] = "";
    if (length == 0) {
        if (unitId[0] != '\0') {
            unitId[0] = '\0';
            Serial.println("Unit cleared — no unit under test, publishing paused");
        }
        return;
    }
    if (length >= sizeof(incoming)) {
        Serial.printf("Ignoring unit id of length %u on %s\n", length, topic);
        return;
    }
    memcpy(incoming, payload, length);
    incoming[length] = '\0';

    char* end = nullptr;
    long unit = strtol(incoming, &end, 10);
    if (end == incoming || *end != '\0' || unit < 1 || unit > 16) {
        Serial.printf("Ignoring invalid unit id '%s' (expected 1-16)\n", incoming);
        return;
    }

    if (strcmp(unitId, incoming) != 0) {
        snprintf(unitId, sizeof(unitId), "%s", incoming);
        Serial.printf("Unit under test is now %s\n", unitId);
    }
}

// ─── Sampling ────────────────────────────────────────────────────
// The mean of ADC_SAMPLES back-to-back reads spread over ~100 ms.
// Per-read noise on the SEN0308 is the thing being measured here, so
// it is knocked down at the reading level and left visible at the
// session level — 16 samples is a compromise, not a filter.
int readMeanAdc() {
    selectChannel(MUX_CHANNEL);

    long sum = 0;
    for (int i = 0; i < ADC_SAMPLES; i++) {
        sum += analogRead(SOIL_PIN);
        delay(ADC_SAMPLE_GAP_MS);
    }
    // Round rather than truncate: the mean is the measurement.
    return (int)((sum + ADC_SAMPLES / 2) / ADC_SAMPLES);
}

void publishMetric(const char* metric, const char* value) {
    if (unitId[0] == '\0') return;
    snprintf(topicBuf, sizeof(topicBuf), "charstation/%s/%s", unitId, metric);
    mqtt.publish(topicBuf, value);
    Serial.printf("Published %s = %s\n", topicBuf, value);
}

void publishRawAdc() {
    int raw = readMeanAdc();
    snprintf(payloadBuf, sizeof(payloadBuf), "%d", raw);
    publishMetric("raw_adc", payloadBuf);
}

#if ENABLE_DHT22
void publishAmbient() {
    float temperature = dht.readTemperature(true);  // Fahrenheit
    if (isnan(temperature)) {
        // No sensor, or a failed read. Either way the ADC loop keeps
        // going — the DHT22 is context, not the measurement.
        Serial.println("DHT22 read failed, skipping");
        return;
    }
    dtostrf(temperature, 4, 1, payloadBuf);
    publishMetric("temperature", payloadBuf);
}
#endif

// ─── Main ────────────────────────────────────────────────────────
void setup() {
    Serial.begin(115200);
    delay(100);
    Serial.println("\nGroundTruth characterization station starting...");

    // Explicit, not inherited: 0-4095 counts. The C3's ADC is 12-bit,
    // as is the S2's in the garden nodes, so the two produce counts on
    // the same scale even though they are different parts. The station's
    // whole output is ADC counts, so the scale is stated rather than
    // assumed — and it is what the server's 100-3995 raw-range check
    // expects.
    analogReadResolution(12);

#if ENABLE_DHT22
    dht.begin();
#endif

    mqtt.setServer(MQTT_BROKER, MQTT_PORT);
    mqtt.setCallback(onControlMessage);

    connectWiFi();
    connectMQTT();

    Serial.printf("Waiting for a unit id on %s...\n", CONTROL_TOPIC);
}

void loop() {
    // Reconnect without blocking the bench: no sleep, no reboot.
    if (WiFi.status() != WL_CONNECTED) {
        connectWiFi();
    } else if (!mqtt.connected()) {
        connectMQTT();
    }
    mqtt.loop();

    unsigned long now = millis();

    // Publish nothing until a unit id has been received. Sampling is
    // skipped too — an unattributed reading has no value.
    if (unitId[0] != '\0' && now - lastPublishMs >= PUBLISH_INTERVAL_MS) {
        lastPublishMs = now;
        publishRawAdc();
    }

#if ENABLE_DHT22
    if (unitId[0] != '\0' && now - lastDhtMs >= DHT_INTERVAL_MS) {
        lastDhtMs = now;
        publishAmbient();
    }
#endif

    delay(10);
}
