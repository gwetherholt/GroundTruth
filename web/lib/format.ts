export function formatValue(metric: string, value: number): string {
  switch (metric) {
    case "moisture":
    case "humidity":
      return `${value.toFixed(1)}%`;
    case "temperature":
      return `${value.toFixed(1)}°F`;
    case "moisture_raw":
      return `${value.toFixed(0)} ADC`;
    default:
      return value.toFixed(1);
  }
}

export function formatRelativeTime(iso: string): string {
  const then = new Date(iso).getTime();
  const seconds = Math.floor((Date.now() - then) / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function formatSensorLabel(zone: string, zoneId: string): string {
  if (zone === "greenhouse") return "Greenhouse";
  return `Bed ${zoneId}`;
}

export function formatMetricLabel(metric: string): string {
  switch (metric) {
    case "moisture":
      return "Soil moisture";
    case "moisture_raw":
      return "Raw ADC";
    case "temperature":
      return "Temperature";
    case "humidity":
      return "Humidity";
    default:
      return metric;
  }
}
