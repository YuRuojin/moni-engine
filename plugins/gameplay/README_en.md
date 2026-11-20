# Gameplay system plugins

[中文](README.md) | English

`gameplay/` owns reusable runtime game mechanics, including physics, animation, audio, input, game UI, networking, navigation, and other systems with explicit ownership. Game-specific characters, levels, and combat logic remain in independent game repositories rather than official plugins.

## Responsibility boundary

- Physics: collision shapes, rigid bodies, queries, stepping, and determinism requirements.
- Animation: clips, states, blending, skeletons, and event tracks.
- Audio and input: replaceable runtime providers, resource lifetimes, action mapping, and high-level device state; [`platforms/`](../platforms/README_en.md) adapts raw OS events.
- Game UI and networking: runtime interfaces, sessions, synchronization, and message limits.
- Reusable mechanics: read and write the world through core command and component interfaces rather than direct internal storage access.

Texture and model parsing belongs to [`assets/`](../assets/README_en.md), visual effects to [`rendering/`](../rendering/README_en.md), system panels to [`editor/`](../editor/README_en.md), and build targets to [`platforms/`](../platforms/README_en.md).

## Initial official capabilities

Define component data, system execution order, and fixed-tick interfaces first, then implement one vertical slice in input, physics, or animation. Every system remains independently optional; adding one mechanic must not make every backend a required core dependency.

## Performance and stability

- System order, time steps, and ownership must be explicit; render frame rate must not change deterministic simulation results.
- Physics catch-up, network messages, audio callbacks, and entity events need time, count, or memory bounds.
- Measure cache behavior and allocation for large entity queries instead of copying the entire world every frame.
- Network and device callbacks must not block the simulation thread indefinitely; asynchronous results need cancellation and expiry.
- Deactivation must release input, audio, physics, and network resources without disturbing other systems.

## Current limitations

Only basic entities, transforms, models, script commands, and fixed simulation ticks exist. Physics, animation, audio, formal input components, game UI, navigation, and networking are not implemented.
