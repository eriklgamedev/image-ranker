import { invoke, convertFileSrc, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
export interface Source {
  id: string;
  path: string;
  name: string;
  recursive: boolean;
  count: number;
  available?: boolean;
}
export interface ImageItem {
  id: string;
  path: string;
  originalName: string;
  extension: string;
  sizeBytes: number;
  modifiedAt: number;
  width: number;
  height: number;
  sourceIds: string[];
  previewPath: string;
  contentRevision: string;
  availability?: "ready" | "missing" | "changed";
  pathRevision?: number;
}
export interface ScanReport {
  sources: Source[];
  images: ImageItem[];
  ignored: { path: string; reason: string }[];
  cancelled?: boolean;
  retainedImages?: ImageItem[];
}
export interface ScanProgress {
  contextId?: string;
  scanned: number;
  eligible: number;
  sourceId: string;
  batch: ImageItem[];
  done: boolean;
  cancelled: boolean;
}
export interface CacheClearResult {
  requestId: string;
  generation: string;
  state:
    | "pending"
    | "running"
    | "waiting-for-pins"
    | "completed"
    | "completed-with-retained-items"
    | "cancelled"
    | "failed";
  retainedFiles: number;
  retainedBytes: number;
  retainedCountsKnown: boolean;
  reasons: string[];
  retryOf: string | null;
}
export interface CacheInfo {
  sizeBytes: number;
  files: number;
  budgetBytes: number;
  pinnedFiles: number;
  pinnedBytes: number;
  pendingDeleteFiles: number;
  pendingPreviews: number;
  pixelReservedBytes: number;
  retirementIssues: string[];
  temporaryFiles: number;
  temporaryBytes: number;
  maintenancePending: boolean;
  maintenanceRunning: boolean;
  inventoryKnown: boolean;
  clearRequest: CacheClearResult | null;
}
export interface DecodeStatus {
  active: number;
  pending: number;
  reservedBytes: number;
  liveOutputs: number;
  outputReservedBytes: number;
  unavailable: boolean;
  workerPid: number | null;
  stage: string | null;
  recoveryAction:
    "retry-reap" | "retry-output-cleanup" | "restart-application" | null;
}
export interface ImageWorkStatus {
  active: number;
  limit: number;
  jobs: Array<{
    requestId: string;
    contextId: string;
    kind:
      "preview" | "scan" | "reconnect" | "worker-recovery" | "file-mutation";
    phase: string;
    cancelled: boolean;
  }>;
  decoder: DecodeStatus;
  cacheMaintenance?: {
    active: boolean;
    ownerLimit: number;
    clearResults?: CacheClearResult[];
    owners: Array<{
      requestId: string;
      phase: string;
      cancelled: boolean;
      runnable: boolean;
      retirementComplete: boolean;
      lastError: string | null;
      usage: CacheInfo;
    }>;
  };
}
export interface CapacityInfo {
  policyVersion: number;
  usedBytes: number;
  limitBytes: number;
  remainingBytes: number;
  usedNodes: number;
  limitNodes: number;
  remainingNodes: number;
  eventCount: number;
  eventLimit: number;
  reservedBytes: number;
  reservedNodes: number;
  undoReserveQualified: boolean;
}
export interface RegistryCapacity {
  hardBytes: number;
  hardClaims: number;
  ordinaryByteLimit: number;
  ordinaryClaimLimit: number;
  usedBytes: number;
  usedClaims: number;
  reservedCompletionBytes: number;
  reservedCompletionClaims: number;
  emergencyByteAllowance: number;
  emergencyClaimAllowance: number;
  maintenanceByteAllowance: number;
  maintenanceClaimAllowance: number;
  ordinaryBytesRemaining: number;
  ordinaryClaimsRemaining: number;
  publicationBytesRemaining: number;
  publicationClaimsRemaining: number;
}
export interface ProtectionStatus {
  state: "ready" | "ordinary-limit" | "recovery-limit" | "unavailable";
  capacity: RegistryCapacity | null;
  message: string | null;
}
export interface ProtectionMaintenanceResult {
  requestId: string;
  state: "completed" | "no-reclaim";
  generation: string;
  beforeBytes: number;
  afterBytes: number;
  preservedClaims: number;
  reservedCompletionBytes: number;
  reservedCompletionClaims: number;
}
export interface ProtectionMaintenanceStatus {
  registry: {
    requestId: string | null;
    state:
      | "ready"
      | "append-reconciliation-required"
      | "maintenance-pending"
      | "cleanup-pending"
      | "review-required";
    generation: string;
    canResume: boolean;
    lastResult: ProtectionMaintenanceResult | null;
  } | null;
  result: ProtectionMaintenanceResult | null;
  active: boolean;
  fresh: boolean;
  error: string | null;
}
export interface ProtectionMaintenancePreview {
  token: string;
  requestId: string;
  contextId: string;
  profileGeneration: string;
  expiresInMs: number;
  plan: {
    generation: string;
    token: string;
    usedBytes: number;
    compactedBytesUpperBound: number;
    reclaimableBytesLowerBound: number;
    preservedClaims: number;
    addedManagedClaims: number;
    reservedCompletionBytes: number;
    reservedCompletionClaims: number;
    extraDiskBytesUpperBound: number;
    canReclaim: boolean;
  };
}
export interface AppInfo {
  name: string;
  version: string;
  platform: string;
  architecture: string;
  buildType: "debug" | "release";
  testDriverEnabled: boolean;
  applicationLicense: string | null;
  decode: {
    formats: string[];
    maxInputBytes: number;
    maxPixels: number;
    maxEdge: number;
    maxDecodeBytes: number;
    maxIccBytes: number;
    channelBits: number[];
    gif: "first-frame";
    tiff: "first-page";
    orientation: "exif-normalized";
    color: "embedded-profile-to-srgb";
  };
}
export interface LicenseNotice {
  id: string;
  component: string;
  version: string;
  spdx: string;
  text: string;
}
export type StorageLocationKind = "profile" | "session" | "operations";
export interface StorageLocation {
  kind: StorageLocationKind;
  label: string;
  displayPath: string;
  available: boolean;
}
export interface DiagnosticsPreview {
  token: string;
  createdAt: string;
  redacted: true;
  text: string;
  sizeBytes: number;
  limitations: string[];
}
export interface SessionSaveStatus {
  state: "saved" | "pending" | "checkpoint-pending";
  durableRevision: number | null;
  pendingEventId?: string;
  message?: string;
}
export interface ProfileSummary {
  id: string | null;
  displayPath: string;
  isDefault: boolean;
  writable: boolean;
}
export interface ProfileSwitchPreview {
  token: string;
  mode: "create" | "open";
  profile: ProfileSummary;
  hasPendingSave: boolean;
  pendingOperations: number;
  requiresDisposition: boolean;
  rendererChangesUnadmitted: boolean;
  warnings: string[];
}
export type ProfileDisposition = "saved" | "recovery-copy" | "discard";
export interface PendingProfileSwitch {
  token: string;
  disposition: ProfileDisposition;
  profile: ProfileSummary;
  warnings: string[];
}
export interface OperationRestriction {
  kind: "publication" | "restart";
  message: string;
}
export interface OperationRestartPreview {
  token: string;
  requestId: string;
  savedRevision: number;
  pendingActions: number;
  rendererProposal: boolean;
  copyReady: boolean;
  copyPath: string | null;
  warnings: string[];
}
export type RecoveryReplacementSubject =
  | { kind: "fresh"; token: string; acceptPending: boolean }
  | { kind: "resume"; token: string };
export interface RecoveryReplacementPreview {
  token: string;
  subjectToken: string;
  subjectKind: "fresh" | "resume";
  currentSaveState: "saved" | "pending" | "checkpoint-pending" | "unavailable";
  hasCurrentSession: boolean;
  requiresDisposition: boolean;
  rendererChangesUnadmitted: boolean;
  pendingLayers: Array<{
    kind: "pending-event" | "pending-proposal";
    id: string;
    summary: string;
  }>;
  warnings: string[];
}
export interface LoadedSessionState {
  session: unknown;
  saveStatus: SessionSaveStatus;
  profileGeneration: string;
  sessionGeneration: string;
  contextId: string;
}
export interface BootStatus {
  pendingProfileSwitch?: PendingProfileSwitch;
  mode: "ready" | "recovery";
  state:
    | "ready"
    | "read-only-recovery"
    | "unsupported-version"
    | "storage-unavailable"
    | "loading"
    | "load-cancelled"
    | "profile-selection-unresolved"
    | "file-operation-unresolved";
  issues: { code: string; message: string }[];
  canRestoreBackup: boolean;
  pendingOperations: number;
  trustedOperationsAvailable?: boolean;
  profile: ProfileSummary;
  profileGeneration: string;
  contextId: string;
  canChooseRecoveryStorage: boolean;
}
export interface CloseRequest {
  token: string;
  contextId?: string;
}
export interface RecoveryPreview {
  token: string;
  createdAt: string;
  summary: {
    images: number;
    runs: number;
    decisions: number;
    reviewEdits: number;
  };
  warnings: string[];
  restorable: boolean;
  missingJournals: string[];
  missingBackups: string[];
  savedRevision: number | null;
  pendingActions: number;
  pendingSummary: string[];
}
export interface RecoveryWorkJob {
  id: string;
  contextId: string;
  profileGeneration: string;
  kind:
    | "storage-status"
    | "prepare-resume"
    | "preview-cleanup"
    | "cleanup"
    | "inspect-copy"
    | "restore-copy"
    | "startup-load"
    | "prepare-profile"
    | "switch-profile"
    | "registry-status"
    | "preview-registry-maintenance"
    | "registry-maintenance";
  phase: string;
  state: "running" | "cancelling" | "completed" | "cancelled" | "failed";
  completed: number;
  total: number | null;
  cancelRequested: boolean;
  message: string | null;
}
export interface RecoveryWorkStatus {
  active: RecoveryWorkJob | null;
  recent: RecoveryWorkJob[];
  limit: number;
}
export interface RecoveryStorageStatus {
  selectedRequestId: string | null;
  selectedBranchId: string | null;
  activationState: "original" | "committed" | "incomplete" | "damaged";
  usedBranches: number;
  usedBytes: number;
  limits: { branches: number; bytes: number; requests: number };
  requests: Array<{
    requestId: string;
    branchId: string;
    state:
      | "staged"
      | "ready"
      | "activation-incomplete"
      | "active"
      | "superseded"
      | "cleanup-pending"
      | "retired"
      | "blocked";
    pendingActions: number;
    acceptedPending: boolean;
    bytes: number;
    canResume: boolean;
    canCleanup: boolean;
    reason: string | null;
  }>;
  warnings: string[];
}
export interface RecoveryResumePreview {
  token: string;
  requestId: string;
  branchId: string;
  inspection: RecoveryPreview;
  acceptedPending: boolean;
  action: "resume-bootstrap" | "finish-activation" | "reopen-committed";
  warnings: string[];
}
export interface RecoveryCleanupPreview {
  token: string;
  requestId: string;
  branchId: string;
  totalBytes: number;
  files: Array<{ relativePath: string; sizeBytes: number }>;
  preserved: Array<{ relativePath: string; reason: string }>;
  warnings: string[];
}
export interface CleanupPlan {
  id: string;
  operationId: string;
  files: { path: string; sizeBytes: number; status: string }[];
  totalBytes: number;
  status: string;
}
export interface PreviewResponse {
  path: string;
  assetToken: string;
  leaseId: string;
  contentRevision: string;
  requestId: string;
  width: number;
  height: number;
  scale: number;
  pipelineVersion: string;
  status: "ready";
}
export interface PlanRequest {
  kind: "rename" | "copy";
  imageIds: string[];
  ranks: number[];
  reviewCount: number;
  naming: "numeric" | "prefix" | "custom" | "original" | "current";
  template: string;
  start: number;
  padding: number;
  destination: string | null;
  conflict: "suffix" | "skip" | "overwrite";
  manifest: boolean;
  verifyCopy?: boolean;
}
export interface MoveSelectionFolderRequest {
  imageIds: string[];
  folderName: string;
}
export interface MoveSelectionFolderResult {
  operation: Operation;
  report: ScanReport;
}
export interface PlanRow {
  imageId: string;
  from: string;
  to: string;
  temporary: string;
  rank: number;
  outputNumber: number;
  sizeBytes: number;
  issue: string | null;
  noop: boolean;
  step?: string;
  verification?: string;
}
export interface Operation {
  id: string;
  kind: string;
  status: string;
  rows: PlanRow[];
  errors: string[];
  completed: number;
  manifestPath: string | null;
  order: number;
  dependencies: string[];
  historyRetired?: boolean;
  exfatOutputs?: boolean;
  context?: {
    sourceGroup?: {
      folderName: string;
      parentPath: string;
    };
  };
  cleanup?: CleanupPlan | null;
  sequence?: number;
}
export interface OperationRowProgress {
  index: number;
  step: string;
  verification: string;
}
export interface OperationProgress {
  contextId?: string;
  id: string;
  sequence: number;
  status: string;
  completed: number;
  total: number;
  rows: OperationRowProgress[];
  errors: string[];
  historyRetired: boolean;
  snapshot: Operation | null;
}
export const desktop = isTauri();
export function previewUrl(item: ImageItem) {
  return desktop ? convertFileSrc(item.previewPath) : item.previewPath;
}
let previewContext: Promise<string> | undefined;
let rendererRetired = false;
let rendererGeneration = 0;
const activationListeners = new Set<(status: BootStatus) => void>();
const operationListeners = new Set<(failure: OperationRestriction) => void>();
function acquirePreviewContext(): Promise<string> {
  if (rendererRetired)
    return Promise.reject(
      new Error(
        "STALE_CONTEXT: reload the selected profile before continuing.",
      ),
    );
  if (!previewContext) {
    previewContext = invoke<string>("preview_context")
      .then((contextId) => {
        if (typeof contextId !== "string" || !contextId.trim())
          throw new Error("The native preview context is unavailable.");
        return contextId;
      })
      .catch((error) => {
        previewContext = undefined;
        throw error;
      });
  }
  return previewContext;
}
async function contextInvoke<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const generation = rendererGeneration;
  const contextId = await acquirePreviewContext();
  if (rendererRetired || generation !== rendererGeneration)
    throw new Error("STALE_CONTEXT: this renderer has been replaced.");
  let result: T;
  try {
    result = await invoke<T>(command, { ...args, contextId });
  } catch (error) {
    const message = String(error).replace(/^Error:\s*/, "");
    const operationCode =
      /^(FILE_OPERATION_PUBLICATION_REQUIRED|FILE_OPERATION_RESTART_REQUIRED)(?::|$)/.exec(
        message,
      )?.[1];
    if (
      operationCode &&
      !rendererRetired &&
      generation === rendererGeneration
    ) {
      const failure: OperationRestriction = {
        kind:
          operationCode === "FILE_OPERATION_RESTART_REQUIRED"
            ? "restart"
            : "publication",
        message,
      };
      for (const listener of operationListeners) listener(failure);
    }
    if (
      /(?:ACTIVATION_RECOVERY_REQUIRED|PROFILE_SELECTION_RECOVERY_REQUIRED)/.test(
        String(error),
      ) &&
      !rendererRetired &&
      generation === rendererGeneration
    ) {
      let status: BootStatus;
      try {
        status = await invoke<BootStatus>("get_boot_status");
      } catch {
        throw error;
      } // Keep the original actionable native failure.
      if (
        !rendererRetired &&
        generation === rendererGeneration &&
        status.contextId === contextId &&
        status.mode === "recovery"
      ) {
        for (const listener of activationListeners) listener(status);
      }
    }
    throw error;
  }
  if (rendererRetired || generation !== rendererGeneration)
    throw new Error(
      "STALE_CONTEXT: ignored a response from the previous profile.",
    );
  return result;
}
async function contextListen<T extends { contextId?: string }>(
  eventName: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  const generation = rendererGeneration;
  const contextId = await acquirePreviewContext();
  return listen<T>(eventName, (event) => {
    if (
      !rendererRetired &&
      generation === rendererGeneration &&
      event.payload.contextId === contextId
    )
      handler(event.payload);
  });
}
export const api = {
  onOperationRestriction: (
    listener: (failure: OperationRestriction) => void,
  ) => {
    operationListeners.add(listener);
    return () => {
      operationListeners.delete(listener);
    };
  },
  retryOperationPublication: () =>
    contextInvoke<void>("retry_operation_publication"),
  prepareOperationRestart: (expectedCurrentSession?: unknown) =>
    contextInvoke<OperationRestartPreview>(
      "prepare_operation_restart",
      expectedCurrentSession === undefined ? {} : { expectedCurrentSession },
    ),
  exportOperationRestart: (token: string) =>
    contextInvoke<OperationRestartPreview | null>("export_operation_restart", {
      token,
    }),
  confirmOperationRestart: (token: string, expectedCurrentSession?: unknown) =>
    contextInvoke<void>("confirm_operation_restart", {
      token,
      ...(expectedCurrentSession === undefined
        ? {}
        : { expectedCurrentSession }),
    }),
  cancelOperationRestart: (token: string) =>
    contextInvoke<void>("cancel_operation_restart", { token }),
  onActivationRestriction: (listener: (status: BootStatus) => void) => {
    activationListeners.add(listener);
    return () => {
      activationListeners.delete(listener);
    };
  },
  rendererContext: acquirePreviewContext,
  reloadSelectedProfile: () => window.location.reload(),
  chooseRecoveryStorage: (expectedCurrentSession?: unknown) =>
    contextInvoke<ProfileSwitchPreview | null>("choose_recovery_storage", {
      expectedCurrentSession,
    }),
  openRecoveryProfile: (expectedCurrentSession?: unknown) =>
    contextInvoke<ProfileSwitchPreview | null>("open_recovery_profile", {
      expectedCurrentSession,
    }),
  refreshProfileSwitch: (token: string, expectedCurrentSession?: unknown) =>
    contextInvoke<ProfileSwitchPreview>("refresh_profile_switch", {
      token,
      expectedCurrentSession,
    }),
  exportProfileReplacementCopy: (token: string) =>
    contextInvoke<string | null>("export_profile_replacement_copy", { token }),
  confirmProfileSwitch: async (
    token: string,
    disposition: ProfileDisposition = "saved",
  ) => {
    const result = await contextInvoke<BootStatus>("confirm_profile_switch", {
      token,
      disposition,
    });
    rendererRetired = true;
    rendererGeneration++;
    return result;
  },
  cancelProfileSwitch: (token: string) =>
    contextInvoke<void>("cancel_profile_switch", { token }),
  requestPreview: (
    id: string,
    size: number,
    expectedContentRevision: string,
    requestId: string,
  ) =>
    contextInvoke<PreviewResponse>("request_preview", {
      id,
      size,
      expectedContentRevision,
      requestId,
    }),
  releasePreview: (leaseId: string) =>
    contextInvoke<void>("release_preview", { leaseId }),
  imageWorkStatus: () => contextInvoke<ImageWorkStatus>("image_work_status"),
  cancelImageWork: (requestId: string | null = null) =>
    contextInvoke<number>("cancel_image_work", { requestId }),
  retryCacheMaintenance: (requestId: string) =>
    contextInvoke<NonNullable<ImageWorkStatus["cacheMaintenance"]>>(
      "retry_cache_maintenance",
      { requestId },
    ),
  reapImageWorker: () => contextInvoke<DecodeStatus>("reap_image_worker"),
  scanSources: (recursive: boolean) =>
    contextInvoke<ScanReport>("scan_sources", { recursive }),
  removeSource: (id: string) =>
    contextInvoke<ScanReport>("remove_source", { id }),
  cancelScan: () => contextInvoke<void>("cancel_scan"),
  subscribeScanProgress: (
    handler: (progress: ScanProgress) => void,
  ): Promise<UnlistenFn> =>
    contextListen<ScanProgress>("scan-progress", handler),
  loadSession: () => contextInvoke<unknown>("load_session"),
  loadSessionState: () =>
    contextInvoke<LoadedSessionState>("load_session_state"),
  sessionSaveStatus: () =>
    contextInvoke<SessionSaveStatus>("session_save_status"),
  saveSession: (value: unknown) =>
    contextInvoke<void>("save_session", { value }),
  subscribeCloseRequested: (
    handler: (request: CloseRequest) => void,
  ): Promise<UnlistenFn> =>
    contextListen<CloseRequest>("close-requested", handler),
  confirmClose: (
    token: string,
    disposition: "saved" | "recovery-copy" | "discard" = "saved",
  ) => contextInvoke<void>("confirm_close", { token, disposition }),
  cancelClose: (token: string) =>
    contextInvoke<void>("cancel_close", { token }),
  exportSessionCopy: (value: unknown) =>
    contextInvoke<string | null>("export_session_copy", { value }),
  getBootStatus: () => invoke<BootStatus>("get_boot_status"),
  retryBoot: () => contextInvoke<BootStatus>("retry_boot"),
  inspectRecoveryCopy: () =>
    contextInvoke<RecoveryPreview | null>("inspect_recovery_copy"),
  recoveryStorageStatus: () =>
    contextInvoke<RecoveryStorageStatus>("recovery_storage_status"),
  recoveryWorkStatus: () =>
    contextInvoke<RecoveryWorkStatus>("recovery_work_status"),
  cancelRecoveryWork: (jobId: string) =>
    contextInvoke<{ id: string; cancelRequested: true }>(
      "cancel_recovery_work",
      { jobId },
    ),
  prepareRecoveryResume: (requestId: string) =>
    contextInvoke<RecoveryResumePreview | null>("prepare_recovery_resume", {
      requestId,
    }),
  prepareRecoveryReplacement: (
    subject: RecoveryReplacementSubject,
    expectedCurrentSession?: unknown,
  ) =>
    contextInvoke<RecoveryReplacementPreview>("prepare_recovery_replacement", {
      subject,
      expectedCurrentSession,
    }),
  cancelRecoveryReplacement: (token: string) =>
    contextInvoke<void>("cancel_recovery_replacement", { token }),
  exportRecoveryReplacementCopy: (token: string) =>
    contextInvoke<string | null>("export_recovery_replacement_copy", { token }),
  confirmRecoveryResume: async (
    token: string,
    replacementToken: string,
    disposition: ProfileDisposition,
  ) => {
    const result = await contextInvoke<BootStatus>("confirm_recovery_resume", {
      token,
      replacementToken,
      disposition,
    });
    rendererRetired = true;
    rendererGeneration++;
    return result;
  },
  cancelRecoveryResume: (token: string) =>
    contextInvoke<void>("cancel_recovery_resume", { token }),
  previewRecoveryCleanup: (requestId: string) =>
    contextInvoke<RecoveryCleanupPreview>("preview_recovery_cleanup", {
      requestId,
    }),
  confirmRecoveryCleanup: (token: string) =>
    contextInvoke<RecoveryStorageStatus>("confirm_recovery_cleanup", { token }),
  cancelRecoveryCleanup: (token: string) =>
    contextInvoke<void>("cancel_recovery_cleanup", { token }),
  restoreRecoveryCopy: async (
    token: string,
    acceptPending: boolean,
    replacementToken: string,
    disposition: ProfileDisposition,
  ) => {
    const result = await contextInvoke<BootStatus>("restore_recovery_copy", {
      token,
      acceptPending,
      replacementToken,
      disposition,
    });
    rendererRetired = true;
    rendererGeneration++;
    return result;
  },
  reconnectSource: (id: string) =>
    contextInvoke<ScanReport | null>("reconnect_source", { id }),
  recoveryNotes: () => contextInvoke<string[]>("recovery_notes"),
  cacheInfo: (refresh = false, requestId?: string) =>
    contextInvoke<CacheInfo>("cache_info", { refresh, requestId }),
  capacityInfo: () => contextInvoke<CapacityInfo>("capacity_info"),
  protectionStatus: () => contextInvoke<ProtectionStatus>("protection_status"),
  protectionMaintenanceStatus: (requestId?: string) =>
    contextInvoke<ProtectionMaintenanceStatus>(
      "protection_maintenance_status",
      { requestId },
    ),
  previewProtectionMaintenance: () =>
    contextInvoke<ProtectionMaintenancePreview>(
      "preview_protection_maintenance",
    ),
  confirmProtectionMaintenance: (token: string) =>
    contextInvoke<ProtectionMaintenanceResult>(
      "confirm_protection_maintenance",
      { token },
    ),
  resumeProtectionMaintenance: (requestId: string) =>
    contextInvoke<ProtectionMaintenanceResult>(
      "resume_protection_maintenance",
      { requestId },
    ),
  appInfo: () => invoke<AppInfo>("app_info"),
  licenseNotices: () => invoke<LicenseNotice[]>("license_notices"),
  storageLocations: () => contextInvoke<StorageLocation[]>("storage_locations"),
  openStorageLocation: (kind: StorageLocationKind) =>
    contextInvoke<void>("open_storage_location", { kind }),
  previewDiagnostics: () =>
    contextInvoke<DiagnosticsPreview>("preview_diagnostics"),
  exportDiagnostics: (token: string) =>
    contextInvoke<string | null>("export_diagnostics", { token }),
  clearCache: (retryOf?: string) =>
    contextInvoke<CacheInfo>("clear_cache", { retryOf }),
  setCacheLimit: (budgetBytes: number) =>
    contextInvoke<CacheInfo>("set_cache_limit", { budgetBytes }),
  chooseDestination: () => contextInvoke<string | null>("choose_destination"),
  moveSelectionToFolder: (request: MoveSelectionFolderRequest) =>
    contextInvoke<MoveSelectionFolderResult>("move_selection_to_folder", {
      request,
    }),
  planOperation: (request: PlanRequest) =>
    contextInvoke<Operation>("plan_operation", { request }),
  commitOperation: (id: string) =>
    contextInvoke<Operation>("commit_operation", { id }),
  listOperations: () => contextInvoke<Operation[]>("list_operations"),
  getOperation: (id: string) =>
    contextInvoke<Operation>("get_operation", { id }),
  cancelOperation: () => contextInvoke<void>("cancel_operation"),
  subscribeOperationProgress: (
    handler: (operation: OperationProgress) => void,
  ): Promise<UnlistenFn> =>
    contextListen<OperationProgress>("operation-progress", handler),
  recoverOperation: (id: string, action: "resume" | "rollback" | "abandon") =>
    contextInvoke<Operation>("recover_operation", { id, action }),
  undoOperation: (id: string) =>
    contextInvoke<Operation>("undo_operation", { id }),
  planCleanup: (id: string) =>
    contextInvoke<CleanupPlan>("plan_cleanup", { id }),
  cleanupHistory: (id: string, cleanupId: string) =>
    contextInvoke<Operation>("cleanup_history", { id, cleanupId }),
  openFolder: (path: string) => contextInvoke<void>("open_folder", { path }),
};
