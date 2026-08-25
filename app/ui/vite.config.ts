import { defineConfig } from 'vite';

export default defineConfig({
  // Let the Tauri CLI own the terminal output during `tauri dev`.
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
});
