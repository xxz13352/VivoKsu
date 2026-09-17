import path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Options } from '@wdio/types';

const directory = path.dirname(fileURLToPath(import.meta.url));

export const config: Options.Testrunner = {
  runner: 'local',
  specs: ['./specs/visual-baseline.e2e.ts'],
  maxInstances: 1,
  services: [['@wdio/tauri-service', {
    mode: 'browser',
    devServerUrl: 'http://127.0.0.1:5173',
    devServer: {
      command: 'npm run preview -- --host 127.0.0.1 --port 5173 --strictPort',
      cwd: path.resolve(directory, '..'),
    },
    captureFrontendLogs: true,
  }]],
  logLevel: 'warn',
  framework: 'mocha',
  reporters: ['spec'],
  mochaOpts: { ui: 'bdd', timeout: 60_000 },
  capabilities: [{ browserName: 'tauri' }],
};
