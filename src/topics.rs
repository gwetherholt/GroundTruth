#[derive(Debug, PartialEq)]
pub struct TopicReading {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
}

impl TopicReading {
    pub fn is_raw_adc(&self) -> bool {
        self.metric == "moisture_raw"
    }
}

pub fn parse_topic(topic: &str) -> Option<TopicReading> {
    let parts: Vec<&str> = topic.split('/').collect();

    if parts.first() != Some(&"groundtruth") {
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
        assert!(!result.is_raw_adc());
    }

    #[test]
    fn test_parse_bed_moisture_raw() {
        let result = parse_topic("groundtruth/bed/1/moisture_raw").unwrap();
        assert_eq!(result.metric, "moisture_raw");
        assert!(result.is_raw_adc());
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
    fn test_reject_greenhouse_moisture_raw() {
        assert!(parse_topic("groundtruth/greenhouse/moisture_raw").is_none());
    }
}
