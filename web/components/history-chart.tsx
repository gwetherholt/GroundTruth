"use client";

import { useEffect, useState } from "react";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { getHistory } from "@/lib/api";
import { formatMetricLabel } from "@/lib/format";
import type { Reading } from "@/lib/types";

interface Props {
  zone: string;
  zoneId: string;
  metric: string;
  hours?: number;
}

export function HistoryChart({ zone, zoneId, metric, hours = 24 }: Props) {
  const [data, setData] = useState<Reading[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    getHistory(zone, zoneId, metric, hours)
      .then((d) => {
        if (!cancelled) setData(d);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [zone, zoneId, metric, hours]);

  if (error) {
    return <div className="text-sm text-red-500">Failed to load: {error}</div>;
  }
  if (!data) {
    return (
      <div className="text-sm text-[hsl(var(--muted-foreground))]">
        Loading...
      </div>
    );
  }
  if (data.length === 0) {
    return (
      <div className="text-sm text-[hsl(var(--muted-foreground))]">
        No readings in the last {hours}h
      </div>
    );
  }

  const chartData = data.map((r) => ({
    time: new Date(r.timestamp).getTime(),
    value: r.value,
    quality: r.quality,
  }));

  return (
    <div>
      <div className="mb-2 text-sm font-medium">
        {formatMetricLabel(metric)} — last {hours}h
      </div>
      <ResponsiveContainer width="100%" height={220}>
        <LineChart data={chartData}>
          <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
          <XAxis
            dataKey="time"
            type="number"
            domain={["dataMin", "dataMax"]}
            tickFormatter={(t) =>
              new Date(t).toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
              })
            }
            stroke="hsl(var(--muted-foreground))"
            fontSize={11}
          />
          <YAxis stroke="hsl(var(--muted-foreground))" fontSize={11} />
          <Tooltip
            contentStyle={{
              backgroundColor: "hsl(var(--card))",
              border: "1px solid hsl(var(--border))",
              borderRadius: 8,
              fontSize: 12,
            }}
            labelFormatter={(t) => new Date(t).toLocaleString()}
          />
          <Line
            type="monotone"
            dataKey="value"
            stroke="rgb(59 130 246)"
            strokeWidth={2}
            dot={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}
