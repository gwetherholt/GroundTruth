//! MQTT topic parsing.
//!
//! Two topic families arrive on the broker:
//!
//! - `groundtruth/...` — the garden: beds and the greenhouse.
//! - `charstation/...` — the sensor characterization station, a bench
//!   fixture that measures one SEN0308 at a time in a prepared batch.
//!   It only ever publishes *raw ADC*: calibration curves are the
//!   study's output, so a calibrated `moisture` value under
//!   `charstation` would be circular and is rejected here.
//!
//! Station control traffic (`charstation/session`,
//! `charstation/control/unit`) is not a reading and is handled by the
//! caller before it reaches [`parse_topic`]; both fail to parse here.

/// The zone name given to characterization-station streams. The
/// validator applies its Tier-1-only policy to this source group.
pub const CHARSTATION_ZONE: &str = "charstation";

/// Retained topic carrying one session-context record.
pub const CHARSTATION_SESSION_TOPIC: &str = "charstation/session";

/// Prefix for retained station control topics (e.g. the unit under
/// test). Carries no readings.
pub const CHARSTATION_CONTROL_PREFIX: &str = "charstation/control/";

/// Highest unit id the station bench can hold — one 16-channel analog
/// mux. Ids run 1..=16.
pub const CHARSTATION_MAX_UNIT: u8 = 16;

#[derive(Debug, PartialEq)]
pub struct TopicReading {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
}

impl TopicReading {
    /// Whether this reading is the raw ADC *side channel* of a garden
    /// node: `moisture_raw` is buffered and attached to the calibrated
    /// `moisture` reading that follows it, rather than stored on its
    /// own.
    ///
    /// The station's `raw_adc` is deliberately not this — there is no
    /// calibrated value to attach it to, so it is stored as a reading
    /// in its own right.
    pub fn is_buffered_raw(&self) -> bool {
        self.metric == "moisture_raw"
    }
}

/// Whether `id` names a bench position on the station: 1..=16, no
/// leading zeros or signs.
pub fn is_charstation_unit(id: &str) -> bool {
    matches!(id.parse::<u8>(), Ok(n) if (1..=CHARSTATION_MAX_UNIT).contains(&n))
        && !id.starts_with('0')
}

pub fn parse_topic(topic: &str) -> Option<TopicReading> {
    let parts: Vec<&str> = topic.split('/').collect();

    if !matches!(
        parts.first(),
        Some(&"groundtruth") | Some(&CHARSTATION_ZONE)
    ) {
        return None;
    }

    match parts.as_slice() {
        ["groundtruth", "bed", bed_id, metric]
            if matches!(
                *metric,
                "moisture" | "moisture_raw" | "temperature" | "humidity"
            ) =>
        {
            Some(TopicReading {
                zone: "bed".to_string(),
                zone_id: bed_id.to_string(),
                metric: metric.to_string(),
            })
        }
        // The station: raw ADC only, plus an optional bench DHT22.
        // `moisture` and `moisture_raw` are absent on purpose.
        ["charstation", unit_id, metric]
            if is_charstation_unit(unit_id) && matches!(*metric, "raw_adc" | "temperature") =>
        {
            Some(TopicReading {
                zone: CHARSTATION_ZONE.to_string(),
                zone_id: unit_id.to_string(),
                metric: metric.to_string(),
            })
        }
        ["groundtruth", "greenhouse", metric] if matches!(*metric, "temperature" | "humidity") => {
            Some(TopicReading {
                zone: "greenhouse".to_string(),
                zone_id: "greenhouse".to_string(),
                metric: metric.to_string(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bed_moisture() {
        let result = parse_topic("groundtruth/bed/1/moisture").unwrap();
        assert_eq!(result.zone, "bed");
        assert_eq!(result.zone_id, "1");
        assert_eq!(result.metric, "moisture");
        assert!(!result.is_buffered_raw());
    }

    #[test]
    fn test_parse_bed_moisture_raw() {
        let result = parse_topic("groundtruth/bed/1/moisture_raw").unwrap();
        assert_eq!(result.metric, "moisture_raw");
        assert!(result.is_buffered_raw());
    }

    #[test]
    fn test_parse_bed_temperature() {
        let result = parse_topic("groundtruth/bed/3/temperature").unwrap();
        assert_eq!(result.zone, "bed");
        assert_eq!(result.zone_id, "3");
        assert_eq!(result.metric, "temperature");
    }

    #[test]
    fn test_parse_bed_humidity() {
        let result = parse_topic("groundtruth/bed/7/humidity").unwrap();
        assert_eq!(result.zone, "bed");
        assert_eq!(result.zone_id, "7");
        assert_eq!(result.metric, "humidity");
    }

    #[test]
    fn test_parse_greenhouse_temperature() {
        let result = parse_topic("groundtruth/greenhouse/temperature").unwrap();
        assert_eq!(result.zone, "greenhouse");
        assert_eq!(result.zone_id, "greenhouse");
        assert_eq!(result.metric, "temperature");
    }

    #[test]
    fn test_parse_greenhouse_humidity() {
        let result = parse_topic("groundtruth/greenhouse/humidity").unwrap();
        assert_eq!(result.zone, "greenhouse");
        assert_eq!(result.zone_id, "greenhouse");
        assert_eq!(result.metric, "humidity");
    }

    #[test]
    fn test_reject_water_command() {
        assert!(parse_topic("groundtruth/bed/1/water/command").is_none());
    }

    #[test]
    fn test_reject_unknown_prefix() {
        assert!(parse_topic("quailsync/brooder/1/temperature").is_none());
    }

    #[test]
    fn test_reject_unknown_metric() {
        assert!(parse_topic("groundtruth/bed/1/pressure").is_none());
    }

    #[test]
    fn test_reject_empty() {
        assert!(parse_topic("").is_none());
    }

    #[test]
    fn test_parse_charstation_raw_adc() {
        let result = parse_topic("charstation/3/raw_adc").unwrap();
        assert_eq!(result.zone, "charstation");
        assert_eq!(result.zone_id, "3");
        assert_eq!(result.metric, "raw_adc");
        // Stored as a reading, not buffered for a calibrated value.
        assert!(!result.is_buffered_raw());
    }

    #[test]
    fn test_parse_charstation_temperature() {
        let result = parse_topic("charstation/16/temperature").unwrap();
        assert_eq!(result.zone, "charstation");
        assert_eq!(result.zone_id, "16");
        assert_eq!(result.metric, "temperature");
    }

    #[test]
    fn test_charstation_accepts_the_whole_unit_range() {
        for unit in 1..=CHARSTATION_MAX_UNIT {
            let topic = format!("charstation/{}/raw_adc", unit);
            assert!(parse_topic(&topic).is_some(), "{topic}");
        }
    }

    #[test]
    fn test_reject_charstation_moisture() {
        // The station records raw ADC only — a calibration curve is
        // the study's output, so calibrated moisture must not be an
        // input to it.
        assert!(parse_topic("charstation/3/moisture").is_none());
        assert!(parse_topic("charstation/3/moisture_raw").is_none());
    }

    #[test]
    fn test_reject_charstation_other_metrics() {
        assert!(parse_topic("charstation/3/humidity").is_none());
        assert!(parse_topic("charstation/3/pressure").is_none());
    }

    #[test]
    fn test_reject_charstation_unit_out_of_range() {
        assert!(parse_topic("charstation/0/raw_adc").is_none());
        assert!(parse_topic("charstation/17/raw_adc").is_none());
        assert!(parse_topic("charstation/-1/raw_adc").is_none());
        assert!(parse_topic("charstation/03/raw_adc").is_none());
        assert!(parse_topic("charstation/abc/raw_adc").is_none());
        assert!(parse_topic("charstation//raw_adc").is_none());
    }

    #[test]
    fn test_reject_charstation_control_topics() {
        // Handled by the ingest loop as control messages, never as
        // readings.
        assert!(parse_topic(CHARSTATION_SESSION_TOPIC).is_none());
        assert!(parse_topic("charstation/control/unit").is_none());
        assert!(parse_topic("charstation/3/raw_adc/extra").is_none());
    }

    #[test]
    fn test_reject_bed_raw_adc_topic() {
        // Garden nodes publish `moisture_raw`; `raw_adc` is the
        // station's spelling and is not accepted under `bed`.
        assert!(parse_topic("groundtruth/bed/1/raw_adc").is_none());
    }

    #[test]
    fn test_is_charstation_unit() {
        assert!(is_charstation_unit("1"));
        assert!(is_charstation_unit("16"));
        assert!(!is_charstation_unit("0"));
        assert!(!is_charstation_unit("17"));
        assert!(!is_charstation_unit("007"));
        assert!(!is_charstation_unit(""));
        assert!(!is_charstation_unit("one"));
    }

    #[test]
    fn test_reject_greenhouse_moisture_raw() {
        assert!(parse_topic("groundtruth/greenhouse/moisture_raw").is_none());
    }
}
