import { ImagePreview } from "./components/ImagePreview";
import { ImageAction } from "./components/ImageAction";
import { RecompareDialog } from "./components/RecompareDialog";
import { CloseGuard } from "./components/CloseGuard";
import {
  ProfileSwitcher,
  type ProfileBootView,
} from "./components/ProfileSwitcher";
import { ProfileControls } from "./components/ProfileControls";
import {
  RecoveryReplacement,
  type RecoveryReplacementCommands,
} from "./components/RecoveryReplacement";
import {
  useState,
  useRef,
  useEffect,
  useCallback,
  useMemo,
  type MouseEvent,
} from "react";
import {
  Images,
  FolderPlus,
  FolderOpen,
  Search,
  Check,
  ChevronRight,
  ArrowLeft,
  ArrowRight,
  ArrowDown,
  ArrowUp,
  Undo2,
  Star,
  GripVertical,
  Settings,
  HelpCircle,
  History,
  ShieldCheck,
  X,
  Maximize2,
  Minimize2,
  Play,
  CheckCircle2,
  LoaderCircle,
  AlertTriangle,
  Shuffle,
  Download,
  PenLine,
  RotateCcw,
  Archive,
  SlidersHorizontal,
} from "lucide-react";
import {
  boundPreview,
  prefetchPreviews,
  type BoundPreview,
} from "./components/preview";
import { previewError } from "./components/previewScheduler";
import {
  api,
  desktop,
  previewUrl,
  type ImageItem,
  type ScanReport,
  type ScanProgress,
  type Operation,
  type OperationRestriction,
  type BootStatus,
  type RecoveryReplacementSubject,
} from "./native";
import {
  createRun,
  getPair,
  decide,
  undo,
  getResult,
  progress,
  estimate,
  assertValidRun,
  MAX_CANDIDATES,
  MAX_DECISIONS,
  type RankingRun,
  type RankingMode,
} from "./domain/ranking";
import { rankingStageProgress } from "./domain/ranking/progress";
import { Modal } from "./components/Modal";
import { VirtualGrid, type GridFocusRequest } from "./components/VirtualGrid";
import { demoReport } from "./components/demo";
import { CacheSettings } from "./components/CacheSettings";
import { ImageWorkSettings } from "./components/ImageWorkSettings";
import { HelpDialog } from "./components/HelpDialog";
import {
  FileOperationRecovery,
  operationRestrictionFromBoot,
} from "./components/FileOperationRecovery";
import { StartupRecovery } from "./components/StartupRecovery";
import { StartupLoading } from "./components/StartupLoading";
import { ProtectionMaintenance } from "./components/ProtectionMaintenance";
import { SelectedFolderDialog } from "./components/SelectedFolderDialog";
import type { RecoveryStorageActions } from "./components/RecoveryStoragePanel";
import type { RecoveryWorkActions } from "./components/RecoveryWorkControls";
import {
  OperationDialog,
  OperationHistory,
  basename,
  bytes,
} from "./components/OperationDialog";
import {
  changeReview,
  undoReview,
  moveReview,
  compareReview,
  rangeSelection,
  type ReviewState,
} from "./components/review";
import "./styles.css";

type Screen = "sources" | "arena" | "review";
type SettingsState = {
  recursive: boolean;
  randomize: boolean;
  mode: RankingMode;
  cardSize: number;
  showNames: boolean;
  manifestDefault?: boolean;
  verifyCopyDefault?: boolean;
};
type SavedRun = { run: RankingRun; review: ReviewState | null; date: string };
type Session = {
  schemaVersion: 1;
  name: string;
  report: ScanReport;
  selected: string[];
  incomingSelected: boolean;
  run: RankingRun | null;
  review: ReviewState | null;
  archivedRuns: SavedRun[];
  screen: Screen;
  settings: SettingsState;
};
const emptyReport: ScanReport = { sources: [], images: [], ignored: [] };
const baseRecoveryStorageActions: Omit<
  RecoveryStorageActions,
  "confirmResume"
> = {
  loadStatus: () => api.recoveryStorageStatus(),
  prepareResume: (id) => api.prepareRecoveryResume(id),
  cancelResume: (token) => api.cancelRecoveryResume(token),
  previewCleanup: (id) => api.previewRecoveryCleanup(id),
  confirmCleanup: (token) => api.confirmRecoveryCleanup(token),
  cancelCleanup: (token) => api.cancelRecoveryCleanup(token),
};
const recoveryReplacementCommands: RecoveryReplacementCommands = {
  prepare: (subject, expected) =>
    api.prepareRecoveryReplacement(subject, expected),
  cancel: (token) => api.cancelRecoveryReplacement(token),
  exportCopy: (token) => api.exportRecoveryReplacementCopy(token),
  confirm: (subject, token, disposition) =>
    subject.kind === "fresh"
      ? api.restoreRecoveryCopy(
          subject.token,
          subject.acceptPending,
          token,
          disposition,
        )
      : api.confirmRecoveryResume(subject.token, token, disposition),
};
const recoveryWorkActions: RecoveryWorkActions = {
  status: () => api.recoveryWorkStatus(),
  cancel: (id) => api.cancelRecoveryWork(id),
};
const freshSession = (): Session => ({
  schemaVersion: 1,
  name: "Untitled collection",
  report: emptyReport,
  selected: [],
  incomingSelected: true,
  run: null,
  review: null,
  archivedRuns: [],
  screen: "sources",
  settings: {
    recursive: false,
    randomize: true,
    mode: "winner-stays",
    cardSize: 172,
    showNames: true,
  },
});
const modeNames: Record<RankingMode, string> = {
  "winner-stays": "Winner stays",
  adaptive: "Fast adaptive",
  "full-pairwise": "Full pairwise",
};
const modeDescriptions: Record<RankingMode, string> = {
  "winner-stays":
    "Keep the preferred image on screen. Lock each winning band, then rank the remaining images.",
  adaptive:
    "Place images with fewer comparisons, then refine provisional bands. Best for larger collections.",
  "full-pairwise":
    "Compare every unique pair once. Review combines aggregate scores with head-to-head evidence.",
};
const storageKey = "image-ranker-demo-session-v1";
const available = (image: ImageItem) =>
  !image.availability || image.availability === "ready";
const allImages = (report: ScanReport) => [
  ...report.images,
  ...(report.retainedImages || []),
];
export function mergeScanReport(current: Session, report: ScanReport): Session {
  const known = new Set(current.report.images.map((image) => image.id)),
    incoming = new Set(report.images.map((image) => image.id));
  const protectedIds = new Set([
    ...(current.run?.candidates || []),
    ...current.archivedRuns.flatMap((saved) => saved.run.candidates),
  ]);
  const retained = new Map(
    (report.retainedImages || [])
      .filter((image) => !incoming.has(image.id))
      .map((image) => [image.id, image]),
  );
  for (const image of allImages(current.report))
    if (
      protectedIds.has(image.id) &&
      !incoming.has(image.id) &&
      !retained.has(image.id)
    )
      retained.set(image.id, { ...image, sourceIds: [] });
  return {
    ...current,
    report: { ...report, retainedImages: [...retained.values()] },
    selected: [
      ...new Set([
        ...current.selected.filter((id) => incoming.has(id)),
        ...report.images
          .filter(
            (image) =>
              !known.has(image.id) &&
              available(image) &&
              current.incomingSelected,
          )
          .map((image) => image.id),
      ]),
    ],
    name:
      current.name === "Untitled collection" && report.sources.length
        ? report.sources[0].name
        : current.name,
  };
}
export function mergeScanBatch(
  current: Session,
  event: ScanProgress,
  recursive: boolean,
): Session {
  if (!event.batch.length) return current;
  const incoming = new Map(event.batch.map((image) => [image.id, image]));
  const images = current.report.images.map(
      (image) => incoming.get(image.id) || image,
    ),
    known = new Set(current.report.images.map((image) => image.id));
  images.push(...event.batch.filter((image) => !known.has(image.id)));
  const path = event.batch[0].path.replace(/[\\/][^\\/]*$/, "");
  const sources = current.report.sources.some(
    (source) => source.id === event.sourceId,
  )
    ? current.report.sources
    : [
        ...current.report.sources,
        {
          id: event.sourceId,
          path,
          name: basename(path) || "Scanning folder",
          recursive,
          count: event.eligible,
        },
      ];
  return mergeScanReport(current, { ...current.report, images, sources });
}
function stringArray(value: unknown): value is string[] {
  return (
    Array.isArray(value) &&
    value.every((x) => typeof x === "string") &&
    new Set(value).size === value.length
  );
}
function validateReview(
  review: ReviewState,
  run: RankingRun,
  ids: Set<string>,
): void {
  const validSnapshot = (
    snapshot: ReviewState | ReviewState["history"][number],
  ) => {
    if (
      !snapshot ||
      !stringArray(snapshot.order) ||
      snapshot.order.length !== run.candidates.length ||
      snapshot.order.some(
        (id) => !Object.hasOwn(run.indices, id) || !ids.has(id),
      ) ||
      !stringArray(snapshot.picks) ||
      snapshot.picks.some((id) => !snapshot.order.includes(id)) ||
      !stringArray(snapshot.manual) ||
      snapshot.manual.some((id) => !snapshot.order.includes(id)) ||
      !Array.isArray(snapshot.bands)
    )
      return false;
    const seen = new Set<string>();
    return snapshot.bands.every(
      (band) =>
        stringArray(band) &&
        band.length > 1 &&
        band.every((id) => {
          if (seen.has(id) || !snapshot.order.includes(id)) return false;
          seen.add(id);
          return true;
        }),
    );
  };
  if (
    getPair(run) ||
    !validSnapshot(review) ||
    !stringArray(review.computed) ||
    !Number.isSafeInteger(review.revision) ||
    review.revision < 1 ||
    !Array.isArray(review.history) ||
    !review.history.every(validSnapshot) ||
    JSON.stringify(review.computed) !== JSON.stringify(getResult(run).order)
  )
    throw new Error(
      "The saved review is invalid. Its original document has been preserved.",
    );
}
export function validateSession(value: unknown): Session {
  if (!value || typeof value !== "object")
    throw new Error("Saved session is not a valid document.");
  const s = value as Session;
  if (s.schemaVersion !== 1)
    throw new Error(
      "This session uses an unsupported version. Its saved files have been preserved.",
    );
  if (
    typeof s.name !== "string" ||
    typeof s.incomingSelected !== "boolean" ||
    !s.report ||
    !Array.isArray(s.report.images) ||
    !Array.isArray(s.report.sources) ||
    !Array.isArray(s.report.ignored) ||
    !stringArray(s.selected) ||
    !Array.isArray(s.archivedRuns) ||
    !s.settings ||
    !Object.hasOwn(modeNames, s.settings.mode) ||
    !Number.isFinite(s.settings.cardSize) ||
    s.settings.cardSize < 130 ||
    s.settings.cardSize > 260 ||
    typeof s.settings.recursive !== "boolean" ||
    typeof s.settings.randomize !== "boolean" ||
    typeof s.settings.showNames !== "boolean" ||
    (s.settings.manifestDefault !== undefined &&
      typeof s.settings.manifestDefault !== "boolean") ||
    (s.settings.verifyCopyDefault !== undefined &&
      typeof s.settings.verifyCopyDefault !== "boolean") ||
    !["sources", "arena", "review"].includes(s.screen)
  )
    throw new Error(
      "The saved session is incomplete. Its saved files have been preserved.",
    );
  const sourceIds = new Set<string>();
  for (const source of s.report.sources) {
    if (
      !source ||
      typeof source.id !== "string" ||
      sourceIds.has(source.id) ||
      typeof source.name !== "string" ||
      typeof source.path !== "string" ||
      typeof source.recursive !== "boolean" ||
      !Number.isSafeInteger(source.count) ||
      source.count < 0
    )
      throw new Error("The saved folders are invalid.");
    sourceIds.add(source.id);
  }
  if (
    s.report.ignored.some(
      (entry) =>
        !entry ||
        typeof entry.path !== "string" ||
        typeof entry.reason !== "string",
    )
  )
    throw new Error("The saved scan report is invalid.");
  const ids = new Set<string>();
  if (
    s.report.retainedImages !== undefined &&
    !Array.isArray(s.report.retainedImages)
  )
    throw new Error("The retained image catalog is invalid.");
  for (const image of allImages(s.report)) {
    if (
      !image ||
      typeof image.id !== "string" ||
      ids.has(image.id) ||
      typeof image.path !== "string" ||
      typeof image.previewPath !== "string" ||
      typeof image.originalName !== "string" ||
      typeof image.contentRevision !== "string" ||
      !stringArray(image.sourceIds) ||
      !Number.isFinite(image.width) ||
      !Number.isFinite(image.height)
    )
      throw new Error("The saved image catalog is invalid.");
    ids.add(image.id);
  }
  if (s.selected.some((id) => !ids.has(id)))
    throw new Error("Saved selection references missing catalog entries.");
  if (s.run) {
    assertValidRun(s.run);
    if (s.run.candidates.some((id) => !ids.has(id)))
      throw new Error("Saved ranking references missing catalog entries.");
  }
  if (s.review) {
    if (!s.run) throw new Error("The saved review has no ranking run.");
    validateReview(s.review, s.run, ids);
  }
  if ((s.screen === "arena" && !s.run) || (s.screen === "review" && !s.review))
    throw new Error("The saved workspace references a missing ranking.");
  for (const saved of s.archivedRuns) {
    if (!saved || typeof saved.date !== "string")
      throw new Error("An archived run is invalid.");
    assertValidRun(saved.run);
    if (saved.run.candidates.some((id) => !ids.has(id)))
      throw new Error("An archived run references missing catalog entries.");
    if (saved.review) validateReview(saved.review, saved.run, ids);
  }
  return s;
}

export function Inspection({
  item,
  onClose,
}: {
  item: ImageItem;
  onClose: () => void;
}) {
  const [full, setFull] = useState(false),
    [retry, setRetry] = useState(0),
    [canRetry, setCanRetry] = useState(false),
    [display, setDisplay] = useState<{
      key: string;
      url: string;
      width: number;
    } | null>(null),
    [error, setError] = useState(""),
    [loading, setLoading] = useState(desktop);
  const key = `${item.id}:${item.contentRevision}:${item.availability || "ready"}:${item.width}:${item.height}:${full}:${retry}`;
  const activeKey = useRef(key),
    leaseRef = useRef<BoundPreview | null>(null);
  activeKey.current = key;
  const release = () => {
    const lease = leaseRef.current;
    leaseRef.current = null;
    if (lease) void lease.release().catch(() => {});
  };
  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    let lease: BoundPreview | undefined;
    activeKey.current = key;
    setDisplay(null);
    setLoading(desktop);
    setError("");
    setCanRetry(false);
    if (desktop) {
      boundPreview(item, full ? Math.max(item.width, item.height) : 2048, {
        priority: "current",
        signal: controller.signal,
      })
        .then((preview) => {
          if (!active || activeKey.current !== key) {
            void preview.release().catch(() => {});
            return;
          }
          lease = preview;
          leaseRef.current = preview;
          setDisplay({ key, url: preview.url, width: preview.response.width });
        })
        .catch((reason) => {
          if (active) {
            const failure = previewError(reason);
            setError(failure.message);
            setCanRetry(failure.retryable);
            setLoading(false);
          }
        });
    } else setDisplay({ key, url: previewUrl(item), width: item.width });
    return () => {
      active = false;
      controller.abort();
      if (activeKey.current === key) activeKey.current = "";
      if (lease) void lease.release().catch(() => {});
      if (leaseRef.current === lease) leaseRef.current = null;
    };
  }, [key]);
  useEffect(() => {
    const keyboard = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() === "f" && !event.repeat) {
        event.preventDefault();
        setFull((value) => !value);
      }
    };
    window.addEventListener("keydown", keyboard);
    return () => window.removeEventListener("keydown", keyboard);
  }, []);
  const fail = () => {
    if (activeKey.current !== key) return;
    release();
    setDisplay(null);
    setLoading(false);
    setCanRetry(true);
    setError(
      "The full-size preview could not be displayed. Reconnect the source folder and retry.",
    );
  };
  return (
    <Modal
      title={basename(item.path)}
      onClose={onClose}
      wide
      footer={
        <>
          <span className="footer-note">
            {item.width} × {item.height} · {bytes(item.sizeBytes)}
          </span>
          <button onClick={() => setFull((value) => !value)}>
            {full ? <Minimize2 size={17} /> : <Maximize2 size={17} />}{" "}
            {full ? "Fit image" : "Actual size"} <kbd>F</kbd>
          </button>
          {canRetry &&
            (!item.availability || item.availability === "ready") && (
              <button onClick={() => setRetry((value) => value + 1)}>
                Retry preview
              </button>
            )}
          <button onClick={onClose}>
            Close <kbd>Esc</kbd>
          </button>
        </>
      }
    >
      <div className={`inspection ${full ? "actual" : ""}`}>
        {loading && (
          <div className="inspection-loading">
            <LoaderCircle className="spin" /> Loading full-size preview
          </div>
        )}
        {display?.key === key && (
          <img
            key={display.url}
            src={display.url}
            alt={basename(item.path)}
            style={
              full ? { width: display.width, maxWidth: "none" } : undefined
            }
            onLoad={(event) => {
              const image = event.currentTarget;
              const ready = () => {
                if (activeKey.current === key) setLoading(false);
              };
              if (typeof image.decode === "function")
                void image.decode().then(ready).catch(fail);
              else ready();
            }}
            onError={fail}
          />
        )}
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <p className="muted break">{item.path}</p>
    </Modal>
  );
}
export default function App() {
  const [session, setSession] = useState<Session>(freshSession),
    sessionRef = useRef(session);
  const [ready, setReady] = useState(false),
    [saveStatus, setSaveStatus] = useState<"saved" | "saving" | "failed">(
      "saved",
    ),
    [saveError, setSaveError] = useState(""),
    [loadError, setLoadError] = useState("");
  const queue = useRef<Promise<void>>(Promise.resolve()),
    loadGeneration = useRef(0),
    loadInFlight = useRef(false),
    startupWait = useRef(false),
    startupJob = useRef<string | null>(null),
    closeFrozen = useRef(false),
    profileFrozen = useRef(false),
    protectionFrozen = useRef(false),
    operationRestrictionRef = useRef<OperationRestriction | null>(null),
    profileEpoch = useRef(0),
    profileReloading = useRef(false),
    revision = useRef(0),
    saveBusy = useRef(false),
    sessionHydrated = useRef(false),
    voteGuard = useRef(false);
  const [closeRequested, setCloseRequested] = useState(false),
    [nativeWorkBusy, setNativeWorkBusy] = useState(false);
  const [operationRestriction, setOperationRestriction] =
    useState<OperationRestriction | null>(null);
  const [startupWaiting, setStartupWaiting] = useState(false);
  const [profileHelp, setProfileHelp] = useState(false);
  const [protectionOpen, setProtectionOpen] = useState(false),
    [protectionBusy, setProtectionBusy] = useState(false);
  const protectionChanged = useCallback((busy: boolean) => {
    protectionFrozen.current = busy;
    setProtectionBusy(busy);
  }, []);
  const openProtection = () => {
    if (
      operationRestrictionRef.current ||
      profileFrozen.current ||
      closeFrozen.current
    )
      return;
    protectionFrozen.current = true;
    setProtectionOpen(true);
    setProtectionBusy(true);
    setModal(null);
  };
  const [profileRequest, setProfileRequest] = useState<{
    mode: "create" | "open";
    generation: number;
  } | null>(null);
  // FIX 1: Both recovery entrances share one synchronously frozen replacement.
  const [replacementRequest, setReplacementRequest] = useState<{
    subject: RecoveryReplacementSubject;
    generation: number;
    resolve: (status: BootStatus) => void;
    reject: (reason: Error) => void;
  } | null>(null);
  const renderedProfileEpoch = profileEpoch.current;
  const [committedReload, setCommittedReload] = useState<{
    status: ProfileBootView;
    error: string;
  } | null>(null);
  const freezeForClose = useCallback((frozen: boolean) => {
    closeFrozen.current = frozen;
    setCloseRequested(frozen);
  }, []);
  const [search, setSearch] = useState(""),
    [sort, setSort] = useState("name"),
    [folder, setFolder] = useState("all"),
    [scanning, setScanning] = useState(false),
    [error, setError] = useState(""),
    [toast, setToast] = useState("");
  const [scanProgress, setScanProgress] = useState({ scanned: 0, eligible: 0 }),
    [cancellingScan, setCancellingScan] = useState(false),
    scanGeneration = useRef(0);
  const [scanDraft, setScanDraft] = useState<ScanReport | null>(null),
    [scanIntents, setScanIntents] = useState<Map<string, boolean>>(new Map()),
    scanIntentsRef = useRef(new Map<string, boolean>()),
    scanIncomingRule = useRef<((image: ImageItem) => boolean) | null>(null);
  const rememberScanIntents = (next: Map<string, boolean>) => {
    scanIntentsRef.current = next;
    setScanIntents(next);
  };
  const [sourceSidebarExpanded, setSourceSidebarExpanded] = useState(false);
  const [bootStatus, setBootStatus] = useState<BootStatus | null>(null),
    [recoveryBusy, setRecoveryBusy] = useState(false),
    [reconnecting, setReconnecting] = useState<string | null>(null);
  const [recoveryNotes, setRecoveryNotes] = useState<string[]>([]),
    [incompleteOperations, setIncompleteOperations] = useState(0);
  useEffect(() => {
    if (!desktop) return;
    return api.onActivationRestriction?.((status) => {
      // Native still owns the original context and pending work. Restrict the
      // view without replacing sessionRef, the save queue, or its revisions.
      loadGeneration.current++;
      scanGeneration.current++;
      setBootStatus(status);
      setReady(false);
      setLoadError("");
      setSaveStatus("failed");
      setSaveError(status.issues.map((issue) => issue.message).join(" "));
      setModal(null);
    });
  }, []);
  const restrictOperation = useCallback((failure: OperationRestriction) => {
    // A generic publication error cannot downgrade a known unverified panic.
    const retained =
      operationRestrictionRef.current?.kind === "restart"
        ? operationRestrictionRef.current
        : failure;
    operationRestrictionRef.current = retained;
    setOperationRestriction(retained);
    setModal(null);
    setInspection(null);
  }, []);
  useEffect(
    () =>
      desktop ? api.onOperationRestriction?.(restrictOperation) : undefined,
    [restrictOperation],
  );
  const [recoveryCopy, setRecoveryCopy] = useState<{
    state: "idle" | "exporting" | "saved" | "cancelled" | "failed";
    message: string;
    revision: number;
  }>({ state: "idle", message: "", revision: 0 });
  const [modal, setModal] = useState<
      | null
      | "preflight"
      | "settings"
      | "help"
      | "ignored"
      | "history"
      | "saved"
      | "rename"
      | "copy"
      | "copy-all"
      | "selected-folder"
      | "move"
      | "recompare"
      | "recovery"
    >(null),
    [inspection, setInspection] = useState<ImageItem | null>(null);
  const [reviewFilter, setReviewFilter] = useState("all"),
    [reviewSelected, setReviewSelected] = useState<string[]>([]),
    [reviewFocus, setReviewFocus] = useState<GridFocusRequest | null>(null),
    [moveRank, setMoveRank] = useState(1),
    [dragging, setDragging] = useState<string | null>(null);
  const [pairReady, setPairReady] = useState<{
    id: string;
    left: boolean;
    right: boolean;
  }>({ id: "", left: false, right: false });
  const selectionAnchor = useRef<string | null>(null),
    clickTimer = useRef<ReturnType<typeof setTimeout> | null>(null),
    toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const items = useMemo(
    () => new Map(allImages(session.report).map((item) => [item.id, item])),
    [session.report],
  );
  const displayedReport = scanDraft || session.report,
    committedIds = new Set(session.report.images.map((image) => image.id)),
    sourceItems = new Map(
      displayedReport.images.map((item) => [item.id, item]),
    );
  const sources = new Set(displayedReport.sources.map((source) => source.id)),
    collection = displayedReport.images.filter((image) =>
      image.sourceIds.some((id) => sources.has(id)),
    );
  const authorizedSources = new Set(
    session.report.sources
      .filter((source) => source.available !== false)
      .map((source) => source.id),
  );
  const collectionIds = new Set(
    collection
      .filter(
        (image) =>
          available(image) &&
          image.sourceIds.some(
            (id) =>
              authorizedSources.has(id) ||
              (scanning && !committedIds.has(image.id)),
          ),
      )
      .map((image) => image.id),
  );
  const selectedIds = session.selected.filter((id) => collectionIds.has(id));
  const selectedSet = new Set(selectedIds);
  for (const image of collection)
    if (
      !committedIds.has(image.id) &&
      collectionIds.has(image.id) &&
      (scanIntents.get(image.id) ??
        scanIncomingRule.current?.(image) ??
        session.incomingSelected)
    )
      selectedSet.add(image.id);
  const pair = session.run ? getPair(session.run) : null,
    runProgress = session.run ? progress(session.run) : null,
    stageProgress = session.run ? rankingStageProgress(session.run) : null;
  const rankingResult = useMemo(
    () => (session.run && !session.run.pair ? getResult(session.run) : null),
    [session.run],
  );
  const unavailableRun =
    session.run?.candidates.filter(
      (id) =>
        !items.has(id) ||
        !available(items.get(id)!) ||
        !items
          .get(id)!
          .sourceIds.some((sourceId) => authorizedSources.has(sourceId)),
    ) || [];
  const blocked =
    saveStatus === "failed" ||
    !ready ||
    closeRequested ||
    !!profileRequest ||
    !!replacementRequest ||
    protectionBusy ||
    !!operationRestriction;
  const say = (text: string) => {
    setToast(text);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(""), 4200);
  };
  const save = useCallback(
    (next: Session, allowDuringClose = false, allowDuringProfile = false) => {
      if (operationRestrictionRef.current)
        return Promise.reject(
          new Error(operationRestrictionRef.current.message),
        );
      if (protectionFrozen.current)
        return Promise.reject(
          new Error(
            "Protection storage maintenance is in progress. Wait for native work to finish before editing.",
          ),
        );
      if (
        profileReloading.current ||
        (profileFrozen.current && !allowDuringProfile)
      )
        return Promise.reject(
          new Error(
            "Workspace replacement is in progress. Keep the current workspace before editing.",
          ),
        );
      if (closeFrozen.current && !allowDuringClose)
        return Promise.reject(
          new Error("Closing is in progress. Choose Stay open before editing."),
        );
      sessionRef.current = next;
      sessionHydrated.current = true;
      setSession(next);
      const expected = ++revision.current;
      const expectedProfile = profileEpoch.current;
      saveBusy.current = true;
      setSaveStatus("saving");
      setSaveError("");
      const pending = queue.current
        .catch(() => {})
        .then(async () => {
          if (
            expectedProfile !== profileEpoch.current ||
            profileReloading.current
          )
            throw new Error(
              "STALE_CONTEXT: ignored a queued save from the previous profile.",
            );
          if (operationRestrictionRef.current)
            throw new Error(operationRestrictionRef.current.message);
          if (desktop) await api.saveSession(next);
          else localStorage.setItem(storageKey, JSON.stringify(next));
        });
      queue.current = pending;
      pending.then(
        () => {
          if (
            expected === revision.current &&
            expectedProfile === profileEpoch.current
          ) {
            saveBusy.current = false;
            setSaveStatus("saved");
          }
        },
        (reason) => {
          if (
            expected === revision.current &&
            expectedProfile === profileEpoch.current
          ) {
            saveBusy.current = false;
            setSaveStatus("failed");
            setSaveError(String(reason));
          }
        },
      );
      return pending;
    },
    [],
  );
  const update = (
    change: (current: Session) => Session,
    allowDuringClose = false,
  ) => save(change(sessionRef.current), allowDuringClose);
  const handle = (promise: Promise<unknown>) => {
    const expectedProfile = profileEpoch.current;
    promise.catch((reason) => {
      if (expectedProfile === profileEpoch.current) setError(String(reason));
    });
  };
  const exportRecoveryCopy = async () => {
    if (!desktop || recoveryCopy.state === "exporting") return;
    const capturedRevision = revision.current;
    const expectedProfile = profileEpoch.current;
    const capturedSession = structuredClone(sessionRef.current);
    setRecoveryCopy({
      state: "exporting",
      message:
        "Choose a new filename in the save dialog. Cancel there to keep your workspace unchanged.",
      revision: capturedRevision,
    });
    try {
      const path = await api.exportSessionCopy(capturedSession);
      if (expectedProfile !== profileEpoch.current) return;
      setRecoveryCopy(
        path
          ? { state: "saved", message: path, revision: capturedRevision }
          : {
              state: "cancelled",
              message: "Recovery copy cancelled. Your work remains in memory.",
              revision: capturedRevision,
            },
      );
    } catch (reason) {
      if (expectedProfile !== profileEpoch.current) return;
      setRecoveryCopy({
        state: "failed",
        message: String(reason),
        revision: capturedRevision,
      });
    }
  };
  const load = useCallback(async (retry = false) => {
    if (
      profileFrozen.current ||
      profileReloading.current ||
      loadInFlight.current
    )
      return;
    loadInFlight.current = true;
    const expectedLoad = ++loadGeneration.current;
    let acknowledge!: () => void, rejectHydration!: (reason: unknown) => void;
    const hydration = new Promise<void>((resolve, reject) => {
      acknowledge = resolve;
      rejectHydration = reject;
    });
    // Observe rejection immediately without replacing the rejected barrier:
    // CloseGuard must still fail its flush until a real retry is acknowledged.
    void hydration.catch(() => {});
    queue.current = hydration;
    saveBusy.current = true;
    setLoadError("");
    setReady(false);
    try {
      let observedBoot: BootStatus | null = null;
      if (desktop) {
        const boot = retry ? await api.retryBoot() : await api.getBootStatus();
        if (expectedLoad !== loadGeneration.current) {
          acknowledge();
          return;
        }
        setBootStatus(boot);
        const retainedOperation = operationRestrictionFromBoot(boot);
        if (retainedOperation) restrictOperation(retainedOperation);
        observedBoot = boot;
        // FIX 1: Boot publication and resource disposal are distinct. Never
        // hydrate a placeholder while startup still owns its native job.
        if (boot.state === "loading") {
          startupWait.current = true;
          setStartupWaiting(true);
        }
        if (boot.mode === "ready" || startupWait.current) {
          const work = await api.recoveryWorkStatus();
          if (expectedLoad !== loadGeneration.current) {
            acknowledge();
            return;
          }
          if (
            work.active &&
            ["startup-load", "switch-profile"].includes(work.active.kind)
          ) {
            startupWait.current = true;
            startupJob.current = work.active.id;
          }
          const terminal =
            !startupJob.current ||
            work.recent.some(
              (job) =>
                job.id === startupJob.current &&
                ["completed", "cancelled", "failed"].includes(job.state),
            );
          if (
            boot.state === "loading" ||
            (startupWait.current && (!!work.active || !terminal))
          ) {
            setStartupWaiting(true);
            saveBusy.current = false;
            rejectHydration(
              new Error("Native workspace loading has not finished."),
            );
            return;
          }
          startupWait.current = false;
          startupJob.current = null;
          setStartupWaiting(false);
        }
        if (boot.mode !== "ready") {
          saveBusy.current = false;
          rejectHydration(
            new Error(
              "The current workspace requires recovery before its saved state can be confirmed.",
            ),
          );
          return;
        }
      }
      const loaded = desktop ? await api.loadSessionState() : null;
      if (
        desktop &&
        (!loaded || loaded.session === undefined || !loaded.saveStatus)
      )
        throw new Error(
          "The native session hydration response is incomplete. Retry loading before editing.",
        );
      if (desktop) {
        const context = await api.rendererContext();
        if (
          !observedBoot ||
          observedBoot.contextId !== context ||
          loaded!.contextId !== context ||
          !observedBoot.profileGeneration ||
          loaded!.profileGeneration !== observedBoot.profileGeneration ||
          typeof loaded!.sessionGeneration !== "string" ||
          !loaded!.sessionGeneration
        )
          throw new Error(
            "STALE_CONTEXT: the session does not belong to the displayed profile. Reload this window before editing.",
          );
      }
      const value = desktop
        ? loaded!.session
        : JSON.parse(localStorage.getItem(storageKey) || "null");
      if (expectedLoad !== loadGeneration.current) {
        acknowledge();
        return;
      }
      const next = value === null ? freshSession() : validateSession(value);
      let pendingFailure = "";
      if (loaded) {
        const status = loaded.saveStatus;
        if (status.state !== "saved") {
          pendingFailure =
            status.message ||
            (status.state === "checkpoint-pending"
              ? "The session checkpoint is pending. Retry saving to confirm a durable boundary."
              : status.state === "pending"
                ? "The latest session changes have not received a durable acknowledgment. Retry saving."
                : "The native session save state is unrecognized. Retry saving before continuing.");
        }
      }
      if (expectedLoad !== loadGeneration.current) {
        acknowledge();
        return;
      }
      sessionRef.current = next;
      setSession(next);
      saveBusy.current = false;
      sessionHydrated.current = value !== null;
      setSaveStatus(pendingFailure ? "failed" : "saved");
      setSaveError(pendingFailure);
      if (pendingFailure) rejectHydration(new Error(pendingFailure));
      else acknowledge();
      setReady(true);
    } catch (reason) {
      if (expectedLoad !== loadGeneration.current) {
        acknowledge();
        return;
      }
      saveBusy.current = false;
      rejectHydration(reason);
      setLoadError(String(reason));
    } finally {
      loadInFlight.current = false;
    }
  }, []);
  const retryOperationPublication = async () => {
    if (operationRestrictionRef.current?.kind !== "publication") return;
    const contextId = await api.rendererContext();
    const observed = await api.getBootStatus();
    if (observed.contextId !== contextId)
      throw new Error(
        "STALE_CONTEXT: reload the selected workspace before checking file recovery.",
      );
    const before = operationRestrictionFromBoot(observed);
    if (before?.kind === "restart") {
      restrictOperation(before);
      throw new Error(before.message);
    }
    // A lost response may have published already. Fresh native boot decides
    // whether a retry is needed; local errors never substitute for that proof.
    if (before?.kind === "publication") await api.retryOperationPublication();
    const current = await api.getBootStatus();
    if (current.contextId !== contextId)
      throw new Error(
        "STALE_CONTEXT: file recovery belongs to a replaced workspace.",
      );
    const retained = operationRestrictionFromBoot(current);
    if (retained) {
      restrictOperation(retained);
      throw new Error(retained.message);
    }
    if (current.mode !== "ready")
      throw new Error(
        "Native workspace readiness is still unconfirmed. Check recovery again before editing.",
      );
    operationRestrictionRef.current = null;
    setOperationRestriction(null);
    setBootStatus(current);
    await load();
  };
  const openRecovery = async () => {
    if (
      !desktop ||
      operationRestrictionRef.current ||
      scanning ||
      nativeWorkBusy ||
      profileFrozen.current ||
      closeFrozen.current
    )
      return;
    const expectedProfile = profileEpoch.current;
    // Inspection is read-only. A rejected save remains rejected and must be
    // resolved by replacement authorization rather than hiding recovery.
    await queue.current.catch(() => {});
    if (expectedProfile !== profileEpoch.current || profileFrozen.current)
      return;
    setModal("recovery");
  };
  const flushSession = async () => {
    let pending: Promise<void>;
    do {
      pending = queue.current;
      await pending;
    } while (queue.current !== pending);
  };
  const requestReplacement = useCallback(
    (subject: RecoveryReplacementSubject): Promise<BootStatus> => {
      if (
        !desktop ||
        operationRestrictionRef.current ||
        profileFrozen.current ||
        closeFrozen.current
      )
        return Promise.reject(
          new Error("RECOVERY_BUSY: another workspace transition is active"),
        );
      profileFrozen.current = true;
      if (clickTimer.current) clearTimeout(clickTimer.current);
      return new Promise((resolve, reject) =>
        setReplacementRequest({
          subject,
          generation: profileEpoch.current,
          resolve,
          reject,
        }),
      );
    },
    [],
  );
  const recoveryStorageActions = useMemo<RecoveryStorageActions>(
    () => ({
      ...baseRecoveryStorageActions,
      confirmResume: (token) => requestReplacement({ kind: "resume", token }),
    }),
    [requestReplacement],
  );
  const beginProfileSwitch = (mode: "create" | "open") => {
    if (
      !desktop ||
      operationRestrictionRef.current ||
      profileFrozen.current ||
      closeFrozen.current
    )
      return;
    if (
      scanning ||
      nativeWorkBusy ||
      recoveryBusy ||
      recoveryCopy.state === "exporting"
    ) {
      setError(
        "Finish or cancel the active file or recovery work before changing profiles.",
      );
      return;
    }
    // Set the ref in the click handler, before React can repaint or a queued
    // keyboard/input callback can enqueue another mutation.
    profileFrozen.current = true;
    if (clickTimer.current) clearTimeout(clickTimer.current);
    setProfileRequest({ mode, generation: profileEpoch.current });
  };
  const profileSwitched = (status: ProfileBootView) => {
    profileReloading.current = true;
    profileEpoch.current++;
    loadGeneration.current++;
    scanGeneration.current++;
    revision.current++;
    saveBusy.current = false;
    setReady(false);
    setModal(null);
    setInspection(null);
    setBootStatus(status as BootStatus);
    setCommittedReload({ status, error: "" });
    try {
      api.reloadSelectedProfile();
    } catch (reason) {
      setCommittedReload({ status, error: String(reason) });
      throw reason;
    }
  };
  useEffect(() => {
    void load();
    return () => {
      if (clickTimer.current) clearTimeout(clickTimer.current);
      if (toastTimer.current) clearTimeout(toastTimer.current);
    };
  }, [load]);
  useEffect(() => {
    if (!desktop || !ready) return;
    let active = true;
    const expectedProfile = profileEpoch.current;
    api
      .recoveryNotes()
      .then((notes) => {
        if (active && expectedProfile === profileEpoch.current)
          setRecoveryNotes(notes);
      })
      .catch((reason) => {
        if (active && expectedProfile === profileEpoch.current)
          setError(String(reason));
      });
    api
      .listOperations()
      .then((operations) => {
        if (active && expectedProfile === profileEpoch.current)
          setIncompleteOperations(
            operations.filter(
              (operation) =>
                !["planned", "complete", "rolled-back", "abandoned"].includes(
                  operation.status,
                ),
            ).length,
          );
      })
      .catch((reason) => {
        if (active && expectedProfile === profileEpoch.current)
          setError(String(reason));
      });
    return () => {
      active = false;
    };
  }, [ready]);
  useEffect(() => {
    const unload = (event: BeforeUnloadEvent) => {
      if (
        !profileReloading.current &&
        (saveBusy.current || saveStatus === "failed")
      ) {
        event.preventDefault();
      }
    };
    window.addEventListener("beforeunload", unload);
    return () => window.removeEventListener("beforeunload", unload);
  }, [saveStatus]);
  useEffect(() => {
    if (
      profileFrozen.current ||
      !ready ||
      saveStatus !== "saved" ||
      !session.run ||
      pair ||
      session.screen !== "arena"
    )
      return;
    const result = getResult(session.run);
    handle(
      update((s) => ({
        ...s,
        screen: "review",
        review: {
          computed: result.order,
          order: result.order,
          bands: result.bands.filter((b) => b.length > 1),
          picks: [],
          manual: [],
          revision: 1,
          history: [],
        },
      })),
    );
    say("Ranking complete. Make the final order yours.");
  }, [pair?.id, session.screen, session.run, ready, saveStatus]);
  const closeModal = useCallback(() => setModal(null), []),
    closeInspection = useCallback(() => setInspection(null), []);
  const currentReady =
    pair &&
    pairReady.id === pair.id &&
    pairReady.left &&
    pairReady.right &&
    !unavailableRun.length;
  useEffect(() => {
    if (
      !desktop ||
      !ready ||
      session.screen !== "arena" ||
      !pair ||
      !session.run ||
      unavailableRun.length
    )
      return;
    const state = session.run.state;
    const next =
      state.kind === "winner-stays"
        ? state.queue
        : state.kind === "adaptive"
          ? session.run.order.slice(state.next)
          : session.run.order;
    const opponents = next
      .filter((id) => id !== pair.left && id !== pair.right)
      .slice(0, 2)
      .map((id) => items.get(id))
      .filter((item): item is ImageItem => !!item);
    return prefetchPreviews(opponents, 2048);
  }, [pair?.id, session.screen, ready, unavailableRun.length, items]);
  const vote = (decision: "left" | "right" | "both") => {
    const current = sessionRef.current,
      currentPair = current.run ? getPair(current.run) : null;
    if (
      !current.run ||
      !currentPair ||
      currentPair.id !== pair?.id ||
      voteGuard.current ||
      saveBusy.current ||
      blocked ||
      !currentReady ||
      inspection ||
      modal
    )
      return;
    voteGuard.current = true;
    try {
      const run = decide(current.run, currentPair.id, decision);
      handle(
        save({ ...current, run }).finally(() => {
          voteGuard.current = false;
        }),
      );
    } catch (reason) {
      voteGuard.current = false;
      setError(String(reason));
    }
  };
  const undoComparison = () => {
    const current = sessionRef.current;
    if (
      !current.run ||
      !current.run.history.length ||
      saveBusy.current ||
      blocked
    )
      return;
    try {
      const archived = current.review
        ? [
            ...current.archivedRuns,
            {
              run: current.run,
              review: current.review,
              date: new Date().toISOString(),
            },
          ]
        : current.archivedRuns;
      handle(
        save({
          ...current,
          run: undo(current.run),
          review: null,
          archivedRuns: archived,
          screen: "arena",
        }),
      );
    } catch (reason) {
      setError(String(reason));
    }
  };
  useEffect(() => {
    const keyboard = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        event.repeat ||
        modal ||
        profileFrozen.current ||
        inspection ||
        !ready ||
        event.target instanceof HTMLInputElement ||
        event.target instanceof HTMLTextAreaElement ||
        event.target instanceof HTMLSelectElement
      )
        return;
      if (
        event.key.toLowerCase() === "z" &&
        (event.ctrlKey || event.metaKey || session.screen === "arena")
      ) {
        event.preventDefault();
        if (session.screen === "review" && session.review?.history.length)
          handle(update((s) => ({ ...s, review: undoReview(s.review!) })));
        else undoComparison();
        return;
      }
      if (session.screen !== "arena") return;
      const decision = (
        {
          ArrowLeft: "left",
          "1": "left",
          ArrowDown: "both",
          "2": "both",
          b: "both",
          B: "both",
          ArrowRight: "right",
          "3": "right",
        } as Record<string, "left" | "right" | "both">
      )[event.key];
      if (decision) {
        event.preventDefault();
        vote(decision);
      }
    };
    window.addEventListener("keydown", keyboard);
    return () => window.removeEventListener("keydown", keyboard);
  });
  const scan = async () => {
    if (scanning || blocked) return;
    if (!desktop) {
      const report = demoReport();
      handle(
        update((s) => ({
          ...mergeScanReport(s, report),
          name: "Landscape study",
        })),
      );
      say("12 generated demo images loaded. No real files are accessed.");
      return;
    }
    setScanning(true);
    setScanDraft(null);
    scanIncomingRule.current = null;
    rememberScanIntents(new Map());
    setCancellingScan(false);
    setScanProgress({ scanned: 0, eligible: 0 });
    setError("");
    const generation = ++scanGeneration.current;
    let detach: (() => void) | undefined;
    try {
      detach = await api.subscribeScanProgress((event) => {
        if (scanGeneration.current !== generation) return;
        setScanProgress({ scanned: event.scanned, eligible: event.eligible });
        if (event.batch.length)
          setScanDraft(
            (prior) =>
              mergeScanBatch(
                {
                  ...sessionRef.current,
                  report: prior || sessionRef.current.report,
                },
                event,
                sessionRef.current.settings.recursive,
              ).report,
          );
      });
      const report = await api.scanSources(session.settings.recursive);
      if (scanGeneration.current === generation) {
        setScanDraft(null);
        await update((s) => {
          const merged = mergeScanReport(s, report);
          const admitted = new Set(
            report.images.filter(available).map((image) => image.id),
          );
          const selected = new Set(merged.selected);
          if (scanIncomingRule.current) {
            const known = new Set(s.report.images.map((image) => image.id));
            for (const image of report.images) {
              if (known.has(image.id) || !admitted.has(image.id)) continue;
              if (scanIncomingRule.current(image)) selected.add(image.id);
              else selected.delete(image.id);
            }
          }
          for (const [id, chosen] of scanIntentsRef.current) {
            if (!admitted.has(id)) continue;
            if (chosen) selected.add(id);
            else selected.delete(id);
          }
          return { ...merged, selected: [...selected] };
        }, true);
        if (report.cancelled)
          say(
            "Scan stopped. Discovered images and your existing work are saved.",
          );
      }
    } catch (reason) {
      if (scanGeneration.current === generation) setError(String(reason));
    } finally {
      detach?.();
      if (scanGeneration.current === generation) {
        scanGeneration.current++;
        setScanDraft(null);
        scanIncomingRule.current = null;
        rememberScanIntents(new Map());
        setScanning(false);
        setCancellingScan(false);
      }
    }
  };
  const reconnect = async (id: string) => {
    if (!desktop || scanning || blocked) return;
    const expectedProfile = profileEpoch.current;
    setReconnecting(id);
    setScanning(true);
    setError("");
    try {
      const report = await api.reconnectSource(id);
      if (expectedProfile !== profileEpoch.current) return;
      if (report) {
        await update((current) => mergeScanReport(current, report), true);
        say(
          "Folder identity checked. Unchanged images can resume their saved ranking.",
        );
      } else say("Reconnection cancelled. Your saved ranking is unchanged.");
    } catch (reason) {
      if (expectedProfile === profileEpoch.current) setError(String(reason));
    } finally {
      if (expectedProfile === profileEpoch.current) {
        setReconnecting(null);
        setScanning(false);
      }
    }
  };
  const bulk = (action: "all" | "none" | "invert") => {
    if (blocked) return;
    selectionAnchor.current = null;
    if (scanning) {
      const capturedFolder = folder,
        capturedSearch = search.toLowerCase(),
        previousRule = scanIncomingRule.current,
        previousDefault = sessionRef.current.incomingSelected;
      const matches = (image: ImageItem) =>
        (capturedFolder === "all" ||
          image.sourceIds.includes(capturedFolder)) &&
        basename(image.path).toLowerCase().includes(capturedSearch);
      scanIncomingRule.current =
        action === "none"
          ? () => false
          : action === "all"
            ? matches
            : (image) =>
                matches(image) && (previousRule?.(image) ?? previousDefault);
      const intents = new Map(scanIntentsRef.current);
      for (const image of collection)
        if (!committedIds.has(image.id))
          intents.set(
            image.id,
            bulkIds.has(image.id) &&
              (action === "all" ||
                (action === "invert" && !selectedSet.has(image.id))),
          );
      rememberScanIntents(intents);
    }
    handle(
      update((s) => {
        const ids = [...bulkIds].filter((id) => committedIds.has(id)),
          set = new Set(s.selected);
        return {
          ...s,
          selected:
            action === "all"
              ? ids
              : action === "none"
                ? []
                : ids.filter((id) => !set.has(id)),
          incomingSelected:
            action === "invert"
              ? s.incomingSelected
              : action === "all" && folder === "all" && !search,
        };
      }),
    );
  };
  const visible = useMemo(
    () =>
      collection
        .filter(
          (i) =>
            (folder === "all" || i.sourceIds.includes(folder)) &&
            basename(i.path).toLowerCase().includes(search.toLowerCase()),
        )
        .sort((a, b) =>
          sort === "date"
            ? b.modifiedAt - a.modifiedAt
            : sort === "size"
              ? b.sizeBytes - a.sizeBytes
              : sort === "folder"
                ? a.path.localeCompare(b.path)
                : basename(a.path).localeCompare(basename(b.path)),
        ),
    [displayedReport, folder, search, sort],
  );
  const bulkIds = new Set(
    visible
      .filter((image) => collectionIds.has(image.id))
      .map((image) => image.id),
  );
  const applySelection = (ids: string[]) => {
    if (blocked) return;
    const next = ids.filter((id) => collectionIds.has(id));
    const selected = new Set(next);
    if (scanning) {
      const intents = new Map(scanIntentsRef.current);
      for (const incoming of collectionIds)
        if (!committedIds.has(incoming))
          intents.set(incoming, selected.has(incoming));
      rememberScanIntents(intents);
    }
    const committed = next.filter((imageId) => committedIds.has(imageId));
    const previouslySelected = new Set(sessionRef.current.selected);
    if (
      committed.length !== previouslySelected.size ||
      committed.some((imageId) => !previouslySelected.has(imageId))
    )
      handle(update((s) => ({ ...s, selected: committed })));
  };
  const toggleSelection = (id: string, range = false) => {
    if (blocked || !collectionIds.has(id)) return;
    selectionAnchor.current = range ? selectionAnchor.current : id;
    applySelection(
      rangeSelection(
        [...selectedSet],
        visible.filter((image) => collectionIds.has(image.id)).map((i) => i.id),
        range ? selectionAnchor.current : null,
        id,
      ),
    );
  };
  const cardClick = (event: MouseEvent, id: string) => {
    if (clickTimer.current) clearTimeout(clickTimer.current);
    if (event.detail > 1) return;
    const shift = event.shiftKey;
    clickTimer.current = setTimeout(() => toggleSelection(id, shift), 220);
  };
  const inspectDouble = (item: ImageItem) => {
    if (clickTimer.current) clearTimeout(clickTimer.current);
    if (!committedIds.has(item.id)) return;
    setInspection(item);
  };
  const removeSource = async (id: string) => {
    if (scanning || blocked) return;
    const expectedProfile = profileEpoch.current;
    setNativeWorkBusy(true);
    try {
      if (desktop) {
        const report = await api.removeSource(id);
        if (expectedProfile !== profileEpoch.current) return;
        await update((s) => mergeScanReport(s, report));
      } else
        await update((s) => ({
          ...s,
          report: {
            ...s.report,
            sources: s.report.sources.filter((source) => source.id !== id),
          },
          selected: s.selected.filter((imageId) =>
            items
              .get(imageId)
              ?.sourceIds.some(
                (sourceId) =>
                  sourceId !== id &&
                  s.report.sources.some((source) => source.id === sourceId),
              ),
          ),
        }));
      if (folder === id) setFolder("all");
      say("Folder removed from selection. Saved ranking runs are preserved.");
    } catch (reason) {
      if (expectedProfile === profileEpoch.current) setError(String(reason));
    } finally {
      if (expectedProfile === profileEpoch.current) setNativeWorkBusy(false);
    }
  };
  const begin = (candidateIds = selectedIds) => {
    if (
      scanning ||
      !candidateIds.length ||
      candidateIds.some((id) => !collectionIds.has(id)) ||
      blocked ||
      saveStatus !== "saved"
    )
      return;
    try {
      const ids = [...candidateIds].sort((a, b) => {
        const aa = items.get(a)!.path,
          bb = items.get(b)!.path;
        return aa < bb ? -1 : aa > bb ? 1 : 0;
      });
      const run = createRun(
        ids,
        session.settings.mode,
        crypto.getRandomValues(new Uint32Array(1))[0],
        session.settings.randomize,
      );
      const complete = !getPair(run),
        result = complete ? getResult(run) : null;
      handle(
        update((s) => ({
          ...s,
          selected: [...candidateIds],
          run,
          review: result
            ? {
                computed: result.order,
                order: result.order,
                bands: [],
                picks: [],
                manual: [],
                revision: 1,
                history: [],
              }
            : null,
          archivedRuns: s.run
            ? [
                ...s.archivedRuns,
                {
                  run: s.run,
                  review: s.review,
                  date: new Date().toISOString(),
                },
              ]
            : s.archivedRuns,
          screen: complete ? "review" : "arena",
        })),
      );
      setReviewSelected([]);
      setReviewFilter("all");
      setSearch("");
      setFolder("all");
      closeModal();
    } catch (reason) {
      setError(String(reason));
    }
  };
  const editReview = (action: (review: ReviewState) => ReviewState) => {
    if (!session.review || blocked) return;
    handle(update((s) => ({ ...s, review: action(s.review!) })));
  };
  const move = (ids: string[], rank: number) => {
    editReview((review) => moveReview(review, ids, rank));
    say(
      `Moved ${ids.length === 1 ? "image" : `${ids.length} images`} to rank ${rank}.`,
    );
  };
  const operationComplete = async (
    operation: Operation,
    authoritativeReport?: ScanReport,
  ) => {
    if (
      operationRestrictionRef.current ||
      profileFrozen.current ||
      renderedProfileEpoch !== profileEpoch.current
    )
      return;
    if (operation.context?.sourceGroup) {
      const report =
        authoritativeReport || validateSession(await api.loadSession()).report;
      if (
        operationRestrictionRef.current ||
        profileFrozen.current ||
        renderedProfileEpoch !== profileEpoch.current
      )
        return;
      await update((s) => mergeScanReport(s, report), true);
    } else if (
      operation.kind === "rename" &&
      ["complete", "files-committed", "rolled-back"].includes(operation.status)
    ) {
      const reverse = operation.status === "rolled-back",
        rows = new Map(operation.rows.map((row) => [row.imageId, row]));
      const updateImage = (image: ImageItem) => {
        const row = rows.get(image.id);
        return row ? { ...image, path: reverse ? row.from : row.to } : image;
      };
      handle(
        update(
          (s) => ({
            ...s,
            report: {
              ...s.report,
              images: s.report.images.map(updateImage),
              retainedImages: s.report.retainedImages?.map(updateImage),
            },
          }),
          true,
        ),
      );
    }
    say(`File operation: ${operation.status}.`);
    if (desktop)
      api
        .listOperations()
        .then((operations) =>
          setIncompleteOperations(
            operations.filter(
              (op) =>
                !["planned", "complete", "rolled-back", "abandoned"].includes(
                  op.status,
                ),
            ).length,
          ),
        )
        .catch((reason) => setError(String(reason)));
  };
  const review = session.review,
    reviewIds = review?.order || [],
    picks = new Set(review?.picks || []),
    rankMap = new Map(reviewIds.map((id, index) => [id, index + 1]));
  const reviewEvidence = useMemo(() => {
    const labels = new Map<string, string>(),
      manual = new Set(review?.manual || []),
      computedRanks = new Map(
        (review?.computed || []).map((id, index) => [id, index + 1]),
      );
    for (const ambiguity of rankingResult?.ambiguities || [])
      for (const id of ambiguity.ids)
        if (!manual.has(id) && !labels.has(id))
          labels.set(
            id,
            ambiguity.kind === "cycle"
              ? "Circular preference"
              : ambiguity.kind === "draw"
                ? "Draw evidence"
                : ambiguity.kind === "anchor-conflict"
                  ? "Contradictory evidence"
                  : "Unresolved evidence",
          );
    for (const band of review?.bands || []) {
      const label = `Band ${Math.min(...band.map((id) => computedRanks.get(id) || 1))}`;
      for (const id of band) labels.set(id, label);
    }
    return labels;
  }, [review?.bands, review?.manual, review?.computed, rankingResult]);
  const tied = new Set(reviewEvidence.keys());
  useEffect(() => {
    const current = new Set(session.review?.order || []);
    setReviewSelected((selected) => selected.filter((id) => current.has(id)));
  }, [session.review?.computed]);
  const reviewVisible = reviewIds.filter((id) => {
    const item = items.get(id);
    return (
      item &&
      (reviewFilter !== "picks" || picks.has(id)) &&
      (reviewFilter !== "tied" || tied.has(id)) &&
      (folder === "all" || item.sourceIds.includes(folder)) &&
      basename(item.path).toLowerCase().includes(search.toLowerCase())
    );
  });
  const reviewSelectedSet = new Set(reviewSelected),
    reviewSelectedRanked = reviewIds.filter((id) => reviewSelectedSet.has(id)),
    reviewInteractionBlocked =
      blocked || saveStatus === "saving" || !!modal || !!inspection,
    canDragReview =
      !reviewInteractionBlocked &&
      reviewFilter === "all" &&
      folder === "all" &&
      !search;
  useEffect(() => {
    if (!canDragReview) setDragging(null);
  }, [canDragReview]);
  const workEstimate = (() => {
    try {
      return selectedIds.length
        ? estimate(selectedIds.length, session.settings.mode)
        : null;
    } catch {
      return null;
    }
  })();
  const workloadAllowed =
    !!workEstimate &&
    selectedIds.length <= MAX_CANDIDATES &&
    workEstimate.maximum <= MAX_DECISIONS;
  const setSetting = <K extends keyof SettingsState>(
    key: K,
    value: SettingsState[K],
  ) =>
    handle(
      update((s) => ({ ...s, settings: { ...s.settings, [key]: value } })),
    );

  return (
    <>
      <div
        className="app-shell"
        inert={
          !!operationRestriction ||
          protectionBusy ||
          protectionOpen ||
          closeRequested ||
          !!profileRequest ||
          !!replacementRequest ||
          !!committedReload
        }
      >
        <header className="app-header">
          <button
            className="brand"
            onClick={() => handle(update((s) => ({ ...s, screen: "sources" })))}
            disabled={!ready}
          >
            <span className="brand-mark">
              <Images size={20} />
            </span>
            <span>
              Image Ranker
              <small>
                {desktop ? "Your eye. Your order." : "Interactive browser demo"}
              </small>
            </span>
          </button>
          <nav className="workflow" aria-label="Workflow">
            <button
              aria-current={session.screen === "sources" ? "step" : undefined}
              onClick={() =>
                handle(update((s) => ({ ...s, screen: "sources" })))
              }
              disabled={!ready}
            >
              <span>1</span> Sources
            </button>
            <ChevronRight size={13} />
            <button
              aria-current={session.screen === "arena" ? "step" : undefined}
              disabled={!session.run || !pair || blocked}
              onClick={() => handle(update((s) => ({ ...s, screen: "arena" })))}
            >
              <span>2</span> Compare
            </button>
            <ChevronRight size={13} />
            <button
              aria-current={session.screen === "review" ? "step" : undefined}
              disabled={!review || blocked}
              onClick={() =>
                handle(update((s) => ({ ...s, screen: "review" })))
              }
            >
              <span>3</span> Review
            </button>
          </nav>
          <div className="header-actions">
            <span className={`save-status ${saveStatus}`} aria-live="polite">
              {saveStatus === "saving" ? (
                <LoaderCircle size={13} className="spin" />
              ) : saveStatus === "failed" ? (
                <AlertTriangle size={13} />
              ) : (
                <CheckCircle2 size={13} />
              )}{" "}
              {!ready
                ? !loadError &&
                  (startupWaiting || bootStatus?.state === "loading")
                  ? "Loading workspace…"
                  : loadError || bootStatus?.mode === "recovery"
                    ? "Read-only recovery"
                    : "Checking local records…"
                : saveStatus === "saved"
                  ? desktop
                    ? "Saved locally"
                    : "Saved in browser"
                  : saveStatus === "saving"
                    ? "Saving…"
                    : "Save failed"}
            </span>
            <button
              className="icon-button"
              title="File operation history"
              aria-label="File operation history"
              disabled={!ready}
              onClick={() => setModal("history")}
            >
              <History size={18} />
            </button>
            <button
              className="icon-button"
              title="Settings"
              aria-label="Settings"
              disabled={!ready}
              onClick={() => setModal("settings")}
            >
              <Settings size={18} />
            </button>
            <button
              className="icon-button"
              title="Help and keyboard shortcuts"
              aria-label="Help and keyboard shortcuts"
              onClick={() => setModal("help")}
            >
              <HelpCircle size={18} />
            </button>
          </div>
        </header>
        {saveStatus === "failed" && (
          <div className="error-bar" role="alert">
            <AlertTriangle size={18} />
            <span>
              Your latest work is still in memory. Saving failed: {saveError}
            </span>
            <button
              disabled={recoveryCopy.state === "exporting"}
              onClick={() => {
                setError("");
                handle(save(sessionRef.current));
              }}
            >
              Retry save
            </button>
            {desktop && (
              <button
                disabled={recoveryCopy.state === "exporting"}
                onClick={() => void exportRecoveryCopy()}
              >
                {recoveryCopy.state === "exporting" && (
                  <LoaderCircle size={16} className="spin" />
                )}
                {recoveryCopy.state === "exporting"
                  ? "Saving recovery copy…"
                  : "Save recovery copy"}
              </button>
            )}
          </div>
        )}
        {recoveryCopy.state !== "idle" && (
          <div
            className={`recovery-banner ${recoveryCopy.state === "failed" ? "failed" : ""}`}
            role={recoveryCopy.state === "failed" ? "alert" : "status"}
          >
            <div>
              {recoveryCopy.state === "saved" ? (
                <>
                  <strong>
                    {recoveryCopy.revision === revision.current
                      ? "Session recovery copy saved."
                      : "A recovery copy of the earlier state is saved; newer work still needs saving."}
                  </strong>
                  <p className="break">{recoveryCopy.message}</p>
                  <p>
                    This is session history, not a backup of your image files or
                    replacement-file backups.
                  </p>
                </>
              ) : (
                <p>{recoveryCopy.message}</p>
              )}
            </div>
            {recoveryCopy.state !== "exporting" && (
              <button
                className="icon-button"
                aria-label="Dismiss recovery copy status"
                onClick={() =>
                  setRecoveryCopy({ state: "idle", message: "", revision: 0 })
                }
              >
                <X size={16} />
              </button>
            )}
          </div>
        )}
        {error && (
          <div className="error-bar" role="alert">
            <AlertTriangle size={18} />
            <span>{error}</span>
            <button
              className="icon-button"
              aria-label="Dismiss error"
              onClick={() => setError("")}
            >
              <X size={17} />
            </button>
          </div>
        )}
        {ready && (incompleteOperations > 0 || recoveryNotes.length > 0) && (
          <div className="recovery-banner" role="status">
            <AlertTriangle size={17} />
            <div>
              {incompleteOperations > 0 && (
                <strong>
                  {incompleteOperations} file operations need recovery.
                </strong>
              )}
              {recoveryNotes.map((note, index) => (
                <p key={index}>{note}</p>
              ))}
            </div>
            <button onClick={() => setModal("history")}>
              Inspect recovery
            </button>
            {!incompleteOperations && (
              <button
                className="icon-button"
                aria-label="Dismiss recovery notes"
                onClick={() => setRecoveryNotes([])}
              >
                <X size={16} />
              </button>
            )}
          </div>
        )}
        {ready && unavailableRun.length > 0 && session.screen !== "sources" && (
          <div className="error-bar" role="alert">
            <AlertTriangle size={17} />
            <span>
              {unavailableRun.length} images in this saved run are missing,
              changed, or disconnected. Reconnect unchanged files, or start a
              new run from available images. Existing comparison history is
              preserved.
            </span>
            <button
              onClick={() =>
                handle(update((s) => ({ ...s, screen: "sources" })))
              }
            >
              Open sources
            </button>
          </div>
        )}
        {!ready ? (
          !loadError &&
          (startupWaiting || bootStatus?.state === "loading" || !bootStatus) ? (
            <StartupLoading
              watch={startupWaiting || bootStatus?.state === "loading"}
              onRefresh={load}
              onHelp={() => setModal("help")}
              work={desktop ? recoveryWorkActions : undefined}
            />
          ) : loadError || bootStatus?.mode === "recovery" ? (
            <main className="recovery-page">
              <StartupRecovery
                storage={desktop ? recoveryStorageActions : undefined}
                work={desktop ? recoveryWorkActions : undefined}
                onBusyChange={setRecoveryBusy}
                onChooseStorage={
                  desktop ? () => beginProfileSwitch("create") : undefined
                }
                onOpenProfile={
                  desktop ? () => beginProfileSwitch("open") : undefined
                }
                onHelp={() => setModal("help")}
                onMaintenance={desktop ? openProtection : undefined}
                onResumeProfile={desktop ? api.confirmProfileSwitch : undefined}
                onProfileResumed={(status) =>
                  profileSwitched(status as ProfileBootView)
                }
                status={bootStatus}
                failure={loadError}
                onOperations={
                  desktop &&
                  bootStatus?.pendingOperations &&
                  bootStatus.trustedOperationsAvailable !== false
                    ? () => setModal("history")
                    : undefined
                }
                onRetry={() => load(true)}
                onOpen={desktop ? () => api.inspectRecoveryCopy() : undefined}
                onRestore={
                  desktop
                    ? (token, acceptPending) =>
                        requestReplacement({
                          kind: "fresh",
                          token,
                          acceptPending,
                        })
                    : undefined
                }
                onRestored={() => {}}
              />
            </main>
          ) : (
            <main className="loading-screen">
              <>
                <LoaderCircle className="spin" size={28} />
                <p>Restoring your workspace…</p>
              </>
            </main>
          )
        ) : (
          <>
            {session.screen === "sources" && (
              <main className="source-layout">
                <aside
                  className={`source-sidebar ${sourceSidebarExpanded ? "expanded" : " "}`}
                >
                  <button
                    className="mobile-sources"
                    aria-expanded={sourceSidebarExpanded}
                    onClick={() => setSourceSidebarExpanded((value) => !value)}
                  >
                    <FolderOpen size={16} /> Folders (
                    {session.report.sources.length})
                  </button>
                  <div className="sidebar-heading">
                    <h2>Source folders</h2>
                    <span>{session.report.sources.length}</span>
                  </div>
                  <button
                    className="add-folder"
                    onClick={scan}
                    disabled={scanning || blocked}
                  >
                    <FolderPlus size={18} />
                    {scanning
                      ? reconnecting
                        ? "Checking folder…"
                        : "Scanning folders…"
                      : desktop
                        ? "Add folder"
                        : "Load demo images"}
                  </button>
                  <label className="check-label recursive">
                    <input
                      type="checkbox"
                      checked={session.settings.recursive}
                      onChange={(e) =>
                        setSetting("recursive", e.target.checked)
                      }
                      disabled={scanning}
                    />{" "}
                    Include subfolders
                  </label>
                  <div className="source-list">
                    <button
                      className={`source-item all-source ${folder === "all" ? "active" : ""}`}
                      onClick={() => setFolder("all")}
                    >
                      <Images size={18} />
                      <div>
                        <b>All images</b>
                        <small>
                          {collection.length} images · {selectedSet.size}{" "}
                          selected
                        </small>
                      </div>
                    </button>
                    {session.report.sources.map((source) => (
                      <div
                        key={source.id}
                        className={`source-item ${folder === source.id ? "active" : ""} ${source.available === false ? "disconnected" : ""}`}
                      >
                        <button
                          className="source-main"
                          onClick={() => setFolder(source.id)}
                          title={source.path}
                        >
                          <FolderOpen size={19} />
                          <span>
                            <b>{source.name}</b>
                            <small>
                              {
                                collection.filter((i) =>
                                  i.sourceIds.includes(source.id),
                                ).length
                              }{" "}
                              images ·{" "}
                              {
                                selectedIds.filter((id) =>
                                  items.get(id)?.sourceIds.includes(source.id),
                                ).length
                              }{" "}
                              selected
                            </small>
                          </span>
                        </button>
                        {desktop &&
                          (source.available === false ||
                            collection.some(
                              (image) =>
                                image.sourceIds.includes(source.id) &&
                                !available(image),
                            )) && (
                            <button
                              className="reconnect-source"
                              disabled={scanning || blocked}
                              onClick={() => void reconnect(source.id)}
                            >
                              <RotateCcw size={14} />
                              {reconnecting === source.id
                                ? "Checking…"
                                : "Reconnect folder"}
                            </button>
                          )}
                        <button
                          className="icon-button remove-source"
                          aria-label={`Remove ${source.name} from collection`}
                          disabled={scanning}
                          onClick={() => removeSource(source.id)}
                        >
                          <X size={14} />
                        </button>
                      </div>
                    ))}
                  </div>
                  {session.report.ignored.length > 0 && (
                    <button
                      className="text-button ignored-button"
                      onClick={() => setModal("ignored")}
                    >
                      <AlertTriangle size={15} />
                      {session.report.ignored.length} skipped or unavailable
                    </button>
                  )}
                  <div className="sidebar-bottom">
                    <ShieldCheck size={20} />
                    <div>
                      <b>Entirely local</b>
                      <p>
                        Your images stay on this computer. No account, uploads,
                        or cloud services.
                      </p>
                    </div>
                  </div>
                </aside>
                <section className="workspace">
                  <div className="workspace-heading">
                    <div>
                      <h1>{session.name}</h1>
                      <p>Select the images worth a closer look.</p>
                    </div>
                    <span className="collection-count">
                      {collection.length}
                      <small>images</small>
                    </span>
                  </div>
                  {session.run && (
                    <div className="resume-bar">
                      <div>
                        <Play size={17} />
                        <span>
                          {unavailableRun.length
                            ? `${unavailableRun.length} ranking images need their source folders reconnected.`
                            : pair
                              ? `${runProgress?.done} comparisons saved. Your ranking is ready to resume.`
                              : "Your completed ranking is saved."}
                        </span>
                      </div>
                      <button
                        onClick={() =>
                          handle(
                            update((s) => ({
                              ...s,
                              screen: pair ? "arena" : "review",
                            })),
                          )
                        }
                      >
                        {pair ? "Resume ranking" : "Open review"}
                        <ChevronRight size={15} />
                      </button>
                      {session.archivedRuns.length > 0 && (
                        <button
                          className="text-button"
                          onClick={() => setModal("saved")}
                        >
                          <Archive size={15} /> Saved runs (
                          {session.archivedRuns.length})
                        </button>
                      )}
                    </div>
                  )}
                  <div className="toolbar">
                    <div className="selection-tools">
                      <button
                        onClick={() => bulk("all")}
                        disabled={blocked || !collection.length}
                        title="Select all eligible images in this view. Selections outside this view are cleared."
                      >
                        All
                      </button>
                      <button
                        onClick={() => bulk("none")}
                        disabled={blocked || !collection.length}
                        title="Clear the selection. Selections outside this view are cleared."
                      >
                        None
                      </button>
                      <button
                        onClick={() => bulk("invert")}
                        disabled={blocked || !collection.length}
                        title="Invert eligible images in this view. Selections outside this view are cleared."
                      >
                        Invert
                      </button>
                      <span className="muted">
                        {folder === "all" && !search
                          ? "Entire collection"
                          : "Current view"}{" "}
                        · {bulkIds.size} matching
                      </span>
                    </div>
                    <div className="filter-tools">
                      <div className="search-field">
                        <Search size={16} />
                        <input
                          aria-label="Search filenames"
                          placeholder="Search filenames"
                          value={search}
                          onChange={(e) => setSearch(e.target.value)}
                        />
                        {search && (
                          <button
                            aria-label="Clear search"
                            className="icon-button"
                            onClick={() => setSearch("")}
                          >
                            <X size={13} />
                          </button>
                        )}
                      </div>
                      <select
                        aria-label="Sort images"
                        value={sort}
                        onChange={(e) => setSort(e.target.value)}
                      >
                        <option value="name">Name</option>
                        <option value="folder">Folder / path</option>
                        <option value="date">Newest first</option>
                        <option value="size">Largest first</option>
                      </select>
                      <label className="size-control">
                        <SlidersHorizontal size={16} />
                        <input
                          type="range"
                          min="130"
                          max="260"
                          step="10"
                          aria-label="Thumbnail size"
                          value={session.settings.cardSize}
                          onChange={(e) =>
                            setSetting("cardSize", Number(e.target.value))
                          }
                        />
                      </label>
                    </div>
                  </div>
                  {scanning && (
                    <div className="scan-banner" role="status">
                      <LoaderCircle className="spin" size={17} />
                      <span>
                        {reconnecting ? (
                          "Checking folder identity and image contents before reconnecting the saved run…"
                        ) : (
                          <>
                            {scanProgress.scanned} files checked ·{" "}
                            {scanProgress.eligible} images discovered. Selection
                            changes apply to incoming files.
                          </>
                        )}
                      </span>
                      {!reconnecting && (
                        <button
                          disabled={cancellingScan}
                          onClick={() => {
                            setCancellingScan(true);
                            api.cancelScan().catch((reason) => {
                              setError(String(reason));
                              setCancellingScan(false);
                            });
                          }}
                        >
                          {cancellingScan ? "Stopping…" : "Cancel scan"}
                        </button>
                      )}
                    </div>
                  )}
                  {collection.length === 0 ? (
                    <div className="empty-state">
                      <div className="empty-icon">
                        <Images size={42} />
                      </div>
                      <h2>Make room for your best images.</h2>
                      <p>
                        Add folders, choose your candidates, and compare them
                        two at a time. The final ranking is yours to refine.
                      </p>
                      <button
                        className="primary"
                        onClick={scan}
                        disabled={scanning}
                      >
                        <FolderPlus size={18} />
                        {desktop
                          ? "Add your first folder"
                          : "Explore with demo images"}
                      </button>
                      <small>
                        {desktop
                          ? "JPEG · PNG · WebP · GIF · BMP · TIFF"
                          : "12 generated landscape examples · no file access"}
                      </small>
                      {desktop && (
                        <button
                          className="text-button"
                          disabled={scanning}
                          onClick={() => void openRecovery()}
                        >
                          <FolderOpen size={16} /> Open recovery copy
                        </button>
                      )}
                    </div>
                  ) : visible.length === 0 ? (
                    <div className="empty-state">
                      <Search size={30} />
                      <h2>No matching images</h2>
                      <p>
                        Your selection is unchanged. Try a different filename or
                        folder.
                      </p>
                      <button
                        onClick={() => {
                          setSearch("");
                          setFolder("all");
                        }}
                      >
                        Clear filters
                      </button>
                    </div>
                  ) : (
                    <VirtualGrid
                      ids={visible.map((i) => i.id)}
                      size={session.settings.cardSize}
                      selection={{
                        selected: [...selectedSet],
                        selectable: [...collectionIds],
                        disabled: blocked || !!modal || !!inspection,
                        onStart: () => {
                          if (clickTimer.current)
                            clearTimeout(clickTimer.current);
                        },
                        onCommit: (ids) => {
                          selectionAnchor.current = ids.at(-1) ?? null;
                          applySelection(ids);
                        },
                      }}
                      render={(id, _index, dragSelected) => {
                        const item = sourceItems.get(id)!,
                          pending = !committedIds.has(id),
                          selected = dragSelected ?? selectedSet.has(id);
                        return (
                          <article
                            className={`image-card ${selected ? "selected" : "unselected"}`}
                          >
                            <ImageAction
                              className="thumbnail"
                              data-grid-selection-surface="true"
                              aria-label={`${selected ? "Deselect" : "Select"} ${basename(item.path)}`}
                              aria-pressed={selected}
                              onClick={(e) => cardClick(e, id)}
                              onDoubleClick={() => inspectDouble(item)}
                              onKeyDown={(e) => {
                                if (e.key === " ") {
                                  e.preventDefault();
                                  toggleSelection(id, e.shiftKey);
                                }
                              }}
                            >
                              {pending ? (
                                <div className="image-loading" role="status">
                                  <LoaderCircle className="spin" size={24} />
                                  <span>Discovered · preview after scan</span>
                                </div>
                              ) : (
                                <ImagePreview item={items.get(id)!} />
                              )}
                            </ImageAction>
                            <label
                              className="card-check"
                              title="Select candidate"
                            >
                              <input
                                type="checkbox"
                                checked={selected}
                                disabled={!collectionIds.has(item.id)}
                                aria-label={`Select ${basename(item.path)}`}
                                onChange={() => toggleSelection(id)}
                              />
                              <span>
                                <Check size={15} />
                              </span>
                            </label>
                            <button
                              className="inspect-button icon-button"
                              aria-label={`Inspect ${basename(item.path)}`}
                              disabled={pending}
                              onClick={() => setInspection(item)}
                            >
                              <Maximize2 size={16} />
                            </button>
                            <div className="card-meta">
                              <b title={item.path}>{basename(item.path)}</b>
                              <small>
                                {available(item)
                                  ? `${item.width} × ${item.height}`
                                  : `${item.availability} · reconnect folder`}
                                <span>{bytes(item.sizeBytes)}</span>
                              </small>
                            </div>
                          </article>
                        );
                      }}
                    />
                  )}
                  <footer className="action-bar">
                    <div
                      role="status"
                      aria-label="Selection summary"
                      aria-atomic="true"
                    >
                      <strong>{selectedSet.size} selected</strong>
                      <span className="muted">
                        Drag to select · Ctrl adds · Esc cancels{" · "}
                        {visible.length !== collection.length
                          ? `${visible.length} visible · `
                          : ""}
                        {collection.length} images in collection
                      </span>
                    </div>
                    <div className="button-row">
                      <button
                        disabled={
                          !desktop ||
                          !selectedIds.length ||
                          scanning ||
                          nativeWorkBusy ||
                          blocked ||
                          saveStatus !== "saved"
                        }
                        onClick={() => setModal("selected-folder")}
                      >
                        <FolderPlus size={16} /> Move to subfolder
                      </button>
                      <select
                        aria-label="Ranking mode"
                        value={session.settings.mode}
                        onChange={(e) =>
                          setSetting("mode", e.target.value as RankingMode)
                        }
                      >
                        {Object.entries(modeNames).map(([value, label]) => (
                          <option key={value} value={value}>
                            {label}
                          </option>
                        ))}
                      </select>
                      <button
                        className="primary"
                        disabled={
                          !selectedIds.length ||
                          scanning ||
                          blocked ||
                          saveStatus === "saving"
                        }
                        onClick={() => setModal("preflight")}
                      >
                        {selectedIds.length === 1
                          ? "Review 1 image"
                          : `Rank ${selectedIds.length} images`}
                        <ArrowRight size={17} />
                      </button>
                    </div>
                  </footer>
                </section>
              </main>
            )}
            {session.screen === "arena" && pair && session.run && (
              <main className="ranking-screen">
                <div className="arena-heading">
                  <button
                    onClick={() =>
                      handle(update((s) => ({ ...s, screen: "sources" })))
                    }
                    disabled={saveStatus === "saving"}
                  >
                    <ArrowLeft size={16} /> Save & exit
                  </button>
                  <div>
                    <h1>Which image do you prefer?</h1>
                    <p>
                      {modeNames[session.run.mode]} · {runProgress?.phase}
                    </p>
                  </div>
                  <button
                    disabled={
                      !session.run.history.length || saveStatus !== "saved"
                    }
                    onClick={undoComparison}
                  >
                    <Undo2 size={16} /> Undo <kbd>Ctrl Z</kbd>
                  </button>
                </div>
                <div
                  className="ranking-progress"
                  role="status"
                  aria-label="Comparison progress"
                  aria-atomic="true"
                >
                  <span>{runProgress?.done} comparisons made</span>
                  {stageProgress && (
                    <>
                      <span>
                        {stageProgress.label}: {stageProgress.done} of{" "}
                        {stageProgress.total}
                      </span>
                      <progress
                        aria-label={stageProgress.label}
                        max={stageProgress.total || 1}
                        value={stageProgress.done}
                      />
                    </>
                  )}
                  <span>
                    {runProgress?.remaining !== undefined
                      ? `${runProgress.remaining} remaining`
                      : "Both can change the number of comparisons"}
                  </span>
                  {stageProgress?.note && <span>{stageProgress.note}</span>}
                </div>
                <div className="arena">
                  {(["left", "right"] as const).map((side, index) => {
                    const item = items.get(pair[side]);
                    return (
                      <div className="choice" key={side}>
                        {item ? (
                          <>
                            <ImageAction
                              className="comparison-image"
                              aria-label={`Inspect ${side} image`}
                              onClick={() => setInspection(item)}
                            >
                              <ImagePreview
                                key={`${pair.id}-${side}`}
                                item={item}
                                size={2048}
                                onReady={(loaded) =>
                                  setPairReady((value) =>
                                    value.id === pair.id
                                      ? { ...value, [side]: loaded }
                                      : {
                                          id: pair.id,
                                          left: side === "left" && loaded,
                                          right: side === "right" && loaded,
                                        },
                                  )
                                }
                              />
                              <span className="side-tag">
                                {index === 0 ? "A" : "B"}
                              </span>
                              {pair.carried === item.id && (
                                <span className="carry-tag">
                                  Carried forward
                                </span>
                              )}
                              <span className="inspect-hint">
                                <Maximize2 size={14} /> Inspect
                              </span>
                            </ImageAction>
                            <div className="comparison-meta">
                              <b>
                                {session.settings.showNames
                                  ? basename(item.path)
                                  : `Image ${index === 0 ? "A" : "B"}`}
                              </b>
                              <span>
                                {item.width} × {item.height}
                              </span>
                            </div>
                          </>
                        ) : (
                          <div className="image-error">
                            <AlertTriangle />
                            Image unavailable. Return to sources and reconnect
                            its folder.
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
                <div className="vote-panel">
                  <div className="vote-buttons">
                    <button
                      className="vote"
                      disabled={!currentReady || saveStatus !== "saved"}
                      onClick={() => vote("left")}
                    >
                      <ArrowLeft size={20} />
                      <span>
                        Prefer left<small>← or 1</small>
                      </span>
                    </button>
                    <button
                      className="vote both"
                      disabled={!currentReady || saveStatus !== "saved"}
                      onClick={() => vote("both")}
                    >
                      <span>
                        Both<small>↓, B or 2</small>
                      </span>
                    </button>
                    <button
                      className="vote"
                      disabled={!currentReady || saveStatus !== "saved"}
                      onClick={() => vote("right")}
                    >
                      <span>
                        Prefer right<small>→ or 3</small>
                      </span>
                      <ArrowRight size={20} />
                    </button>
                  </div>
                  <p>
                    {saveStatus === "saving"
                      ? "Saving this decision…"
                      : !currentReady
                        ? "Waiting for both images to load…"
                        : "Both records a draw for this matchup. It does not permanently tie the images."}
                  </p>
                </div>
              </main>
            )}
            {session.screen === "review" && review && (
              <main className="review-screen">
                <div className="workspace-heading">
                  <div>
                    <h1>Your ranking, refined.</h1>
                    <p>
                      Drag to select. Use the handle to reorder. Star the images
                      you want to take forward.
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      disabled={
                        !session.run?.history.length || saveStatus !== "saved"
                      }
                      onClick={undoComparison}
                    >
                      <Undo2 size={16} /> Last comparison
                    </button>
                    <button
                      disabled={
                        !review.history.length || saveStatus !== "saved"
                      }
                      onClick={() => editReview(undoReview)}
                    >
                      <Undo2 size={16} /> Review edit
                    </button>
                  </div>
                </div>
                <div className="toolbar">
                  <div className="filter-tools">
                    <select
                      aria-label="Review filter"
                      value={reviewFilter}
                      onChange={(e) => setReviewFilter(e.target.value)}
                    >
                      <option value="all">All {reviewIds.length} images</option>
                      <option value="picks">Final picks ({picks.size})</option>
                      <option value="tied">
                        Unresolved evidence ({tied.size})
                      </option>
                    </select>
                    <select
                      aria-label="Review source folder"
                      value={folder}
                      onChange={(e) => setFolder(e.target.value)}
                    >
                      <option value="all">All folders</option>
                      {session.report.sources.map((source) => (
                        <option key={source.id} value={source.id}>
                          {source.name}
                        </option>
                      ))}
                    </select>
                    <div className="search-field">
                      <Search size={16} />
                      <input
                        aria-label="Search ranked images"
                        placeholder="Find in ranking"
                        value={search}
                        onChange={(e) => setSearch(e.target.value)}
                      />
                    </div>
                  </div>
                  <div className="button-row">
                    <button
                      disabled={
                        reviewInteractionBlocked ||
                        !reviewSelectedRanked.some((id) => !picks.has(id))
                      }
                      onClick={() => {
                        if (reviewInteractionBlocked) return;
                        editReview((r) => {
                          const added = reviewSelectedRanked.filter(
                            (id) => !r.picks.includes(id),
                          );
                          return added.length
                            ? changeReview(r, { picks: [...r.picks, ...added] })
                            : r;
                        });
                      }}
                    >
                      <Star size={16} /> Star selected
                    </button>
                    <button
                      disabled={
                        reviewInteractionBlocked || !reviewSelected.length
                      }
                      onClick={() => {
                        setMoveRank(1);
                        setModal("move");
                      }}
                    >
                      Move {reviewSelected.length || ""} to rank…
                    </button>
                    <button
                      disabled={
                        reviewInteractionBlocked ||
                        reviewSelected.length !== 2 ||
                        Math.abs(
                          (rankMap.get(reviewSelected[0]) || 0) -
                            (rankMap.get(reviewSelected[1]) || 0),
                        ) !== 1
                      }
                      onClick={() => setModal("recompare")}
                    >
                      Recompare two
                    </button>
                    <button
                      disabled={
                        reviewInteractionBlocked || !reviewSelected.length
                      }
                      onClick={() => setReviewSelected([])}
                    >
                      Clear selection
                    </button>
                  </div>
                </div>
                {rankingResult && rankingResult.ambiguities.length > 0 && (
                  <div className="notice compact">
                    <AlertTriangle size={16} />
                    {rankingResult.ambiguities.length} unresolved evidence
                    groups. Circular preferences and equal scores do not imply a
                    permanent tie. Your manual order is authoritative.
                  </div>
                )}
                {reviewVisible.length ? (
                  <VirtualGrid
                    ids={reviewVisible}
                    size={session.settings.cardSize}
                    extraHeight={120}
                    focusRequest={reviewFocus}
                    selection={{
                      selected: reviewSelected,
                      selectable: reviewIds,
                      disabled: reviewInteractionBlocked,
                      onCommit: setReviewSelected,
                    }}
                    render={(id, _index, dragSelected) => {
                      const item = items.get(id)!,
                        rank = rankMap.get(id)!,
                        selected = dragSelected ?? reviewSelectedSet.has(id);
                      return (
                        <article
                          className={`image-card review-card ${selected ? "selected" : ""} ${dragging === id ? "dragging" : ""}`}
                          onDragOver={(e) => {
                            if (!canDragReview || !dragging) return;
                            e.preventDefault();
                            e.dataTransfer.dropEffect = "move";
                          }}
                          onDrop={(e) => {
                            e.preventDefault();
                            const origin = e.dataTransfer.getData("text/plain");
                            if (
                              canDragReview &&
                              dragging === origin &&
                              review.order.includes(origin)
                            )
                              move(
                                reviewSelected.includes(origin)
                                  ? reviewSelected
                                  : [origin],
                                rank,
                              );
                            setDragging(null);
                          }}
                        >
                          <ImageAction
                            className="thumbnail"
                            data-grid-selection-surface="true"
                            disabled={reviewInteractionBlocked}
                            onClick={() => {
                              if (reviewInteractionBlocked) return;
                              setReviewSelected((ids) =>
                                ids.includes(id)
                                  ? ids.filter((i) => i !== id)
                                  : [...ids, id],
                              );
                            }}
                            onDoubleClick={() => setInspection(item)}
                            aria-label={`Select rank ${rank}: ${basename(item.path)}`}
                            aria-pressed={selected}
                          >
                            <ImagePreview item={item} />
                          </ImageAction>
                          <button
                            className="rank-number"
                            aria-label={`Inspect rank ${rank}`}
                            onClick={() => setInspection(item)}
                          >
                            {String(rank).padStart(2, "0")}
                          </button>
                          <button
                            className={`pick-button icon-button ${picks.has(id) ? "picked" : ""}`}
                            aria-label={`${picks.has(id) ? "Remove" : "Add"} final pick: ${basename(item.path)}`}
                            aria-pressed={picks.has(id)}
                            disabled={reviewInteractionBlocked}
                            onClick={() =>
                              editReview((r) =>
                                changeReview(r, {
                                  picks: r.picks.includes(id)
                                    ? r.picks.filter((p) => p !== id)
                                    : [...r.picks, id],
                                }),
                              )
                            }
                          >
                            <Star
                              size={19}
                              fill={picks.has(id) ? "currentColor" : "none"}
                            />
                          </button>
                          <div className="card-meta">
                            <b title={item.path}>{basename(item.path)}</b>
                            <small>
                              {reviewEvidence.get(id) ||
                                (review.manual.includes(id)
                                  ? "Manually ordered"
                                  : "Ranked by comparisons")}
                              {selected && (
                                <span className="selected-text">Selected</span>
                              )}
                            </small>
                            <div className="card-actions">
                              <button
                                className="icon-button drag-handle"
                                title="Drag to reorder; Alt + arrow keys to move"
                                aria-label={`Reorder rank ${rank}. Use Alt plus arrow keys.`}
                                disabled={blocked || !!modal || !!inspection}
                                draggable={canDragReview}
                                onDragStart={(e) => {
                                  if (!canDragReview) {
                                    e.preventDefault();
                                    return;
                                  }
                                  setDragging(id);
                                  e.dataTransfer.setData("text/plain", id);
                                  e.dataTransfer.effectAllowed = "move";
                                }}
                                onDragEnd={() => setDragging(null)}
                                onKeyDown={(e) => {
                                  if (
                                    e.altKey &&
                                    [
                                      "ArrowLeft",
                                      "ArrowUp",
                                      "ArrowDown",
                                      "ArrowRight",
                                    ].includes(e.key)
                                  ) {
                                    e.preventDefault();
                                    if (reviewInteractionBlocked) return;
                                    setReviewFocus({
                                      id,
                                      selector: ".drag-handle",
                                      origin: e.currentTarget,
                                    });
                                    move(
                                      selected ? reviewSelected : [id],
                                      rank +
                                        (["ArrowUp", "ArrowLeft"].includes(
                                          e.key,
                                        )
                                          ? -1
                                          : 1),
                                    );
                                  }
                                }}
                              >
                                <GripVertical size={17} />
                              </button>
                              <button
                                className="icon-button"
                                aria-label={`Move rank ${rank} up`}
                                disabled={
                                  reviewInteractionBlocked || rank === 1
                                }
                                onClick={() => move([id], rank - 1)}
                              >
                                <ArrowUp size={15} />
                              </button>
                              <button
                                className="icon-button"
                                aria-label={`Move rank ${rank} down`}
                                disabled={
                                  reviewInteractionBlocked ||
                                  rank === reviewIds.length
                                }
                                onClick={() => move([id], rank + 1)}
                              >
                                <ArrowDown size={15} />
                              </button>
                            </div>
                          </div>
                        </article>
                      );
                    }}
                  />
                ) : (
                  <div className="empty-state">
                    <Star size={30} />
                    <h2>
                      {reviewFilter === "picks"
                        ? "No final picks yet"
                        : "No matching ranked images"}
                    </h2>
                    <p>
                      {reviewFilter === "picks"
                        ? "Star images to build your final selection."
                        : "Try another filter. Your full ranking is unchanged."}
                    </p>
                    <button
                      onClick={() => {
                        setReviewFilter("all");
                        setFolder("all");
                        setSearch("");
                      }}
                    >
                      Show all images
                    </button>
                  </div>
                )}
                <footer className="action-bar">
                  <div
                    role="status"
                    aria-label="Review summary"
                    aria-atomic="true"
                  >
                    <strong>{picks.size} final picks</strong>
                    <span className="muted">
                      {reviewSelected.length} selected · {reviewIds.length}{" "}
                      ranked
                    </span>
                  </div>
                  <div className="button-row">
                    <button
                      onClick={() => setModal("rename")}
                      disabled={
                        blocked ||
                        saveStatus === "saving" ||
                        !!unavailableRun.length
                      }
                    >
                      <PenLine size={16} /> Rename originals
                    </button>
                    <button
                      onClick={() => setModal("copy-all")}
                      disabled={
                        blocked ||
                        saveStatus === "saving" ||
                        !!unavailableRun.length
                      }
                    >
                      <Download size={16} /> Create ranked copies
                    </button>
                    <button
                      className="primary"
                      onClick={() => setModal("copy")}
                      disabled={
                        !picks.size ||
                        blocked ||
                        saveStatus === "saving" ||
                        !!unavailableRun.length
                      }
                    >
                      <Download size={17} /> Export {picks.size} picks
                    </button>
                  </div>
                </footer>
              </main>
            )}
          </>
        )}
        {modal === "preflight" && (
          <Modal
            title={
              selectedIds.length === 1 ? "Review your image" : "Start a ranking"
            }
            onClose={closeModal}
            footer={
              <>
                <button onClick={closeModal}>Cancel</button>
                <button
                  className="primary"
                  disabled={
                    !selectedIds.length ||
                    scanning ||
                    blocked ||
                    !workloadAllowed
                  }
                  onClick={() => begin()}
                >
                  {selectedIds.length === 1 ? "Open review" : "Start comparing"}
                  <ArrowRight size={17} />
                </button>
              </>
            }
          >
            <p className="muted">
              {selectedIds.length} selected images from{" "}
              {session.report.sources.length} folders. This candidate set is
              fixed when you start.
            </p>
            <div className="mode-options">
              {(Object.keys(modeNames) as RankingMode[]).map((mode) => (
                <label
                  className={`mode-option ${session.settings.mode === mode ? "chosen" : ""}`}
                  key={mode}
                >
                  <input
                    type="radio"
                    name="mode"
                    checked={session.settings.mode === mode}
                    onChange={() => setSetting("mode", mode)}
                  />
                  <span>
                    <b>
                      {modeNames[mode]}
                      {mode === "adaptive" && selectedIds.length > 80 && (
                        <em>Recommended for this collection</em>
                      )}
                    </b>
                    <small>{modeDescriptions[mode]}</small>
                  </span>
                </label>
              ))}
            </div>
            <div className="estimate">
              <strong>
                {workEstimate
                  ? `${workEstimate.baseline.toLocaleString()} ${workEstimate.exact ? "comparisons" : session.settings.mode === "adaptive" ? "placement comparisons at most" : "comparisons without draws"}`
                  : "This mode exceeds the supported workload."}
              </strong>
              <p>
                {workEstimate
                  ? workEstimate.exact
                    ? "Every unique pair is compared exactly once."
                    : `Conservative upper bound: ${workEstimate.maximum.toLocaleString()}. Draws and refinement change the workload.`
                  : "Use Fast adaptive or select fewer images."}
              </p>
              {workEstimate && (
                <small>
                  At about 3 seconds per decision:{" "}
                  {Math.max(1, Math.round((workEstimate.baseline * 3) / 60))}{" "}
                  minutes{" "}
                  {session.settings.mode === "adaptive"
                    ? "at the placement bound"
                    : "without draws"}
                  . Pause and resume at any time.
                </small>
              )}
            </div>
            <>
              {!workloadAllowed && modal === "preflight" && (
                <p className="error">
                  This run exceeds the supported limit of{" "}
                  {MAX_CANDIDATES.toLocaleString()} candidates or{" "}
                  {MAX_DECISIONS.toLocaleString()} retained decisions. Choose
                  Fast adaptive or select fewer images.
                </p>
              )}
            </>
            <label className="check-label">
              <input
                type="checkbox"
                checked={session.settings.randomize}
                onChange={(e) => setSetting("randomize", e.target.checked)}
              />
              <Shuffle size={16} /> Randomize starting order to reduce filename
              bias
            </label>
            {session.run && (
              <p className="notice">
                Your current run will be kept in Saved runs when this new
                ranking starts.
              </p>
            )}
          </Modal>
        )}
        {modal === "settings" && (
          <Modal
            title="Workspace settings"
            onClose={closeModal}
            footer={
              <button className="primary" onClick={closeModal}>
                Done
              </button>
            }
          >
            {desktop && (
              <ProfileControls
                profile={bootStatus?.profile}
                disabled={
                  scanning ||
                  nativeWorkBusy ||
                  recoveryBusy ||
                  recoveryCopy.state === "exporting" ||
                  bootStatus?.canChooseRecoveryStorage === false
                }
                onChoose={() => beginProfileSwitch("create")}
                onOpen={() => beginProfileSwitch("open")}
              />
            )}
            <label className="field">
              Collection name
              <input
                maxLength={100}
                value={session.name}
                onChange={(e) =>
                  handle(update((s) => ({ ...s, name: e.target.value })))
                }
              />
            </label>
            <label className="check-label">
              <input
                type="checkbox"
                checked={session.settings.showNames}
                onChange={(e) => setSetting("showNames", e.target.checked)}
              />{" "}
              Show filenames below comparison images
            </label>
            <label className="check-label">
              <input
                type="checkbox"
                checked={session.settings.recursive}
                onChange={(e) => setSetting("recursive", e.target.checked)}
              />{" "}
              Include subfolders when adding folders
            </label>
            <label className="check-label">
              <input
                type="checkbox"
                checked={session.settings.randomize}
                onChange={(e) => setSetting("randomize", e.target.checked)}
              />{" "}
              Randomize new ranking runs
            </label>
            <div className="notice">
              <ShieldCheck size={18} />
              <p>
                {desktop
                  ? "Settings and ranking history are saved in the active profile shown above."
                  : "This is the browser demo. Progress is stored only in this browser. File operations require the desktop application."}{" "}
                Image files are never changed while ranking.
              </p>
            </div>
            <button onClick={() => setModal("saved")}>
              <Archive size={17} /> View saved runs (
              {session.archivedRuns.length})
            </button>
            <h3>New file operation defaults</h3>
            <label className="check-label">
              <input
                type="checkbox"
                checked={session.settings.manifestDefault ?? true}
                onChange={(e) =>
                  setSetting("manifestDefault", e.target.checked)
                }
              />
              Save JSON and CSV manifests by default
            </label>
            <label className="check-label">
              <input
                type="checkbox"
                checked={session.settings.verifyCopyDefault ?? false}
                onChange={(e) =>
                  setSetting("verifyCopyDefault", e.target.checked)
                }
              />
              Verify copied bytes by reading outputs back by default
            </label>
            <p className="muted">
              These defaults apply when you open a new file operation. You can
              change them for that operation before building its preview.
            </p>
            <button onClick={() => setModal("help")}>
              Help, diagnostics and recovery locations
            </button>{" "}
            <CacheSettings />
            <ImageWorkSettings />
            {desktop && (
              <section className="session-recovery-settings">
                <h3>Session recovery</h3>
                <p>
                  Save a portable copy of your ranking history, review edits,
                  and pending state. Image files and overwrite backups are not
                  included.
                </p>
                <div className="button-row">
                  <button
                    disabled={scanning || recoveryCopy.state === "exporting"}
                    onClick={() => void exportRecoveryCopy()}
                  >
                    {recoveryCopy.state === "exporting"
                      ? "Saving recovery copy…"
                      : "Save recovery copy"}
                  </button>
                  <button
                    disabled={scanning || recoveryCopy.state === "exporting"}
                    onClick={() => void openRecovery()}
                  >
                    Open recovery copy
                  </button>
                </div>
                {recoveryCopy.state !== "idle" && (
                  <p role="status">
                    {recoveryCopy.state === "saved"
                      ? `Recovery copy saved: ${recoveryCopy.message}`
                      : recoveryCopy.message}
                  </p>
                )}
              </section>
            )}
          </Modal>
        )}
        {modal === "help" && (
          <HelpDialog
            onMaintenance={desktop ? openProtection : undefined}
            onClose={closeModal}
            profile={bootStatus?.profile}
            work={desktop ? recoveryWorkActions : undefined}
          />
        )}
        {modal === "ignored" && (
          <Modal
            title="Skipped or unavailable files"
            onClose={closeModal}
            wide
            footer={<button onClick={closeModal}>Close</button>}
          >
            <p className="muted">
              These files are excluded from the current collection. Their source
              files are unchanged.
            </p>
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th>Path</th>
                    <th>Reason</th>
                  </tr>
                </thead>
                <tbody>
                  {session.report.ignored.map((entry, index) => (
                    <tr key={index}>
                      <td>{entry.path}</td>
                      <td>{entry.reason}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </Modal>
        )}
        {modal === "saved" && (
          <Modal
            title="Saved ranking runs"
            onClose={closeModal}
            footer={<button onClick={closeModal}>Close</button>}
          >
            <p className="muted">
              Restoring a run preserves the current run in this list. Selection
              changes never delete a ranking.
            </p>
            {!session.archivedRuns.length && <p>No previous runs yet.</p>}
            {session.archivedRuns.map((saved, index) => (
              <div className="saved-run" key={`${saved.date}-${index}`}>
                <div>
                  <b>
                    {modeNames[saved.run.mode]} · {progress(saved.run).done}{" "}
                    decisions
                  </b>
                  <small>
                    {new Date(saved.date).toLocaleString()} ·{" "}
                    {saved.review ? "Review saved" : "In progress"}
                  </small>
                </div>
                <button
                  onClick={() => {
                    handle(
                      update((s) => ({
                        ...s,
                        run: saved.run,
                        review: saved.review,
                        screen: saved.review ? "review" : "arena",
                        archivedRuns: [
                          ...s.archivedRuns.filter((_, i) => i !== index),
                          ...(s.run
                            ? [
                                {
                                  run: s.run,
                                  review: s.review,
                                  date: new Date().toISOString(),
                                },
                              ]
                            : []),
                        ],
                      })),
                    );
                    closeModal();
                  }}
                >
                  Restore
                </button>
              </div>
            ))}
          </Modal>
        )}
        {modal === "move" && review && (
          <Modal
            title={`Move ${reviewSelected.length} selected images`}
            onClose={closeModal}
            footer={
              <>
                <button onClick={closeModal}>Cancel</button>
                <button
                  className="primary"
                  onClick={() => {
                    move(reviewSelected, moveRank);
                    closeModal();
                  }}
                >
                  Move images
                </button>
              </>
            }
          >
            <label className="field">
              New rank for the first selected image
              <input
                type="number"
                min="1"
                max={review.order.length - reviewSelected.length + 1}
                value={moveRank}
                onChange={(e) =>
                  setMoveRank(
                    Math.max(
                      1,
                      Math.min(
                        review.order.length - reviewSelected.length + 1,
                        Number(e.target.value) || 1,
                      ),
                    ),
                  )
                }
              />
            </label>
            <p className="muted">
              Selected images keep their relative order. Hidden images remain in
              the full ranking. Moving images breaks their provisional band
              membership.
            </p>
          </Modal>
        )}
        {modal === "recompare" && review && reviewSelected.length === 2 && (
          <RecompareDialog
            images={
              reviewSelected.map((id) => items.get(id)!) as [
                ImageItem,
                ImageItem,
              ]
            }
            eligible={
              saveStatus === "saved" &&
              !blocked &&
              reviewSelected.every((id) => collectionIds.has(id))
            }
            onClose={closeModal}
            onDecide={(choice) => {
              editReview((r) =>
                compareReview(r, reviewSelected[0], reviewSelected[1], choice),
              );
              closeModal();
            }}
          />
        )}
        {modal === "selected-folder" && (
          <SelectedFolderDialog
            ids={selectedIds}
            items={items}
            eligible={saveStatus === "saved" && !blocked && !scanning}
            move={(imageIds, folderName) =>
              api.moveSelectionToFolder({ imageIds, folderName })
            }
            onClose={closeModal}
            onComplete={operationComplete}
            onRank={(ids) => begin(ids)}
            onRecovery={() => setModal("history")}
            onBusyChange={setNativeWorkBusy}
          />
        )}
        {(modal === "rename" || modal === "copy" || modal === "copy-all") &&
          review && (
            <OperationDialog
              kind={modal === "rename" ? "rename" : "copy"}
              scope={modal === "copy-all" ? "all" : "picks"}
              ids={
                modal === "copy"
                  ? review.order.filter((id) => picks.has(id))
                  : review.order
              }
              defaults={session.settings}
              order={review.order}
              items={items}
              onClose={closeModal}
              onComplete={operationComplete}
              onBusyChange={setNativeWorkBusy}
              onRecovery={() => setModal("history")}
            />
          )}
        {modal === "history" && (
          <OperationHistory
            onClose={closeModal}
            recoveryOnly={!ready}
            onBusyChange={setNativeWorkBusy}
            onComplete={
              ready
                ? operationComplete
                : () => {
                    setModal(null);
                    void load(true);
                  }
            }
          />
        )}
        {modal === "recovery" && (
          <Modal
            title="Session recovery"
            wide
            closeDisabled={recoveryBusy}
            onClose={closeModal}
            footer={
              <button disabled={recoveryBusy} onClick={closeModal}>
                Close
              </button>
            }
          >
            <StartupRecovery
              embedded
              storage={recoveryStorageActions}
              work={recoveryWorkActions}
              onChooseStorage={() => beginProfileSwitch("create")}
              onOpenProfile={() => beginProfileSwitch("open")}
              onMaintenance={openProtection}
              onResumeProfile={api.confirmProfileSwitch}
              onProfileResumed={(status) =>
                profileSwitched(status as ProfileBootView)
              }
              status={bootStatus}
              failure=""
              onBusyChange={setRecoveryBusy}
              onRetry={async () => {
                setModal(null);
                setRecoveryBusy(false);
                await load(true);
              }}
              onOpen={() => api.inspectRecoveryCopy()}
              onRestore={(token, acceptPending) =>
                requestReplacement({ kind: "fresh", token, acceptPending })
              }
              onRestored={() => {}}
            />
          </Modal>
        )}
        {inspection && (
          <Inspection item={inspection} onClose={closeInspection} />
        )}
        <div className={`toast ${toast ? "visible" : ""}`} role="status">
          {toast && (
            <>
              <CheckCircle2 size={17} />
              {toast}
            </>
          )}
        </div>
      </div>
      {committedReload && !profileRequest && !replacementRequest && (
        <div inert={closeRequested}>
          <Modal
            title="Reload restored workspace"
            onClose={() => {}}
            closeDisabled
          >
            <p>
              The native workspace has changed. The previous workspace stays
              frozen until the selected session is loaded in a new renderer.
            </p>
            <ProfileControls profile={committedReload.status.profile} />
            {committedReload.error && (
              <p className="error" role="alert">
                Reload failed: {committedReload.error}
              </p>
            )}
            <button
              className="primary"
              onClick={() => {
                try {
                  api.reloadSelectedProfile();
                } catch (reason) {
                  setCommittedReload({
                    ...committedReload,
                    error: String(reason),
                  });
                }
              }}
            >
              Reload selected profile
            </button>
          </Modal>
        </div>
      )}
      {/* FIX 2: Read-only Help remains accessible outside the frozen profile modal. */}
      {profileRequest && (
        <div inert={closeRequested || profileHelp}>
          <ProfileSwitcher
            mode={profileRequest.mode}
            generation={profileRequest.generation}
            current={bootStatus?.profile}
            commands={{
              prepare: (mode, expected) =>
                mode === "create"
                  ? api.chooseRecoveryStorage(expected)
                  : api.openRecoveryProfile(expected),
              refresh: api.refreshProfileSwitch,
              confirm: api.confirmProfileSwitch,
              cancel: api.cancelProfileSwitch,
              exportCopy: api.exportProfileReplacementCopy,
            }}
            flush={async () => {
              try {
                await flushSession();
              } catch (reason) {
                if (ready || saveStatus === "failed") throw reason;
              }
            }}
            retry={async () => {
              if (!ready)
                throw new Error(
                  "The current session is unavailable. Keep this profile and retry loading first.",
                );
              await save(sessionRef.current, false, true);
            }}
            // FIX 3: Only admitted UI contributes an expectation. Native export
            // derives its current state and binds it to this prepared token.
            snapshot={() =>
              sessionHydrated.current ? sessionRef.current : undefined
            }
            canRetrySave={ready}
            work={recoveryWorkActions}
            onHelp={() => setProfileHelp(true)}
            onClose={() => {
              profileFrozen.current = false;
              setProfileRequest(null);
              setProfileHelp(false);
            }}
            onSwitched={profileSwitched}
            onReload={() => api.reloadSelectedProfile()}
          />
        </div>
      )}
      {desktop && (
        <ProtectionMaintenance
          open={protectionOpen}
          onOpen={openProtection}
          onClose={() => setProtectionOpen(false)}
          onHelp={() => setProfileHelp(true)}
          onBusyChange={protectionChanged}
          flush={flushSession}
          profileGeneration={bootStatus?.profileGeneration}
          work={recoveryWorkActions}
          suspended={profileHelp || closeRequested || !!operationRestriction}
        />
      )}
      {operationRestriction && (
        <div inert={profileHelp || closeRequested}>
          <FileOperationRecovery
            restriction={operationRestriction}
            onRetry={retryOperationPublication}
            onHelp={() => setProfileHelp(true)}
            flush={flushSession}
            snapshot={() =>
              sessionHydrated.current ? sessionRef.current : undefined
            }
            generation={renderedProfileEpoch}
          />
        </div>
      )}
      {profileHelp && (
        <HelpDialog
          onClose={() => setProfileHelp(false)}
          profile={bootStatus?.profile}
          work={recoveryWorkActions}
        />
      )}
      {/* FIX 2: Native derives the export from its prepared admitted state.
          Only a genuinely hydrated renderer supplies an expected UI value. */}
      {replacementRequest && (
        <div inert={closeRequested}>
          <RecoveryReplacement
            subject={replacementRequest.subject}
            generation={replacementRequest.generation}
            commands={recoveryReplacementCommands}
            flush={flushSession}
            retry={async () => {
              if (!ready)
                throw new Error(
                  "Current saving is unavailable. Keep the current session and retry loading, or use native recovery export/discard.",
                );
              await save(sessionRef.current, false, true);
            }}
            snapshot={() =>
              sessionHydrated.current ? sessionRef.current : undefined
            }
            canRetrySave={ready}
            work={recoveryWorkActions}
            onClose={() => {
              profileFrozen.current = false;
              replacementRequest.reject(
                new Error(
                  "RECOVERY_REPLACEMENT_CANCELLED: current work and the inspected restore are unchanged",
                ),
              );
              setReplacementRequest(null);
            }}
            onRestored={(status) => {
              // FIX 3: Confirm native selection before resolving either entrypoint;
              // failed navigation uses the existing committed-only reload shell.
              replacementRequest.resolve(status);
              setReplacementRequest(null);
              setRecoveryBusy(false);
              profileSwitched(status);
            }}
          />
        </div>
      )}
      <CloseGuard
        freeze={freezeForClose}
        flush={flushSession}
        retry={async () => {
          if (ready) await save(sessionRef.current, true);
        }}
        snapshot={() => sessionRef.current}
        activeWork={() =>
          !!operationRestrictionRef.current ||
          protectionBusy ||
          startupWaiting ||
          bootStatus?.state === "loading" ||
          loadInFlight.current ||
          scanning ||
          nativeWorkBusy ||
          recoveryBusy ||
          !!profileRequest ||
          !!replacementRequest ||
          !!committedReload
        }
      />
    </>
  );
}
