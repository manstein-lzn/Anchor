import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const apiTarget = process.env.ANCHOR_WEB_API_URL || 'http://127.0.0.1:8077';

export default defineConfig({
  envDir: false,
  plugins: [react()],
  optimizeDeps: { include: ['elkjs/lib/elk.bundled.js', 'libavoid-js'] },
  preview: {
    proxy: { '/': { target: apiTarget, changeOrigin: true } },
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': { target: apiTarget, changeOrigin: true },
      '/graph-relations': { target: apiTarget, changeOrigin: true },
      '/channel-sessions': { target: apiTarget, changeOrigin: true },
      '/channels': { target: apiTarget, changeOrigin: true },
      '/settings': { target: apiTarget, changeOrigin: true },
      '/sessions': { target: apiTarget, changeOrigin: true },
      '/plugins': { target: apiTarget, changeOrigin: true },
      '/graphs': { target: apiTarget, changeOrigin: true },
      '/runs': { target: apiTarget, changeOrigin: true },
      '/timeline': { target: apiTarget, changeOrigin: true },
      '/schedules': { target: apiTarget, changeOrigin: true },
      '/trigger': { target: apiTarget, changeOrigin: true },
    },
  },
});
