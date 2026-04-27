/*
 * GroundTruth — ESP32-S2 Mini Sensor Node Firmware (Phase 0)
 *
 * Reads DHT22 (temp/humidity) and DFRobot SEN0308 capacitive soil moisture
 * sensor (analog), publishes to MQTT, then enters deep sleep.
 *
 * Target: LOLIN S2 Mini (ESP32-S2FN4R2, 4MB flash, 2MB PSRAM, WiFi only — NO BLE)
 *
 * Wiring (S2 Mini, see board pinout):
 *   DHT22:    VCC  -> 3.3V, GND -> GND, DATA -> GPIO16
 *             (10kΩ pull-up resistor between DATA and 3.3V — REQUIRED)
 *   SEN0308:  VCC  -> 3.3V, GND -> GND, AOUT -> GPIO3 (ADC1_CH2)
 *
 * Why these pins:
 *   - GPIO3 is on ADC1, which works while WiFi is active.
 *   - DO NOT use GPIO11–20 for the soil sensor — those are ADC2, which is
 *     blocked while WiFi is connected on ESP32-S2.
 *   - GPIO16 is just a safe digital pin for DHT22 (any free GPIO works).
 *
 * Power: use a real 5V 2A USB-C wall adapter. A laptop USB port can brown
 * out when WiFi radio fires up. Tin all wire ends before screw terminals.
 */

#include <WiFi.h>
#include <PubSubClient.h>
#include <DHT.h>

// ─── Configuration ───────────────────────────────────────────────
const char* WIFI_SSID     = "YOUR_SSID";
const char* WIFI_PASSWORD = "YOUR_PASSWORD";
const char* MQTT_BROKER   = "192.168.0.114";   // quailsync Pi LAN IP
const int   MQTT_PORT     = 1883;
const char* BED_ID        = "1";               // Change per node before flash

#define DHT_PIN       16                       // safe digital pin on S2 Mini
#define DHT_TYPE      DHT22
#define SOIL_PIN      3                        // ADC1_CH2 — WiFi-safe
#define SLEEP_MINUTES 5
#define SLEEP_US      (SLEEP_MINUTES * 60ULL * 1000000ULL)

// SEN0308 calibration — measure YOUR sensor in dry air and a glass of water,
// update these values, reflash. Defaults are typical starting points.
const int SOIL_RAW_DRY = 2800;   // raw ADC reading in dry air (~0%)
const int SOIL_RAW_WET = 1200;   // raw ADC reading fully submerged (~100%)

// ─── Globals ─────────────────────────────────────────────────────
WiFiClient wifiClient;
PubSubClient mqtt(wifiClient);
DHT dht(DHT_PIN, DHT_TYPE);

char topicBuf[64];
char payloadBuf[16];

// ─── WiFi ────────────────────────────────────────────────────────
void connectWiFi() {
    WiFi.mode(WIFI_STA);
    WiFi.begin(WIFI_SSID, WIFI_PASSWORD);

    int attempts = 0;
    while (WiFi.status() != WL_CONNECTED && attempts < 20) {
        delay(500);
        attempts++;
    }

    if (WiFi.status() != WL_CONNECTED) {
        Serial.println("WiFi failed, sleeping...");
        esp_deep_sleep(SLEEP_US);
    }

    Serial.printf("WiFi connected: %s\n", WiFi.localIP().toString().c_str());
}

// ─── MQTT ────────────────────────────────────────────────────────
void connectMQTT() {
    mqtt.setServer(MQTT_BROKER, MQTT_PORT);

    char clientId[32];
    snprintf(clientId, sizeof(clientId), "gt-bed-%s", BED_ID);

    int attempts = 0;
    while (!mqtt.connected() && attempts < 5) {
        if (mqtt.connect(clientId)) {
            Serial.println("MQTT connected");
        } else {
            Serial.printf("MQTT connect failed rc=%d, retrying...\n", mqtt.state());
            delay(1000);
            attempts++;
        }
    }

    if (!mqtt.connected()) {
        Serial.println("MQTT failed, sleeping...");
        esp_deep_sleep(SLEEP_US);
    }
}

// ─── Publish ─────────────────────────────────────────────────────
void publishReading(const char* metric, float value) {
    snprintf(topicBuf, sizeof(topicBuf), "groundtruth/bed/%s/%s", BED_ID, metric);
    dtostrf(value, 4, 1, payloadBuf);
    mqtt.publish(topicBuf, payloadBuf);
    Serial.printf("Published %s = %s\n", topicBuf, payloadBuf);
}

// ─── Soil moisture: average several reads to smooth ADC noise ────
float readSoilMoisturePercent() {
    const int samples = 16;
    long sum = 0;
    for (int i = 0; i < samples; i++) {
        sum += analogRead(SOIL_PIN);
        delay(5);
    }
    int raw = sum / samples;
    Serial.printf("Soil raw ADC: %d\n", raw);

    // Note: DRY raw > WET raw on capacitive sensors (less capacitance = higher ADC).
    // Map dry->0%, wet->100% with constraint.
    float pct = (float)(SOIL_RAW_DRY - raw) * 100.0f /
                (float)(SOIL_RAW_DRY - SOIL_RAW_WET);
    if (pct < 0.0f)   pct = 0.0f;
    if (pct > 100.0f) pct = 100.0f;
    return pct;
}

// ─── Main ────────────────────────────────────────────────────────
void setup() {
    Serial.begin(115200);
    delay(100);

    Serial.printf("\nGroundTruth node bed/%s waking up (S2 Mini)...\n", BED_ID);

    connectWiFi();
    connectMQTT();

    // DHT22
    dht.begin();
    delay(2000);  // DHT22 needs settling time after power-on
    float temperature = dht.readTemperature(true);  // Fahrenheit
    float humidity    = dht.readHumidity();

    if (!isnan(temperature)) {
        publishReading("temperature", temperature);
    } else {
        Serial.println("DHT22 temperature read failed");
    }
    if (!isnan(humidity)) {
        publishReading("humidity", humidity);
    } else {
        Serial.println("DHT22 humidity read failed");
    }

    // SEN0308 analog soil moisture
    float moisture = readSoilMoisturePercent();
    publishReading("moisture", moisture);

    // Ensure MQTT publishes are flushed
    mqtt.loop();
    delay(200);
    mqtt.disconnect();

    Serial.printf("Sleeping for %d minutes...\n", SLEEP_MINUTES);
    esp_deep_sleep(SLEEP_US);
}

void loop() {
    // Never reached — deep sleep resets to setup()
}
