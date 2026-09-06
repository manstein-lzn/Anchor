import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': { target: process.env.ANCHOR_WEB_API_URL || 'http://127.0.0.1:8090', changeOrigin: true },
      '/health': { target: process.env.ANCHOR_WEB_API_URL || 'http://127.0.0.1:8090', changeOrigin: true },
    },
  },
});
