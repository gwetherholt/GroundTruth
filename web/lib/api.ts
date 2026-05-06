import type { Reading, Sensor } from "./types";

const API_BASE = "/api";

async function jsonGet<T>(url: string): Promise<T> {
  const res = await fetch(url, { cache: "no-store" });
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}: ${res.statusText}`);
  }
  return res.json();
}

export function getSensors(): Promise<Sensor[]> {
  return jsonGet<Sensor[]>(`${API_BASE}/sensors`);
}

export function getHistory(
  zone: string,
  zoneId: string,
  metric: string,
  hours = 24,
): Promise<Reading[]> {
  const params = new URLSearchParams({
    zone,
    zone_id: zoneId,
    metric,
    hours: String(hours),
  });
  return jsonGet<Reading[]>(`${API_BASE}/readings/history?${params}`);
}
