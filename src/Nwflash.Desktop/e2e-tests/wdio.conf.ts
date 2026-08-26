import path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Options } from '@wdio/types';

const directory = path.dirname(fileURLToPath(import.meta.url));
const application = process.env.NWFLASH_E2E_BINARY
  ?? path.resolve(directory, '../src-tauri/target/e2e-native/debug/nwflash-desktop.exe');

if (!application.includes(`${path.sep}target${path.sep}e2e-native${path.sep}`)
  && process.env.NWFLASH_ALLOW_EXTERNAL_E2E_BINARY !== 'true') {
  throw new Error('Native E2E requires the dedicated e2e-native build or an explicit external override.');
}

export const config: Options.Testrunner = {
  runner: 'local',
  specs: [
    './specs/navigation.e2e.ts',
    './specs/native-visual-baseline.e2e.ts',
    './specs/native-ui-state-matrix.e2e.ts',
    './specs/dialogs.e2e.ts',
    './specs/progress.e2e.ts',
    './specs/embedded-invoke.e2e.ts',
    './specs/interactions.e2e.ts',
  ],
  maxInstances: 1,
  services: [['@wdio/tauri-service', {
    // Preserve native frontend/backend evidence; command mocks use the direct-eval bridge.
    captureBackendLogs: true,
    captureFrontendLogs: true,
    driverProvider: 'embedded',
  }]],
  logLevel: 'warn',
  framework: 'mocha',
  reporters: ['spec'],
  mochaOpts: { ui: 'bdd', timeout: 60_000, require: ['./specs/direct-mock-bridge.ts'] },
  capabilities: [{
    browserName: 'tauri',
    maxInstances: 1,
    'tauri:options': { application, driverProvider: 'embedded' },
  }],
};
