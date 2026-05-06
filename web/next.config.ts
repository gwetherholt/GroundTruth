import type { NextConfig } from "next";

const config: NextConfig = {
  async rewrites() {
    const apiBase =
      process.env.NEXT_PUBLIC_API_BASE ?? "http://192.168.0.114:3001";
    return [{ source: "/api/:path*", destination: `${apiBase}/api/:path*` }];
  },
};

export default config;
