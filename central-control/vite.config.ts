import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [tailwindcss()],
  root: ".",
  build: {
    outDir: "dist/client",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": "http://localhost:8099",
      "/device": { target: "ws://localhost:8099", ws: true },
      "/ws": { target: "ws://localhost:8099", ws: true },
    },
  },
});
