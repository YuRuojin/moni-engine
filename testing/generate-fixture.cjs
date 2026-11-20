// Deterministically generate the cooked benchmark mesh referenced by
// testing/assets/benchmark-3d.mnsc. The output uses the engine's .mnec format:
// "MNECOOK1" magic, u64 little-endian vertex count, then 44 bytes per vertex
// (10 x f32: position.xyz, normal.xyz, color.rgba, plus u32 color flag).
//
// The mesh is derived data, so it stays out of Git (see testing/assets/.gitignore).
// Run `node testing/generate-fixture.cjs` (done automatically by
// testing/run-benchmarks.cjs) or pass --force to regenerate.
const fs = require("fs");
const path = require("path");

const VERTICES = 1975083; // documented ~1.98M vertex benchmark scene
const QUAD = 0.1;
const HEADER_BYTES = 16;
const VERTEX_BYTES = 44;
const output = path.join(__dirname, "assets", "benchmark.mnec");
const force = process.argv.includes("--force");

const expectedBytes = HEADER_BYTES + VERTICES * VERTEX_BYTES;
if (fs.existsSync(output) && !force) {
  const actual = fs.statSync(output).size;
  if (actual === expectedBytes) {
    console.log(JSON.stringify({
      ok: true,
      output: path.relative(process.cwd(), output),
      vertices: VERTICES,
      bytes: actual,
      cached: true,
    }));
    process.exit(0);
  }
  fs.unlinkSync(output);
}

const quads = Math.floor(VERTICES / 6);
const remainder = VERTICES - quads * 6;
const columns = Math.ceil(Math.sqrt(quads));
const rows = Math.ceil(quads / columns);

const buffer = Buffer.allocUnsafe(expectedBytes);
buffer.write("MNECOOK1", 0, "latin1");
buffer.writeBigUInt64LE(BigInt(VERTICES), 8);

let offset = HEADER_BYTES;
function writeVertex(x, y, z, nx, ny, nz, r, g, b) {
  buffer.writeFloatLE(x, offset);
  buffer.writeFloatLE(y, offset + 4);
  buffer.writeFloatLE(z, offset + 8);
  buffer.writeFloatLE(nx, offset + 12);
  buffer.writeFloatLE(ny, offset + 16);
  buffer.writeFloatLE(nz, offset + 20);
  buffer.writeFloatLE(r, offset + 24);
  buffer.writeFloatLE(g, offset + 28);
  buffer.writeFloatLE(b, offset + 32);
  buffer.writeFloatLE(1.0, offset + 36); // alpha
  buffer.writeUInt32LE(1, offset + 40); // has color
  offset += VERTEX_BYTES;
}

function writeTriangle(a, b, c, normal, tint) {
  for (const [x, y, z] of [a, b, c]) {
    writeVertex(x, y, z, normal[0], normal[1], normal[2], tint[0], tint[1], tint[2]);
  }
}

function height(x, z) {
  return 1.5 * Math.sin(x * 0.35) * Math.cos(z * 0.35);
}

const tint = [0.62, 0.71, 1.0];
const normal = [0, 1, 0];
let emitted = 0;
for (let row = 0; row < rows && emitted < quads; row += 1) {
  for (let column = 0; column < columns && emitted < quads; column += 1) {
    const x0 = (column - columns / 2) * QUAD;
    const x1 = x0 + QUAD;
    const z0 = (row - rows / 2) * QUAD;
    const z1 = z0 + QUAD;
    const y00 = height(x0, z0);
    const y10 = height(x1, z0);
    const y01 = height(x0, z1);
    const y11 = height(x1, z1);
    writeTriangle([x0, y00, z0], [x0, y01, z1], [x1, y11, z1], normal, tint);
    writeTriangle([x0, y00, z0], [x1, y11, z1], [x1, y10, z0], normal, tint);
    emitted += 1;
  }
}
// VERTICES % 6 leftover vertices (currently 3) form one marker triangle above
// the terrain so the count is exact.
if (remainder > 0) {
  const peak = [
    [0, 8, 0],
    [-1, 7, 1],
    [1, 7, 1],
  ];
  for (let index = 0; index < remainder; index += 1) {
    const [x, y, z] = peak[index % 3];
    writeVertex(x, y, z, 0, 1, 0, 1.0, 0.85, 0.3);
  }
}
if (offset !== buffer.length) {
  throw new Error(`fixture layout mismatch: wrote ${offset} of ${buffer.length} bytes`);
}

fs.mkdirSync(path.dirname(output), { recursive: true });
fs.writeFileSync(output, buffer);
console.log(JSON.stringify({
  ok: true,
  output: path.relative(process.cwd(), output),
  vertices: VERTICES,
  bytes: buffer.length,
  cached: false,
}));
