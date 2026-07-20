#!/usr/bin/env python3
# Token-based, fidelity-verified benchmark over a corpus of tool-output shapes.
# Reduction is real token in/out via the shipped tokenizer; fidelity is verified the
# reliable way, byte-exact recovery for offloaded blocks and a certificate `verify`
# for inline transforms, never the approximate richness score. Prints a table + totals.
#
#   cargo build -p secondwind
#   python3 bench/token_bench.py target/debug/secondwind
#
# The corpus is 5 real command captures (when the commands exist) plus representative
# shapes across the categories agents actually hit. Content is synthetic; structure is
# real. Regenerates deterministically (seeded) so runs are comparable.

import json, os, sys, random, subprocess, statistics, tempfile, shutil


def build_corpus(d):
    random.seed(11)
    W = lambda n, o: open(os.path.join(d, n), "w").write(o if isinstance(o, str) else json.dumps(o))
    arr = lambda n, f: [f(i) for i in range(n)]
    # real captures (best-effort; skipped if the command is absent)
    for name, cmd in [
        ("01_cargo_metadata.json", ["cargo", "metadata", "--format-version", "1"]),
        ("02_git_log.txt", ["git", "log", "--pretty=format:%H %an %ad %s", "-60"]),
        ("03_ps_aux.txt", ["ps", "aux"]),
        ("04_env.txt", ["env"]),
        ("05_find_rs.txt", ["bash", "-c", "find . -name '*.rs' 2>/dev/null | head -200"]),
    ]:
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=30).stdout
            if out.strip():
                W(name, out)
        except Exception:
            pass
    W("06_ec2.json", arr(45, lambda i: {"InstanceId": "i-%016x" % i, "State": ["running", "stopped"][i % 2], "Type": "t3.medium", "AZ": "us-east-1%s" % "abc"[i % 3], "PrivateIp": "172.31.%d.%d" % (i, i % 20)}))
    W("09_metrics.json", arr(150, lambda i: {"metric": "cpu.usage", "host": "h%d" % (i % 20), "value": round(random.uniform(0, 100), 2), "ts": 1784380000 + i * 60, "tags": {"region": "us", "env": "prod"}}))
    W("12_file_meta.json", arr(120, lambda i: {"path": "src/mod%d/file%d.rs" % (i % 10, i), "size": i * 128, "mode": "0644", "modified": 1784000000 + i * 3600, "sha": "%040x" % (i * 7919)}))
    W("13_search_results.json", arr(70, lambda i: {"file": "src/handler%d.rs" % (i % 15), "line": i * 3 + 1, "col": 4, "match": "fn process_request()", "context": "    pub fn process_request(&self) -> Result<()>"}))
    W("15_openapi.json", {"paths": {"/api/v1/resource%d" % i: {"get": {"summary": "Get resource %d" % i, "parameters": [{"name": "id", "in": "path", "required": True}], "responses": {"200": {"description": "ok"}}}} for i in range(30)}})
    W("17_stacktrace.txt", "Traceback (most recent call last):\n" + "\n".join('  File "app/mod%d.py", line %d, in handler\n    return process(x)' % (i % 8, i * 7 % 400) for i in range(25)) + "\nValueError: invalid literal")
    W("19_diff.txt", "\n".join(("+" if i % 3 == 0 else "-" if i % 5 == 0 else " ") + "    line %d of the file with some code content here" % i for i in range(120)))
    W("21_kv_dump.json", {"session_%d" % i: {"user": i, "token": "%032x" % (i * 104729), "exp": 1784400000 + i} for i in range(80)})
    W("23_docker_ps.json", arr(35, lambda i: {"Id": "%064x" % i, "Image": "app:v%d" % (i % 5), "Status": "Up %dh" % (i % 48), "Ports": "0.0.0.0:%d->8080" % (8000 + i), "Names": "svc_%d" % i}))
    W("25_pagination.json", {"page": 1, "per_page": 50, "total": 2340, "items": arr(50, lambda i: {"id": i, "name": "item %d" % i, "price": round(i * 1.5, 2), "stock": i % 100})})
    W("27_headers.json", {"request_id": "abc", "headers": {"h-%d" % i: "value-%d" % i for i in range(50)}, "trace": ["span%d" % i for i in range(30)]})
    W("07_k8s_pods.json", arr(60, lambda i: {"name": "api-%d" % i, "namespace": "prod", "status": "Running", "restarts": i % 3, "node": "node-%d" % (i % 8), "ip": "10.0.%d.%d" % (i // 255, i % 255), "age": "%dh" % (i % 72)}))
    W("08_db_users.json", arr(200, lambda i: {"id": i, "email": "user%d@ex.com" % i, "active": bool(i % 2), "role": ["admin", "user", "guest"][i % 3], "created": "2026-0%d-01" % (1 + i % 9), "logins": i * 3}))
    W("10_github_prs.json", arr(50, lambda i: {"number": i + 100, "title": "Fix bug in module %d" % i, "user": "dev%d" % (i % 12), "state": ["open", "closed", "merged"][i % 3], "additions": i * 10, "deletions": i * 3, "labels": ["bug", "p%d" % (i % 3)]}))
    W("11_json_logs.json", arr(300, lambda i: {"ts": "2026-07-18T10:%02d:%02dZ" % (i // 60 % 60, i % 60), "level": ["INFO", "WARN", "ERROR"][i % 3 if i % 17 else 2], "svc": "gateway", "msg": "request handled", "latency_ms": i % 200, "status": 200 if i % 13 else 500}))
    W("14_test_results.json", arr(90, lambda i: {"name": "test_case_%d" % i, "result": "pass" if i % 9 else "fail", "duration_ms": i % 50, "file": "tests/suite%d.rs" % (i % 6), "assertions": i % 8 + 1}))
    W("16_config.json", {"service": {"name": "gateway", "port": 8080, "workers": 8}, "db": {"host": "db.internal", "port": 5432, "pool": {"min": 4, "max": 32}}, "features": {"flag_%d" % i: bool(i % 2) for i in range(40)}})
    W("18_nginx_log.txt", "\n".join('10.0.%d.%d - - [18/Jul/2026:10:%02d:%02d +0000] "GET /api/v%d/x HTTP/1.1" %d %d "-" "curl/8.0"' % (i % 255, i % 99, i // 60 % 60, i % 60, i % 3, [200, 404, 500][i % 3 if i % 11 else 0], i * 13 % 9000) for i in range(200)))
    W("20_cargo_test.txt", "\n".join("test suite::case_%d ... %s" % (i, "ok" if i % 12 else "FAILED") for i in range(140)))
    W("22_grep.txt", "\n".join("src/file%d.rs:%d:    let result = compute_value(input, %d);" % (i % 20, i * 2, i) for i in range(90)))
    W("24_timeseries.json", arr(180, lambda i: [1784380000 + i * 60, round(random.uniform(10, 90), 1)]))
    W("28_kubectl_events.json", arr(65, lambda i: {"type": ["Normal", "Warning"][i % 2], "reason": "Scheduled", "object": "pod/api-%d" % i, "message": "Successfully assigned", "count": i % 5 + 1, "age": "%dm" % (i % 60)}))
    W("29_npm_ls.json", {"dependencies": {"pkg-%d" % i: {"version": "%d.%d.%d" % (i % 5, i % 9, i), "resolved": "https://registry/pkg-%d" % i, "integrity": "sha512-%040x" % (i * 31)} for i in range(70)}})
    W("30_prometheus.txt", "\n".join('http_requests_total{method="get",code="%d",handler="/api/%d"} %d' % ([200, 404][i % 2], i % 20, i * 137) for i in range(150)))


def run(binary, corp, home):
    env = dict(os.environ, HOME=home)
    rows = []
    for fn in sorted(os.listdir(corp)):
        path = os.path.join(corp, fn)
        orig = open(path, encoding="utf-8", errors="replace").read()
        try:
            d = json.loads(subprocess.run([binary, "optimize", "--tokens", path], capture_output=True, text=True, env=env, timeout=60).stdout)
        except Exception:
            rows.append((fn, 0, 0, 0.0, "ERR", False))
            continue
        it, ot, tr = d["input_tokens"], d["output_tokens"], d["transform"]
        red = 100 * (1 - ot / it) if it else 0.0
        if tr == "verbatim":
            fid = True
        elif d.get("recovered") is not None:
            fid = d["recovered"] == orig  # offload: byte-exact recovery
        else:
            with tempfile.NamedTemporaryFile("w", suffix=".wire", delete=False) as t:
                t.write(d["effective"])
                wf = t.name
            fid = "PASS" in subprocess.run([binary, "verify", wf, d["certificate"]], capture_output=True, text=True, env=env).stdout
            os.unlink(wf)
        rows.append((fn, it, ot, red, tr, fid))
    return rows


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/debug/secondwind"
    corp, home = tempfile.mkdtemp(), tempfile.mkdtemp()
    try:
        build_corpus(corp)
        rows = run(binary, corp, home)
        print("%-26s %8s %8s %7s  %-9s %s" % ("tool output", "in tok", "out tok", "reduce", "transform", "lossless"))
        print("-" * 72)
        for fn, it, ot, red, tr, fid in rows:
            print("%-26s %8d %8d %6.1f%%  %-9s %s" % (fn[:26], it, ot, red, tr, "OK" if fid else "LOSS!"))
        tin, tout = sum(r[1] for r in rows), sum(r[2] for r in rows)
        applied = [r[3] for r in rows if r[4] not in ("verbatim", "ERR")]
        print("-" * 72)
        print("N=%d  TOTAL tokens %d -> %d  (%.1f%% removed)" % (len(rows), tin, tout, 100 * (1 - tout / tin) if tin else 0))
        if applied:
            print("Applied reduction: median %.1f%%  mean %.1f%%  range %.1f-%.1f%%" % (statistics.median(applied), statistics.mean(applied), min(applied), max(applied)))
        print("LOSSLESS: %d/%d (%s)" % (sum(1 for r in rows if r[5]), len(rows), "all verified" if all(r[5] for r in rows) else "LOSS DETECTED"))
    finally:
        shutil.rmtree(corp, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()
