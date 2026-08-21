import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';

export default defineConfig(() => {
  const useE2eBridge = process.env.VITE_NWFLASH_WDIO_E2E === 'true';

  return {
    plugins: [react()],
    resolve: {
      alias: {
        '@nwflash/tauri-core-native': path.resolve(
          __dirname,
          'node_modules/@tauri-apps/api/core.js',
        ),
        ...(useE2eBridge
          ? {
            '@tauri-apps/api/core': path.resolve(__dirname, 'src/test/tauri-core.wdio.ts'),
            '@tauri-apps/api/event': path.resolve(__dirname, 'src/test/tauri-event.wdio.ts'),
          }
          : {}),
        '@nwflash/e2e-bridge': path.resolve(
          __dirname,
          useE2eBridge ? 'src/test/e2e-bridge.wdio.ts' : 'src/test/e2e-bridge.ts',
        ),
      },
    },
    test: {
      environment: 'jsdom',
      globals: true,
    },
  };
});
