import type { Quality } from "@/lib/types";

const styles: Record<Quality, string> = {
  good: "bg-green-100 text-green-800 dark:bg-green-900/40 dark:text-green-200",
  suspect: "bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-200",
  invalid: "bg-red-100 text-red-800 dark:bg-red-900/40 dark:text-red-200",
};

export function QualityBadge({ quality }: { quality: Quality }) {
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${styles[quality]}`}
    >
      {quality}
    </span>
  );
}
