#include <WiFi.h>
#include <PubSubClient.h>

const char* WIFI_SSID     = "YOUR_SSID";
const char* WIFI_PASSWORD = "YOUR_PASSWORD";
const char* MQTT_BROKER   = "192.168.0.114";
const int   MQTT_PORT     = 1883;
const char* BED_ID        = "1";

#define SOIL_PIN      3
#define SLEEP_MINUTES 5
#define SLEEP_US      (SLEEP_MINUTES * 60ULL * 1000000ULL)

const int SOIL_RAW_DRY = 2800;
const int SOIL_RAW_WET = 1200;

WiFiClient wifiClient;
PubSubClient mqtt(wifiClient);

char topicBuf[64];
char payloadBuf[16];

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

void publishFloat(const char* metric, float value) {
    snprintf(topicBuf, sizeof(topicBuf), "groundtruth/bed/%s/%s", BED_ID, metric);
    dtostrf(value, 4, 1, payloadBuf);
    mqtt.publish(topicBuf, payloadBuf);
    Serial.printf("Published %s = %s\n", topicBuf, payloadBuf);
}

void publishInt(const char* metric, int value) {
    snprintf(topicBuf, sizeof(topicBuf), "groundtruth/bed/%s/%s", BED_ID, metric);
    snprintf(payloadBuf, sizeof(payloadBuf), "%d", value);
    mqtt.publish(topicBuf, payloadBuf);
    Serial.printf("Published %s = %s\n", topicBuf, payloadBuf);
}

struct SoilReading {
    int raw;
    float percent;
};

SoilReading readSoilMoisture() {
    const int samples = 16;
    long sum = 0;
    for (int i = 0; i < samples; i++) {
        sum += analogRead(SOIL_PIN);
        delay(5);
    }
    int raw = sum / samples;
    Serial.printf("Soil raw ADC: %d\n", raw);
    float pct = (float)(SOIL_RAW_DRY - raw) * 100.0f /
                (float)(SOIL_RAW_DRY - SOIL_RAW_WET);
    if (pct < 0.0f)   pct = 0.0f;
    if (pct > 100.0f) pct = 100.0f;
    return SoilReading{raw, pct};
}

void setup() {
    Serial.begin(115200);
    delay(100);
    Serial.printf("\nGroundTruth node bed/%s waking up (soil-only)...\n", BED_ID);

    connectWiFi();
    connectMQTT();

    SoilReading r = readSoilMoisture();
    publishInt("moisture_raw", r.raw);
    publishFloat("moisture", r.percent);

    mqtt.loop();
    delay(200);
    mqtt.disconnect();

    Serial.printf("Sleeping for %d minutes...\n", SLEEP_MINUTES);
    esp_deep_sleep(SLEEP_US);
}

void loop() {}
