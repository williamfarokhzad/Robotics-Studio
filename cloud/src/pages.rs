//! Small, accessible account and connector pages. No external fonts or tracking.
use axum::{Router, response::IntoResponse, routing::get};
pub(crate) const CSS: &str = r#"
:root{color-scheme:dark;--bg:#0d1119;--panel:#171d28;--line:#2b3547;--ink:#edf3fa;--muted:#a8b4c7;--accent:#8ee3ce}*{box-sizing:border-box}body{margin:0;background:radial-gradient(ellipse at top right,#173c3c55,transparent 65%),var(--bg);color:var(--ink);font:15px/1.6 system-ui,sans-serif;min-height:100vh}main{width:min(100% - 32px,540px);margin:9vh auto;padding:32px;border:1px solid var(--line);border-radius:24px;background:var(--panel);box-shadow:0 24px 90px #0004}h1{font-size:32px;line-height:1.15;letter-spacing:-1px;margin:20px 0}h2{font-size:18px;margin:24px 0 8px}p{margin:14px 0}.eyebrow{font-size:11px;letter-spacing:2px;color:var(--accent);font-weight:700}.muted{color:var(--muted);font-size:13px}.primary,.secondary{display:flex;align-items:center;justify-content:center;gap:10px;padding:13px 18px;border-radius:12px;text-decoration:none;font:600 14px system-ui;cursor:pointer;width:100%;margin:12px 0}.primary{background:#f1f6fb;color:#15202e;border:1px solid transparent}.primary:disabled{opacity:.45;cursor:not-allowed}.secondary{color:var(--ink);background:transparent;border:1px solid var(--line)}a{color:var(--accent)}button:focus-visible,a:focus-visible,input:focus-visible{outline:3px solid var(--accent);outline-offset:3px}label{display:block;font-weight:600;margin:20px 0 7px}input{display:block;width:100%;padding:13px;border:1px solid var(--line);border-radius:10px;background:var(--bg);color:var(--ink);font:inherit;margin-top:8px}code{overflow-wrap:anywhere;font-size:12px}.endpoint{padding:12px 16px;background:var(--bg);border:1px solid var(--line);border-radius:12px}.badge{display:inline-block;border:1px solid #386251;color:var(--accent);padding:3px 9px;border-radius:20px;font-size:11px}ol{padding-left:22px;color:var(--muted)}.logo{width:26px;height:26px}details{margin:18px 0;color:var(--muted)}summary{cursor:pointer;font-size:13px}#account-name{font-size:20px;font-weight:600}#account-email{color:var(--muted)}@media(max-width:480px){main{padding:24px;margin:24px auto}h1{font-size:28px}}@media(prefers-reduced-motion:no-preference){main{animation:appear .18s ease-out}@keyframes appear{from{transform:translateY(6px);opacity:0}to{transform:translateY(0);opacity:1}}}
"#;
pub(crate) const JS: &str = r#"
'use strict';
(async()=>{const account=document.querySelector('#account');if(!account)return;
try{const r=await fetch('/auth/session',{credentials:'same-origin',cache:'no-store'});if(!r.ok)return;const s=await r.json();if(!s.authenticated)return;
account.replaceChildren();const name=document.createElement('p');name.id='account-name';name.textContent=s.account.name;account.append(name);
if(s.account.email){const email=document.createElement('p');email.id='account-email';email.textContent=s.account.email;account.append(email)}
const tag=document.createElement('span');tag.className='badge';tag.textContent='Signed in with ChatGPT';account.append(tag);
const out=document.createElement('button');out.className='secondary';out.textContent='Sign out';out.onclick=async()=>{out.disabled=true;try{const r=await fetch('/auth/logout',{method:'POST',credentials:'same-origin',headers:{'x-studio-csrf':s.csrf}});if(r.ok)location.reload();else{out.disabled=false;out.textContent='Try signing out again'}}catch{out.disabled=false;out.textContent='Connection lost. Try again'}};account.append(out);
}catch{const p=document.createElement('p');p.className='muted';p.textContent='Account service is unavailable. Your local designs are still available.';account.append(p)}})();
const copy=document.querySelector('#copy-endpoint');if(copy)copy.onclick=async()=>{try{await navigator.clipboard.writeText(document.querySelector('#endpoint').textContent);copy.textContent='Copied';setTimeout(()=>copy.textContent='Copy MCP address',1800)}catch{copy.textContent='Select and copy the address above'}};
"#;
pub(crate) fn shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title} | Robotics Studio</title><link rel=\"stylesheet\" href=\"/auth/style.css\"><script defer src=\"/auth/ui.js\"></script></head><body><main>{body}</main></body></html>"
    )
}
pub(crate) fn connect(base: &str) -> String {
    let endpoint = crate::escape(&format!("{base}/mcp"));
    shell(
        "Connect to ChatGPT",
        &format!(
            r#"<span class="eyebrow">ROBOTICS STUDIO</span><h1>Your ideas.<br>Your next design.</h1><p>Create, edit and export CAD models through ChatGPT.</p><span class="badge">Private owner preview</span><h2>Connect your Studio</h2><ol><li>Open Plugins in ChatGPT and add a custom MCP server.</li><li>Paste this address and select OAuth.</li><li>Approve access with your Studio owner password.</li></ol><div class="endpoint"><code id="endpoint">{endpoint}</code></div><button id="copy-endpoint" class="primary">Copy MCP address</button><a class="secondary" href="https://chatgpt.com/plugins" target="_blank" rel="noopener noreferrer">Open ChatGPT Plugins</a><p class="muted">CAD sessions and downloads expire after 30 minutes of inactivity or a server restart. Download the files you want to keep.</p><a href="https://robotics-studio.onrender.com/account">Your ChatGPT account</a> · <a href="https://robotics-studio.onrender.com/">Open Studio</a>"#
        ),
    )
}
pub(crate) fn consent(ticket: &str, client: &str) -> String {
    consent_error(ticket, client, "")
}
pub(crate) fn consent_error(ticket: &str, client: &str, error: &str) -> String {
    let notice = if error.is_empty() { String::new() } else { format!("<p role=\"alert\">{}</p>", crate::escape(error)) };
    shell(
        "Allow CAD access",
        &format!(
            r#"<span class="eyebrow">ROBOTICS STUDIO × CHATGPT</span><h1>Bring your designs<br>into the conversation.</h1><p>Allow ChatGPT to create, edit, measure and export models in temporary cloud sessions.</p>{notice}<form method="post" action="/oauth/authorize"><input type="hidden" name="ticket" value="{ticket}"><label for="owner">Studio owner password<input id="owner" type="password" name="password" required autocomplete="current-password" placeholder="Enter your Studio password"></label><button class="primary" type="submit">Allow CAD access</button></form><p class="muted">Use your Studio owner password, not your ChatGPT password. Find it in Render → Environment → STUDIO_ADMIN_PASSWORD.</p><details><summary>Connection details</summary><code>{client}</code></details><p class="muted">This sign-in form expires after 5 minutes. If it expires, start Connect again in ChatGPT.</p><p class="muted">Download your work. Sessions expire after 30 minutes of inactivity or a server restart.</p><a href="https://chatgpt.com/plugins">Cancel and return to ChatGPT</a>"#
        ),
    )
}
pub(crate) fn routes() -> Router {
    Router::new()
        .route("/auth/style.css", get(|| async { ([("content-type", "text/css; charset=utf-8")], CSS).into_response() }))
        .route("/auth/ui.js", get(|| async { ([("content-type", "text/javascript; charset=utf-8")], JS).into_response() }))
        .route(
            "/auth/logo-black.svg",
            get(|| async { ([("content-type", "image/svg+xml")], include_str!("../../assets/branding/chatgpt/logo-black.svg")).into_response() }),
        )
        .route(
            "/auth/logo.svg",
            get(|| async { ([("content-type", "image/svg+xml")], include_str!("../../assets/branding/chatgpt/logo-white.svg")).into_response() }),
        )
}
