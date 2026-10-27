//! A fixed, once-per-process CAD probe. Public reads never launch work or access user sessions.
use crate::{App, Worker, pages};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub(crate) struct Snapshot(Arc<RwLock<Option<Probe>>>);
struct Probe {
    report: Value,
    step: Vec<u8>,
    stl: Vec<u8>,
    image: Vec<u8>,
}
pub(crate) fn start(app: App) {
    tokio::spawn(async move {
        let _compute = app.compute.lock().await;
        let probe = match tokio::time::timeout(Duration::from_secs(90), run(&app)).await {
            Ok(Ok(probe)) => probe,
            result => Probe {
                report: json!({"status":"failed","message": match result { Ok(Err(message)) => message, _ => "Fixed CAD probe timed out".into() }}),
                step: vec![],
                stl: vec![],
                image: vec![],
            },
        };
        eprintln!("Fixed CAD startup probe: {}", probe.report);
        *app.diagnostics.0.write().await = Some(probe);
    });
}
async fn rpc(worker: &mut Worker, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let reply = worker
        .exchange(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .await
        .map_err(|_| "CAD communication failed")?
        .ok_or("Missing CAD reply")?;
    if reply.get("error").is_some() || reply.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
        return Err("CAD probe command failed".into());
    }
    reply.get("result").cloned().ok_or_else(|| "Missing CAD result".into())
}
async fn tool(worker: &mut Worker, id: u64, name: &str, arguments: Value) -> Result<Value, String> {
    rpc(worker, id, "tools/call", json!({"name":name,"arguments":arguments})).await
}
fn named_number(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == key).then(|| words.next()?.parse().ok()).flatten()
    })
}
async fn cpu_usec() -> Option<u64> {
    named_number(&tokio::fs::read_to_string("/sys/fs/cgroup/cpu.stat").await.ok()?, "usage_usec")
}
async fn run(app: &App) -> Result<Probe, String> {
    let started = Instant::now();
    let cpu_before = cpu_usec().await;
    let mut worker = Worker::start(app, "fixed-startup-probe".into()).await.map_err(|_| "CAD worker unavailable")?;
    rpc(
        &mut worker,
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixed-startup-probe","version":"1"}}),
    )
    .await?;
    worker.exchange(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.map_err(|_| "Initialize notification failed")?;
    let draw_started = Instant::now();
    for (index, (command, params)) in [
        ("sketch.create", json!({"plane":"XY"})),
        ("sketch.rectangle.two_point", json!({"p0":[0,0],"p1":[20,20]})),
        ("sketch.finish", json!({})),
        ("solid.extrude", json!({"distance":2})),
    ]
    .into_iter()
    .enumerate()
    {
        tool(&mut worker, index as u64 + 2, "execute", json!({"command":command,"params":params})).await?;
    }
    let draw_ms = draw_started.elapsed().as_secs_f64() * 1000.0;
    let measured = tool(&mut worker, 6, "measure", json!({})).await?;
    let measurement: Value = serde_json::from_str(measured.pointer("/content/0/text").and_then(Value::as_str).ok_or("Missing measurements")?)
        .map_err(|_| "Invalid measurements")?;
    let volume = measurement.pointer("/total/volume_mm3").and_then(Value::as_f64).ok_or("Missing volume")?;
    if (volume - 800.0).abs() > 0.001 {
        return Err("Incorrect square plate volume".into());
    }
    let mut files = Vec::new();
    for (id, name) in [(7, "square.step"), (8, "square.stl")] {
        tool(&mut worker, id, "export", json!({"path":name})).await?;
        let bytes = tokio::fs::read(worker.directory.path().join(name)).await.map_err(|_| "Export missing")?;
        if bytes.len() < 85 || bytes.len() > 1024 * 1024 {
            return Err("Unexpected export size".into());
        }
        files.push(bytes);
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let cpu_ms = cpu_before.zip(cpu_usec().await).and_then(|(a, b)| b.checked_sub(a)).map(|n| n as f64 / 1000.0);
    let worker_peak_kib = if let Some(pid) = worker.child.id() {
        tokio::fs::read_to_string(format!("/proc/{pid}/status")).await.ok().and_then(|s| named_number(&s, "VmHWM:"))
    } else {
        None
    };
    // Rendering is separate from the timed geometry/export probe and may be unavailable.
    let image = match tool(&mut worker, 9, "screenshot", json!({"view":"top","width":512,"height":512})).await {
        Ok(result) => result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|blocks| blocks.iter().find(|v| v.get("type").and_then(Value::as_str) == Some("image")))
            .and_then(|v| v.get("data"))
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 4 * 1024 * 1024)
            .and_then(|s| STANDARD.decode(s).ok())
            .unwrap_or_default(),
        Err(_) => vec![],
    };
    let _ = worker.child.kill().await;
    let revision = std::env::var("RENDER_GIT_COMMIT").ok().filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    Ok(Probe {
        report: json!({"status":"passed","revision":revision,"completed_at_unix":SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d|d.as_secs()),
            "model":"20 × 20 mm square, extruded 2 mm","volume_mm3":volume,"draw_ms":draw_ms,"total_ms":elapsed_ms,
            "container_cpu_ms":cpu_ms,"worker_peak_rss_kib":worker_peak_kib,"energy_wh":Value::Null,
            "measurement_scope":"One startup sample; total includes worker startup, sketch, extrusion, measurement and STEP/STL export. Image rendering excluded. CPU is the whole container delta and may include HTTP activity. RSS is the worker high-water mark. Electrical energy is not measured.",
            "step_bytes":files.first().map(Vec::len),"stl_bytes":files.get(1).map(Vec::len),"image_available":!image.is_empty()}),
        step: files.first().cloned().unwrap_or_default(),
        stl: files.get(1).cloned().unwrap_or_default(),
        image,
    })
}
async fn report(State(app): State<App>) -> axum::Json<Value> {
    axum::Json(app.diagnostics.0.read().await.as_ref().map(|p| p.report.clone()).unwrap_or_else(|| json!({"status":"running"})))
}
async fn artifact(app: App, kind: &str) -> Response {
    let guard = app.diagnostics.0.read().await;
    let Some(probe) = guard.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (data, mime) = match kind {
        "step" => (&probe.step, "application/octet-stream"),
        "stl" => (&probe.stl, "application/octet-stream"),
        _ => (&probe.image, "image/png"),
    };
    if data.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [
            ("content-type", mime),
            (
                "content-disposition",
                if kind == "png" {
                    "inline"
                } else if kind == "step" {
                    "attachment; filename=\"square.step\""
                } else {
                    "attachment; filename=\"square.stl\""
                },
            ),
        ],
        data.clone(),
    )
        .into_response()
}
async fn page(State(app): State<App>) -> Html<String> {
    let guard = app.diagnostics.0.read().await;
    let content = match guard.as_ref() {
        None => "<p>The fixed CAD test is running. Refresh shortly.</p>".into(),
        Some(probe) => {
            let preview = if probe.image.is_empty() {
                ""
            } else {
                "<img src=\"/diagnostics/square.png\" alt=\"Actual top view of the generated square plate\" width=\"512\" height=\"512\">"
            };
            let downloads = if probe.step.is_empty() {
                ""
            } else {
                "<p><a href=\"/diagnostics/square.step\">Download STEP</a> · <a href=\"/diagnostics/square.stl\">Download STL</a></p>"
            };
            format!("{preview}{downloads}<pre>{}</pre>", crate::escape(&serde_json::to_string_pretty(&probe.report).unwrap_or_default()))
        }
    };
    Html(pages::shell(
        "Cloud CAD test",
        &format!(
            "<span class=\"eyebrow\">ROBOTICS STUDIO</span><h1>Cloud CAD test</h1><p>A fixed public example created by the CAD engine on this server. Your private designs are separate.</p>{content}<p><a href=\"/\">Connect to ChatGPT</a> · <a href=\"/diagnostics/report\">JSON report</a></p>"
        ),
    ))
}
pub(crate) fn routes(app: App) -> Router {
    Router::new()
        .route("/diagnostics", get(page))
        .route("/diagnostics/report", get(report))
        .route("/diagnostics/square.step", get(|State(app): State<App>| async { artifact(app, "step").await }))
        .route("/diagnostics/square.stl", get(|State(app): State<App>| async { artifact(app, "stl").await }))
        .route("/diagnostics/square.png", get(|State(app): State<App>| async { artifact(app, "png").await }))
        .with_state(app)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    #[tokio::test]
    async fn public_reads_cannot_start_cad_or_accept_commands() {
        let app = crate::tests::app();
        let router = routes(app.clone());
        for _ in 0..2 {
            let reply =
                router.clone().oneshot(Request::builder().uri("/diagnostics/report?command=solid.box").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(reply.status(), StatusCode::OK);
            let data = to_bytes(reply.into_body(), 4096).await.unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&data).unwrap(), json!({"status":"running"}));
        }
        let reply = router.clone().oneshot(Request::builder().method("POST").uri("/diagnostics").body(Body::from("{}")).unwrap()).await.unwrap();
        assert_eq!(reply.status(), StatusCode::METHOD_NOT_ALLOWED);
        let reply = router.oneshot(Request::builder().uri("/diagnostics/square.step").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(reply.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(app.inner.lock().await.sessions.is_empty());
        assert!(app.diagnostics.0.read().await.is_none());
    }
    #[tokio::test]
    async fn fixed_probe_uses_real_cad() {
        if std::env::var_os("STUDIO_TEST_CLI").is_none() {
            return;
        }
        let probe = run(&crate::tests::app()).await.unwrap();
        assert_eq!(probe.report["status"], "passed");
        assert_eq!(probe.report["volume_mm3"], 800.0);
        assert!(probe.report["energy_wh"].is_null());
        assert!(probe.step.starts_with(b"ISO-10303-21"));
        assert!(probe.stl.len() > 84);
    }
}
