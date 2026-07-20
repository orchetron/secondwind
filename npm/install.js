// Downloads the native secondwind binary for this platform from the GitHub
// release whose tag matches this package version, verifies its checksum, and
// unpacks it next to the launcher. Runs as an npm postinstall step.
const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const { execFileSync } = require("child_process");
const crypto = require("crypto");

const REPO = process.env.SECONDWIND_REPO || "OWNER/secondwind";
const version = require("./package.json").version;

function triple() {
  const arch = { x64: "x86_64", arm64: "aarch64" }[process.arch];
  const sys = { darwin: "apple-darwin", linux: "unknown-linux-gnu" }[process.platform];
  if (!arch || !sys) {
    console.error(`secondwind: no prebuilt binary for ${process.platform}/${process.arch}`);
    process.exit(0);
  }
  return `${arch}-${sys}`;
}

function get(url, dest) {
  return new Promise((resolve, reject) => {
    https.get(url, { headers: { "User-Agent": "secondwind-npm" } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return get(res.headers.location, dest).then(resolve, reject);
      }
      if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      const file = fs.createWriteStream(dest);
      res.pipe(file);
      file.on("finish", () => file.close(resolve));
    }).on("error", reject);
  });
}

async function main() {
  const t = triple();
  const base = `https://github.com/${REPO}/releases/download/v${version}`;
  const tarball = `secondwind-${t}.tar.gz`;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "secondwind-"));
  const tarPath = path.join(tmp, tarball);

  await get(`${base}/${tarball}`, tarPath);

  try {
    const sums = path.join(tmp, "SHA256SUMS");
    await get(`${base}/SHA256SUMS`, sums);
    const want = fs.readFileSync(sums, "utf8").split("\n").find((l) => l.endsWith(tarball));
    if (want) {
      const got = crypto.createHash("sha256").update(fs.readFileSync(tarPath)).digest("hex");
      if (got !== want.split(/\s+/)[0]) throw new Error("checksum mismatch");
    }
  } catch (e) {
    if (String(e.message).includes("checksum")) throw e;
  }

  const bin = path.join(__dirname, "bin");
  fs.mkdirSync(bin, { recursive: true });
  execFileSync("tar", ["-xzf", tarPath, "-C", bin]);
  fs.chmodSync(path.join(bin, "secondwind"), 0o755);
}

main().catch((e) => {
  console.error(`secondwind: install failed: ${e.message}`);
  process.exit(1);
});
