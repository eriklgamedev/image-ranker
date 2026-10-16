import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
export const sha256 = (bytes) =>
  crypto.createHash("sha256").update(bytes).digest("hex");
const normalizeRepository = (value) =>
  value.replace(/\.git$/, "").replace(/\/$/, "");
const canonical = new Map([
  [
    "MIT",
    {
      url: "https://opensource.org/license/MIT",
      selector: "#separator, #LicenseText",
    },
  ],
  ["Zlib", { url: "https://zlib.net/zlib_license.html", selector: "pre" }],
  [
    "Apache-2.0",
    { url: "https://www.apache.org/licenses/LICENSE-2.0", selector: null },
  ],
  ["MPL-2.0", { url: "https://mozilla.org/MPL/2.0/", selector: "body" }],
]);
export function canonicalTerms(license) {
  const item = canonical.get(license);
  if (!item) throw Error(`Unsupported referenced license ${license}`);
  return item;
}
function htmlText(raw, selector) {
  const dom = new JSDOM(raw, { virtualConsole: new VirtualConsole() }); // No script execution or external resource loading.
  const selected = [...dom.window.document.querySelectorAll(selector)];
  if (!selected.length)
    throw Error("Canonical license text selector is missing");
  function walk(node) {
    if (node.nodeType === 3) return node.textContent.replace(/\s+/g, " ");
    if (node.nodeType !== 1) return "";
    if (["SCRIPT", "STYLE", "IMG"].includes(node.tagName)) return "";
    if (node.tagName === "PRE") return `\n${node.textContent}\n`;
    if (node.tagName === "BR") return "\n";
    let content = [...node.childNodes].map(walk).join("");
    if (node.tagName === "LI") {
      const parent = node.parentElement,
        index = [...parent.children]
          .filter((el) => el.tagName === "LI")
          .indexOf(node);
      const marker =
        parent.tagName === "OL"
          ? (parent.getAttribute("type") === "a"
              ? String.fromCharCode(97 + index)
              : String(index + 1)) + "."
          : "•";
      content = `${marker} ${content.trim()}`;
    }
    return [
      "P",
      "DIV",
      "H1",
      "H2",
      "H3",
      "DT",
      "DD",
      "LI",
      "DL",
      "OL",
      "UL",
      "BLOCKQUOTE",
    ].includes(node.tagName)
      ? `\n${content}\n`
      : content;
  }
  const text = selected
    .map(walk)
    .join("\n")
    .replace(/ *\n */g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  dom.window.close();
  if (text.length < 300) throw Error("Canonical license text is incomplete");
  return text;
}
export function sourceText(raw, source) {
  const text = new TextDecoder("utf-8", { fatal: true })
    .decode(raw)
    .replace(/\r\n/g, "\n");
  if (source.kind === "upstream-source-header") {
    const header = text.match(/^\s*\/\*[\s\S]+?\*\//)?.[0];
    if (!header || !header.includes("Mozilla Public"))
      throw Error("Pinned source license header is missing");
    return header.trim();
  }
  if (source.kind === "linked-license-terms") {
    const { selector } = canonicalTerms(source.license);
    return selector ? htmlText(text, selector) : text.trim();
  }
  if (source.kind !== "upstream-notice")
    throw Error("Unknown upstream notice kind");
  return text.trim();
}
export function validateLicenseSources(root, pkg, entry) {
  const id = `${pkg.ecosystem}:${pkg.name}@${pkg.version}`;
  const vcs = JSON.parse(
    fs.readFileSync(path.join(pkg.directory, ".cargo_vcs_info.json"), "utf8"),
  );
  if (
    entry.id !== id ||
    entry.repository !== normalizeRepository(pkg.repository) ||
    entry.revision !== vcs.git.sha1 ||
    entry.packagePath !== (vcs.path_in_vcs || "") ||
    entry.license !== pkg.spdx ||
    !/^[0-9a-f]{40}$/.test(entry.revision)
  )
    throw Error(`Stale pinned license source for ${id}`);
  if (!Array.isArray(entry.sources) || !entry.sources.length)
    throw Error(`Missing pinned license sources for ${id}`);
  const sourceRoot = fs.realpathSync(path.join(root, "assets/license-sources"));
  const readSource = (source) => {
    if (
      typeof source.path !== "string" ||
      !source.path.startsWith("license-sources/")
    )
      throw Error(`Invalid license source path for ${id}`);
    const file = path.resolve(root, "assets", source.path);
    const actual = fs.realpathSync(file);
    if (
      !actual.startsWith(sourceRoot + path.sep) ||
      !fs.statSync(actual).isFile()
    )
      throw Error(`License source escapes inventory for ${id}`);
    const raw = fs.readFileSync(actual);
    if (raw.length > 2 * 1024 * 1024 || sha256(raw) !== source.sha256)
      throw Error(`License source hash mismatch for ${id}: ${source.path}`);
    return raw;
  };
  const output = [];
  for (const source of entry.sources) {
    const raw = readSource(source);
    if (source.kind === "linked-license-terms") {
      const allowed = canonicalTerms(source.license),
        declared = entry.sources.find(
          (item) =>
            item.repositoryPath === source.referencedBy &&
            item.kind !== "linked-license-terms",
        );
      if (
        source.url !== allowed.url ||
        source.revision !== entry.revision ||
        !declared
      )
        throw Error(`Unbound canonical terms for ${id}`);
      if (!readSource(declared).toString("utf8").includes(source.url))
        throw Error(
          `Canonical terms were not referenced by pinned upstream notice for ${id}`,
        );
    } else {
      const repo = entry.repository.slice("https://github.com/".length);
      const url = `https://raw.githubusercontent.com/${repo}/${entry.revision}/${source.repositoryPath}`;
      if (
        source.url !== url ||
        source.revision !== entry.revision ||
        source.repositoryPath.split("/").includes("..")
      )
        throw Error(`Unpinned source URL for ${id}`);
      const blob = crypto
        .createHash("sha1")
        .update(`blob ${raw.length}\0`)
        .update(raw)
        .digest("hex");
      if (blob !== source.gitBlob) throw Error(`Git blob mismatch for ${id}`);
    }
    const text = sourceText(raw, source);
    if (!text) throw Error(`Empty notice for ${id}`);
    output.push({ source, text });
  }
  // The declarations linked here deliberately retain their own wording/caveats.
  // Canonical terms supplement them; they are not fabricated package originals.
  if (entry.repository === "https://github.com/madsmtm/objc2")
    for (const name of ["MIT", "Zlib", "Apache-2.0"])
      if (
        !entry.sources.some(
          (source) =>
            source.kind === "linked-license-terms" && source.license === name,
        )
      )
        throw Error(`Missing referenced ${name} terms for ${id}`);
  if (
    pkg.name === "selectors" &&
    !entry.sources.some(
      (source) =>
        source.kind === "linked-license-terms" && source.license === "MPL-2.0",
    )
  )
    throw Error(`Missing referenced MPL-2.0 terms for ${id}`);
  return output;
}
