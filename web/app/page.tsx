"use client";

import { useEffect, useState } from "react";
import { getSensors } from "@/lib/api";
import type { Sensor } from "@/lib/types";
import { SensorCard } from "@/components/sensor-card";
import { HistoryChart } from "@/components/history-chart";

const REFRESH_MS = 30_000;

export default function Dashboard() {
  const [sensors, setSensors] = useState<Sensor[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdate, setLastUpdate] = useState<Date | null>(null);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | null = null;

    async function load() {
      try {
        const s = await getSensors();
        if (!cancelled) {
          setSensors(s);
          setError(null);
          setLastUpdate(new Date());
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    }

    load();
    timer = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, []);

  return (
    <main className="mx-auto max-w-6xl p-6">
      <header className="mb-6 flex items-end justify-between">
        <div>
          <h1 className="text-2xl font-semibold">GroundTruth</h1>
          <p className="text-sm text-[hsl(var(--muted-foreground))]">
            Garden sensor dashboard
          </p>
        </div>
        {lastUpdate && (
          <div className="text-xs text-[hsl(var(--muted-foreground))]">
            Updated {lastUpdate.toLocaleTimeString()}
          </div>
        )}
      </header>

      {error && (
        <div className="mb-4 rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-800 dark:border-red-900 dark:bg-red-950 dark:text-red-200">
          API error: {error}. Is the Rust backend running on the Pi?
        </div>
      )}

      <section className="mb-8">
        <h2 className="mb-3 text-lg font-medium">Current readings</h2>
        {sensors === null ? (
          <div className="text-sm text-[hsl(var(--muted-foreground))]">
            Loading...
          </div>
        ) : sensors.length === 0 ? (
          <div className="text-sm text-[hsl(var(--muted-foreground))]">
            No sensors reporting yet.
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {sensors
              .filter((s) => s.metric !== "moisture_raw")
              .map((s) => (
                <SensorCard
                  key={`${s.zone}/${s.zone_id}/${s.metric}`}
                  sensor={s}
                />
              ))}
          </div>
        )}
      </section>

      <section>
        <h2 className="mb-3 text-lg font-medium">History</h2>
        <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
          {sensors
            ?.filter((s) => s.metric !== "moisture_raw")
            .map((s) => (
              <div
                key={`chart-${s.zone}/${s.zone_id}/${s.metric}`}
                className="rounded-xl border border-[hsl(var(--border))] bg-[hsl(var(--card))] p-4"
              >
                <div className="mb-1 text-sm text-[hsl(var(--muted-foreground))]">
                  {s.zone === "greenhouse"
                    ? "Greenhouse"
                    : `Bed ${s.zone_id}`}
                </div>
                <HistoryChart
                  zone={s.zone}
                  zoneId={s.zone_id}
                  metric={s.metric}
                />
              </div>
            ))}
        </div>
      </section>
    </main>
  );
}
