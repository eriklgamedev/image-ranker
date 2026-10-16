# Third-party material

The root MIT license covers project-authored files. It does not replace licenses on dependencies or upstream material retained in this repository.

- `assets/third-party-notices.json` contains the bundled runtime dependency notice texts shown in Help.
- `assets/third-party-notices.provenance.json` records component identities, source hashes and retained review entries.
- `assets/license-sources.manifest.json` and `assets/license-sources/` preserve pinned supplemental upstream notices and referenced terms.
- `installer/nsis/upstream/` retains Tauri's upstream MIT/Apache-2.0 notices and provenance.
- `installer/nsis-plugin/upstream/` retains the NSIS headers, their source manifest and upstream COPYING terms.

The existing notice inventory includes additional platform graphs captured during development. Their presence does not claim a supported macOS release. The installer sources are optional developer tooling, not a qualified public installer.

Use `npm run notices:check` after fetching the locked Cargo dependencies to verify the runtime notice inventory. Its generator works offline once those dependencies are available. For deliberate dependency maintenance, the scripts can refresh pinned source material; review attribution and component-specific obligations before redistributing a changed binary. Dependencies are not relicensed as MIT by this project.
