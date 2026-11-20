const { spawnSync } = require("child_process");
const fs = require("fs");
const path = require("path");

function option(name, fallback) {
  const index = process.argv.indexOf(name);
  return index === -1 ? fallback : process.argv[index + 1];
}

const label = option("--label", "latest");
const frames = Number(option("--frames", "600"));
const warmup = Number(option("--warmup", "120"));
if (!/^[a-z0-9_-]+$/i.test(label)) {
  throw new Error("--label may contain only letters, numbers, underscores, and hyphens");
}
if (!Number.isInteger(frames) || frames <= 0 || !Number.isInteger(warmup) || warmup < 0 || warmup >= frames) {
  throw new Error("--frames must be positive and --warmup must be non-negative and smaller");
}

function run(command, args, capture = false) {
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    encoding: "utf8",
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with ${result.status}`);
  if (capture) {
    process.stdout.write(result.stdout);
    return JSON.parse(result.stdout);
  }
  return null;
}

run("cargo", ["build", "--release"]);
const executable = path.join("target", "release", `moni-engine${process.platform === "win32" ? ".exe" : ""}`);
const fixture = path.join("testing", "assets", "benchmark.mnec");
if (!fs.existsSync(fixture)) {
  run(process.execPath, [path.join("testing", "generate-fixture.cjs")]);
}
const directory = path.join("target", "benchmarks", label);
fs.mkdirSync(directory, { recursive: true });
const results = [];
for (const scene of ["2d", "3d"]) {
  for (const mode of ["simulation", "render"]) {
    const output = path.join(directory, `${mode}-${scene}.json`);
    const args = mode === "simulation"
      ? ["benchmark", "--scene", scene]
      : ["client", "--benchmark", "--scene", scene];
    args.push("--frames", String(frames), "--warmup", String(warmup), "--output", output);
    const result = run(executable, args, true);
    results.push({
      mode,
      scene,
      output,
      summary: mode === "simulation" ? result.summary : result.benchmark.summary,
      preparation: mode === "render" ? result.benchmark.preparation : undefined,
      gpu: mode === "render" ? result.benchmark.gpu : undefined,
    });
  }
}
const summaryPath = path.join(directory, "summary.json");
fs.writeFileSync(summaryPath, `${JSON.stringify({
  formatVersion: 1,
  label,
  frames,
  warmup,
  results,
}, null, 2)}\n`);
console.log(JSON.stringify({ ok: true, summary: summaryPath }));
