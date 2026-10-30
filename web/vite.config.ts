import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  build: {
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            // Separate shared dependencies from console code on Windows and macOS.
            { name: 'vendor', test: /[\\/]node_modules[\\/]/ },
          ],
        },
      },
    },
  },
  server: { host: '127.0.0.1', port: 5173, strictPort: true, proxy: {
    '/api': { target: 'http://127.0.0.1:4317', changeOrigin: true,
      configure(proxy) { proxy.on('proxyReq', request => { request.setHeader('origin', 'http://127.0.0.1:4317'); }); },
    },
  } },
});
