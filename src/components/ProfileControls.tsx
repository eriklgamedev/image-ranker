import type { ProfileSummary } from "../native";

export function ProfileControls({
  profile,
  disabled = false,
  onChoose,
  onOpen,
}: {
  profile?: ProfileSummary;
  disabled?: boolean;
  onChoose?: () => void;
  onOpen?: () => void;
}) {
  return (
    <section className="support-section" aria-label="Active storage profile">
      <h3>Active storage profile</h3>
      {profile ? (
        <>
          <p>
            <strong>
              {profile.isDefault
                ? "Default profile"
                : "Selected recovery profile"}
            </strong>{" "}
            · {profile.id || "Identity unavailable"}
          </p>
          <p className="break">{profile.displayPath}</p>
          {!profile.writable && (
            <p className="notice">
              This storage is currently unavailable for writes. Choose another
              location or reconnect it and retry.
            </p>
          )}
        </>
      ) : (
        <p>
          The active profile could not be identified. Choosing recovery storage
          preserves existing records.
        </p>
      )}
      {(onChoose || onOpen) && (
        <>
          <p>
            Create storage in a folder you choose, or open an existing profile.
            Saved history and file-operation backups stay in their original
            profile.
          </p>
          <div className="button-row">
            {onChoose && (
              <button disabled={disabled} onClick={onChoose}>
                Choose recovery storage
              </button>
            )}
            {onOpen && (
              <button disabled={disabled} onClick={onOpen}>
                Open existing profile
              </button>
            )}
          </div>
        </>
      )}
    </section>
  );
}
