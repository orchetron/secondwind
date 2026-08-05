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
            Response::from_string(page()).with_header(html_header())
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
        let rule = "\u{2500}".repeat(46);
        out.push_str(&format!(
            "  \u{224b} \u{1b}[1msecondwind\u{1b}[0m   {dot}  {state}\u{1b}[K\n"
        ));
        out.push_str(&format!("  \u{1b}[2m{rule}\u{1b}[0m\u{1b}[K\n\u{1b}[K\n"));
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

// Text wordmark, no logo image, so the page is one static string; still no network
// request (system fonts, inline SVG only).
fn page() -> &'static str {
    PAGE
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>secondwind proof</title>
<!-- System fonts only; the dashboard makes no network requests. -->
<style>
/* secondwind proof dashboard */
.sw{
  --bg:#0B0D13; --panel:#12151D; --panel2:#161A23;
  --ink:#EEF1F7; --body:#C3C9D6; --muted:#858C9A; --muted2:#98A0B0; --faint:#5B6472;
  --blue:#5C7CEF; --blue2:#829CF6; --teal:#26B6CE; --rust:#E4614A;
  --h1:rgba(255,255,255,0.06); --h2:rgba(255,255,255,0.10);
  --sans:'Plus Jakarta Sans','Segoe UI',system-ui,-apple-system,Roboto,Helvetica,Arial,sans-serif;
  --mono:'Space Grotesk',ui-monospace,'SF Mono','Menlo','Roboto Mono',monospace;

  position:relative;
  overflow-x:clip;
  background:
    radial-gradient(120% 80% at 78% -10%, rgba(38,182,206,.05), transparent 60%),
    var(--bg);
  color:var(--body);
  font-family:var(--sans);
  font-size:16px; line-height:1.5;
  -webkit-font-smoothing:antialiased; text-rendering:optimizeLegibility;
  padding:clamp(18px,3.4vw,48px) clamp(16px,4vw,56px) 40px;
}
.sw *{box-sizing:border-box;}
.sw .num{font-family:var(--mono); font-variant-numeric:tabular-nums; font-feature-settings:"tnum" 1; letter-spacing:-.01em;}
.sw .mono-t{font-family:var(--mono); font-variant-numeric:tabular-nums; letter-spacing:-.01em;}
.sw .wrap{max-width:1180px; margin:0 auto;}

/* ---- masthead ---- */
.sw .masthead{
  display:flex; align-items:center; justify-content:space-between; gap:18px;
  flex-wrap:wrap; padding-bottom:18px; border-bottom:1px solid var(--h1);
}
.sw .wordmark{
  font-weight:800; font-size:15px; letter-spacing:.02em; color:var(--ink);
  display:flex; align-items:baseline; gap:.35em;
}
.sw .wordmark b{font-weight:800;}
.sw .wordmark b .w2{color:var(--teal);}
.sw .wordmark .slash{color:var(--faint); font-weight:600; letter-spacing:.16em; font-size:12px; text-transform:uppercase;}
.sw .status{display:flex; align-items:center; gap:10px; flex-wrap:wrap;}
.sw .pill{
  font-family:var(--mono); font-size:11.5px; letter-spacing:.06em;
  color:var(--muted2); border:1px solid var(--h2); border-radius:999px;
  padding:5px 11px; white-space:nowrap;
}
.sw .pill b{color:var(--ink); font-weight:700;}
.sw .pill.live{color:var(--teal); border-color:rgba(38,182,206,.35);}
.sw .pill.live .dot{
  display:inline-block; width:6px; height:6px; border-radius:50%;
  background:var(--teal); margin-right:7px; vertical-align:middle;
  box-shadow:0 0 0 0 rgba(38,182,206,.55); animation:swpulse 2.4s ease-out infinite;
}
@keyframes swpulse{0%{box-shadow:0 0 0 0 rgba(38,182,206,.5)} 70%{box-shadow:0 0 0 7px rgba(38,182,206,0)} 100%{box-shadow:0 0 0 0 rgba(38,182,206,0)}}

/* hero: vessel + numbers */
.sw .hero{position:relative; padding:clamp(30px,6vw,72px) 0 clamp(26px,4vw,44px);}
.sw .glow-wrap{position:absolute; inset:-40% -20% -30% -20%; overflow:hidden; z-index:0; pointer-events:none;}
.sw .glow{
  position:absolute; left:6%; top:42%; width:min(760px,86%); aspect-ratio:1/.62;
  transform:translateY(-50%);
  background:
    radial-gradient(closest-side, rgba(92,124,239,.30), rgba(92,124,239,.10) 46%, transparent 72%),
    radial-gradient(closest-side at 74% 62%, rgba(38,182,206,.22), transparent 70%);
  filter:blur(24px); opacity:.9; animation:swglow 9s ease-in-out infinite;
}
@keyframes swglow{0%,100%{opacity:.72; transform:translateY(-50%) scale(1)} 50%{opacity:1; transform:translateY(-52%) scale(1.05)}}

.sw .hero-grid{
  position:relative; z-index:1;
  display:grid; grid-template-columns:minmax(300px,340px) 1fr; gap:clamp(24px,4vw,56px);
  align-items:center;
}

/* the vessel column */
.sw .wall{min-width:0; max-width:340px;}
.sw .wall-cap{
  font-family:var(--mono); font-size:10.5px; letter-spacing:.26em; text-transform:uppercase;
  color:var(--faint); margin:0 0 10px 4px;
}
.sw .vessel{width:100%; height:auto; display:block; max-width:340px;}

/* the numbers column */
.sw .hero-read{min-width:0; display:flex; flex-direction:column; gap:clamp(16px,2.4vw,28px);}

.sw .eyebrow{
  font-size:12px; letter-spacing:.22em; text-transform:uppercase; color:var(--muted);
  margin:0 0 clamp(14px,2vw,22px); font-weight:600;
}
.sw .eyebrow .sep{color:var(--faint); margin:0 .55em;}
.sw .eyebrow .ok{color:var(--teal);}

/* stage */
.sw .stage{display:flex; flex-direction:column; align-items:flex-start; gap:clamp(10px,1.6vw,16px);}
.sw .before-line{display:flex; align-items:baseline; gap:.6em; min-height:1.1em;}
.sw .fig-before .num{
  display:inline-block; transform-origin:left center;
  font-size:clamp(1.4rem,3.6vw,2.35rem); font-weight:700; color:var(--muted2);
  transition:transform .72s cubic-bezier(.36,0,.12,1), color .72s ease;
}
.sw .before-cap{font-size:13px; color:var(--faint); letter-spacing:.02em; transition:opacity .5s ease;}

.sw .after-line{display:flex; align-items:flex-end; flex-wrap:wrap; gap:0 .55em; margin-top:2px;}
.sw .fig-after{transition:opacity .6s ease .06s, transform .62s cubic-bezier(.18,.72,.24,1) .06s;}
.sw .fig-after .num{
  font-size:clamp(3.1rem,9.5vw,7.4rem); font-weight:800; line-height:.9; color:var(--ink);
  letter-spacing:-.03em; text-shadow:0 0 46px rgba(130,156,246,.28);
}
.sw .after-cap{
  font-size:clamp(12px,1.4vw,15px); color:var(--muted2); letter-spacing:.02em; padding-bottom:.7em;
}

/* headline */
.sw .headline{transition:opacity .5s ease .18s, transform .5s ease .18s;}
.sw .pct{display:flex; align-items:flex-start; color:var(--blue2); line-height:.85;}
.sw .pct .num{font-size:clamp(3.4rem,8.5vw,6.4rem); font-weight:800; letter-spacing:-.04em;}
.sw .pct i{font-style:normal; font-family:var(--mono); font-size:clamp(1.5rem,3.4vw,2.6rem); font-weight:600; margin-top:.15em; color:var(--blue);}
.sw .hl-line{color:var(--ink); font-size:clamp(15px,1.8vw,19px); font-weight:600; margin-top:6px;}
.sw .hl-sub{color:var(--muted); font-size:13.5px; margin-top:6px; line-height:1.45;}
.sw .hl-sub .keep{color:var(--teal); font-weight:600;}
.sw .hl-sub .num{color:var(--body); font-weight:600;}

/* animation-armed initial state (added only when motion allowed) */
.sw.sw-anim .fig-before .num{transform:scale(2.6); color:var(--ink);}
.sw.sw-anim .before-cap{opacity:0;}
.sw.sw-anim .fig-after{opacity:0; transform:translateY(20px) scale(.9);}
.sw.sw-anim .headline{opacity:0; transform:translateY(14px);}

/* ---- vessel svg animations ---- */
.sw .v-fill{transform-box:fill-box; transform-origin:bottom center;
  animation:swvFill 1.25s cubic-bezier(.2,.85,.25,1) .3s both;}
@keyframes swvFill{from{transform:scaleY(0)}to{transform:scaleY(1)}}
.sw .v-glow{animation:swvFade 1s ease-out .9s both;}
.sw .v-band{animation:swvFade 1.1s ease-out .5s both;}
.sw .v-lab{animation:swvFade .9s ease-out .8s both;}
@keyframes swvFade{from{opacity:0}to{opacity:1}}
.sw .v-arrow{stroke-dasharray:280; stroke-dashoffset:0; animation:swvDraw 1.1s ease-out 1s both;}
@keyframes swvDraw{from{stroke-dashoffset:280}to{stroke-dashoffset:0}}
.sw .partic{animation:swvFloaty 5s ease-in-out infinite;}
.sw .partic.b{animation-duration:6.5s; animation-delay:-2s;}
.sw .partic.c{animation-duration:7.5s; animation-delay:-4s;}
@keyframes swvFloaty{0%,100%{transform:translateY(0); opacity:.55}50%{transform:translateY(-5px); opacity:.9}}

/* lower grid */
.sw .settle{transition:opacity .55s ease var(--d,.15s), transform .55s ease var(--d,.15s);}
.sw.sw-anim .settle{opacity:0; transform:translateY(16px);}

.sw .rule{border-top:1px solid var(--h1); margin:clamp(30px,4vw,52px) 0 0;}
.sw .sect-head{display:flex; align-items:baseline; justify-content:space-between; gap:16px; flex-wrap:wrap; padding:26px 0 18px;}
.sw .sect-title{font-size:12px; letter-spacing:.2em; text-transform:uppercase; color:var(--muted); font-weight:600;}
.sw .sect-note{font-size:12.5px; color:var(--faint);}
.sw .sect-note .num{color:var(--muted2);}

.sw .ledger{display:grid; grid-template-columns:1fr 1.35fr; gap:clamp(20px,3vw,40px); align-items:end;}

/* period buckets */
.sw .cols{display:flex; align-items:flex-end; gap:clamp(10px,2vw,22px); height:150px;}
.sw .col{flex:1; display:flex; flex-direction:column; align-items:center; gap:8px; min-width:0;}
.sw .col-v{font-family:var(--mono); font-size:13px; color:var(--ink); font-weight:600; font-variant-numeric:tabular-nums;}
.sw .col-track{width:100%; max-width:38px; height:96px; display:flex; align-items:flex-end; border-bottom:1px solid var(--h2);}
.sw .col-bar{width:100%; transform-origin:bottom; border-radius:3px 3px 0 0;
  background:linear-gradient(180deg,var(--blue2),rgba(92,124,239,.35));
  transition:transform .8s cubic-bezier(.4,0,.15,1) var(--d,0s);
}
.sw .col:last-child .col-bar{background:linear-gradient(180deg,var(--teal),rgba(38,182,206,.3));}
.sw .col-k{font-size:11px; color:var(--muted); letter-spacing:.04em; text-align:center;}
.sw.sw-anim .col-bar{transform:scaleY(0);}

/* cumulative chart */
.sw .spark{position:relative; padding-top:6px;}
.sw .spark-svg{display:block; width:100%; height:clamp(130px,17vw,180px);}
.sw .spark-area{opacity:1; transition:opacity .9s ease .2s;}
.sw .spark-line{fill:none; stroke:var(--blue2); stroke-width:2; stroke-linecap:round; stroke-linejoin:round;
  stroke-dasharray:1; stroke-dashoffset:0; transition:stroke-dashoffset 1.1s cubic-bezier(.5,0,.2,1) .1s;}
.sw .spark-svg .grid{stroke:var(--h1); stroke-width:1;}
.sw .spark-dot{position:absolute; top:9%; right:1px; width:11px; height:11px; border-radius:50%;
  background:var(--blue2); box-shadow:0 0 0 4px rgba(130,156,246,.18), 0 0 18px rgba(130,156,246,.7); transform:translate(-2px,-2px);}
.sw .spark-tag{position:absolute; top:calc(9% + 16px); right:2px; font-size:12px; color:var(--muted2); text-align:right;}
.sw .spark-tag .num{color:var(--ink); font-weight:600;}
.sw .spark-foot{display:flex; justify-content:space-between; margin-top:8px; font-size:11px; color:var(--faint);}
.sw.sw-anim .spark-line{stroke-dashoffset:1;}
.sw.sw-anim .spark-area{opacity:0;}

/* three panels */
.sw .grid3{display:grid; grid-template-columns:repeat(3,1fr); gap:clamp(14px,1.8vw,22px);}
.sw .panel{background:linear-gradient(180deg,var(--panel2),var(--panel)); border:1px solid var(--h1); border-radius:12px; padding:20px 20px 22px; display:flex; flex-direction:column;}
.sw .panel-h{font-size:12px; letter-spacing:.16em; text-transform:uppercase; color:var(--muted); font-weight:600; margin-bottom:16px;}

/* blocks split */
.sw .big-stat{display:flex; align-items:baseline; gap:.5em; margin-bottom:16px;}
.sw .big-stat .num{font-size:2.6rem; font-weight:800; color:var(--ink); line-height:1;}
.sw .big-stat span{font-size:13px; color:var(--muted);}
.sw .seg-bar{display:flex; height:12px; border-radius:4px; overflow:hidden; gap:2px; margin-bottom:12px;}
.sw .seg{display:block; transform-origin:left; border-radius:2px; transition:transform .8s cubic-bezier(.4,0,.15,1) .1s;}
.sw .seg-inline{background:linear-gradient(90deg,var(--blue),var(--blue2));}
.sw .seg-off{background:linear-gradient(90deg,var(--teal),rgba(38,182,206,.6));}
.sw.sw-anim .seg{transform:scaleX(0);}
.sw .legend{display:flex; gap:16px; font-size:12.5px; margin-bottom:16px; flex-wrap:wrap;}
.sw .legend i{width:9px; height:9px; border-radius:2px; display:inline-block; margin-right:6px; vertical-align:baseline;}
.sw .legend .num{color:var(--ink); font-weight:600;}
.sw .legend .lg-in i{background:var(--blue2);}
.sw .legend .lg-off i{background:var(--teal);}
.sw .kv{display:flex; justify-content:space-between; padding:9px 0; border-top:1px solid var(--h1); font-size:13px;}
.sw .kv:first-of-type{border-top:1px solid var(--h1);}
.sw .kv .k{color:var(--muted);}
.sw .kv .v{font-family:var(--mono); color:var(--ink); font-weight:600; font-variant-numeric:tabular-nums;}
.sw .kv .v.good{color:var(--teal);}
.sw .kv .v.zero{color:var(--muted2);}

/* transform bars */
.sw .tbar{display:grid; grid-template-columns:96px 1fr auto; align-items:center; gap:12px; padding:10px 0;}
.sw .tbar+.tbar{border-top:1px solid var(--h1);}
.sw .tbar-label{font-size:13px; color:var(--body);}
.sw .tbar-track{height:8px; border-radius:5px; background:rgba(255,255,255,.04); overflow:hidden;}
.sw .tbar-fill{display:block; height:100%; border-radius:5px; transition:width .85s cubic-bezier(.4,0,.15,1) .1s;}
.sw .tf-a{background:linear-gradient(90deg,var(--blue),var(--blue2));}
.sw .tf-b{background:linear-gradient(90deg,var(--teal),rgba(38,182,206,.55));}
.sw .tf-c{background:linear-gradient(90deg,var(--blue2),rgba(130,156,246,.5));}
.sw .tf-d{background:linear-gradient(90deg,var(--muted),rgba(133,140,154,.4));}
.sw.sw-anim .tbar-fill{width:0!important;}
.sw .tbar-val{font-family:var(--mono); font-size:12px; color:var(--muted2); white-space:nowrap; font-variant-numeric:tabular-nums;}
.sw .tbar-val b{color:var(--ink); font-weight:600;}

/* fidelity certificate */
.sw .cert{position:relative; overflow:hidden;}
.sw .cert-seal{position:absolute; top:16px; right:16px; color:var(--teal); opacity:.9;}
.sw .cert-sel{font-size:12.5px; color:var(--muted2); margin-bottom:6px;}
.sw .cert-sel b{color:var(--ink); font-weight:600; font-family:var(--mono);}
.sw .cert-eq{display:flex; align-items:center; gap:9px; margin:4px 0 16px; font-size:14px; color:var(--ink);}
.sw .cert-eq .eqm{font-family:var(--mono); color:var(--teal); font-weight:700;}
.sw .cert-eq .sub{color:var(--muted); font-size:12.5px;}
.sw .cert-digest{font-family:var(--mono); font-size:12px; line-height:1.65; color:var(--body); word-break:break-all;
  background:rgba(38,182,206,.05); border:1px solid rgba(38,182,206,.16); border-radius:8px; padding:11px 13px; margin-bottom:14px;}
.sw .cert-digest .tag{color:var(--teal); font-weight:700;}
.sw .cert-foot{font-size:12px; color:var(--muted); margin-top:auto;}
.sw .cert-foot .num{color:var(--teal); font-weight:700;}

/* recent table */
.sw .table-wrap{overflow-x:auto; border:1px solid var(--h1); border-radius:12px; -webkit-overflow-scrolling:touch;}
.sw table{width:100%; border-collapse:collapse; min-width:720px;}
.sw thead th{
  text-align:left; font-size:11px; letter-spacing:.13em; text-transform:uppercase; color:var(--muted);
  font-weight:600; padding:14px 16px; border-bottom:1px solid var(--h2); white-space:nowrap; background:rgba(255,255,255,.015);
}
.sw thead th.r, .sw td.r{text-align:right;}
.sw tbody td{padding:13px 16px; border-bottom:1px solid var(--h1); font-size:13px; white-space:nowrap;}
.sw tbody tr:last-child td{border-bottom:none;}
.sw tbody tr:hover td{background:rgba(130,156,246,.04);}
.sw td .num{color:var(--ink); font-variant-numeric:tabular-nums;}
.sw td.when{color:var(--muted); font-family:var(--mono); font-size:12.5px;}
.sw td .arw{color:var(--faint); margin:0 .35em;}
.sw td .saved{color:var(--teal); font-family:var(--mono); font-weight:600;}
.sw .tag-surface{font-family:var(--mono); font-size:11.5px; color:var(--muted2);}
.sw .tag-xf{font-size:12px; color:var(--body);}
.sw .proof{display:inline-flex; align-items:center; gap:6px; font-family:var(--mono); font-size:11px; letter-spacing:.05em;
  color:var(--teal); border:1px solid rgba(38,182,206,.3); border-radius:999px; padding:3px 9px;}
.sw .proof .chk{width:10px; height:10px;}
.sw .place{font-family:var(--mono); font-size:11.5px; color:var(--muted); }
.sw .place.resolve{color:var(--blue2);}

/* footer: sources + wordmark */
.sw .foot{display:grid; grid-template-columns:1fr auto; gap:clamp(24px,3vw,44px); align-items:center; padding:26px 0 4px;}
.sw .foot-sources{display:grid; grid-template-columns:1fr 1fr; gap:clamp(20px,3vw,36px); min-width:0;}

/* sources component (grafted from context-wall, rescoped) */
.sw .src{min-width:0;}
.sw .src .sh{display:flex; justify-content:space-between; align-items:baseline; margin-bottom:8px;}
.sw .src .sh .lbl{font-family:var(--mono); font-size:10.5px; letter-spacing:.14em; text-transform:uppercase; color:var(--faint);}
.sw .src .sh .tot{font-family:var(--mono); font-size:12px; color:var(--muted2);}
.sw .src-seg{display:flex; height:12px; border-radius:6px; overflow:hidden; background:rgba(255,255,255,.05);
  transform-origin:left center; animation:swBarGrow 1.05s cubic-bezier(.2,.8,.2,1) both;}
.sw .src-seg span{height:100%; display:block;}
.sw .src-seg span + span{box-shadow:inset 1px 0 0 rgba(11,13,19,.6);}
.sw .src .keys{display:flex; flex-wrap:wrap; gap:12px 16px; margin-top:9px;}
.sw .src .keys .key{display:flex; align-items:center; gap:7px; font-size:12px; color:var(--muted2);}
.sw .src .keys .key i{width:9px; height:9px; border-radius:2px; display:block; flex:0 0 auto;}
.sw .src .keys .key b{font-family:var(--mono); color:var(--ink); font-weight:600; letter-spacing:-.01em;}
@keyframes swBarGrow{from{transform:scaleX(0)}to{transform:scaleX(1)}}

.sw .foot-mark{justify-self:end; text-align:right; color:var(--faint); font-size:11.5px; line-height:1.7;}
.sw .foot-mark b{color:var(--muted2); font-weight:700;}

@media (max-width:900px){
  .sw .foot{grid-template-columns:1fr;}
  .sw .foot-mark{justify-self:start; text-align:left;}
}
@media (max-width:860px){
  .sw .hero-grid{grid-template-columns:1fr; gap:32px;}
  .sw .wall{margin:0 auto; max-width:360px;}
  .sw .ledger{grid-template-columns:1fr; gap:34px;}
  .sw .grid3{grid-template-columns:1fr;}
  .sw.sw-anim .fig-before .num{transform:scale(1.75);}
}
@media (max-width:520px){
  .sw .foot-sources{grid-template-columns:1fr;}
  .sw .cols{gap:8px; height:140px;}
  .sw .big-stat .num{font-size:2.1rem;}
}

@media (prefers-reduced-motion:reduce){
  .sw *, .sw *::before, .sw *::after{transition:none!important; animation:none!important;}
}
</style>
<style>
/* live shell: banner, empty state, connection state */
html,body{margin:0; background:#0B0D13;}
.sw .banner{display:none; align-items:center; justify-content:center; gap:10px; padding:11px 16px; margin-top:14px;
  background:var(--panel2); border:1px solid var(--h1); border-radius:10px;}
.sw .banner span{font-family:var(--mono); font-size:11px; letter-spacing:.2em; color:var(--muted2);}
.sw #main{transition:opacity .4s ease, filter .4s ease;}
.sw #main.disc{filter:grayscale(1); opacity:.4;}
.sw .emptystate{display:none; max-width:640px; margin:0 auto; padding:clamp(64px,11vw,132px) 8px; text-align:center;}
.sw .emptystate .eyebrow{color:var(--blue2); letter-spacing:.28em;}
.sw .emptystate h2{margin:0; font-weight:800; letter-spacing:-.03em; font-size:clamp(28px,5vw,40px); color:var(--ink); line-height:1.06;}
.sw .emptystate p{color:var(--muted); font-size:15px; line-height:1.7; margin:20px 0 28px;}
.sw .emptystate .term{text-align:left; background:#0E1119; border:1px solid rgba(92,124,239,.18); border-radius:10px;
  padding:18px 22px; font-family:var(--mono); font-size:13px; color:var(--blue2);}
.sw .emptystate .term .c{color:var(--muted2);}
.sw .emptystate .term .cm{color:var(--faint);}
.sw tbody tr.frow{cursor:pointer;}
.sw tbody tr.frow.sel td{background:rgba(38,182,206,.09);}
</style>
</head>
<body>
<section class="sw" id="sw">
  <script>
    /* arm the intro pre-paint, only when motion is welcome */
    (function(){
      var r=document.getElementById('sw');
      if(!r) return;
      try{ if(window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches) return; }catch(e){}
      r.classList.add('sw-anim');
    })();
  </script>

  <div class="wrap">

    <!-- masthead -->
    <div class="masthead">
      <div class="wordmark">
        <b>second<span class="w2">wind</span></b>
        <span class="slash">proof</span>
      </div>
      <div class="status">
        <span class="pill live" id="st-pill"><span class="dot" id="st-dot"></span><span id="st-txt">CONNECTING</span></span>
        <span class="pill">LOSSLESS &middot; <b id="verify-count">0/0</b> verified</span>
        <span class="pill">blake3</span>
      </div>
    </div>

    <div class="banner" id="banner"><span>LINK DOWN &middot; RECONNECTING</span></div>

    <section class="emptystate" id="emptystate">
      <div class="eyebrow">NO TRAFFIC YET</div>
      <h2>Nothing to prove.<br>Yet.</h2>
      <p>This ledger only shows real events. Run your agent through secondwind and the first block appears here within a second.</p>
      <div class="term">
        <div><span class="c">$</span> secondwind run -- claude</div>
        <div style="margin-top:6px"><span class="c">$</span> secondwind exec -- ls -la &nbsp;<span class="cm"># or serve, the Bash hook, the mcp server</span></div>
      </div>
    </section>

    <div id="main">

    <!-- hero -->
    <div class="hero">
      <div class="glow-wrap" aria-hidden="true"><div class="glow"></div></div>

      <div class="hero-grid">

        <!-- THE VESSEL -->
        <div class="wall">
          <p class="wall-cap">The context vessel</p>
          <svg class="vessel" id="vessel" viewBox="0 0 360 540" role="img"
               aria-label="Context vessel: tokens actually sent to the model versus tokens removed and byte-exact recoverable."></svg>
        </div>

        <!-- THE NUMBERS -->
        <div class="hero-read">
          <div class="stage">
            <p class="eyebrow">The raw context <span class="sep">&middot;</span> compressed <span class="sep">&middot;</span> <span class="ok">nothing lost</span></p>

            <div class="before-line">
              <span class="fig-before"><span class="num" id="fig-raw">0</span></span>
              <span class="before-cap">tokens in the raw context</span>
            </div>

            <div class="after-line">
              <span class="fig-after"><span class="num" id="fig-sent">0</span></span>
              <span class="after-cap">tokens actually sent to the model</span>
            </div>
          </div>

          <div class="headline">
            <div class="pct"><span class="num" id="fig-pct">0</span><i>%</i></div>
            <div class="hl-line">fewer tokens sent to the model</div>
            <div class="hl-sub">
              <span class="num" id="fig-reclaimed">0</span> tokens removed: every one present inline
              or byte-exact recoverable from the local store.
              <span class="keep" id="hl-keep"></span>
            </div>
          </div>
        </div>

      </div>
    </div>

    <!-- ledger: buckets + cumulative -->
    <div class="rule"></div>
    <div class="sect-head">
      <span class="sect-title">Tokens removed &middot; recoverable</span>
      <span class="sect-note">climbing to <span class="num" id="ledger-note">0</span> lifetime</span>
    </div>

    <div class="ledger">
      <div class="settle" style="--d:.14s">
        <div class="cols" id="cols"></div>
      </div>

      <div class="settle spark" style="--d:.2s">
        <svg class="spark-svg" viewBox="0 0 100 40" preserveAspectRatio="none" aria-hidden="true">
          <defs>
            <linearGradient id="swArea" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stop-color="#26B6CE" stop-opacity=".30"/>
              <stop offset=".55" stop-color="#5C7CEF" stop-opacity=".12"/>
              <stop offset="1" stop-color="#5C7CEF" stop-opacity="0"/>
            </linearGradient>
          </defs>
          <line class="grid" x1="0" y1="22" x2="100" y2="22" vector-effect="non-scaling-stroke"/>
          <line class="grid" x1="0" y1="39.5" x2="100" y2="39.5" vector-effect="non-scaling-stroke"/>
          <path class="spark-area" id="spark-area" fill="url(#swArea)" d="M0,40 L100,40 Z"/>
          <path class="spark-line" id="spark-line" pathLength="1" vector-effect="non-scaling-stroke" d="M0,40 L100,40"/>
        </svg>
        <span class="spark-dot" aria-hidden="true"></span>
        <span class="spark-tag"><span class="num" id="spark-val">0</span> removed<br>recoverable</span>
        <div class="spark-foot"><span>cumulative</span><span>now</span></div>
      </div>
    </div>

    <!-- three panels -->
    <div class="rule"></div>
    <div class="sect-head">
      <span class="sect-title">Blocks &middot; transforms &middot; fidelity</span>
      <span class="sect-note"><span class="num" id="note-blocks">0</span> compressed &middot; <span class="num" id="note-kept">0</span> kept verbatim &middot; <span class="num">0</span> dropped</span>
    </div>

    <div class="grid3">
      <!-- blocks -->
      <div class="panel settle" style="--d:.14s">
        <div class="panel-h">Blocks compressed</div>
        <div class="big-stat"><span class="num" id="blk-count">0</span><span>this session</span></div>
        <div class="seg-bar" aria-hidden="true">
          <span class="seg seg-inline" id="seg-inline" style="flex:0"></span>
          <span class="seg seg-off" id="seg-off" style="flex:0"></span>
        </div>
        <div class="legend">
          <span class="lg-in"><i></i><span class="num" id="lg-inline">0</span> inline</span>
          <span class="lg-off"><i></i><span class="num" id="lg-off">0</span> recoverable</span>
        </div>
        <div class="kv"><span class="k">Verified lossless</span><span class="v good" id="kv-verified">0 / 0</span></div>
        <div class="kv"><span class="k">Kept verbatim</span><span class="v zero" id="kv-kept">0</span></div>
        <div class="kv"><span class="k">Dropped</span><span class="v zero">0</span></div>
      </div>

      <!-- transforms -->
      <div class="panel settle" style="--d:.2s">
        <div class="panel-h">By transform</div>
        <div id="transforms"></div>
      </div>

      <!-- fidelity certificate -->
      <div class="panel cert settle" style="--d:.26s">
        <svg class="cert-seal" width="26" height="26" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="M12 2 3 6v6c0 5 3.8 8.4 9 10 5.2-1.6 9-5 9-10V6l-9-4Z" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/>
          <path d="M8.4 12.2 11 14.8 15.9 9.6" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        <div class="panel-h">Fidelity &middot; blake3</div>
        <div id="cert-body"><div class="cert-sel" style="color:var(--faint)">Select a row below to inspect its blake3 certificate: the digest of the original and the proof the same atoms survive compression.</div></div>
        <div class="cert-foot"><span class="num" id="cert-foot-count">0/0</span> blocks certified &middot; digests match on decode</div>
      </div>
    </div>

    <!-- recent blocks -->
    <div class="rule"></div>
    <div class="sect-head">
      <span class="sect-title">Recent blocks</span>
      <span class="sect-note">every row proven: present inline or resolvable byte-exact</span>
    </div>

    <div class="table-wrap settle" style="--d:.16s">
      <table>
        <thead>
          <tr>
            <th>When</th><th>Surface</th><th>Transform</th>
            <th class="r">In</th><th class="r">Out</th><th class="r">Saved</th>
            <th>Proof</th><th>Placement</th>
          </tr>
        </thead>
        <tbody id="feed"></tbody>
      </table>
    </div>

    <!-- footer -->
    <div class="rule"></div>
    <div class="foot settle" style="--d:.12s">
      <div class="foot-sources">
        <!-- By platform -->
        <div class="src">
          <div class="sh"><span class="lbl">By platform</span><span class="tot" id="plat-tot">0</span></div>
          <div class="src-seg" id="plat-seg"></div>
          <div class="keys" id="plat-keys"></div>
        </div>
        <!-- By surface -->
        <div class="src">
          <div class="sh"><span class="lbl">By surface</span><span class="tot" id="surf-tot">0</span></div>
          <div class="src-seg" id="surf-seg"></div>
          <div class="keys" id="surf-keys"></div>
        </div>
      </div>
      <div class="foot-mark">
        <b>second<span style="color:var(--teal)">wind</span> proof</b><br>
        same context &middot; fewer tokens &middot; nothing lost
      </div>
    </div>

    </div><!-- /#main -->

  </div>

  <script>
  (function(){
    var root=document.getElementById('sw');
    var reduce=false; try{ reduce=window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches; }catch(e){}
    function $(id){ return document.getElementById(id); }

    // ---- helpers ----
    function fmtTok(n){ n=Math.round(n); return n>=1e6?(n/1e6).toFixed(2)+"M":n>=1e3?(n/1e3).toFixed(n>=1e4?0:1)+"K":""+n; }
    function rel(ms){ var s=Math.max(0,(Date.now()-ms)/1000); return s<60?Math.round(s)+"s":s<3600?Math.round(s/60)+"m":s<86400?(s/3600).toFixed(1)+"h":Math.round(s/86400)+"d"; }
    function fmtDate(ms){ try{ return new Date(ms).toLocaleString([],{month:"short",day:"numeric",hour:"2-digit",minute:"2-digit"}).toUpperCase(); }catch(e){ return ""; } }
    function esc(s){ return String(s).replace(/[&<>"]/g,function(c){ return {"&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;"}[c]; }); }
    function fmtN(v,plain){ return plain?String(v):Number(v).toLocaleString("en-US"); }

    // ---- count-up with per-element cancellation ----
    function count(el,to,dur,plain){
      to=Math.round(to);
      var token=(el._ct||0)+1; el._ct=token;
      if(reduce||dur<=0){ el.textContent=fmtN(to,plain); return; }
      var start=null;
      function step(ts){
        if(el._ct!==token) return;
        if(start===null) start=ts;
        var p=Math.min(1,(ts-start)/dur); p=1-Math.pow(1-p,3);
        el.textContent=fmtN(Math.round(to*p),plain);
        if(p<1) requestAnimationFrame(step); else el.textContent=fmtN(to,plain);
      }
      requestAnimationFrame(step);
    }
    function setNum(el,v,plain){ el._ct=(el._ct||0)+1; el.textContent=fmtN(Math.round(v),plain); }

    var selected=null, prevBlocks=0, flashUntil=0, prevVessel="", firstData=true, introUntil=0;

    function setStatus(kind){
      var pill=$("st-pill"), txt=$("st-txt"), dot=$("st-dot");
      var label = kind==="compressing"?"COMPRESSING":kind==="waiting"?"WAITING":kind==="disc"?"DISCONNECTED":"LIVE";
      txt.textContent=label;
      pill.className = "pill"+(kind==="disc"?"":" live");
      dot.style.animationDuration = kind==="compressing"?"0.8s":"";
    }

    // ---- the vessel: fill = sent/raw of the glass column; band above = removed & recoverable ----
    function drawVessel(raw,sent,reclaimed,animate){
      var TOP=44, BOT=500, H=456;                       // glass inner geometry (x 84..234)
      var frac = raw>0 ? Math.max(0,Math.min(1,sent/raw)) : 0;
      var fillH = frac*H;
      var yFill = BOT - fillH;                           // top of the teal fill
      var yMid  = BOT - 0.5*H;                           // raw/2 tick
      var bandH = Math.max(0, yFill-TOP);                // reclaimed region height
      var pctRaw = raw>0 ? Math.round(sent/raw*100) : 0;
      var bandMid = Math.max(TOP+70, Math.min((TOP+yFill)/2, yFill-52));
      var vf=animate?"v-fill":"", vb=animate?"v-band":"", vg=animate?"v-glow":"", vl=animate?"v-lab":"";
      var s="";
      s+='<defs>';
      s+='<linearGradient id="swVfill" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#26B6CE"/><stop offset="1" stop-color="#5C7CEF"/></linearGradient>';
      s+='<linearGradient id="swVband" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="rgba(92,124,239,0.18)"/><stop offset="1" stop-color="rgba(38,182,206,0.05)"/></linearGradient>';
      s+='<linearGradient id="swVglass" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="rgba(255,255,255,0.09)"/><stop offset="0.14" stop-color="rgba(255,255,255,0.02)"/><stop offset="1" stop-color="rgba(255,255,255,0)"/></linearGradient>';
      s+='<filter id="swVsoft" x="-40%" y="-40%" width="180%" height="180%"><feGaussianBlur stdDeviation="3.4"/></filter>';
      s+='<pattern id="swVhatch" width="8" height="8" patternTransform="rotate(45)" patternUnits="userSpaceOnUse"><rect width="8" height="8" fill="rgba(92,124,239,0.03)"/><line x1="0" y1="0" x2="0" y2="8" stroke="rgba(38,182,206,0.20)" stroke-width="1.1"/></pattern>';
      s+='<clipPath id="swVclip"><rect x="84" y="44" width="150" height="456" rx="16"/></clipPath>';
      s+='</defs>';
      // axis ticks: 0, raw/2, raw
      s+='<g class="mono-t" fill="#5B6472" font-size="10.5" text-anchor="end">';
      s+='<g stroke="rgba(255,255,255,0.10)"><line x1="80" y1="500" x2="84" y2="500"/><line x1="80" y1="'+yMid+'" x2="84" y2="'+yMid+'"/><line x1="80" y1="44" x2="84" y2="44"/></g>';
      s+='<text x="76" y="503">'+fmtTok(0)+'</text>';
      s+='<text x="76" y="'+(yMid+3)+'">'+fmtTok(raw/2)+'</text>';
      s+='<text x="76" y="47">'+fmtTok(raw)+'</text>';
      s+='</g>';
      // clipped inner layers
      s+='<g clip-path="url(#swVclip)">';
      s+='<rect x="84" y="44" width="150" height="456" fill="#0E1119"/>';
      s+='<rect class="'+vb+'" x="84" y="44" width="150" height="'+bandH+'" fill="url(#swVband)"/>';
      s+='<rect class="'+vb+'" x="84" y="44" width="150" height="'+bandH+'" fill="url(#swVhatch)"/>';
      s+='<g class="'+vf+'"><rect x="84" y="'+yFill+'" width="150" height="'+fillH+'" fill="url(#swVfill)"/></g>';
      s+='<g class="'+vg+'"><rect x="84" y="'+(yFill-4)+'" width="150" height="9" fill="#26B6CE" filter="url(#swVsoft)" opacity="0.5"/><line x1="84" y1="'+yFill+'" x2="234" y2="'+yFill+'" stroke="#7DF4FF" stroke-width="1.6"/></g>';
      s+='<g fill="#AEBEFB"><circle class="partic" cx="112" cy="486" r="2.1" opacity="0.6"/><circle class="partic b" cx="176" cy="480" r="1.7" opacity="0.5"/><circle class="partic c" cx="204" cy="490" r="2.2" opacity="0.6"/></g>';
      s+='<rect x="84" y="44" width="150" height="456" fill="url(#swVglass)"/>';
      s+='</g>';
      // vessel outline
      s+='<rect x="84" y="44" width="150" height="456" rx="16" fill="none" stroke="rgba(255,255,255,0.14)"/>';
      // teal boundary marker at the sent/reclaimed divide
      s+='<g class="'+vl+'"><circle cx="234" cy="'+yFill+'" r="3.4" fill="#26B6CE" stroke="#0B0D13" stroke-width="1"/></g>';
      // right-side labels, all from data
      // The "reclaimed" cluster (bandMid..bandMid+30) only has room when the band above the
      // fill line is tall enough; below that, bandMid's clamp degenerates and it collides with
      // the "sent" cluster anchored at yFill. Hide it rather than overlap unreadable text.
      var showReclaimed = bandH >= 122;
      s+='<g class="'+vl+'">';
      s+='<text x="244" y="49" fill="#98A0B0" font-size="10.5" letter-spacing="0.06em">RAW SESSION</text>';
      s+='<text class="mono-t" x="244" y="65" fill="#EEF1F7" font-size="12.5">'+fmtTok(raw)+'</text>';
      if(showReclaimed){
        s+='<text class="mono-t" x="244" y="'+bandMid+'" fill="#829CF6" font-size="22" letter-spacing="-0.02em">+'+fmtTok(reclaimed)+'</text>';
        s+='<text x="244" y="'+(bandMid+16)+'" fill="#98A0B0" font-size="11">tokens reclaimed</text>';
        s+='<text x="244" y="'+(bandMid+30)+'" fill="#26B6CE" font-size="10.5" letter-spacing="0.04em">100% RECOVERABLE</text>';
      }
      s+='<text x="244" y="'+(yFill-2)+'" fill="#26B6CE" font-size="10.5" letter-spacing="0.06em">SECOND WIND HOLDS</text>';
      s+='<text class="mono-t" x="244" y="'+(yFill+13)+'" fill="#EEF1F7" font-size="13">'+fmtTok(sent)+'</text>';
      s+='<text x="244" y="'+(yFill+27)+'" fill="#5B6472" font-size="10">'+pctRaw+'% of raw context</text>';
      s+='</g>';
      return s;
    }

    function drawSpark(curve,saved){
      var area=$("spark-area"), line=$("spark-line");
      $("spark-val").textContent=fmtTok(saved);
      if(!curve || curve.length<2){ line.setAttribute("d","M0,40 L100,40"); area.setAttribute("d","M0,40 L100,40 Z"); return; }
      var n=curve.length, max=0, i;
      for(i=0;i<n;i++){ if(curve[i].cumulative_tokens>max) max=curve[i].cumulative_tokens; }
      if(max<=0) max=1;
      var pts=[];
      for(i=0;i<n;i++){
        var x=(i/(n-1))*100;
        var y=40 - Math.max(0,Math.min(1,curve[i].cumulative_tokens/max))*36;
        pts.push(x.toFixed(2)+","+y.toFixed(2));
      }
      var lp="M"+pts[0]; for(i=1;i<n;i++) lp+=" L"+pts[i];
      line.setAttribute("d", lp);
      area.setAttribute("d", lp+" L100,40 L0,40 Z");
    }

    function buildCols(h1,td,wk,mo,life){
      var ps=[["last hr",h1],["today",td],["this week",wk],["this month",mo],["lifetime",life]];
      var mx=1,i; for(i=0;i<ps.length;i++) if(ps[i][1]>mx) mx=ps[i][1];
      var out="";
      for(i=0;i<ps.length;i++){
        var v=ps[i][1], pct=Math.round(v/mx*100); if(v>0&&pct<3) pct=3;
        out+='<div class="col"><span class="col-v">'+fmtTok(v)+'</span><span class="col-track"><span class="col-bar" style="height:'+pct+'%"></span></span><span class="col-k">'+ps[i][0]+'</span></div>';
      }
      $("cols").innerHTML=out;
    }

    function buildTransforms(by,saved){
      var names=Object.keys(by);
      names.sort(function(a,b){ return (saved[b]||0)-(saved[a]||0); });
      var mx=1,i; for(i=0;i<names.length;i++) if((saved[names[i]]||0)>mx) mx=saved[names[i]];
      var cls=["tf-a","tf-b","tf-c","tf-d"], out="";
      for(i=0;i<names.length;i++){
        var n=names[i], sv=saved[n]||0, cnt=by[n]||0, w=Math.round(sv/mx*100);
        out+='<div class="tbar"><span class="tbar-label">'+esc(n)+'</span><span class="tbar-track"><span class="tbar-fill '+cls[i%4]+'" style="width:'+w+'%"></span></span><span class="tbar-val"><b>'+cnt+'</b> blk &middot; '+fmtTok(sv)+'</span></div>';
      }
      if(!names.length) out='<div style="color:var(--faint);font-size:12.5px">waiting for traffic</div>';
      $("transforms").innerHTML=out;
    }

    function buildSrc(map,segId,keysId,totId,first){
      var names=Object.keys(map);
      names.sort(function(a,b){ return map[b]-map[a]; });
      var sum=0,i; for(i=0;i<names.length;i++) sum+=map[names[i]]; if(sum<=0) sum=1;
      var pal=["#5C7CEF","#26B6CE","#829CF6"], seg="", keys="";
      for(i=0;i<names.length;i++){
        var n=names[i], c=pal[i%3], w=(map[n]/sum*100);
        seg+='<span style="width:'+w.toFixed(2)+'%;background:'+c+'"></span>';
        keys+='<span class="key"><i style="background:'+c+'"></i>'+esc(n)+' <b>'+map[n]+'</b></span>';
      }
      var segEl=$(segId);
      segEl.innerHTML=seg;
      segEl.style.animation = first?"":"none";
      $(keysId).innerHTML=keys;
      $(totId).textContent=names.length;
    }

    function buildFeed(recent){
      var feed=$("feed");
      if(!recent || !recent.length){ feed.innerHTML='<tr><td colspan="8" style="padding:22px 16px;color:var(--faint)">No blocks yet.</td></tr>'; return; }
      var out="",i;
      for(i=0;i<recent.length;i++){
        var e=recent[i];
        var it=e.input_tokens||0, ot=e.output_tokens||0, sv=Math.max(0,it-ot);
        var key=e.at_ms+":"+(e.cert||"");
        var sel=(selected&&selected.key===key)?" sel":"";
        var proof = e.verified
          ? '<span class="proof"><svg class="chk" viewBox="0 0 12 12" fill="none"><path d="M2.5 6.2 5 8.6 9.6 3.6" stroke="#26B6CE" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>LOSSLESS</span>'
          : '<span class="place">&middot;</span>';
        var place = e.inline ? '<span class="place">inline</span>' : '<span class="place resolve">resolve</span>';
        out+='<tr class="frow'+sel+'" data-key="'+esc(key)+'" data-cert="'+esc(e.cert||"")+'" data-atoms="'+(e.atoms||0)+'" data-transform="'+esc(e.transform||"")+'" data-surface="'+esc(e.surface||"")+'" data-in="'+it+'" data-out="'+ot+'" data-inline="'+(e.inline?1:0)+'" data-verified="'+(e.verified?1:0)+'">'
          +'<td class="when" title="'+esc(fmtDate(e.at_ms))+'">'+rel(e.at_ms)+'</td>'
          +'<td><span class="tag-surface">'+esc(e.surface||"")+'</span></td>'
          +'<td><span class="tag-xf">'+esc(e.transform||"")+'</span></td>'
          +'<td class="r"><span class="num">'+fmtTok(it)+'</span></td>'
          +'<td class="r"><span class="num">'+fmtTok(ot)+'</span></td>'
          +'<td class="r"><span class="saved">'+fmtTok(sv)+'</span></td>'
          +'<td>'+proof+'</td>'
          +'<td>'+place+'</td>'
          +'</tr>';
      }
      feed.innerHTML=out;
    }

    function renderCert(){
      var box=$("cert-body");
      if(!selected){ box.innerHTML='<div class="cert-sel" style="color:var(--faint)">Select a row below to inspect its blake3 certificate: the digest of the original and the proof the same atoms survive compression.</div>'; return; }
      var c=selected, atoms=Number(c.atoms||0).toLocaleString("en-US");
      // honesty: the equality claim only appears when the block is verified.
      var eq = c.verified
        ? '<div class="cert-eq"><span class="eqm">atoms in == atoms out</span><span class="sub">&middot; '+atoms+' atoms equal</span></div>'
        : '<div class="cert-eq"><span class="sub" style="color:var(--rust)">atoms in / out &middot; unverified</span></div>';
      var h='<div class="cert-sel">'+esc(c.surface)+' <span style="color:var(--faint)">&middot;</span> <b>'+esc(c.transform)+'</b> <span style="color:var(--faint)">&middot;</span> '+fmtTok(c.inTok)+' &rarr; '+fmtTok(c.outTok)+'</div>';
      h+=eq;
      h+='<div class="cert-digest"><span class="tag">blake3:</span> '+esc(c.cert||"(none recorded)")+'</div>';
      if(!c.inline) h+='<div class="cert-sel" style="color:var(--muted)">recoverable &middot; <span style="color:var(--blue2)">secondwind resolve &lt;marker&gt;</span></div>';
      box.innerHTML=h;
    }

    function render(d,first){
      var inTok=d.input_tokens||0, outTok=d.output_tokens||0, saved=Math.max(0,inTok-outTok);
      var blocks=d.blocks||0, verified=d.verified||0, inline=d.inline||0, off=d.offloaded||0, kept=d.kept||0;
      var pct = inTok?Math.round(saved/inTok*100):0;
      var keptPct = blocks?(verified/blocks)*100:100;
      var keptStr = keptPct===100?"100":keptPct.toFixed(1);

      // honesty: verify pill is verified/blocks, tinted rust when short
      var vc=$("verify-count"); vc.textContent=verified+"/"+blocks; vc.style.color=verified<blocks?"var(--rust)":"";

      // hero numbers count up to the data values on first paint; set directly after
      if(first && !reduce){
        count($("fig-raw"), inTok, 760, false);
        setTimeout(function(){
          root.classList.remove("sw-anim");
          count($("fig-sent"), outTok, 640, false);
          count($("fig-pct"), pct, 640, true);
          count($("fig-reclaimed"), saved, 640, false);
        }, 820);
      } else {
        setNum($("fig-raw"), inTok, false);
        setNum($("fig-sent"), outTok, false);
        setNum($("fig-pct"), pct, true);
        setNum($("fig-reclaimed"), saved, false);
      }

      // honesty: "information kept" is verified/blocks, never a hardcoded 100
      var hk=$("hl-keep");
      hk.textContent = keptStr+"% of the information kept · 0 dropped.";
      hk.style.color = keptPct===100?"var(--teal)":"var(--rust)";

      // the vessel: rebuilt only when geometry changes so the intro plays once
      var vkey=inTok+":"+outTok;
      if(first || vkey!==prevVessel){
        $("vessel").innerHTML = drawVessel(inTok,outTok,saved, first && !reduce);
        prevVessel=vkey;
      }

      // ledger
      $("ledger-note").textContent = fmtTok(saved);
      buildCols(d.saved_hour||0, d.saved_today||0, d.saved_week||0, d.saved_month||0, saved);
      drawSpark(d.curve, saved);

      // section note + blocks panel
      $("note-blocks").textContent = blocks;
      $("note-kept").textContent = kept;
      $("blk-count").textContent = blocks;
      $("seg-inline").style.flex = inline;
      $("seg-off").style.flex = off;
      $("lg-inline").textContent = inline;
      $("lg-off").textContent = off;
      var kv=$("kv-verified"); kv.textContent = verified+" / "+blocks; kv.style.color = verified<blocks?"var(--rust)":"var(--teal)";
      $("kv-kept").textContent = kept;

      buildTransforms(d.by_transform||{}, d.transform_saved||{});

      // fidelity certificate footer + selection card
      var cf=$("cert-foot-count"); cf.textContent = verified+"/"+blocks; cf.style.color = verified<blocks?"var(--rust)":"var(--teal)";
      renderCert();

      buildFeed(d.recent);

      // sources
      buildSrc(d.by_platform||{}, "plat-seg", "plat-keys", "plat-tot", first);
      buildSrc(d.by_surface||{}, "surf-seg", "surf-keys", "surf-tot", first);
    }

    $("feed").addEventListener("click", function(ev){
      var row=ev.target.closest(".frow"); if(!row) return;
      var key=row.getAttribute("data-key");
      if(selected && selected.key===key){ selected=null; }
      else {
        selected={ key:key, cert:row.getAttribute("data-cert"), atoms:row.getAttribute("data-atoms"),
          transform:row.getAttribute("data-transform"), surface:row.getAttribute("data-surface"),
          inTok:+row.getAttribute("data-in"), outTok:+row.getAttribute("data-out"),
          inline:row.getAttribute("data-inline")==="1", verified:row.getAttribute("data-verified")==="1" };
      }
      var sels=root.querySelectorAll(".frow.sel"); for(var i=0;i<sels.length;i++) sels[i].classList.remove("sel");
      if(selected) row.classList.add("sel");
      renderCert();
    });

    function tick(){
      fetch("/events.json?tz="+new Date().getTimezoneOffset()).then(function(r){ return r.json(); }).then(function(d){
        $("banner").style.display="none";
        $("main").classList.remove("disc");
        var blocks=d.blocks||0;
        if(blocks===0){ $("emptystate").style.display="block"; $("main").style.display="none"; setStatus("waiting"); prevBlocks=0; return; }
        $("emptystate").style.display="none"; $("main").style.display="block";
        if(blocks>prevBlocks) flashUntil=Date.now()+1600;
        prevBlocks=blocks;
        setStatus(Date.now()<flashUntil?"compressing":"live");
        if(firstData){ render(d,true); firstData=false; introUntil=reduce?0:Date.now()+1600; }
        else if(Date.now()>introUntil){ render(d,false); }
      }).catch(function(e){
        $("banner").style.display="flex";
        $("main").classList.add("disc");
        setStatus("disc");
      });
    }
    tick();
    setInterval(tick,1000);
  })();
  </script>
</section>
</body>
</html>
"##;
