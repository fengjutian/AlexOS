import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const inputs = [path.join(root, "README.md"), ...walk(path.join(root, "docs"))];
const errors = [];

for (const file of inputs) {
  const text = fs.readFileSync(file, "utf8");
  const relative = path.relative(root, file).replaceAll("\\", "/");
  for (const match of text.matchAll(/\[[^\]]*\]\(([^)]+)\)/g)) {
    const raw = match[1].trim().replace(/^<|>$/g, "");
    const target = raw.split("#", 1)[0];
    if (!target || /^(https?:|mailto:)/i.test(target)) continue;
    const decoded = decodeURIComponent(target);
    const resolved = path.resolve(path.dirname(file), decoded);
    if (!fs.existsSync(resolved)) errors.push(`${relative}: broken link ${raw}`);
  }
}

const combined = inputs.map((file) => fs.readFileSync(file, "utf8")).join("\n");
const forbidden = [
  [/335\/335 passed/g, "fixed stale test count"],
  [/`src\/package\.rs`/g, "removed source path src/package.rs"],
  [/`src\/update\.rs`/g, "removed source path src/update.rs"],
  [/没有 `\.alexignore`/g, "obsolete .alexignore limitation"],
  [/Cargo\.lock[^\n]{0,30}锁定[^\n]{0,10}工具链/g, "Cargo.lock does not pin Rust"],
];
for (const [pattern, label] of forbidden) {
  if (pattern.test(combined)) errors.push(`documentation contains ${label}`);
}

if (errors.length) {
  console.error(errors.join("\n"));
  process.exitCode = 1;
} else {
  console.log(`Documentation check passed (${inputs.length} Markdown files).`);
}

function walk(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) return walk(full);
    return entry.isFile() && entry.name.endsWith(".md") ? [full] : [];
  });
}
