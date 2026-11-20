// End-to-end acceptance test for the minimal-game project template.
// Covers the agreed contract: `moni new` creates an independent repository,
// `moni doctor` and `moni profile dump` work inside it, `moni run` installs
// dependencies, builds the TypeScript entry, runs the project in a window for
// a few frames, and the scene can be saved back out as a v2 .mnsc — all without
// the generated game repository referencing engine source paths.
//
// Usage: node testing/template-e2e.cjs [--keep]
//   MONI_ENGINE  path to the engine binary (default target/debug/moni-engine*)
//   MONI_E2E_SKIP_RUN=1  skip the windowed `moni run` step (e.g. on headless CI)
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const executable = path.resolve(process.env.MONI_ENGINE
  || path.join("target", "debug", `moni-engine${process.platform === "win32" ? ".exe" : ""}`));
if (!fs.existsSync(executable)) {
  throw new Error(`engine binary not found at ${executable}; run cargo build first`);
}
const keep = process.argv.includes("--keep");
const skipRun = process.env.MONI_E2E_SKIP_RUN === "1";
const results = [];

const projectsRoot = path.join("target", "testing", "e2e");
fs.mkdirSync(projectsRoot, { recursive: true });
const project = path.join(projectsRoot, `template-${Date.now()}`);

function run(args, options = {}, expectedStatus = 0) {
  const result = spawnSync(executable, args, {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: options.timeout || 120000,
    cwd: options.cwd || process.cwd(),
  });
  if (result.error) throw result.error;
  if (result.status !== expectedStatus) {
    throw new Error(
      `${args.join(" ")} exited with ${result.status}, expected ${expectedStatus}\n${result.stdout}\n${result.stderr}`
    );
  }
  return `${result.stdout}\n${result.stderr}`;
}

function jsonOutput(output) {
  // Engine results are pretty-printed JSON on stdout; wrapper commands (npm)
  // may print their own output first, so scan brace positions back to front.
  // Not every command includes an `ok` field (e.g. profile dump).
  const starts = [];
  let index = output.indexOf("{");
  while (index !== -1) {
    starts.push(index);
    index = output.indexOf("{", index + 1);
  }
  for (const start of starts.reverse()) {
    try {
      const value = JSON.parse(output.slice(start));
      if (value && typeof value === "object" && !Array.isArray(value)) return value;
    } catch {
      // try the next earlier brace
    }
  }
  throw new Error(`no JSON object found in output:\n${output}`);
}

function verify(name, fn) {
  fn();
  results.push({ name, ok: true });
  console.log(`ok - ${name}`);
}

function collectFiles(root, out) {
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    if (entry.name === ".git" || entry.name === "node_modules" || entry.name === "build") continue;
    const full = path.join(root, entry.name);
    if (entry.isDirectory()) collectFiles(full, out);
    else out.push(full);
  }
  return out;
}

try {
  verify("new-creates-project", () => {
    const output = jsonOutput(run(["new", path.resolve(project)]));
    if (!fs.existsSync(path.join(project, "game.mngp"))) throw new Error("game.mngp missing");
    if (!fs.existsSync(path.join(project, "moni.lock"))) throw new Error("moni.lock missing");
    if (!fs.existsSync(path.join(project, "scenes", "main.mnsc"))) throw new Error("scene missing");
    if (!fs.existsSync(path.join(project, "scripts", "main.ts"))) throw new Error("script missing");
    if (!fs.existsSync(path.join(project, ".git"))) throw new Error("git repository not initialized");
    if (output.gitInitialized !== true) throw new Error("moni new did not report git init");
  });

  verify("generated-project-is-independent", () => {
    const files = collectFiles(project, []);
    if (files.length === 0) throw new Error("no template files copied");
    for (const file of files) {
      const contents = fs.readFileSync(file, "utf8");
      if (contents.includes("CARGO_MANIFEST_DIR")) {
        throw new Error(`${file} references CARGO_MANIFEST_DIR`);
      }
      // Drive paths such as E:\repo or E:/repo; URL schemes like https:// are
      // legitimate in package-lock.json and skipped by the double-slash guard.
      if (/[A-Za-z]:[\\/](?![\\/])/.test(contents)) {
        throw new Error(`${file} contains an absolute Windows drive path`);
      }
    }
  });

  verify("generated-project-parses-and-locks", () => {
    const manifest = JSON.parse(fs.readFileSync(path.join(project, "game.mngp"), "utf8"));
    if (manifest.name !== path.basename(project)) {
      throw new Error(`project name mismatch: ${manifest.name}`);
    }
    const lock = JSON.parse(fs.readFileSync(path.join(project, "moni.lock"), "utf8"));
    if (lock.format !== "moni-project-lock") throw new Error("unexpected lock format");
    if (!lock.plugins["core.render"] || !lock.plugins["core.simulation"]) {
      throw new Error("lock does not contain default profile plugins");
    }
  });

  verify("doctor-passes-in-project", () => {
    const output = jsonOutput(run(["doctor"], { cwd: project }));
    if (output.permissionChanges.added && Object.keys(output.permissionChanges.added).length !== 0) {
      throw new Error("unexpected permission drift in fresh project");
    }
  });

  verify("profile-dump-uses-project-manifest", () => {
    const output = jsonOutput(run(["profile", "dump"], { cwd: project }));
    const ids = output.plugins.map((plugin) => plugin.manifest.id);
    if (!ids.includes("core.simulation")) throw new Error(`unexpected plugin tree: ${ids}`);
    if (output.name !== "default") throw new Error(`unexpected profile ${output.name}`);
  });

  if (!skipRun) {
    verify("run-installs-builds-and-runs-frames", () => {
      const output = run(["run", "--frames", "3"], { cwd: project, timeout: 600000 });
      const result = jsonOutput(output);
      if (result.frames !== 3) throw new Error(`expected 3 frames, got ${result.frames}`);
      if (!fs.existsSync(path.join(project, "build", "main.js"))) {
        throw new Error("tsc build output build/main.js missing");
      }
      if (!fs.existsSync(path.join(project, "node_modules", "typescript"))) {
        throw new Error("npm ci did not install typescript");
      }
    });
  } else {
    console.log("skip - run-installs-builds-and-runs-frames (MONI_E2E_SKIP_RUN=1)");
  }

  verify("scene-saves-as-v2-mnsc", () => {
    const outputPath = path.join(project, "target", "e2e-saved.mnsc");
    jsonOutput(run(["scene", "scenes/main.mnsc", "--output", "target/e2e-saved.mnsc"], { cwd: project }));
    const saved = JSON.parse(fs.readFileSync(outputPath, "utf8"));
    if (saved.version !== 2) throw new Error(`saved scene version ${saved.version}, expected 2`);
    if (!saved.entities[0].components["core.transform"]) {
      throw new Error("saved scene lost the transform component");
    }
  });

  verify("doctor-passes-after-run", () => {
    jsonOutput(run(["doctor"], { cwd: project }));
  });

  console.log(JSON.stringify({ ok: true, project: path.basename(project), results }, null, 2));
  if (!keep) fs.rmSync(project, { recursive: true, force: true });
} catch (error) {
  console.error(JSON.stringify({ ok: false, project, error: String(error.message || error) }, null, 2));
  console.error(`project kept for inspection at ${project}`);
  process.exit(1);
}
