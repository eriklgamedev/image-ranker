import { spawnSync } from "node:child_process";

// Derive machine-local paths at runtime; never require a captured private report.
export function cargoLicenseMetadata() {
  const result = spawnSync(
    "cargo",
    ["metadata", "--manifest-path", "src-tauri/Cargo.toml", "--format-version", "1", "--locked", "--offline"],
    { encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024 },
  );
  if (result.status !== 0) {
    throw new Error(result.stderr || "Fetch the locked Cargo dependencies before reading license metadata.");
  }
  return JSON.parse(result.stdout);
}
