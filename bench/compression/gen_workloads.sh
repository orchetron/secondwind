#!/usr/bin/env bash
# Regenerate a REAL agent tool-output corpus from public/safe sources only: this repo itself, plus
# (optionally) public GitHub. Output goes to ./workloads, which is gitignored, so nothing generated
# here is ever committed. No personal environment, process list, or home-directory content is used.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT="bench/compression/workloads"
mkdir -p "$OUT"

echo "repo-local workloads (network-free, this repo only)..."
# Nested JSON: this workspace's own dependency graph. Home path scrubbed even though gitignored.
cargo metadata --format-version 1 2>/dev/null | sed "s#$HOME#~#g" > "$OUT/cargo_metadata.json"
# Code search (path:line:content) and a path list, over our own source.
grep -rn --include='*.rs' 'pub fn ' crates > "$OUT/code_search_pubfn.txt" || true
grep -rn --include='*.rs' 'unwrap(' crates > "$OUT/code_search_grep.txt" || true
find crates -name '*.rs' | sort > "$OUT/find_rs.txt"
# Tabular text, dependency tree, lockfiles, path list, and version-control output, all from this repo.
ls -laR crates > "$OUT/ls_recursive.txt"
cargo tree > "$OUT/dep_cargo_tree.txt" 2>/dev/null || true
sed "s#$HOME#~#g" Cargo.lock > "$OUT/lock_cargo.toml" 2>/dev/null || true
[ -f bindings/node/package-lock.json ] && cp bindings/node/package-lock.json "$OUT/lock_package.json"
git ls-files > "$OUT/paths_git_ls.txt"
git log -n 40 > "$OUT/git_log.txt"
git diff HEAD~8 HEAD > "$OUT/git_diff.txt" 2>/dev/null || git diff > "$OUT/git_diff.txt"

if command -v gh >/dev/null 2>&1 && gh api rate_limit >/dev/null 2>&1; then
  echo "public GitHub workloads (gh + network; representative, not frozen)..."
  gh api 'repos/cli/cli/pulls?per_page=60&state=all' > "$OUT/github_prs.json" 2>/dev/null || true
  gh api 'repos/cli/cli/issues?per_page=80&state=all' > "$OUT/github_issues.json" 2>/dev/null || true
  if command -v jq >/dev/null 2>&1; then
    jq '[.[]|{number,state,draft,author:.user.login,title:(.title[0:40])}]' \
      < "$OUT/github_prs.json" > "$OUT/github_prs_flat.json" 2>/dev/null || true
  fi
else
  echo "gh/network unavailable: skipping public GitHub workloads (repo-local core still runs)."
fi

if command -v curl >/dev/null 2>&1; then
  echo "public API responses (object-map JSON) + public-domain prose (curl)..."
  # Registry metadata: object-map-heavy JSON (versions/releases maps), not record arrays.
  curl -sSL 'https://registry.npmjs.org/react' -o "$OUT/api_npm_react.json" 2>/dev/null || true
  curl -sSL 'https://pypi.org/pypi/requests/json' -o "$OUT/api_pypi_requests.json" 2>/dev/null || true
  # High-entropy prose (Project Gutenberg, public domain): the honest floor for a lossless codec.
  curl -sSL 'https://www.gutenberg.org/files/1342/1342-0.txt' 2>/dev/null | head -c 150000 > "$OUT/prose_book.txt" || true
fi
echo "done -> $OUT"
