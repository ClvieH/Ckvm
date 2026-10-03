import { defineConfig } from 'vitest/config'

// Kept separate from vite.config.mjs: the Tauri build pipeline owns that file,
// and the unit tests are pure-Node — no DOM, no React plugin, no web deps.
export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
})
