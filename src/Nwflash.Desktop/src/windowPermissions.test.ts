import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, test } from 'vitest';

const capabilityPath = resolve(process.cwd(), 'src-tauri/capabilities/default.json');

describe('desktop window capabilities', () => {
  test('allows the login-to-main window resize transition', () => {
    const capability = JSON.parse(readFileSync(capabilityPath, 'utf8')) as {
      permissions?: string[];
    };

    expect(capability.permissions).toEqual(
      expect.arrayContaining([
        'core:window:allow-close',
        'core:window:allow-minimize',
        'core:window:allow-set-resizable',
        'core:window:allow-set-size',
        'core:window:allow-toggle-maximize',
      ]),
    );
  });
});
