# GroundTruth web dashboard

Local-dev Next.js dashboard for the GroundTruth IoT pipeline.

## Setup

```bash
cd web
npm install
```

## Dev

The dev server proxies /api/* to the Rust backend on the Pi by default.

```bash
npm run dev
```

Open http://localhost:3000.

If the Rust backend is on a different host, set `NEXT_PUBLIC_API_BASE`:

```bash
NEXT_PUBLIC_API_BASE=http://10.0.0.50:3001 npm run dev
```

## Build

```bash
npm run build
npm run start
```
