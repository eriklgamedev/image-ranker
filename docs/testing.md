# Developer checks

Run `npm run check`, `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` and `cargo test --manifest-path src-tauri/Cargo.toml --lib --locked` from the repository root on Windows. CI also builds a normal optimized executable with no e2e feature and no installer.

Frontend/domain tests use synthetic sessions and mocked native APIs. Ordinary native tests create temporary fixture directories. Explicit host tests requiring actual exFAT storage are ignored by default; inspect their source and guard requirements before enabling them. Never supply personal images or a real working profile to mutation tests.

Desktop automation under `tests/desktop/` uses generated fixtures and an instrumented debug executable. It requires tauri-driver and a Microsoft Edge WebDriver compatible with the local Edge version. The Windows configuration expects the drivers at `.cargo-home/bin/tauri-driver.exe` and `.drivers/msedgedriver.exe`; it never downloads a driver automatically. Build with `npm run desktop:build:e2e` before `npm run test:desktop`. Do not distribute an e2e executable.

`tests/qualification/` and `installer/` contain optional developer capture, process-measurement and installer tooling retained from development. They are not part of routine acceptance and are not a qualified public-release pipeline. Their output goes to ignored `tests-artifacts/` directories. Historical machine reports, private test outputs and original development plans are intentionally absent from the public repository.

`npm run notices:check` verifies captured runtime notices against installed locked dependencies, including the existing historical multi-platform inventory. Fetch locked Cargo dependencies first. `node scripts/fetch-license-sources.mjs` derives locked metadata at runtime when deliberately maintaining supplemental sources; no private development metadata is needed.

Size practical verification to the change. A sampled 500-image session is different from 500 decisions, and Full pairwise at 500 images needs 124,750 comparisons. Avoid exhaustive runs unless they investigate a real issue. Existing developer evidence is not a fresh test of another PC or a newly changed build.
