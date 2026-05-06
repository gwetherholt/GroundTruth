import type { Sensor } from "@/lib/types";
import {
  formatMetricLabel,
  formatRelativeTime,
  formatSensorLabel,
  formatValue,
} from "@/lib/format";
import { QualityBadge } from "./quality-badge";

export function SensorCard({ sensor }: { sensor: Sensor }) {
  const { latest } = sensor;
  const label = formatSensorLabel(sensor.zone, sensor.zone_id);
  const metricLabel = formatMetricLabel(sensor.metric);

  return (
    <div className="rounded-xl border border-[hsl(var(--border))] bg-[hsl(var(--card))] p-4 shadow-sm">
      <div className="flex items-start justify-between gap-2">
        <div>
          <div className="text-sm text-[hsl(var(--muted-foreground))]">
            {label}
          </div>
          <div className="text-base font-medium">{metricLabel}</div>
        </div>
        {latest && <QualityBadge quality={latest.quality} />}
      </div>
      <div className="mt-3 text-3xl font-semibold">
        {latest ? formatValue(sensor.metric, latest.value) : "—"}
      </div>
      {latest && (
        <div className="mt-1 text-xs text-[hsl(var(--muted-foreground))]">
          {formatRelativeTime(latest.timestamp)}
          {latest.validation_reason && (
            <span className="ml-2 italic">{latest.validation_reason}</span>
          )}
        </div>
      )}
    </div>
  );
}
