import type { Config } from "tailwindcss";

const config: Config = {
  content: ["./app/**/*.{ts,tsx}", "./components/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        good: "rgb(34 197 94)",
        suspect: "rgb(245 158 11)",
        invalid: "rgb(239 68 68)",
      },
    },
  },
  plugins: [],
};

export default config;
