# Using Image Ranker

1. Add a folder in **Sources**. Include subfolders if needed. Unsupported, damaged or unavailable files appear in the scan report and stay unchanged.
2. Select the images you want to rank. Click, Shift-select or drag a rectangle over thumbnails/empty space. Ctrl adds to a drag selection; Esc cancels. All / None / Invert use the current folder and filename search and replace selections elsewhere, including offscreen matches.
3. Start with **Adaptive**. Choose the left image, right image or Both for a draw. Undo restores the preceding comparison. The progress bar measures images placed and subsequent refinement checks; it is not an exact total of Adaptive's future decisions.
4. In **Review**, drag thumbnails/grid space to select images. Reorder using the grip, up/down controls or Alt + arrow keys on the focused grip. Stars are separate from selection. **Star selected** adds selected ranked images to existing picks as one Undoable saved edit.
5. Preview an original rename, ranked copy or final-pick export. Review every destination and conflict before approving. File-operation Undo is separate from ranking/Review Undo.

To split styles into separate rankings, select originals from one actual folder and choose **Move to subfolder**. Enter a new child folder, review all selected files and approve. This moves originals without copying their contents. An existing folder is not reused. After saving succeeds, **Rank this group** starts a ranking for that group and archives the previous run.

Original rename works within existing parent folders on qualified fixed NTFS or exFAT volumes. Mixed NTFS/exFAT batches are refused. exFAT rename manifests are saved under the NTFS profile's operation folder. exFAT operations require live ownership; Undo ends when that app/profile closes. A completed rename then keeps its new names. See [recovery](recovery.md) for interrupted work.

Wait for **Saved locally** before closing. If saving fails, keep the app open and follow its retry or recovery-copy controls. A separate backup is necessary for valuable images: Undo cannot restore externally deleted/replaced files or recover physical storage loss.

Keyboard comparison shortcuts: Left/1, Down/2/B, Right/3. Ctrl+Z undoes a comparison or Review edit. Space toggles the focused candidate, Shift+Space extends a range, F switches inspection fit/actual size, and Escape closes an idle dialog. Shortcuts pause in text fields and dialogs.
