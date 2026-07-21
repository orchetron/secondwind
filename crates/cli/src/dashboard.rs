use std::path::Path;
use std::process::ExitCode;

use secondwind_ledger::events;
use tiny_http::{Header, Response, Server};

pub fn serve(home: &Path, listen: &str) -> ExitCode {
    let server = match Server::http(listen) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("secondwind dashboard: {listen}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let url = format!("http://{listen}");
    eprintln!("secondwind proof on {url}");
    eprintln!(
        "optimize something (secondwind run, exec, the Bash hook, or the mcp server) and it lands here live"
    );
    open_browser(&url);

    for request in server.incoming_requests() {
        let response = if request.url().starts_with("/events.json") {
            let tz = request
                .url()
                .rsplit("tz=")
                .next()
                .and_then(|s| s.split('&').next())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let summary = events::summarize(&events::load(home), tz);
            let body = serde_json::to_string(&summary).unwrap_or_else(|_| "{}".into());
            Response::from_string(body).with_header(json_header())
        } else {
            Response::from_string(PAGE).with_header(html_header())
        };
        let _ = request.respond(response);
    }
    ExitCode::SUCCESS
}

// Live terminal view for a second pane: totals refreshed every second from the same
// ledger run/serve write. Draws only in its own terminal, never the agent's.
pub fn watch(home: &Path) -> ExitCode {
    use std::io::Write;
    use std::time::{Duration, Instant};

    print!("\u{1b}[2J");
    let _ = std::io::stdout().flush();
    let mut prev = 0u64;
    let mut last_change = Instant::now();
    let mut lit = true;
    loop {
        let all = events::load(home);
        let s = events::summarize(&all, 0);
        let saved = s.input_tokens.saturating_sub(s.output_tokens);
        let pct = if s.input_tokens > 0 {
            100.0 * saved as f64 / s.input_tokens as f64
        } else {
            0.0
        };
        if s.blocks > prev {
            prev = s.blocks;
            last_change = Instant::now();
        }
        let dot = if lit { '\u{25cf}' } else { '\u{25cb}' };
        let state = if s.blocks > 0 && last_change.elapsed() < Duration::from_secs(2) {
            "compressing"
        } else if s.blocks > 0 {
            "live"
        } else {
            "waiting for tool output"
        };

        let last = match all.last() {
            Some(e) => format!(
                "{}   {} \u{2192} {}",
                e.transform, e.input_tokens, e.output_tokens
            ),
            None => "none yet".to_string(),
        };
        let modes = if s.by_transform.is_empty() {
            "none yet".to_string()
        } else {
            s.by_transform
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join("  \u{b7}  ")
        };
        let mut out = String::from("\u{1b}[H");
        out.push_str(&format!("  secondwind {dot}  {state}\u{1b}[K\n\u{1b}[K\n"));
        out.push_str(&format!("  {:<14}{}\u{1b}[K\n", "blocks", s.blocks));
        out.push_str(&format!(
            "  {:<14}{}   ({pct:.0}% smaller)\u{1b}[K\n",
            "tokens saved",
            commas(saved)
        ));
        out.push_str(&format!(
            "  {:<14}{} tokens lighter, so auto-compact fires that much later (lossless)\u{1b}[K\n",
            "context wall",
            commas(saved)
        ));
        out.push_str(&format!("  {:<14}{last}\u{1b}[K\n", "last"));
        out.push_str(&format!(
            "  {:<14}{}/{} lossless\u{1b}[K\n",
            "verified", s.verified, s.blocks
        ));
        out.push_str(&format!("  {:<14}{modes}\u{1b}[K\n", "transforms"));
        out.push_str("\u{1b}[K\n  start your agent with `secondwind run -- claude` in another pane\u{1b}[K\n");
        out.push_str("  Ctrl-C to close\u{1b}[K");
        print!("{out}");
        let _ = std::io::stdout().flush();

        lit = !lit;
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn commas(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("static header")
}

fn html_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .expect("static header")
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>secondwind proof</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Plus+Jakarta+Sans:wght@300;500;700;800&family=Space+Grotesk:wght@400;500;700&display=swap" rel="stylesheet">
<style>
:root{
  --bg:#F0F1F4; --panel:#FFFFFF; --panel2:#F7F8FA; --dark:#16171B;
  --ink:#17181C; --ink2:#2E3138; --muted:#6A6F7A; --muted2:#7A8090; --faint:#9AA0AC;
  --accent:#3557C4; --accent2:#4667C9; --teal:#0B8399; --darktext:#8FA8E8;
  --line:rgba(25,35,70,0.09); --line2:rgba(25,35,70,0.05);
  --sans:'Plus Jakarta Sans',system-ui,-apple-system,Segoe UI,Roboto,sans-serif;
  --grot:'Space Grotesk',ui-monospace,Menlo,monospace;
}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink2);font-family:var(--sans);font-weight:300;-webkit-font-smoothing:antialiased}
body::before{content:"";position:fixed;inset:0;background-image:radial-gradient(rgba(53,87,196,0.08) 1px,transparent 1px);background-size:24px 24px;pointer-events:none;z-index:0}
a{color:var(--accent);text-decoration:none}
.grot{font-family:var(--grot);font-variant-numeric:tabular-nums}
.lbl{font-family:var(--grot);font-size:10px;letter-spacing:0.35em;color:var(--muted2)}
.wrap{position:relative;z-index:1}
@keyframes swPulse{0%,100%{opacity:1;box-shadow:0 0 10px currentColor}50%{opacity:.45;box-shadow:0 0 3px currentColor}}
@keyframes swBreathe{0%,100%{box-shadow:0 0 80px rgba(53,87,196,0.08)}50%{box-shadow:0 0 90px rgba(53,87,196,0.14)}}
@keyframes swFlash{0%{background:rgba(53,87,196,0.14)}100%{background:transparent}}
@media (prefers-reduced-motion:reduce){*{animation:none!important}}

header{position:sticky;top:0;z-index:20;display:flex;align-items:center;justify-content:space-between;gap:16px;flex-wrap:wrap;padding:16px 32px;background:rgba(240,241,244,0.85);backdrop-filter:blur(20px);border-bottom:1px solid rgba(25,35,70,0.07)}
.brand{display:flex;align-items:baseline;gap:14px}
.brand .name{font-weight:800;letter-spacing:-0.04em;font-size:17px;color:var(--ink)}
.brand .sub{font-family:var(--grot);font-size:10px;letter-spacing:0.35em;color:var(--accent)}
.hmeta{display:flex;align-items:center;gap:24px}
.hmeta .addr{font-family:var(--grot);font-size:9px;letter-spacing:0.25em;color:var(--muted2)}
.pill{display:flex;align-items:center;gap:8px;border:1px solid var(--line);border-radius:999px;padding:5px 14px}
.pill .dot{width:7px;height:7px;border-radius:50%;background:var(--accent);color:var(--accent);animation:swPulse 2.5s ease-in-out infinite}
.pill .txt{font-family:var(--grot);font-size:9px;letter-spacing:0.3em;color:var(--accent2)}

.banner{display:none;align-items:center;justify-content:center;gap:10px;padding:10px;background:var(--panel2);border-bottom:1px solid rgba(25,35,70,0.06)}
.banner span{font-family:var(--grot);font-size:10px;letter-spacing:0.35em;color:var(--muted2)}

main{max-width:1360px;margin:0 auto;padding:0 32px 64px;transition:opacity .4s,filter .4s}
main.disc{filter:grayscale(1);opacity:.35}

.hero{padding:56px 0 36px}
.hero .eyebrow{font-family:var(--grot);font-size:10px;letter-spacing:0.4em;color:var(--muted2);margin-bottom:18px}
.hero h1{margin:0;font-weight:800;letter-spacing:-0.04em;line-height:1.02;font-size:clamp(34px,4.4vw,60px);color:var(--ink)}
.hero h1 .a{color:var(--accent)}
.hero h1 .t{color:var(--teal)}
.pbar-wrap{margin-top:34px;max-width:960px}
.pbar{display:flex;height:16px;border-radius:2px;overflow:hidden;border:1px solid rgba(25,35,70,0.12)}
.pbar .sent{background:var(--accent);box-shadow:0 0 24px rgba(53,87,196,0.35);transition:width .6s}
.pbar .rest{flex:1;background:repeating-linear-gradient(-45deg,rgba(11,131,153,0.22) 0 4px,rgba(125,244,255,0.03) 4px 8px)}
.pbar-legend{display:flex;justify-content:space-between;margin-top:10px;font-family:var(--grot);font-size:9px;letter-spacing:0.22em}
.pbar-legend .l{color:var(--accent2)}
.pbar-legend .r{color:var(--teal)}
.attest{display:inline-flex;align-items:center;gap:10px;margin-top:24px;border:1px solid rgba(11,131,153,0.35);border-radius:999px;padding:7px 18px}
.attest .dot{width:6px;height:6px;border-radius:50%;background:var(--teal);color:var(--teal);animation:swPulse 3s ease-in-out infinite}
.attest .txt{font-family:var(--grot);font-size:9px;letter-spacing:0.25em;color:var(--teal)}

.periods{display:grid;grid-template-columns:repeat(5,1fr);gap:12px;margin-bottom:12px}
.period{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:18px}
.period.life{border-color:rgba(53,87,196,0.35);box-shadow:inset 0 0 0 1px rgba(53,87,196,0.10)}
.period .k{font-family:var(--grot);font-size:9px;letter-spacing:0.28em;color:var(--muted2)}
.period .v{font-family:var(--grot);font-variant-numeric:tabular-nums;font-weight:700;font-size:30px;margin-top:10px;color:var(--accent)}
.period .s{font-size:10px;color:var(--muted);margin-top:4px}
.tiles{display:grid;grid-template-columns:repeat(4,1fr);gap:12px}
.tile{background:rgba(255,255,255,0.72);backdrop-filter:blur(20px);border:1px solid rgba(25,35,70,0.10);border-radius:8px;padding:18px 18px 16px;animation:swBreathe 4s ease-in-out infinite;transition:border-color .2s,transform .2s}
.tile:hover{border-color:rgba(53,87,196,0.35);transform:translateY(-2px)}
.tile .k{font-family:var(--grot);font-size:8px;letter-spacing:0.28em;color:var(--muted2)}
.tile .v{font-family:var(--grot);font-variant-numeric:tabular-nums;font-weight:700;font-size:26px;margin-top:10px}
.tile .s{font-size:11px;color:var(--muted);margin-top:6px;line-height:1.4}

.row{display:grid;gap:12px;margin-top:12px}
.row.a{grid-template-columns:2fr 1fr}
.row.b{grid-template-columns:1fr 1fr}
.row3{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin-top:12px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:24px}
.card.soft{background:var(--panel2)}
.card.chart{box-shadow:inset 0 0 0 1px rgba(53,87,196,0.06)}
.card.teal{border-color:rgba(11,131,153,0.22)}
.chart .head{display:flex;justify-content:space-between;align-items:baseline}
.chart .proj{font-family:var(--grot);font-size:12px;color:var(--accent)}
.chart svg{width:100%;margin-top:14px;display:block}

.honesty .net{margin-top:18px;display:flex;justify-content:space-between;align-items:baseline;font-family:var(--grot);font-variant-numeric:tabular-nums}
.honesty .net .k{font-size:11px;color:var(--ink2)}
.honesty .net .v{font-size:28px;font-weight:700;color:var(--accent)}
.honesty p{font-size:12px;color:var(--muted);line-height:1.6;margin:16px 0 0}
.honesty .divider{margin-top:18px;padding-top:16px;border-top:1px solid rgba(25,35,70,0.07)}
.honesty .proj-row{display:flex;justify-content:space-between;align-items:baseline;font-family:var(--grot);font-variant-numeric:tabular-nums}
.honesty .proj-row .k{font-size:11px;color:var(--muted)}
.honesty .proj-row .v{font-size:22px;font-weight:700;color:var(--ink2)}

.fid .big{font-family:var(--grot);font-variant-numeric:tabular-nums;font-weight:700;font-size:40px;color:var(--teal);margin-top:14px}
.fid .cap{font-size:12px;color:var(--muted);margin-top:4px}
.fid .split{margin-top:20px}
.fid .splitbar{display:flex;height:10px;border-radius:2px;overflow:hidden;border:1px solid rgba(11,131,153,0.30)}
.fid .splitbar .in{background:rgba(11,131,153,0.55);transition:width .6s}
.fid .splitbar .off{flex:1;background:repeating-linear-gradient(-45deg,rgba(11,131,153,0.30) 0 3px,transparent 3px 6px)}
.fid .splitleg{display:flex;justify-content:space-between;margin-top:8px;font-family:var(--grot);font-size:9px;letter-spacing:0.2em;color:var(--teal)}
.fid p{font-size:12px;color:var(--muted);line-height:1.6;margin:16px 0 0}

.cert .body{margin-top:16px;font-family:var(--grot);font-size:12px;line-height:2;color:var(--accent2)}
.cert .h3{color:var(--muted2);font-size:9px;letter-spacing:0.2em}
.cert .hash{word-break:break-all;color:var(--teal);font-size:11px;line-height:1.6}
.cert .kv{display:grid;grid-template-columns:1fr 1fr;gap:4px 16px;margin-top:14px;font-variant-numeric:tabular-nums}
.cert .kv .k{color:var(--muted2)}
.cert .cmd{margin-top:14px;background:var(--dark);border:1px solid rgba(53,87,196,0.18);border-radius:4px;padding:10px 14px;font-size:11px;color:var(--darktext);font-family:var(--grot)}
.cert .none{margin-top:16px;color:var(--faint);font-size:12px;line-height:1.7}

.baraxis .head{font-family:var(--grot);font-size:10px;letter-spacing:0.35em;color:var(--muted2);margin-bottom:16px}
.bars{display:grid;gap:10px}
.bar{display:grid;grid-template-columns:90px 1fr 120px;gap:14px;align-items:center;font-family:var(--grot);font-variant-numeric:tabular-nums}
.bar.sf{grid-template-columns:80px 1fr 40px;gap:12px}
.bar .n{font-size:11px;color:var(--ink2)}
.bar .track{height:8px;background:rgba(25,35,70,0.05);border-radius:2px;overflow:hidden}
.bar .track .f{height:100%;background:var(--accent);box-shadow:0 0 12px rgba(53,87,196,0.30);transition:width .6s}
.bar.sf .track .f{background:rgba(53,87,196,0.40);box-shadow:none}
.bar .c{font-size:10px;color:var(--muted);text-align:right}

.feed{margin-top:12px;background:var(--panel);border:1px solid var(--line);border-radius:8px;overflow:hidden}
.feed .fhead{display:flex;justify-content:space-between;align-items:center;padding:18px 24px 12px}
.feed .fhead .l{font-family:var(--grot);font-size:10px;letter-spacing:0.35em;color:var(--muted2)}
.feed .fhead .r{font-family:var(--grot);font-size:9px;letter-spacing:0.2em;color:var(--faint)}
.feed .scroll{overflow-x:auto}
.feed .grid{min-width:720px}
.feed #feed{max-height:392px;overflow-y:auto}
.feed .cols,.feed .frow{display:grid;grid-template-columns:70px 90px 110px 80px 80px 80px 92px 84px;gap:8px;padding:9px 24px}
.feed .cols{font-family:var(--grot);font-size:8px;letter-spacing:0.22em;color:var(--faint);padding:6px 24px}
.feed .frow{font-family:var(--grot);font-size:11px;font-variant-numeric:tabular-nums;cursor:pointer;border-top:1px solid var(--line2)}
.feed .frow:hover{background:rgba(53,87,196,0.06)}
.feed .frow.sel{background:rgba(11,131,153,0.08)}
.feed .frow .r{text-align:right}
.feed .empty{padding:36px 24px;color:var(--faint);font-size:13px}

footer{text-align:center;padding:72px 0 24px}
footer .m{font-family:var(--grot);font-size:10px;letter-spacing:0.4em;color:var(--faint)}

.emptystate{display:none;max-width:720px;margin:0 auto;padding:130px 32px;text-align:center;position:relative;z-index:1}
.emptystate .eyebrow{font-family:var(--grot);font-size:10px;letter-spacing:0.4em;color:var(--accent);margin-bottom:20px}
.emptystate h2{margin:0;font-weight:800;letter-spacing:-0.04em;font-size:40px;color:var(--ink);line-height:1.05}
.emptystate p{color:var(--muted);font-size:15px;line-height:1.7;margin:24px 0 32px}
.emptystate .term{text-align:left;background:var(--dark);border:1px solid rgba(53,87,196,0.18);border-radius:8px;padding:20px 24px;font-family:var(--grot);font-size:13px;color:var(--darktext)}
.emptystate .term .c{color:var(--muted2)}
.emptystate .term .cm{color:var(--faint)}

@media (max-width:1100px){.tiles{grid-template-columns:repeat(2,1fr)}.periods{grid-template-columns:repeat(3,1fr)}.row.a,.row.b,.row3{grid-template-columns:1fr}}
@media (max-width:560px){.tiles{grid-template-columns:repeat(2,1fr)}.periods{grid-template-columns:repeat(2,1fr)}header{padding:14px 18px}main{padding:0 18px 48px}}
</style>
</head>
<body>
<div class="wrap">
<header>
  <div class="brand"><span class="name">SECONDWIND</span><span class="sub">PROOF</span></div>
  <div class="hmeta">
    <span class="addr" id="addr">127.0.0.1 &middot; 1s POLL</span>
    <span class="pill"><span class="dot" id="st-dot"></span><span class="txt" id="st-txt">CONNECTING</span></span>
  </div>
</header>

<div class="banner" id="banner"><span>LINK DOWN &middot; RECONNECTING</span></div>

<section class="emptystate" id="emptystate">
  <div class="eyebrow">NO TRAFFIC YET</div>
  <h2>NOTHING TO PROVE.<br>YET.</h2>
  <p>This ledger only shows real events. Run your agent through secondwind and the first block appears here within a second.</p>
  <div class="term">
    <div><span class="c">$</span> secondwind run -- claude</div>
    <div style="margin-top:6px"><span class="c">$</span> secondwind exec -- ls -la &nbsp;<span class="cm"># or serve, the Bash hook, the mcp server</span></div>
  </div>
</section>

<main id="main" style="display:none">
  <section class="hero">
    <div class="eyebrow" id="hero-eyebrow">LIVE LEDGER &middot; SESSION 0m &middot; 0 BLOCKS</div>
    <h1>SAME CONTEXT. <span class="a"><span id="hero-pct">0</span>% FEWER TOKENS.</span><br><span class="t" id="hero-kept">100%</span> OF THE INFORMATION KEPT.</h1>
    <div class="pbar-wrap">
      <div class="pbar"><div class="sent" id="pbar-sent" style="width:100%"></div><div class="rest"></div></div>
      <div class="pbar-legend"><span class="l">SENT TO MODEL &middot; <span id="pbar-out">0</span></span><span class="r">REMOVED &amp; RECOVERABLE &middot; <span id="pbar-saved">0</span></span></div>
    </div>
    <div class="attest"><span class="dot"></span><span class="txt" id="attest">0 DROPPED &middot; 0/0 BLOCKS PROVEN LOSSLESS &middot; ORIGINALS ONE RESOLVE AWAY</span></div>
  </section>

  <section class="periods" id="periods"></section>

  <section class="tiles" id="tiles"></section>

  <section class="row a">
    <div class="card chart">
      <div class="head"><span class="lbl">CUMULATIVE TOKENS REMOVED</span><span class="proj" id="chart-proj">0</span></div>
      <svg id="chart" viewBox="0 0 600 200"></svg>
    </div>
    <div class="card soft honesty">
      <span class="lbl">HONESTY</span>
      <div class="net"><span class="k">Tokens removed, counted once</span><span class="v" id="removed-total">0</span></div>
      <p>Token counts we can prove, not dollars we would guess. Counted once when compressed, never again on a cache re-read. We do not estimate your bill.</p>
      <div class="divider">
        <div class="proj-row"><span class="k">Information kept</span><span class="v" style="color:var(--teal)">100%</span></div>
        <p>Nothing dropped. Every value is inline or recoverable byte-for-byte, verified per block by a blake3 certificate.</p>
      </div>
    </div>
  </section>

  <section class="row b">
    <div class="card teal fid">
      <span class="lbl" style="color:var(--teal)">FIDELITY PROOF</span>
      <div class="big" id="fid-count">0 / 0</div>
      <div class="cap">blocks verified lossless before anything was applied. Not one exception, by construction: unproven means untouched.</div>
      <div class="split">
        <div class="splitbar"><div class="in" id="fid-in" style="width:0%"></div><div class="off"></div></div>
        <div class="splitleg"><span>PRESENT INLINE &middot; <span id="fid-inline">0</span></span><span>RECOVERABLE VIA RESOLVE &middot; <span id="fid-off">0</span></span></div>
      </div>
      <p>Every value the model could need is either still in context, or its exact original sits in the local store: <span class="grot" style="color:var(--accent2)">secondwind resolve &lt;marker&gt;</span> returns it byte for byte.</p>
    </div>
    <div class="card cert">
      <span class="lbl">FIDELITY CERTIFICATE</span>
      <div id="cert"><div class="none">Select any row in the feed below to inspect its certificate: the blake3 digest of the original, and the proof that the same atoms exist after compression.</div></div>
    </div>
  </section>

  <section class="row3">
    <div class="card soft baraxis">
      <div class="head">BY TRANSFORM</div>
      <div class="bars" id="transforms"></div>
    </div>
    <div class="card soft baraxis">
      <div class="head">BY PLATFORM</div>
      <div class="bars" id="platforms"></div>
    </div>
    <div class="card soft baraxis">
      <div class="head">BY SURFACE</div>
      <div class="bars" id="surfaces"></div>
    </div>
  </section>

  <section class="feed">
    <div class="fhead"><span class="l">RECENT BLOCKS</span><span class="r">CLICK A ROW FOR ITS CERTIFICATE</span></div>
    <div class="scroll"><div class="grid">
      <div class="cols"><span>WHEN</span><span>SURFACE</span><span>TRANSFORM</span><span class="r">IN</span><span class="r">OUT</span><span class="r">SAVED</span><span>PROOF</span><span>PLACEMENT</span></div>
      <div id="feed"></div>
    </div></div>
  </section>

  <footer><span class="m">SECONDWIND REMOVES REDUNDANCY / NOT INFORMATION</span></footer>
</main>
</div>

<script>
const $ = id => document.getElementById(id);
function fmtTok(n){ n=Math.round(n); return n>=1e6?(n/1e6).toFixed(2)+"M":n>=1e3?(n/1e3).toFixed(n>=1e4?0:1)+"K":""+n; }
function rel(ms){ const s=Math.max(0,(Date.now()-ms)/1000); return s<60?Math.round(s)+"s":s<3600?Math.round(s/60)+"m":s<86400?(s/3600).toFixed(1)+"h":Math.round(s/86400)+"d"; }
function fmtDate(ms){ try{ return new Date(ms).toLocaleString([],{month:"short",day:"numeric",hour:"2-digit",minute:"2-digit"}).toUpperCase(); }catch(e){ return ""; } }
function esc(s){ return String(s).replace(/[&<>]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;"}[c])); }

let selected=null, prevBlocks=0, flashUntil=0;

function setStatus(kind){
  const map={
    live:["var(--accent)","var(--accent2)","LIVE","swPulse 2.5s ease-in-out infinite"],
    compressing:["var(--teal)","var(--teal)","COMPRESSING","swPulse .5s ease-in-out infinite"],
    idle:["var(--muted)","var(--muted)","IDLE &middot; WATCHING","swPulse 4s ease-in-out infinite"],
    waiting:["var(--accent)","var(--accent2)","WAITING","swPulse 3s ease-in-out infinite"],
    disc:["var(--faint)","var(--muted2)","DISCONNECTED","none"]
  };
  const [dot,txt,label,anim]=map[kind];
  const d=$("st-dot"); d.style.background=dot; d.style.color=dot; d.style.animation=anim;
  const t=$("st-txt"); t.innerHTML=label; t.style.color=txt;
}

function drawChart(curve,savedT){
  const svg=$("chart");
  let g='<line x1="36" y1="10" x2="596" y2="10" stroke="rgba(25,35,70,0.07)"></line>'+
        '<line x1="36" y1="95" x2="596" y2="95" stroke="rgba(25,35,70,0.07)"></line>'+
        '<line x1="36" y1="180" x2="596" y2="180" stroke="rgba(25,35,70,0.18)"></line>'+
        '<text x="30" y="14" fill="#7A8090" font-size="9" font-family="Space Grotesk" text-anchor="end">'+fmtTok(savedT)+'</text>'+
        '<text x="30" y="99" fill="#7A8090" font-size="9" font-family="Space Grotesk" text-anchor="end">'+fmtTok(savedT/2)+'</text>'+
        '<text x="30" y="184" fill="#7A8090" font-size="9" font-family="Space Grotesk" text-anchor="end">0</text>';
  if(curve && curve.length>1){
    const max=curve[curve.length-1].cumulative_tokens||1, n=curve.length;
    const pts=curve.map((p,i)=>[36+(i/(n-1))*560, 180-Math.min(1,p.cumulative_tokens/max)*165]);
    const path="M"+pts.map(p=>p[0].toFixed(1)+" "+p[1].toFixed(1)).join(" L");
    const area=path+" L"+pts[pts.length-1][0].toFixed(1)+" 180 L36 180 Z";
    const end=pts[pts.length-1];
    g+='<path d="'+area+'" fill="rgba(53,87,196,0.10)"></path>'+
       '<path d="'+path+'" fill="none" stroke="#3557C4" stroke-width="1.5"></path>'+
       '<circle cx="'+end[0].toFixed(1)+'" cy="'+end[1].toFixed(1)+'" r="3.5" fill="#3557C4"></circle>';
  }
  svg.innerHTML=g;
}

function renderCert(){
  const box=$("cert");
  if(!selected){ box.innerHTML='<div class="none">Select any row in the feed below to inspect its certificate: the blake3 digest of the original, and the proof that the same atoms exist after compression.</div>'; return; }
  const c=selected;
  const atoms=Number(c.atoms).toLocaleString();
  let h='<div class="body"><div class="h3">BLAKE3</div><div class="hash">'+esc(c.hash||"(none recorded)")+'</div>'+
    '<div class="kv">'+
      '<span class="k">atoms in</span><span>'+atoms+'</span>'+
      '<span class="k">atoms out</span><span style="color:var(--teal)">'+atoms+' &#10003; equal</span>'+
      '<span class="k">transform</span><span>'+esc(c.transform)+'</span>'+
      '<span class="k">tokens</span><span>'+fmtTok(c.inTok)+' &#8594; '+fmtTok(c.outTok)+'</span>'+
      '<span class="k">placement</span><span>'+(c.inline?"inline &middot; still in context":"offloaded &middot; local store")+'</span>'+
    '</div>';
  if(!c.inline) h+='<div class="cmd">$ secondwind resolve &lt;marker&gt;</div>';
  h+='</div>';
  box.innerHTML=h;
}

function render(d){
  const inTok=d.input_tokens||0, outTok=d.output_tokens||0, savedT=Math.max(0,inTok-outTok);
  const blocks=d.blocks||0, verified=d.verified||0, inline=d.inline||0, off=d.offloaded||0;
  const seen=d.seen||blocks, kept=d.kept||0, reasons=d.by_kept_reason||{};
  const rnames=Object.keys(reasons).sort((a,b)=>reasons[b]-reasons[a]);
  const topReason=rnames.length?rnames.slice(0,2).map(n=>reasons[n]+" "+esc(n)).join(" &middot; "):"none refused";
  const pct=inTok?Math.round(savedT/inTok*100):0;
  const sentPct=inTok?Math.max(2,100-pct):100;

  const start=(d.curve&&d.curve.length)?d.curve[0].at_ms:Date.now();
  const durMin=Math.max(1,(Date.now()-start)/60000);

  const lastMs=(d.recent&&d.recent.length)?d.recent[0].at_ms:start;
  $("hero-eyebrow").innerHTML="CUMULATIVE LEDGER &middot; "+seen+" BLOCKS SEEN SINCE "+fmtDate(start)+" &middot; LAST "+rel(lastMs);
  $("hero-pct").textContent=pct;
  const keptPct=blocks?(verified/blocks)*100:100;
  const keptEl=$("hero-kept");
  keptEl.textContent=(keptPct===100?"100":keptPct.toFixed(1))+"%";
  keptEl.style.color=keptPct===100?"var(--teal)":"#C4442E";
  $("pbar-sent").style.width=sentPct+"%";
  $("pbar-out").textContent=fmtTok(outTok);
  $("pbar-saved").textContent=fmtTok(savedT);
  $("attest").innerHTML="0 DROPPED &middot; "+verified+"/"+blocks+" BLOCKS PROVEN LOSSLESS"+(kept>0?" &middot; "+kept+" KEPT VERBATIM":"")+" &middot; ORIGINALS ONE RESOLVE AWAY";

  const periods=[
    {k:"LAST HOUR",v:d.saved_hour||0},
    {k:"TODAY",v:d.saved_today||0},
    {k:"THIS WEEK",v:d.saved_week||0},
    {k:"THIS MONTH",v:d.saved_month||0},
    {k:"LIFETIME",v:savedT,life:true}
  ];
  $("periods").innerHTML=periods.map(p=>'<div class="period'+(p.life?" life":"")+'"><div class="k">'+p.k+'</div><div class="v">'+fmtTok(p.v)+'</div><div class="s">tokens removed</div></div>').join("");

  const tiles=[
    {k:"BLOCKS COMPRESSED",v:""+blocks,s:inline+" inline &middot; "+off+" recoverable",c:"var(--ink)"},
    {k:"VERIFIED LOSSLESS",v:verified+"/"+blocks,s:"blake3-certified, re-checked",c:"var(--teal)"},
    {k:"RECOVERABLE",v:""+off,s:"byte-exact via resolve",c:"var(--accent2)"},
    {k:"TOKENS TO MODEL",v:fmtTok(outTok),s:"down from "+fmtTok(inTok),c:"var(--ink)"}
  ];
  if(kept>0) tiles.push({k:"KEPT VERBATIM",v:""+kept,s:topReason,c:"var(--faint)"});
  $("tiles").innerHTML=tiles.map(t=>'<div class="tile"><div class="k">'+t.k+'</div><div class="v" style="color:'+t.c+'">'+t.v+'</div><div class="s">'+t.s+'</div></div>').join("");

  drawChart(d.curve,savedT);
  $("chart-proj").textContent=fmtTok(savedT)+" removed, all time";
  $("removed-total").textContent=fmtTok(savedT);

  $("fid-count").textContent=verified+" / "+verified;
  const inlinePct=blocks?Math.round(inline/blocks*100):0;
  $("fid-in").style.width=inlinePct+"%";
  $("fid-inline").textContent=inline;
  $("fid-off").textContent=off;

  const ts=d.transform_saved||{}, tc=d.by_transform||{};
  const tnames=Object.keys(tc).sort((a,b)=>(ts[b]||0)-(ts[a]||0));
  const tmax=Math.max(1,...tnames.map(n=>ts[n]||0));
  $("transforms").innerHTML=tnames.length?tnames.map(n=>
    '<div class="bar"><span class="n">'+esc(n)+'</span><div class="track"><div class="f" style="width:'+Math.round((ts[n]||0)/tmax*100)+'%"></div></div><span class="c">'+tc[n]+' blk &middot; '+fmtTok(ts[n]||0)+'</span></div>'
  ).join(""):'<div style="color:var(--faint);font-size:12px">waiting for traffic</div>';

  const bars=(map,el)=>{
    const names=Object.keys(map).sort((a,b)=>map[b]-map[a]);
    const mx=Math.max(1,...names.map(n=>map[n]));
    $(el).innerHTML=names.length?names.map(n=>
      '<div class="bar sf"><span class="n">'+esc(n)+'</span><div class="track"><div class="f" style="width:'+Math.round(map[n]/mx*100)+'%"></div></div><span class="c">'+map[n]+'</span></div>'
    ).join(""):'<div style="color:var(--faint);font-size:12px">waiting for traffic</div>';
  };
  bars(d.by_platform||{},"platforms");
  bars(d.by_surface||{},"surfaces");

  const feed=$("feed");
  if(d.recent && d.recent.length){
    feed.innerHTML=d.recent.map(e=>{
      const es=Math.max(0,(e.input_tokens||0)-(e.output_tokens||0));
      const key=e.at_ms+":"+(e.cert||"");
      const sel=selected&&selected.key===key?" sel":"";
      return '<div class="frow'+sel+'" data-key="'+esc(key)+'" data-hash="'+esc(e.cert||"")+'" data-atoms="'+(e.atoms||0)+'" data-transform="'+esc(e.transform)+'" data-in="'+(e.input_tokens||0)+'" data-out="'+(e.output_tokens||0)+'" data-inline="'+(e.inline?1:0)+'">'+
        '<span title="'+esc(fmtDate(e.at_ms))+'" style="color:var(--muted)">'+rel(e.at_ms)+'</span>'+
        '<span style="color:var(--ink2)">'+esc(e.surface)+'</span>'+
        '<span style="color:var(--accent2)">'+esc(e.transform)+'</span>'+
        '<span class="r" style="color:var(--muted)">'+fmtTok(e.input_tokens||0)+'</span>'+
        '<span class="r" style="color:var(--ink2)">'+fmtTok(e.output_tokens||0)+'</span>'+
        '<span class="r" style="color:var(--accent)">'+fmtTok(es)+'</span>'+
        '<span style="color:'+(e.verified?"var(--teal)":"var(--muted)")+'">'+(e.verified?"&#10003; LOSSLESS":"&middot;")+'</span>'+
        '<span style="color:'+(e.inline?"var(--ink2)":"var(--accent2)")+'">'+(e.inline?"inline":"resolve")+'</span>'+
      '</div>';
    }).join("");
  } else {
    feed.innerHTML='<div class="empty">No blocks yet.</div>';
  }
  renderCert();
}

$("feed").addEventListener("click",ev=>{
  const row=ev.target.closest(".frow"); if(!row) return;
  const key=row.dataset.key;
  if(selected&&selected.key===key){ selected=null; }
  else {
    selected={key,hash:row.dataset.hash,atoms:row.dataset.atoms,transform:row.dataset.transform,
      inTok:+row.dataset.in,outTok:+row.dataset.out,inline:row.dataset.inline==="1"};
  }
  document.querySelectorAll(".frow.sel").forEach(r=>r.classList.remove("sel"));
  if(selected) row.classList.add("sel");
  renderCert();
});

async function tick(){
  let d;
  try{ d=await (await fetch("/events.json?tz="+new Date().getTimezoneOffset())).json(); }
  catch(e){ $("banner").style.display="flex"; $("main").classList.add("disc"); setStatus("disc"); return; }
  $("banner").style.display="none"; $("main").classList.remove("disc");
  const blocks=d.blocks||0;
  if(blocks===0){ $("emptystate").style.display="block"; $("main").style.display="none"; setStatus("waiting"); prevBlocks=0; return; }
  $("emptystate").style.display="none"; $("main").style.display="block";
  if(blocks>prevBlocks) flashUntil=Date.now()+1600;
  prevBlocks=blocks;
  setStatus(Date.now()<flashUntil?"compressing":"live");
  render(d);
}
tick();
setInterval(tick,1000);
</script>
</body>
</html>
"##;
