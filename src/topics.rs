/// Parsed representation of a GroundTruth MQTT topic.
#[derive(Debug, PartialEq)]
pub struct TopicReading {
    /// "bed" or "greenhouse"
    pub zone: String,
    /// bed_id (e.g. "1", "2") or "greenhouse"
    pub zone_id: String,
    /// "moisture", "temperature", or "humidity"
    pub metric: String,
}

/// Parse a GroundTruth MQTT topic into its components.
///
/// Expected formats:
///   groundtruth/bed/{bed_id}/moisture
///   groundtruth/bed/{bed_id}/temperature
///   groundtruth/bed/{bed_id}/humidity
///   groundtruth/greenhouse/temperature
///   groundtruth/greenhouse/humidity
///
/// Returns None for unrecognized topics (including water/command, water/status
/// which are Phase 2 actuation topics handled separately).
pub fn parse_topic(topic: &str) -> Option<TopicReading> {
    let parts: Vec<&str> = topic.split('/').collect();

    if parts.first() != Some(&"groundtruth") {
        return None;
    }

    match parts.as_slice() {
        // groundtruth/bed/{id}/{metric}
        ["groundtruth", "bed", bed_id, metric]
            if matches!(*metric, "moisture" | "temperature" | "humidity") =>
        {
            Some(TopicReading {
                zone: "bed".to_string(),
                zone_id: bed_id.to_string(),
                metric: metric.to_string(),
            })
        }
        // groundtruth/greenhouse/{metric}
        ["groundtruth", "greenhouse", metric]
            if matches!(*metric, "temperature" | "humidity") =>
        {
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
}
