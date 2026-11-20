# Engine examples

[中文](README.md) | English

`examples/` contains runnable validation scripts for the engine itself, not a game project template. Games belong in independent repositories; the project creator is not implemented yet.

## Files

- `model-orbit.ts`: rotates the garden model in the external FBX scene for `npm run demo` and `npm run demo:3d`.
- `player.ts`: legacy 2D sprite script example.
- `spinning-cube.ts`: legacy built-in cube script example.
- `engine-api.d.ts`: TypeScript declarations for the current script API.
- `tsconfig.json`: strict compilation settings; `dist/` is generated and must not be edited manually.

The scripts share the global `onTick` entry, so they use identically typed global function variables to avoid duplicate TypeScript function implementations. Every build compiles all TypeScript examples into `examples/dist/`.

## Verify

```bash
npm run check
npm run build
cargo run -- client --frames 5
cargo run -- client --scene 3d --frames 10
```

Use `--frames` only for automated exit during verification. For normal runs, use `npm run demo` or `npm run demo:3d` and press Escape to close the window.
