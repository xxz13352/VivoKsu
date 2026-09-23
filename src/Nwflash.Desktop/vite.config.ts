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
      // 默认 5s 在**全量并行**跑时不够：这些用例要等「invoke 解析 -> 状态更新
      // -> React 渲染」乃至同一进程里其它测试文件抢 CPU，实测同一个用例单跑
      // 几十毫秒、全量跑却偶发超过 5s 而被 vitest 自身掐断（报错停在 5xxx ms，
      // 不是我们自己的 waitUntil 超时）。调到 20s：正常路径该立刻返回，
      // 只是不再让「机器忙」把用例判死。
      testTimeout: 20_000,
    },
  };
});
