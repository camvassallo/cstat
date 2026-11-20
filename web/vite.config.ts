import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Read `.env` from the repo root, where the backend's lives, so frontend
  // settings sit beside it. Only `VITE_`-prefixed variables reach the bundle,
  // so DATABASE_URL and the API keys in that file stay out of it.
  envDir: '..',
  build: {
    // With routes code-split (issue #267) the largest remaining chunk is the
    // shared AG Grid vendor chunk, ~700 kB raw / ~190 kB gzip. That is the real
    // floor for the Rankings landing route, which is a grid — so the default
    // 500 kB limit fired on every build and had stopped carrying information.
    // Keep the ceiling just above the floor so a genuine regression still
    // warns rather than hiding under a permanently-tripped threshold.
    chunkSizeWarningLimit: 750,
  },
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
    },
  },
})
