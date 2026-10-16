# Saves, file recovery and Undo

**If saving fails, keep the app open.** The visible workspace may contain a pending action that the original storage has not acknowledged. Saved locally, Saving, checkpoint warnings and failed saves describe different states.

Retry through the offered controls after restoring storage access. A retry remains bound to the retained action. If needed, export and review a recovery copy before deliberately discarding pending changes. Recovery copies preserve ranking/history and identified pending work; they do not grant filesystem permission to modify their recorded source paths. Reconnect approved folders through the native picker after restoration.

Managed profiles, session snapshots, event journals and operation records require qualified local fixed NTFS storage. Do not delete or truncate protection/operation journals to work around a restriction. Use the app's reviewed profile/recovery controls and retain evidence until recovery is complete.

File history presents verified Resume, Roll back or Undo when the operation and storage still permit them. Stop if it reports replaced sources, occupied destinations, unexpected identities or missing backups. Undo later dependent operations before earlier ones. Ranking/Review Undo does not reverse filesystem changes.

**exFAT has a stricter lifetime rule.** Rename, group-move and output Undo require the same open app/profile and its live file ownership. Persisted IDs never authorize an old operation to mutate newly opened files. After restart or interruption, **Keep files where they are** observes and records supported current paths without moving or deleting images and ends Resume/Undo for that operation. An interrupted rename may retain a staging filename; its image extension is preserved so it can be reopened and previewed. Do not rename those files manually merely to bypass protective history.

Cancellation requests do not mean that native work has already stopped. Wait until the app reports the actual work retired. Avoid switching profiles or closing during a commit; use offered recovery controls when the app cannot safely finish.

The mechanisms cover verified process interruption and filesystem ownership checks. They do not promise recovery from power loss, hardware damage or unrelated external deletion. Always keep independent backups of irreplaceable files.
