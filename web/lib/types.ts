export type Quality = "good" | "suspect" | "invalid";

export interface Reading {
  id: number;
  zone: string;
  zone_id: string;
  metric: string;
  value: number;
  raw_adc: number | null;
  quality: Quality;
  validation_reason: string | null;
  timestamp: string;
}

export interface Sensor {
  zone: string;
  zone_id: string;
  metric: string;
  latest: Reading | null;
}
