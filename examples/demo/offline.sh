#!/bin/bash
# No-quota preview of the demo: a stand-in agent whose "tool calls" are REAL `secondwind exec`
# compressions, so the watch pane climbs with genuine numbers. The live version is demo.tape.
SW="target/release/secondwind"; H="--home /tmp/sw_demo"
e(){ printf '\033[%sm' "$1"; }
clear
e 1; printf '  ▍ coding agent'; e 0; e 2; printf '   secondwind run -- claude\n'; e 0
e 2; printf '  ──────────────────────────────────────\n\n'; e 0
sleep 1.2
e 36; printf '  ❯ '; e 0; printf 'read the optimize crate codecs and summarize\n\n'; sleep 1.3
e 2; printf '  ● Read  '; e 0; printf 'crates/optimize/src/columnar.rs\n'; $SW $H exec -- cat crates/optimize/src/columnar.rs >/dev/null 2>&1; sleep 1.0
e 2; printf '  ● Read  '; e 0; printf 'crates/optimize/src/text_columnar.rs\n'; $SW $H exec -- cat crates/optimize/src/text_columnar.rs >/dev/null 2>&1; sleep 1.0
e 2; printf '  ● Bash  '; e 0; printf 'cargo metadata --format-version 1\n'; $SW $H exec -- cargo metadata --format-version 1 >/dev/null 2>&1; sleep 1.6
e 32; printf '  ↳'; e 0; printf ' columnar, text-columnar, dict, line/log templates, offload.\n\n'; sleep 2.0
e 36; printf '  ❯ '; e 0; printf '/exit\n\n'; sleep 0.9
python3 - <<'PY'
import json
rows=[json.loads(l) for l in open('/tmp/sw_demo/.secondwind/events/events.jsonl')]
seen=set(); bl=[r for r in rows if not (r.get('cert') in seen or seen.add(r.get('cert')))]
ti=sum(r.get('input_tokens',0) for r in bl); to=sum(r.get('output_tokens',0) for r in bl)
v=sum(1 for r in bl if r.get('verified'))
print(f"  secondwind receipt: {len(bl)} blocks, {ti-to:,} tokens saved ({100*(ti-to)//max(ti,1)}%), {v}/{len(bl)} verified lossless")
PY
