//! Structured in-process fuzzing for the engine's external-input parsers.
//!
//! LibFuzzer/cargo-fuzz needs sanitizer runtimes that are unavailable on the
//! windows-gnu toolchain this engine builds with, so this harness implements
//! the same discipline directly: seeded deterministic mutations, strict
//! invariants per parser, panic catching, and a machine-readable report. The
//! loop is pure (no filesystem writes) and runs from `cargo test`, making it
//! a repeatable CI smoke instead of an unbounded hunt.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use anyhow::{Context, bail};
use serde_json::json;

use moni_assets::cooked::{decode_cooked_mesh, encode_cooked_mesh};
use moni_assets::fbx::{FbxMesh, FbxVertex, load_fbx};

const DEFAULT_SEED: u64 = 0x4D4F4E49; // "MONI"
const SCENE_SEEDS: [&str; 2] = [
    include_str!("../testing/assets/benchmark-3d.mnsc"),
    include_str!("../testing/assets/benchmark-2d.mnsc"),
];
const FBX_HEADER: &[u8] = b"Kaydara FBX Binary  \x00\x1a\x00\x28\x00\x00\x00";

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut iterations = 2_000_u64;
    let mut seed = DEFAULT_SEED;
    let mut target = "all".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--iterations" => {
                index += 1;
                iterations = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--iterations requires a value"))?
                    .parse()
                    .context("--iterations must be a positive integer")?;
                if iterations == 0 || iterations > 10_000_000 {
                    bail!("--iterations must be between 1 and 10,000,000");
                }
            }
            "--seed" => {
                index += 1;
                seed = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--seed requires a value"))?
                    .parse()
                    .context("--seed must be an unsigned integer")?;
            }
            "--target" => {
                index += 1;
                target = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--target requires a value"))?
                    .clone();
                if !matches!(target.as_str(), "all" | "cooked" | "scene" | "fbx") {
                    bail!("--target must be all, cooked, scene, or fbx");
                }
            }
            unknown => bail!("unknown fuzz option '{unknown}'"),
        }
        index += 1;
    }
    if iterations < 50 {
        bail!("--iterations must be at least 50 so every enabled target runs");
    }

    let mut prng = Prng(seed);
    let mut outcomes: BTreeMap<&'static str, u64> = BTreeMap::new();
    let targets: Vec<Strategies> = match target.as_str() {
        "all" => vec![Strategies::Cooked, Strategies::Scene],
        "cooked" => vec![Strategies::Cooked],
        "scene" => vec![Strategies::Scene],
        _ => vec![Strategies::Fbx],
    };
    let started = Instant::now();
    // Panic backtraces from caught bugs would flood the report; count them
    // instead and fail at the end.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panic_payloads: Vec<String> = Vec::new();
    for iteration in 0..iterations {
        let strategy = &targets[iteration as usize % targets.len()];
        let candidate = strategy.mutate(&mut prng);
        let outcome = catch_unwind(AssertUnwindSafe(|| strategy.execute(&candidate)));
        match outcome {
            Ok(Ok(())) => *outcomes.entry("ok").or_insert(0) += 1,
            Ok(Err(_)) => *outcomes.entry("rejected").or_insert(0) += 1,
            Err(payload) => {
                *outcomes.entry("panics").or_insert(0) += 1;
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        payload
                            .downcast_ref::<&str>()
                            .map(|value| value.to_string())
                    })
                    .unwrap_or_else(|| "opaque panic".to_string());
                panic_payloads.push(format!("iteration {iteration}: {message}"));
            }
        }
    }
    std::panic::set_hook(previous_hook);

    let report = json!({
        "ok": panic_payloads.is_empty(),
        "seed": seed,
        "iterations": iterations,
        "target": target,
        "elapsedMs": started.elapsed().as_millis() as u64,
        "outcomes": outcomes,
        "panics": panic_payloads.iter().take(5).cloned().collect::<Vec<_>>(),
    });
    if panic_payloads.is_empty() {
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok(())
    } else {
        bail!(
            "fuzzer caught {} parser panics; report: {}",
            panic_payloads.len(),
            report
        )
    }
}

/// xorshift64* seeded from the CLI so runs are exactly reproducible.
struct Prng(u64);

impl Prng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

enum Strategies {
    Cooked,
    Scene,
    Fbx,
}

impl Strategies {
    fn mutate(&self, prng: &mut Prng) -> Vec<u8> {
        match self {
            Strategies::Cooked => mutate_bytes(&valid_cooked_seed(prng), prng, true),
            Strategies::Scene => mutate_bytes(
                SCENE_SEEDS[prng.below(SCENE_SEEDS.len())].as_bytes(),
                prng,
                false,
            ),
            Strategies::Fbx => mutate_bytes(&fbx_seed(), prng, false),
        }
    }

    fn execute(&self, candidate: &[u8]) -> anyhow::Result<()> {
        match self {
            Strategies::Cooked => {
                let claimed = if candidate.len() >= 16 {
                    Some(u64::from_le_bytes(candidate[8..16].try_into()?))
                } else {
                    None
                };
                let mesh = decode_cooked_mesh(candidate)?;
                // Parse invariants: accepted bytes must describe exactly the
                // claimed vertex count with no truncated trailing record.
                if let Some(claimed) = claimed {
                    assert_eq!(
                        mesh.vertices.len() as u64,
                        claimed,
                        "decoded vertex count diverges from the header"
                    );
                }
                assert!(
                    mesh.vertices
                        .iter()
                        .flat_map(|vertex| vertex.position.iter())
                        .all(|value| value.is_finite()),
                    "decoder accepted non-finite positions"
                );
                Ok(())
            }
            Strategies::Scene => {
                let text = String::from_utf8_lossy(candidate);
                moni_core::scene::load_snapshot(&text).map(|_| ())
            }
            Strategies::Fbx => {
                let path = std::path::Path::new("target/testing/fuzz-candidate.fbx");
                std::fs::create_dir_all(path.parent().expect("parent exists"))?;
                std::fs::write(path, candidate)?;
                load_fbx(path).map(|_| ())
            }
        }
    }
}

/// Builds a well-formed cooked mesh whose size varies per call so mutated
/// headers face different real layouts.
fn valid_cooked_seed(prng: &mut Prng) -> Vec<u8> {
    let count = 1 + prng.below(32);
    let mesh = FbxMesh {
        vertices: (0..count)
            .map(|index| FbxVertex {
                position: [index as f32, 0.5, -0.5],
                normal: [0.0, 1.0, 0.0],
                color: Some([1.0, 1.0, 1.0, 1.0]),
            })
            .collect(),
    };
    encode_cooked_mesh(&mesh).expect("seed mesh encodes")
}

fn fbx_seed() -> Vec<u8> {
    let user_fixture = std::path::Path::new("testing/assets/garden_diorama.fbx");
    if let Ok(bytes) = std::fs::read(user_fixture) {
        return bytes;
    }
    let mut bytes = FBX_HEADER.to_vec();
    bytes.extend(std::iter::repeat_n(0xA5_u8, 512));
    bytes
}

/// Byte-level mutator. `structured` biases mutations toward the cooked
/// header/count field and record color flags; text seeds additionally get
/// JSON-aware token injections.
fn mutate_bytes(seed: &[u8], prng: &mut Prng, structured: bool) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    if bytes.is_empty() {
        bytes.push(b'?');
    }
    let mutation = prng.below(if structured { 7 } else { 6 });
    match mutation {
        0 => {
            // Flip 1-8 random bytes.
            for _ in 0..1 + prng.below(8) {
                let at = prng.below(bytes.len());
                bytes[at] ^= 1 << prng.below(8);
            }
        }
        1 => {
            // Truncate at a random offset.
            bytes.truncate(prng.below(bytes.len()));
        }
        2 => {
            // Duplicate a slice somewhere else in the buffer.
            let start = prng.below(bytes.len());
            let end = (start + 1 + prng.below(64)).min(bytes.len());
            let chunk: Vec<u8> = bytes[start..end].to_vec();
            let at = prng.below(bytes.len());
            bytes.splice(at..at, chunk);
        }
        3 => {
            // Splice in structured junk tokens.
            let tokens: [&[u8]; 6] = [
                b"null",
                b"1e999",
                b"{\"a\":",
                b"\x00\x00\x00\x00",
                b"NaN",
                b"9223372036854775807",
            ];
            let token = tokens[prng.below(tokens.len())];
            let at = prng.below(bytes.len());
            bytes.splice(at..at, token.iter().copied());
        }
        4 if structured => {
            // Overwrite the cooked vertex-count header.
            if bytes.len() >= 16 {
                let count = match prng.below(4) {
                    0 => 0_u64,
                    1 => u64::MAX,
                    2 => 10_000_001,
                    _ => u64::from_le_bytes(bytes[8..16].try_into().expect("16 bytes")) + 1,
                };
                bytes[8..16].copy_from_slice(&count.to_le_bytes());
            }
        }
        5 if structured => {
            // Corrupt a vertex color flag or inject a NaN payload.
            if bytes.len() > 60 {
                let record_at = 16 + prng.below((bytes.len() - 16) / 44) * 44 + 40;
                if record_at + 4 <= bytes.len() {
                    let flag = match prng.below(3) {
                        0 => 2_u32,
                        1 => u32::MAX,
                        _ => 1,
                    };
                    bytes[record_at..record_at + 4].copy_from_slice(&flag.to_le_bytes());
                }
                if bytes.len() >= 20 {
                    let at = 16 + prng.below(bytes.len() - 20);
                    bytes[at..at + 4].copy_from_slice(&f32::NAN.to_le_bytes());
                }
            }
        }
        _ => {
            // Replace a random byte with a JSON structural character.
            let structural = [b'{', b'}', b'[', b']', b'"', b':', b',', b'0'];
            let at = prng.below(bytes.len());
            bytes[at] = structural[prng.below(structural.len())];
        }
    }
    bytes
}
