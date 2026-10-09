import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri dev expects a fixed local port
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    host: 'localhost',
    port: 5173,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: 'chrome105',
    sourcemap: false,
  },
})
