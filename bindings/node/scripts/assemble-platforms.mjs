// Generate the per-platform native packages, and when given a built library drop it into the
// matching one. Main package lists all as optionalDependencies so npm installs only the host match.
//
//   node scripts/assemble-platforms.mjs                 # (re)write every platform package.json
//   node scripts/assemble-platforms.mjs <built-lib>     # also copy the built lib into its platform
import { mkdirSync, writeFileSync, copyFileSync, existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { platform, arch } from "node:process";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;

// Support matrix; `lib` is the file the loader in secondwind.mjs looks up per OS.
const TARGETS = [
  { os: "darwin", cpu: "arm64", lib: "libsecondwind.dylib" },
  { os: "darwin", cpu: "x64", lib: "libsecondwind.dylib" },
  { os: "linux", cpu: "x64", lib: "libsecondwind.so" },
  { os: "linux", cpu: "arm64", lib: "libsecondwind.so" },
  { os: "win32", cpu: "x64", lib: "secondwind.dll" },
];

for (const t of TARGETS) {
  const name = `secondwind-${t.os}-${t.cpu}`;
  const dir = join(root, "platforms", name);
  mkdirSync(dir, { recursive: true });
  const pkg = {
    name,
    version,
    description: `secondwind native library for ${t.os}-${t.cpu}`,
    license: "Apache-2.0",
    // os/cpu are npm's guard: refuses to install on a non-matching host, so only the right one
    // lands for the loader to resolve.
    os: [t.os],
    cpu: [t.cpu],
    files: [t.lib],
  };
  writeFileSync(join(dir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
}

// If a built library was passed, copy it into the package for THIS host's target.
const builtLib = process.argv[2];
if (builtLib) {
  const t = TARGETS.find((t) => t.os === platform && t.cpu === arch);
  if (!t) throw new Error(`no platform package defined for ${platform}-${arch}`);
  if (!existsSync(builtLib)) throw new Error(`built library not found: ${builtLib}`);
  const dest = join(root, "platforms", `secondwind-${t.os}-${t.cpu}`, t.lib);
  copyFileSync(builtLib, dest);
  console.log(`copied ${builtLib} -> ${dest}`);
}

console.log(`wrote ${TARGETS.length} platform packages under platforms/`);
