const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const executable = process.env.MONI_ENGINE || path.join("target", "debug", `moni-engine${process.platform === "win32" ? ".exe" : ""}`);
const results = [];

function run(args, expectedStatus = 0) {
  const result = spawnSync(executable, args, {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 60000,
  });
  if (result.error) throw result.error;
  if (result.status !== expectedStatus) {
    throw new Error(`${args.join(" ")} exited with ${result.status}, expected ${expectedStatus}\n${result.stdout}\n${result.stderr}`);
  }
  return `${result.stdout}\n${result.stderr}`;
}

function json(args) {
  return JSON.parse(run(args));
}

function verify(name, fn) {
  fn();
  results.push({ name, ok: true });
}

fs.mkdirSync(path.join("target", "testing"), { recursive: true });

verify("capabilities-and-static-api", () => {
  const capabilities = json(["capabilities"]);
  if (capabilities.profiles.pluginApiVersion !== 1 || capabilities.host.maxSystemTasks !== 1024 || capabilities.world.assets.queueCapacity !== 64 || !capabilities.components["core.model"]) {
    throw new Error("capability contract mismatch");
  }
});

verify("profile-dependency-order", () => {
  const profile = json(["profile", "dump", "--profile", "default"]);
  const ids = profile.plugins.map(plugin => plugin.manifest.id);
  if (ids.join(",") !== "core.fbx,core.render,core.simulation") throw new Error("unexpected default profile");
});

verify("headless-and-minimal", () => {
  for (const profile of ["headless", "minimal"]) {
    const result = json(["headless", "--profile", profile]);
    if (result.gpuInitialized || result.quickjsInitialized || result.fbxLoaded) throw new Error(`${profile} initialized a forbidden provider`);
  }
});

verify("permission-denial", () => {
  // --world keeps this case independent of the example script build output.
  const output = run(["client", "--revoke", "core.render=gpu.create", "--world", "testing/assets/benchmark-3d.mnsc", "--frames", "1"], 1);
  if (!output.includes("lacks required permission")) throw new Error("permission denial was not explicit");
});

verify("scene-v2-roundtrip", () => {
  const output = path.join("target", "testing", "compat-roundtrip.mnsc");
  json(["scene", "testing/assets/benchmark-3d.mnsc", "--output", output, "--name", "compat"]);
  const scene = JSON.parse(fs.readFileSync(output, "utf8"));
  if (scene.version !== 2 || !scene.entities[0].components["core.model"]) throw new Error("scene migration failed");
});

verify("corrupt-input-isolation", () => {
  // A real FBX gives the best truncation fixture when present; otherwise a
  // synthetic binary-FBX header keeps the case self-contained.
  const fixtureSource = fs.existsSync("testing/assets/garden_diorama.fbx")
    ? fs.readFileSync("testing/assets/garden_diorama.fbx")
    : Buffer.concat([
        Buffer.from("Kaydara FBX Binary  \0\x1a\0", "latin1"),
        Buffer.from([0x28, 0x00, 0x00, 0x00]), // version 7400
        Buffer.alloc(160, 0x41),
      ]);
  const fixtures = [
    Buffer.alloc(0),
    Buffer.from("not an fbx file"),
    fixtureSource.subarray(0, 128),
  ];
  fixtures.forEach((contents, index) => {
    const input = path.join("target", "testing", `corrupt-${index}.fbx`);
    const output = path.join("target", "testing", `corrupt-${index}.mnec`);
    fs.writeFileSync(input, contents);
    run(["cook", input, "--output", output, "--isolate"], 1);
    if (fs.existsSync(output)) throw new Error("corrupt input produced an output package");
  });
});

verify("golden-rendering", () => {
  const first = path.join("target", "testing", "golden-a.rgba");
  const second = path.join("target", "testing", "golden-b.rgba");
  json(["editor", "--golden", first, "--frames", "2"]);
  json(["editor", "--golden", second, "--frames", "2"]);
  if (!fs.readFileSync(first).equals(fs.readFileSync(second))) throw new Error("golden pixels differ");
  if (fs.readFileSync(first.replace(/\.rgba$/i, ".json"), "utf8") !== fs.readFileSync(second.replace(/\.rgba$/i, ".json"), "utf8")) throw new Error("golden metadata differs");
});

verify("structured-fuzz-smoke", () => {
  const report = json(["fuzz", "--iterations", "400", "--seed", "7"]);
  const outcomes = report.outcomes || {};
  if (report.panics && report.panics.length > 0) throw new Error("fuzzer caught parser panics");
  if (!outcomes.ok && !outcomes.rejected) throw new Error("fuzzer produced no outcomes");
});

// Local test models live in the gitignored testing/assets folder; when a
// developer provides them, every suite run opens both 3D scenes end to end.
const localScenes = [
  ["testing/assets/garden_diorama.mnsc", "garden-diorama"],
  ["testing/assets/xujiahui_cyber_geo.mnsc", "xujiahui-cyber-geo"],
];
for (const [scenePath, name] of localScenes) {
  if (!fs.existsSync(scenePath)) {
    console.log(`skip - local-model-scene:${name} (${scenePath} not provided)`);
    continue;
  }
  verify(`local-model-scene:${name}`, () => {
    const output = json(["editor", "--scene", scenePath, "--frames", "2"]);
    if (!output.loadedMeshVertices || output.loadedMeshVertices <= 0) {
      throw new Error(`${name} rendered no mesh vertices`);
    }
  });
}

console.log(JSON.stringify({ ok: true, results }, null, 2));
