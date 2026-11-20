use std::fs;
use std::time::Instant;

use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::json;

use moni_core::engine::{Engine, Event};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BenchmarkSummary {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub format_version: u32,
    pub engine_version: String,
    pub mode: String,
    pub metric: String,
    pub scene: String,
    pub os: String,
    pub arch: String,
    pub total_frames: u64,
    pub warmup_frames: u64,
    pub sample_count: usize,
    pub summary: BenchmarkSummary,
    pub samples_ms: Vec<f64>,
}

impl BenchmarkReport {
    pub fn new(
        mode: &str,
        metric: &str,
        scene: &str,
        total_frames: u64,
        warmup_frames: u64,
        samples_ms: Vec<f64>,
    ) -> anyhow::Result<Self> {
        if samples_ms.is_empty() {
            bail!("benchmark has no measured samples");
        }
        let summary = summarize(&samples_ms);
        Ok(Self {
            format_version: 1,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            mode: mode.to_string(),
            metric: metric.to_string(),
            scene: scene.to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            total_frames,
            warmup_frames,
            sample_count: samples_ms.len(),
            summary,
            samples_ms,
        })
    }
}

pub fn write_report(report: &BenchmarkReport, raw_path: &str) -> anyhow::Result<()> {
    let path = moni_core::workspace_path(raw_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(report)?)?;
    Ok(())
}

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        bail!("benchmark requires a release build; use cargo run --release -- benchmark");
    }

    let mut scene = "2d";
    let mut script = None;
    let mut frames = 600_u64;
    let mut warmup_frames = 60_u64;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--scene" => {
                index += 1;
                scene = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scene requires 2d or 3d"))?;
                if scene != "2d" && scene != "3d" {
                    bail!("--scene must be 2d or 3d");
                }
            }
            "--script" => {
                index += 1;
                script = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--script requires a path"))?
                        .clone(),
                );
            }
            "--frames" => {
                index += 1;
                frames = parse_positive(args.get(index), "--frames")?;
            }
            "--warmup" => {
                index += 1;
                warmup_frames = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--warmup requires a value"))?
                    .parse()
                    .context("--warmup must be a non-negative integer")?;
            }
            "--output" => {
                index += 1;
                output = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--output requires a path"))?
                        .clone(),
                );
            }
            unknown => bail!("unknown benchmark option '{unknown}'"),
        }
        index += 1;
    }
    if warmup_frames >= frames {
        bail!("--warmup must be smaller than --frames");
    }

    let source = read_script(script.as_deref(), scene)?;
    let mut engine = create_engine(scene, source)?;
    engine.set_script_runtime(Box::new(moni_script::QuickJsScriptRuntime::new()));
    let mut samples = Vec::with_capacity((frames - warmup_frames) as usize);
    for frame in 0..frames {
        let started = Instant::now();
        engine.step(1)?;
        let events = engine.take_events();
        let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
        if events
            .iter()
            .any(|event| matches!(event, Event::ScriptError { .. }))
        {
            bail!("script failed during {scene} benchmark at frame {frame}");
        }
        if frame >= warmup_frames {
            samples.push(elapsed);
        }
    }

    let output = output.unwrap_or_else(|| format!("target/benchmarks/simulation-{scene}.json"));
    let report = BenchmarkReport::new(
        "simulation",
        "engine_step_and_event_drain",
        scene,
        frames,
        warmup_frames,
        samples,
    )?;
    write_report(&report, &output)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "mode": report.mode,
            "scene": report.scene,
            "output": output,
            "summary": report.summary,
        }))?
    );
    Ok(())
}

fn parse_positive(value: Option<&String>, option: &str) -> anyhow::Result<u64> {
    let value = value
        .ok_or_else(|| anyhow::anyhow!("{option} requires a value"))?
        .parse()
        .with_context(|| format!("{option} must be a positive integer"))?;
    if value == 0 {
        bail!("{option} must be at least 1");
    }
    Ok(value)
}

fn read_script(script: Option<&str>, _scene: &str) -> anyhow::Result<String> {
    let default_script = "examples/dist/model-orbit.js";
    let raw_path = script.unwrap_or(default_script);
    let path = moni_core::workspace_path(raw_path)?;
    fs::read_to_string(&path)
        .with_context(|| format!("could not read {}; run npm run build first", path.display()))
}

fn create_engine(scene: &str, source: String) -> anyhow::Result<Engine> {
    if scene == "3d" {
        crate::demo_engine_3d(source)
    } else {
        crate::demo_engine(source)
    }
}

fn summarize(samples: &[f64]) -> BenchmarkSummary {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    BenchmarkSummary {
        mean_ms: samples.iter().sum::<f64>() / samples.len() as f64,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        p99_ms: percentile(&sorted, 0.99),
        max_ms: sorted[sorted.len() - 1],
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let index = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}
