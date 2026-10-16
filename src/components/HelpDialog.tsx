import { useEffect, useRef, useState } from "react";
import {
  api,
  desktop,
  type AppInfo,
  type LicenseNotice,
  type StorageLocation,
  type DiagnosticsPreview,
  type ProfileSummary,
} from "../native";
import { Modal } from "./Modal";
import { bytes } from "./OperationDialog";
import { ProfileControls } from "./ProfileControls";
import { ImageWorkSettings } from "./ImageWorkSettings";
import { ProtectionStatusPanel } from "./ProtectionStatusPanel";
import {
  RecoveryWorkControls,
  type RecoveryWorkActions,
} from "./RecoveryWorkControls";

// FIX 1: Each support read owns its loading/error/result state. A blocked
// filesystem observation cannot withhold already available bundled metadata.
function useSupportMetadata<T>(read: () => Promise<T>, initial: T) {
  const [value, setValue] = useState(initial);
  const [state, setState] = useState<"loading" | "ready" | "failed">(
    desktop ? "loading" : "ready",
  );
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const pending = useRef(desktop);
  useEffect(() => {
    if (!desktop) return;
    let alive = true;
    pending.current = true;
    setState("loading");
    setError("");
    void Promise.resolve()
      .then(read)
      .then(
        (next) => {
          if (!alive) return;
          pending.current = false;
          setValue(next);
          setState("ready");
        },
        (reason) => {
          if (!alive) return;
          pending.current = false;
          setError(String(reason));
          setState("failed");
        },
      );
    return () => {
      alive = false;
    };
  }, [read, revision]);
  return {
    value,
    state,
    error,
    retry: () => {
      if (state !== "failed" || pending.current) return;
      pending.current = true;
      setState("loading");
      setError("");
      setRevision((current) => current + 1);
    },
  };
}

export function HelpDialog({
  onClose,
  profile,
  work,
  onMaintenance,
}: {
  onClose: () => void;
  profile?: ProfileSummary;
  work?: RecoveryWorkActions;
  onMaintenance?: () => void;
}) {
  // FIX 2: Keep errors separate, and retry only failed reads. Pending reads are
  // never duplicated and a healthy result is not cleared by another failure.
  const app = useSupportMetadata<AppInfo | null>(api.appInfo, null);
  const notices = useSupportMetadata<LicenseNotice[]>(api.licenseNotices, []);
  const storage = useSupportMetadata<StorageLocation[]>(
    api.storageLocations,
    [],
  );
  const info = app.value,
    licenses = notices.value,
    locations = storage.value;
  const loading = [app, notices, storage].some(
    (item) => item.state === "loading",
  );
  const loadErrors = [
    app.error && `Application details: ${app.error}`,
    notices.error && `License notices: ${notices.error}`,
    storage.error && `Recovery locations: ${storage.error}`,
  ].filter(Boolean);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [preview, setPreview] = useState<DiagnosticsPreview | null>(null);
  const inspect = async () => {
    if (busy) return;
    setBusy("preview");
    setError("");
    setMessage("");
    setPreview(null);
    try {
      setPreview(await api.previewDiagnostics());
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy("");
    }
  };
  const exportPreview = async () => {
    if (!preview || busy) return;
    setBusy("export");
    setError("");
    setMessage("");
    try {
      const path = await api.exportDiagnostics(preview.token);
      setMessage(
        path
          ? `Displayed diagnostics saved: ${path}`
          : "Export cancelled. No diagnostic file was saved.",
      );
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy("");
    }
  };
  return (
    <Modal
      title="Help and local support"
      wide
      onClose={onClose}
      closeDisabled={!!busy}
      footer={
        <button className="primary" disabled={!!busy} onClick={onClose}>
          Done
        </button>
      }
    >
      {!desktop && (
        <p className="notice">
          Browser demo · Generated images and browser storage demonstrate
          ranking. Native file writes, diagnostics, recovery folders and build
          metadata require the desktop app.
        </p>
      )}
      <div className="help-columns">
        <section>
          <h3>From collection to final picks</h3>
          <ol>
            <li>
              Add source folders. Valid images start selected; filters do not
              change selection.
            </li>
            <li>
              Choose a ranking mode. Prefer left, right, or Both. Both records a
              draw for this matchup, not a permanent tie.
            </li>
            <li>
              Refine the order in Review. Drag cards, use arrow buttons, or
              select several cards and move them to a global rank.
            </li>
            <li>
              Star final picks. Preview every file destination before approving
              a rename or copy.
            </li>
          </ol>
          <h3>Choosing a mode</h3>
          <p>
            <strong>Winner stays</strong> holds the winner through each pass; a
            completed pass starts with the remaining images.{" "}
            <strong>Fast adaptive</strong> uses fewer comparisons to suggest an
            order, then refines provisional groups.{" "}
            <strong>Full pairwise</strong> compares every pair and exposes draws
            or conflicting evidence. Workload grows quickly: inspect the
            estimate before starting.
          </p>
          <h3>Rankings and file safety</h3>
          <p>
            Review order is authoritative. Moving an image splits its
            provisional band. Adjacent recompare changes only that pair and
            preserves final picks. New runs preserve previous runs. File history
            is separate: inspect unfinished work there or undo verified changes.
          </p>
          <p>
            A blocked rename may indicate a changed source, occupied destination
            or unresolved earlier operation. Refresh the preview or reconnect
            unchanged sources. Use recovery history to resume or roll back; it
            verifies recorded identities before changing files.
          </p>
        </section>
        <section>
          <h3>Keyboard shortcuts</h3>
          <dl className="shortcut-list">
            <dt>
              <kbd>← / 1</kbd>
            </dt>
            <dd>Prefer left</dd>
            <dt>
              <kbd>↓ / 2 / B</kbd>
            </dt>
            <dd>Both</dd>
            <dt>
              <kbd>→ / 3</kbd>
            </dt>
            <dd>Prefer right</dd>
            <dt>
              <kbd>Ctrl/⌘ Z</kbd>
            </dt>
            <dd>Undo comparison or review edit</dd>
            <dt>
              <kbd>Space</kbd>
            </dt>
            <dd>Toggle a focused candidate</dd>
            <dt>
              <kbd>Shift + click</kbd>
            </dt>
            <dd>Select a visible range</dd>
            <dt>
              <kbd>Alt + ↑ / ↓</kbd>
            </dt>
            <dd>Move a focused review handle</dd>
            <dt>
              <kbd>F</kbd>
            </dt>
            <dd>Fit / actual size in inspection</dd>
            <dt>
              <kbd>Esc</kbd>
            </dt>
            <dd>Close a dialog when no operation is busy</dd>
          </dl>
          <p className="muted">
            Click a comparison image to inspect it. Inspection never casts a
            vote. Comparison shortcuts are inactive in fields and dialogs.
          </p>
        </section>
      </div>
      {desktop && (
        <>
          <ImageWorkSettings />
          {work && <RecoveryWorkControls actions={work} watch={false} />}
          <ProtectionStatusPanel />
          {onMaintenance && (
            <button onClick={onMaintenance}>
              Review protection storage maintenance
            </button>
          )}
          {loading && <p role="status">Loading local support information…</p>}
          {loadErrors.length > 0 && (
            <div className="error" role="alert">
              {loadErrors.map((value) => (
                <p key={value}>{value}</p>
              ))}
              <button
                onClick={() => {
                  app.retry();
                  notices.retry();
                  storage.retry();
                }}
              >
                Retry support information
              </button>
            </div>
          )}
          {info && (
            <section className="support-section">
              <h3>About {info.name}</h3>
              <p>
                Version {info.version} · {info.platform} / {info.architecture} ·{" "}
                {info.buildType} build
                {info.testDriverEnabled ? " · Test driver enabled" : ""}
              </p>
              <p>
                Application license:{" "}
                {info.applicationLicense ||
                  "No application license declared in this build."}
              </p>
              <h4>Image decoding policy</h4>
              <p>
                {info.decode.formats.join(" · ")}. HEIC/HEIF and RAW are
                unavailable.
              </p>
              <p>
                GIF uses the first frame; TIFF uses the first page of supported
                variants. Previews apply EXIF orientation and convert supported
                embedded color profiles to sRGB at{" "}
                {info.decode.channelBits.join(", ")} bits per channel.
                Unqualified variants or profiles may be rejected with a
                diagnostic.
              </p>
              <dl className="support-limits">
                <dt>Input file limit</dt>
                <dd>{bytes(info.decode.maxInputBytes)}</dd>
                <dt>Decoded image limit</dt>
                <dd>
                  {info.decode.maxPixels.toLocaleString()} pixels ·{" "}
                  {info.decode.maxEdge.toLocaleString()} pixels per edge
                </dd>
                <dt>Decode allocation limit</dt>
                <dd>{bytes(info.decode.maxDecodeBytes)}</dd>
                <dt>Embedded color profile limit</dt>
                <dd>{bytes(info.decode.maxIccBytes)}</dd>
              </dl>
              <p className="muted">
                Inspection uses the same normalized frame/page as comparison.
                Images over supported limits are rejected; a bounded preview is
                not represented as full resolution.
              </p>
            </section>
          )}
          <section className="support-section">
            <h3>Recovery data locations</h3>
            {profile && <ProfileControls profile={profile} />}
            <p>
              Open these known local folders to inspect or preserve recovery
              records. Session recovery copies do not contain source images or
              replacement-file backup bytes. Keep operation backups until you no
              longer need file undo.
            </p>
            {locations.map((location) => (
              <div className="storage-location" key={location.kind}>
                <div>
                  <strong>{location.label}</strong>
                  <p className="break">{location.displayPath}</p>
                  {!location.available && (
                    <small>
                      Currently unavailable. Reconnect the storage and retry
                      loading.
                    </small>
                  )}
                </div>
                <button
                  disabled={!!busy || !location.available}
                  onClick={async () => {
                    setBusy("folder");
                    setError("");
                    try {
                      await api.openStorageLocation(location.kind);
                    } catch (reason) {
                      setError(String(reason));
                    } finally {
                      setBusy("");
                    }
                  }}
                >
                  Open {location.label}
                </button>
              </div>
            ))}
          </section>
          <section className="support-section">
            <h3>Local diagnostics</h3>
            <p>
              Preview the redacted report before saving a local copy. Review it
              before sharing it with support. Nothing is uploaded automatically;
              image bytes are excluded.
            </p>
            <button disabled={!!busy} onClick={() => void inspect()}>
              {busy === "preview"
                ? "Preparing preview…"
                : preview
                  ? "Refresh diagnostic preview"
                  : "Preview diagnostics"}
            </button>
            {preview && (
              <div className="diagnostic-preview">
                <p>
                  Captured {preview.createdAt} · {bytes(preview.sizeBytes)} ·
                  Paths redacted
                </p>
                {preview.limitations.length > 0 && (
                  <ul>
                    {preview.limitations.map((value, index) => (
                      <li key={index}>{value}</li>
                    ))}
                  </ul>
                )}
                <pre tabIndex={0} aria-label="Exact diagnostic export preview">
                  {preview.text}
                </pre>
                <button disabled={!!busy} onClick={() => void exportPreview()}>
                  {busy === "export"
                    ? "Saving displayed diagnostics…"
                    : "Save displayed diagnostics"}
                </button>
                <p className="muted">
                  Export saves this captured preview. Refresh to include later
                  changes.
                </p>
              </div>
            )}
          </section>
          <section className="support-section">
            <h3>Third-party licenses</h3>
            {/* FIX 3: Empty notices require a successful notice response only;
                pending/failed notices must never be represented as empty. */}
            {notices.state === "ready" && !licenses.length && (
              <p>No third-party notices were supplied by this build.</p>
            )}
            {licenses.map((notice) => (
              <details key={notice.id}>
                <summary>
                  {notice.component} {notice.version} · {notice.spdx}
                </summary>
                <pre className="license-text" tabIndex={0}>
                  {notice.text}
                </pre>
              </details>
            ))}
          </section>
          {error && (
            <p className="error" role="alert">
              {error}
            </p>
          )}
          {message && (
            <p className="notice" role="status">
              {message}
            </p>
          )}
        </>
      )}
    </Modal>
  );
}
