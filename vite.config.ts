import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react(), tailwindcss()],

  build: {
    // A single 610KB entry chunk meant every visitor paid for react,
    // react-markdown and the icon set before the first pixel, and any deploy
    // invalidated all of it — the hashed filename changes, so a one-line CSS
    // edit re-downloads the whole engine. Splitting by vendor means those
    // chunks keep their hashes across deploys of this app's own code, which is
    // where nearly all of the churn is.
    rollupOptions: {
      output: {
        // Function form, not the object map: Vite 8 bundles with rolldown, which
        // takes only a function here and rejects the object with a bare
        // "manualChunks is not a function".
        //
        // React is separated and nothing else is, deliberately. It is the floor
        // under every render and it never changes between deploys of this app,
        // so its chunk keeps its hash when this app's own code changes. Markdown
        // stays in the entry: it is the expensive parser, but the transcript is
        // the first thing anyone looks at and a second round trip to reach it
        // would cost more than the bytes it saves.
        manualChunks(id: string) {
          if (/node_modules[/\\](react|react-dom|scheduler)[/\\]/.test(id)) return "react";
          return undefined;
        },
      },
    },
    // The chunks above are deliberate; the default warning fires at 500KB and
    // would be noise about a size that is now split.
    chunkSizeWarningLimit: 700,
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
