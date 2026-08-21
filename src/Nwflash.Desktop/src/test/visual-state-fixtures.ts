export const VISUAL_STATE_FIXTURES = {
  signedOutSession: {
    has_token: false,
    healthy: false,
    running: false,
    session_id: null,
  },
  versionAllowed: {
    latest: '2.0.0',
    min_version: '1.0.0',
    download_url: null,
    update_required: false,
    force_update: false,
  },
  versionUpdateRequired: {
    latest: '2.1.0',
    min_version: '2.1.0',
    download_url: null,
    update_required: true,
    force_update: true,
  },
  operationLogs: [
    {
      timestamp_utc: 0,
      level: 'Success',
      message: '组件检查完成',
      operation_id: null,
    },
  ],
  softwareReady: {
    app_version: '1.0.1',
    adb_ready: true,
    fastboot_ready: true,
    scrcpy_ready: true,
    payload_dumper_ready: true,
    adb_driver_installed: true,
    fastboot_driver_installed: true,
    mediatek_driver_installed: true,
  },
  softwareMissingResources: {
    app_version: '1.0.1',
    adb_ready: true,
    fastboot_ready: true,
    scrcpy_ready: false,
    payload_dumper_ready: false,
    adb_driver_installed: false,
    fastboot_driver_installed: false,
    mediatek_driver_installed: false,
  },
  resourcesMissing: [
    {
      key: 'scrcpy',
      display_name: 'scrcpy',
      is_ready: false,
      default_selected: true,
    },
    {
      key: 'payload_dumper',
      display_name: 'payload_dumper',
      is_ready: false,
      default_selected: true,
    },
  ],
  resourcesReady: [
    {
      key: 'scrcpy',
      display_name: 'scrcpy',
      is_ready: true,
      default_selected: false,
    },
    {
      key: 'payload',
      display_name: 'payload_dumper',
      is_ready: true,
      default_selected: false,
    },
    {
      key: 'manager-KSU',
      display_name: 'KSU 管理器',
      is_ready: true,
      default_selected: false,
    },
    {
      key: 'manager-OfficialKsu',
      display_name: 'KernelSU 管理器',
      is_ready: true,
      default_selected: false,
    },
  ],
  flashOperation: {
    kind: 'Flashing',
    operationId: null,
    title: '快速刷写',
    stage: '正在写入分区',
    progress: 0.5,
    startedAt: 0,
    isCancellable: true,
    isBusy: true,
  },
  idleOperation: {
    kind: 'Completed',
    operationId: null,
    title: '快速刷写',
    stage: '完成',
    progress: 1,
    startedAt: 0,
    isCancellable: false,
    isBusy: false,
  },
  partitionOperationEvent: {
    kind: 'Flashing',
    operationId: 'partition-operation-1',
    title: '分区写入',
    stage: '正在写入 boot 分区',
    progress: 0.6,
    startedAt: 1700000000,
    isCancellable: true,
    isBusy: true,
  },
  fileEntries: [
    {
      name: 'update.zip',
      full_path: 'device-file-1',
      is_directory: false,
      size_bytes: 2048,
    },
  ],
  partitionSnapshot: {
    active_slot: 'a',
    partitions: [
      {
        name: 'boot',
        size_bytes: 67108864,
        is_high_risk: true,
      },
    ],
  },
  partitionConfirmation: {
    task_count: 1,
    high_risk_count: 1,
    mounted_count: 0,
  },
  quickFlashImage: {
    size_bytes: 2048,
  },
  quickFlashDualSlotConfirmation: {
    task_count: 2,
    switch_slot_after_flash: true,
  },
  lineFlashInspection: {
    format: 'zip',
    entries: [
      {
        id: 'line-entry-boot',
        name: 'boot.img',
        sizeBytes: 2048,
      },
    ],
  },
  preparedFirmwareArtifact: {
    artifactId: 'firmware-artifact-boot',
    name: 'boot.img',
    sizeBytes: 2048,
  },
  firmwareArtifactConfirmation: {
    partition: 'boot',
    taskCount: 1,
  },
  firmwareInspection: {
    format: 'vivoGzipTar',
    entries: [
      {
        id: 'firmware-entry-boot',
        name: 'boot.img',
        sizeBytes: 2048,
      },
    ],
  },
  firmwareExtraction: {
    images: [
      {
        name: 'boot.img',
        sizeBytes: 2048,
        resultId: 'firmware-result-boot',
      },
    ],
  },
  safeFlashPreflight: {
    session_id: 'safe-flash-session-1',
    source_label: '在线 OTA',
    partition_count: 4,
    safe_partition_count: 3,
    has_block_based_content: true,
    requires_confirmation: true,
  },
  safeFlashCompletion: {
    flashed_partition_count: 3,
    skipped_partition_count: 1,
    status: 'VIVO 线刷已完成',
  },
  rootInitBootSelection: {
    id: 'root-image-init-boot-1',
    kind: 'initBoot',
    fileName: 'init_boot.img',
    sizeBytes: 2048,
  },
  rootPatchedArtifact: {
    artifactId: 'root-patched-artifact-1',
    partition: 'init_boot',
    fileName: 'init_boot_patched.img',
    sizeBytes: 2048,
  },
  rootPatchedFlashConfirmation: {
    partition: 'init_boot',
    taskCount: 1,
  },
} as const;

export const NATIVE_UI_ACCEPTANCE_SURFACES = [
  { key: 'overview', pageId: 'Overview', label: '设备概览', selector: '[aria-label="设备概览"]' },
  { key: 'filetransfer', pageId: 'FileManager', label: '文件管理', selector: '[aria-label="文件管理"]' },
  { key: 'adbactions', pageId: 'Mirror', label: 'ADB 投屏', selector: '[aria-label="ADB 投屏"]' },
  { key: 'fastbootflash', pageId: 'QuickFlash', label: '快速刷写', selector: '[aria-label="快速刷写"]' },
  { key: 'lineflash', pageId: 'LineFlash', label: '可视刷写', selector: '[aria-label="可视刷写"]' },
  { key: 'safeflash', pageId: 'SafeFlash', label: 'VIVO 线刷', selector: '[aria-label="VIVO 线刷"]' },
  { key: 'firmwareextract', pageId: 'FirmwareExtract', label: '固件提取', selector: '[aria-label="固件提取"]' },
  { key: 'roottools', pageId: 'Root', label: 'Vivo ROOT', selector: '[aria-label="Vivo ROOT"]' },
  { key: 'onlinestatus', pageId: 'Online', label: '在线状态', selector: '[aria-label="在线状态"]' },
  { key: 'software', pageId: 'Software', label: '软件', selector: '.nw-software-page' },
  { key: 'operationlog', pageId: null, label: '操作日志', selector: '[data-role="operation-log-panel"]' },
] as const;

export const NATIVE_UI_ACCEPTANCE_STATES = [
  { key: 'loading', expectedText: '正在加载...' },
  { key: 'error', expectedText: '验收日志读取失败' },
  { key: 'running', expectedText: '正在写入 boot 分区' },
] as const;
