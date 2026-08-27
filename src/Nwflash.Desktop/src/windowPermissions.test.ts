import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, test } from 'vitest';

type JsonObject = Record<string, unknown>;
type Capability = {
  identifier: string;
  windows?: string[];
  permissions?: string[];
};

const desktopRoot = process.cwd();
const tauriRoot = resolve(desktopRoot, 'src-tauri');
const manifestPath = resolve(tauriRoot, 'Cargo.toml');
const configPath = resolve(tauriRoot, 'tauri.conf.json');
const e2eConfigPath = resolve(tauriRoot, 'tauri.e2e.conf.json');
const capabilityRoot = resolve(tauriRoot, 'capabilities');
const nativeE2eBuildScriptPath = resolve(desktopRoot, 'e2e-tests', 'build-native-e2e.ps1');
const cargoTreeTimeoutMs = 45_000;
const cargoGraphTestTimeoutMs = (cargoTreeTimeoutMs * 2) + 10_000;
const normalPermissions = [
  'core:default',
  'dialog:default',
  'core:window:default',
  'core:window:allow-close',
  'core:window:allow-minimize',
  'core:window:allow-set-resizable',
  'core:window:allow-set-size',
  'core:window:allow-toggle-maximize',
];

function readJson<T>(path: string): T {
  return JSON.parse(readFileSync(path, 'utf8')) as T;
}

function mergePatch(base: unknown, patch: unknown): unknown {
  if (patch === null || typeof patch !== 'object' || Array.isArray(patch)) {
    return patch;
  }

  const result: JsonObject = base !== null && typeof base === 'object' && !Array.isArray(base)
    ? { ...(base as JsonObject) }
    : {};
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) {
      delete result[key];
    } else {
      result[key] = mergePatch(result[key], value);
    }
  }
  return result;
}

function selectedCapabilityIds(config: JsonObject): string[] {
  const app = config.app as JsonObject | undefined;
  const security = app?.security as JsonObject | undefined;
  return ((security?.capabilities ?? []) as Array<string | Capability>)
    .map((capability) => typeof capability === 'string' ? capability : capability.identifier);
}

function resolveCapabilities(config: JsonObject): Capability[] {
  const app = config.app as JsonObject | undefined;
  const security = app?.security as JsonObject | undefined;
  return ((security?.capabilities ?? []) as Array<string | Capability>).map((selected) => {
    if (typeof selected !== 'string') return selected;
    const capability = readJson<Capability>(resolve(capabilityRoot, `${selected}.json`));
    expect(capability.identifier).toBe(selected);
    return capability;
  });
}

function cargoTree(features?: string): string {
  const args = [
    'tree',
    '--locked',
    '--manifest-path',
    manifestPath,
    '-p',
    'nwflash-desktop',
    '--edges',
    'normal,build',
    '--no-default-features',
  ];
  if (features) args.push('--features', features);
  return execFileSync('cargo', args, {
    encoding: 'utf8',
    maxBuffer: 2 * 1024 * 1024,
    timeout: cargoTreeTimeoutMs,
    windowsHide: true,
  });
}

describe('desktop window capabilities', () => {
  test('production resolves only the WDIO-free default capability', () => {
    const config = readJson<JsonObject>(configPath);
    const identifiers = selectedCapabilityIds(config);
    const capabilities = resolveCapabilities(config);

    expect(identifiers).toEqual(['default']);
    expect(capabilities).toHaveLength(1);
    expect(capabilities[0].windows).toEqual(['main']);
    expect(capabilities[0].permissions).toEqual(normalPermissions);
    expect(capabilities.flatMap(({ permissions = [] }) => permissions))
      .not.toEqual(expect.arrayContaining(['wdio:default', 'wdio-webdriver:default']));
  });

  test('E2E merge resolves only the self-contained automation capability', () => {
    const base = readJson<JsonObject>(configPath);
    const extension = readJson<JsonObject>(e2eConfigPath);
    const effective = mergePatch(base, extension) as JsonObject;
    const identifiers = selectedCapabilityIds(effective);
    const capabilities = resolveCapabilities(effective);

    expect(identifiers).toEqual(['e2e']);
    expect(existsSync(resolve(capabilityRoot, 'e2e.json'))).toBe(false);
    expect(capabilities).toHaveLength(1);
    expect(capabilities[0].windows).toEqual(['main']);
    expect(capabilities[0].permissions).toEqual([
      ...normalPermissions,
      'wdio:default',
      'wdio-webdriver:default',
    ]);
  });

  test('Cargo activates WDIO plugins only for the E2E graph', () => {
    const productionTree = cargoTree();
    const e2eTree = cargoTree('e2e');

    expect(productionTree).not.toMatch(/tauri-plugin-wdio(?:-webdriver)?\s+v/);
    expect(e2eTree).toMatch(/tauri-plugin-wdio\s+v/);
    expect(e2eTree).toMatch(/tauri-plugin-wdio-webdriver\s+v/);
  }, cargoGraphTestTimeoutMs);

  test('native E2E build injects and restores a deterministic test verification key', () => {
    const script = readFileSync(nativeE2eBuildScriptPath, 'utf8');
    const keyMatch = script.match(/\$e2eVerificationKeyB64\s*=\s*'([^']+)'/);

    expect(keyMatch).not.toBeNull();
    expect(Buffer.from(keyMatch?.[1] ?? '', 'base64')).toHaveLength(32);
    expect(script).toContain('$priorVerificationKey = $env:NWFLASH_SESSION_VERIFY_KEY_B64');
    expect(script).toContain('$env:NWFLASH_SESSION_VERIFY_KEY_B64 = $e2eVerificationKeyB64');
    expect(script).toContain('Remove-Item Env:NWFLASH_SESSION_VERIFY_KEY_B64');
    expect(script).toContain('$env:NWFLASH_SESSION_VERIFY_KEY_B64 = $priorVerificationKey');
  });

  test('native E2E build restores caller environment when its child build fails', () => {
    const scriptPath = nativeE2eBuildScriptPath.replaceAll("'", "''");
    const command = [
      "$ErrorActionPreference='Stop'",
      "$env:CARGO_TARGET_DIR='caller-target-sentinel'",
      "$env:NWFLASH_SESSION_VERIFY_KEY_B64='caller-key-sentinel'",
      "function global:npm { throw 'forced-e2e-build-failure' }",
      '$failed = $false',
      `try { & '${scriptPath}' } catch { $failed = $true }`,
      "if (-not $failed) { throw 'Expected the injected npm failure.' }",
      "if ($env:CARGO_TARGET_DIR -ne 'caller-target-sentinel') { throw 'CARGO_TARGET_DIR was not restored.' }",
      "if ($env:NWFLASH_SESSION_VERIFY_KEY_B64 -ne 'caller-key-sentinel') { throw 'Verification key was not restored.' }",
      "Write-Output 'RESTORED'",
    ].join('; ');
    const output = execFileSync('pwsh', [
      '-NoLogo',
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      command,
    ], {
      encoding: 'utf8',
      timeout: 15_000,
      windowsHide: true,
    });

    expect(output).toContain('RESTORED');
  });
});
