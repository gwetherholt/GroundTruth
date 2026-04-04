/*
 * GroundTruth — ESP32-C3 Sensor Node Firmware (Phase 0)
 *
 * Reads DHT22 (temp/humidity) and I2C capacitive soil moisture sensor,
 * publishes to MQTT, then enters deep sleep for the configured interval.
 *
 * Target: ESP32-C3 Super Mini
 * Sensors: DHT22, Adafruit STEMMA Soil Sensor (I2C address 0x36)
 *
 * TODO: Flash and test with real hardware
 */

#include <WiFi.h>
#include <PubSubClient.h>
#include <DHT.h>
#include <Wire.h>
#include "Adafruit_seesaw.h"

// ─── Configuration ───────────────────────────────────────────────
const char* WIFI_SSID     = "YOUR_SSID";
const char* WIFI_PASSWORD = "YOUR_PASSWORD";
const char* MQTT_BROKER   = "192.168.0.114";
const int   MQTT_PORT     = 1883;
const char* BED_ID        = "1";  // Change per node

#define DHT_PIN       2
#define DHT_TYPE      DHT22
#define SDA_PIN       8
#define SCL_PIN       9
#define SLEEP_MINUTES 5
#define SLEEP_US      (SLEEP_MINUTES * 60 * 1000000ULL)

// ─── Globals ─────────────────────────────────────────────────────
WiFiClient wifiClient;
PubSubClient mqtt(wifiClient);
DHT dht(DHT_PIN, DHT_TYPE);
Adafruit_seesaw soil;

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

// ─── Main ────────────────────────────────────────────────────────
void setup() {
    Serial.begin(115200);
    delay(100);

    Serial.printf("\nGroundTruth node bed/%s waking up...\n", BED_ID);

    connectWiFi();
    connectMQTT();

    // Read DHT22
    dht.begin();
    delay(2000);  // DHT22 needs time after power-on
    float temperature = dht.readTemperature(true);  // Fahrenheit
    float humidity = dht.readHumidity();

    if (!isnan(temperature)) {
        publishReading("temperature", temperature);
    }
    if (!isnan(humidity)) {
        publishReading("humidity", humidity);
    }

    // Read soil moisture (I2C)
    Wire.begin(SDA_PIN, SCL_PIN);
    if (soil.begin(0x36)) {
        uint16_t capRead = soil.touchRead(0);
        // Map raw capacitive reading to percentage
        // Calibrate these per node: dry air ~300, saturated ~1000
        float moisture = constrain(map(capRead, 300, 1000, 0, 100), 0, 100);
        publishReading("moisture", moisture);
    } else {
        Serial.println("Soil sensor not found");
    }

    // Ensure MQTT publishes are sent
    mqtt.loop();
    delay(200);
    mqtt.disconnect();

    // Deep sleep
    Serial.printf("Sleeping for %d minutes...\n", SLEEP_MINUTES);
    esp_deep_sleep(SLEEP_US);
}

void loop() {
    // Never reached — deep sleep resets to setup()
}
