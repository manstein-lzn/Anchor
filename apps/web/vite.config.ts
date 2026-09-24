import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  optimizeDeps: { include: ['elkjs/lib/elk.bundled.js', 'libavoid-js'] },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: {
      '/graphs': { target: process.env.ANCHOR_WEB_API_URL || 'http://127.0.0.1:8077', changeOrigin: true },
      '/runs': { target: process.env.ANCHOR_WEB_API_URL || 'http://127.0.0.1:8077', changeOrigin: true },
      '/trigger': { target: process.env.ANCHOR_WEB_API_URL || 'http://127.0.0.1:8077', changeOrigin: true },
    },
  },
});
