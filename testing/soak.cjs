const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

const executable = process.env.MONI_ENGINE || path.join("target", "debug", `moni-engine${process.platform === "win32" ? ".exe" : ""}`);
const frames = Number(process.env.MONI_SOAK_FRAMES || "1200");
const child = spawn(executable, ["editor", "--frames", String(frames)], {
  stdio: ["ignore", "pipe", "pipe"],
});
let output = "";
let errors = "";
const memory = [];
const started = Date.now();
child.stdout.on("data", chunk => output += chunk);
child.stderr.on("data", chunk => errors += chunk);

function sampleMemory() {
  if (process.platform !== "win32" || !child.pid || child.exitCode !== null) return;
  const result = spawnSync("powershell.exe", [
    "-NoProfile",
    "-Command",
    `(Get-Process -Id ${child.pid}).WorkingSet64`,
  ], { encoding: "utf8", timeout: 1500 });
  const bytes = Number(result.stdout.trim());
  if (Number.isFinite(bytes) && bytes > 0) memory.push({ elapsedMs: Date.now() - started, bytes });
}
const timer = setInterval(sampleMemory, 250);
child.on("close", status => {
  clearInterval(timer);
  sampleMemory();
  const steady = memory.slice(3);
  const minimum = steady.length ? Math.min(...steady.map(sample => sample.bytes)) : 0;
  const last = memory.length ? memory[memory.length - 1].bytes : 0;
  const growthBytes = minimum && last ? last - minimum : 0;
  const growthPercent = minimum ? growthBytes / minimum * 100 : 0;
  const report = {
    ok: status === 0 && (memory.length < 4 || growthBytes <= 16 * 1024 * 1024 || growthPercent <= 25),
    frames,
    status,
    durationMs: Date.now() - started,
    memorySupported: memory.length > 0,
    samples: memory.length,
    minWorkingSetBytes: minimum,
    lastWorkingSetBytes: last,
    growthBytes,
    growthPercent,
    stdout: output.trim(),
    stderr: errors.trim(),
  };
  fs.mkdirSync(path.join("target", "testing"), { recursive: true });
  fs.writeFileSync(path.join("target", "testing", "soak-report.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify({ ok: report.ok, report: "target/testing/soak-report.json", memorySupported: report.memorySupported, growthPercent }));
  if (!report.ok) process.exitCode = 1;
});
