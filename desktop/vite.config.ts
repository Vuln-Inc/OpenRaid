import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: { port: 1420, strictPort: true },
  clearScreen: false,
  build: {
    target: "es2022",
    rollupOptions: { output: { manualChunks: { "ui-runtime": ["react-aria-components"] } } },
  },
});
