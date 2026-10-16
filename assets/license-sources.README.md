# Captured third-party license sources

`npm run notices:generate` and `npm run notices:check` run offline. They union the locked Windows x64, macOS Apple Silicon and macOS Intel Cargo dependency graphs (including build dependencies), plus production npm packages. All advertised targets are inventoried even when generation runs on Windows.

Installed original license and notice files are preferred. Nineteen installed crates omitted these files; `license-sources.manifest.json` binds their supplemental sources to the installed package's exact repository, `.cargo_vcs_info.json` revision and repository path. Each upstream source records its URL, Git blob ID and SHA-256. Original bytes are preserved in `license-sources/`; ordinary builds never fetch them.

The objc2 monorepo supplies `LICENSE.md` declarations linking canonical MIT, Zlib and Apache terms. The selectors crate supplies an MPL-2.0 notice in its source header instead of an ancestor license file. Those exact pinned declarations remain separate from the full canonical terms they explicitly reference. The canonical pages are captured by URL and SHA-256, and extracted with a fixed offline parser. MIT's placeholder copyright line is intentionally unchanged: it is reference text, not invented attribution to these packages.

The provenance inventory retains 13 upstream licensing review entries: the selectors MPL source/distribution obligations and 12 objc2-family declarations that discuss Apple SDK licensing. Passing the inventory check confirms capture, graph coverage and integrity; it is **not legal release clearance** and does not clear these review entries.

For explicit dependency-maintenance work only, `node scripts/fetch-license-sources.mjs` retrieves missing sources using locked Cargo metadata derived at runtime. Fetch the locked dependencies first; no private development metadata file is needed. It rejects unpinned revisions, verifies Git blobs and records supplemental canonical terms only when the pinned upstream declaration explicitly references that URL. Review changes to the manifest and upstream terms before generating a new bundle. A package update with a different revision must update this inventory deliberately; the generator rejects stale or unrelated entries.

Validation: `node --test scripts/license-source-validation.test.mjs` covers modified/missing bytes, stale commits, branch substitutions, unbound terms, path escapes, complete term extraction and macOS-only graph coverage.
