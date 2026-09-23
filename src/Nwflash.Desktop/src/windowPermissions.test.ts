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
const tauriHostSourcePath = resolve(tauriRoot, 'crates', 'nwflash-tauri', 'src', 'lib.rs');
const fileCommandSourcePath = resolve(tauriRoot, 'crates', 'nwflash-tauri', 'src', 'commands', 'files.rs');
const cargoTreeTimeoutMs = 45_000;
const cargoGraphTestTimeoutMs = (cargoTreeTimeoutMs * 2) + 10_000;
/// 生产能力清单，必须与 `src-tauri/capabilities/default.json` 逐项一致。
///
/// `core:window:allow-center` 与 `core:webview:allow-set-webview-zoom` 是
/// 「界面缩放与窗口尺寸过渡」实际用到的权限（`window-transition.ts` 调
/// `appWindow.center()`、`ui-scale.ts` 调 `webview.setZoom()`），当初加配置时漏了
/// 同步这里，于是本文件一直红着、被当成"既有失败"忽略。
const normalPermissions = [
  'core:default',
  'dialog:default',
  'core:window:default',
  'core:window:allow-center',
  'core:window:allow-close',
  'core:window:allow-minimize',
  'core:window:allow-set-resizable',
  'core:window:allow-set-size',
  'core:window:allow-start-dragging',
  'core:window:allow-toggle-maximize',
  'core:webview:allow-set-webview-zoom',
];

function readJson<T>(path: string): T {
  return JSON.parse(readFileSync(path, 'utf8')) as T;
}

/// 读取源码并**统一换行符**后再断言。
///
/// 本仓 .rs 文件的换行并不一致：`commands/files.rs` 是 CRLF，而 `lib.rs` 是 LF。
/// 直接对 `readFileSync(...)` 的原文做多行 `toContain`，会让断言结果取决于
/// 「这个文件恰好是什么换行」——同一段代码在两种换行下断言一真一假，
/// 正是本文件长期报红的原因。先归一化，断言只关心代码内容。
function readSource(path: string): string {
  return readFileSync(path, 'utf8').replaceAll('\r\n', '\n');
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

  test('file transaction harness is feature-gated and restricted to the dedicated target', () => {
    const hostSource = readSource(tauriHostSourcePath);
    const fileSource = readSource(fileCommandSourcePath);
    const buildScript = readFileSync(nativeE2eBuildScriptPath, 'utf8');

    expect(fileSource).toContain('#[cfg(feature = "e2e")]\npub(crate) mod e2e;');
    expect(hostSource).toContain('#[cfg(feature = "e2e")]\n    ensure_dedicated_e2e_binary()?;');
    expect(hostSource).toContain('component.as_os_str() == "e2e-native"');
    expect(hostSource).toContain('#[cfg(all(feature = "e2e", not(test)))]');
    expect(buildScript).toContain("$targetRoot = Join-Path $desktopRoot 'src-tauri\\target\\e2e-native'");
    expect(buildScript).toContain('--features e2e');
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
