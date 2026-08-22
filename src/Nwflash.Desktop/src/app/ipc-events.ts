import type { BusyOperationItem } from './window-state';

export const IPC_EVENTS = {
  operationSnapshot: 'operation:snapshot',
  modalState: 'ui:modal',
  sessionState: 'ui:session',
  sessionForceExit: 'session:force-exit',
  sessionUpdateRequired: 'session:update-required',
  deviceSnapshot: 'device:snapshot',
} as const;

export type OperationKind =
  | 'Idle'
  | 'Discovering'
  | 'Rebooting'
  | 'Installing'
  | 'Transferring'
  | 'Hashing'
  | 'Flashing'
  | 'Mirroring'
  | 'Completed'
  | 'Canceled'
  | 'Failed';

export interface OperationSnapshotPayload {
  readonly kind: OperationKind;
  readonly operationId: string | null;
  readonly title: string;
  readonly stage: string;
  readonly progress: number | null;
  readonly startedAt: number | null;
  readonly isCancellable: boolean;
  readonly partitionTask?: PartitionTaskSnapshotPayload | null;
  readonly partitionTasks?: readonly PartitionTaskSnapshotPayload[];
  readonly isBusy: boolean;
}

export interface PartitionTaskSnapshotPayload {
  readonly partition_name: string;
  readonly state: 'Waiting' | 'Running' | 'Succeeded' | 'Failed' | 'Canceled';
  readonly overall_progress: number;
}

export interface ModalStatePayload {
  readonly open: boolean;
  readonly title: string | null;
}

export interface SessionStatePayload {
  readonly hasToken: boolean;
  readonly healthy: boolean;
  readonly running: boolean;
  readonly sessionId: string | null;
}

export interface SessionStateV2Payload {
  readonly has_token: boolean;
  readonly healthy: boolean;
  readonly running: boolean;
  readonly session_id: string | null;
}

export interface AuthSessionPayload {
  readonly username: string;
  readonly name: string;
}

export interface SessionForceExitPayload {
  readonly reason: string;
}

export interface SessionUpdateRequiredPayload {
  readonly message: string;
  readonly latest: string | null;
  readonly minVersion: string | null;
  readonly downloadUrl: string | null;
}

export interface DeviceSnapshotPayload {
  readonly connection_state: string;
  readonly serial: string;
  readonly connection_label: string;
  readonly model: string;
  readonly android_version: string;
  readonly battery_level: string;
}

export const isConnectedDeviceSnapshot = (
  snapshot?: DeviceSnapshotPayload | null,
): boolean =>
  snapshot?.connection_state === 'AdbConnected' ||
  snapshot?.connection_state === 'FastbootConnected';

export type IpcPayloadByName = {
  [IPC_EVENTS.operationSnapshot]: OperationSnapshotPayload;
  [IPC_EVENTS.modalState]: ModalStatePayload;
  [IPC_EVENTS.sessionState]: SessionStatePayload;
  [IPC_EVENTS.sessionForceExit]: SessionForceExitPayload;
  [IPC_EVENTS.sessionUpdateRequired]: SessionUpdateRequiredPayload;
  [IPC_EVENTS.deviceSnapshot]: DeviceSnapshotPayload;
};
