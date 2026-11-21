//! The core engine's `ScriptRuntime` implementation over QuickJS. Owns one
//! script host per scripted entity and reconciles host state against the
//! world every tick, so despawns, script replacement, and world reloads need
//! no explicit engine hooks. A wall-time budget shared across all hosts caps
//! the scripting cost of a single tick; over-budget entities are skipped for
//! that tick, not disabled.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use moni_core::engine::{Command, EntityId, Event, ScriptRuntime, ScriptTickOutput, World};

use crate::ScriptHost;

/// Default wall-time budget shared by all script hooks within one tick. A
/// 60 FPS frame has roughly 16.7ms of total budget, so scripting takes only
/// a fraction of it by default; the per-hook 50ms limit in `ScriptHost`
/// still applies independently.
const DEFAULT_TICK_BUDGET: Duration = Duration::from_millis(10);

/// Per-entity QuickJS hosts paired with the source they were created from,
/// enabling change detection without engine-side notifications.
pub struct QuickJsScriptRuntime {
    hosts: BTreeMap<EntityId, (ScriptHost, String)>,
    /// Wall-time budget shared by all `run_tick` calls within one tick. When
    /// the cumulative time exceeds it, remaining runnable entities are
    /// skipped (not disabled) for that tick.
    tick_budget: Duration,
    /// Injectable clock for budget accounting; production uses `Instant::now`.
    /// Tests inject a stepped fake clock so budget exhaustion is deterministic
    /// instead of depending on machine speed (CI runners vary widely).
    now: Box<dyn Fn() -> Instant>,
}

impl QuickJsScriptRuntime {
    pub fn new() -> Self {
        Self::with_tick_budget(DEFAULT_TICK_BUDGET)
    }

    /// Creates a runtime whose per-tick script budget is `budget` instead of
    /// the default 10ms. See [`Self::set_tick_budget`] for the semantics.
    pub fn with_tick_budget(budget: Duration) -> Self {
        Self {
            hosts: BTreeMap::new(),
            tick_budget: budget,
            now: Box::new(Instant::now),
        }
    }

    /// Replaces the budget clock. Production always measures `Instant::now`;
    /// tests pass a stepped fake clock so budget accounting is deterministic.
    pub fn with_clock(mut self, now: Box<dyn Fn() -> Instant>) -> Self {
        self.now = now;
        self
    }

    /// Sets the wall-time budget shared by all script hooks within one tick.
    /// The budget accumulates the elapsed time of every `run_tick` call; once
    /// it is exhausted, remaining runnable entities are skipped for the
    /// current tick, each reported through `Event::ScriptError` with code
    /// `script_budget_exceeded`. Skipped entities keep their hosts and
    /// enabled state and simply resume on the next tick. The per-hook 50ms
    /// limit in `ScriptHost` is unaffected.
    pub fn set_tick_budget(&mut self, budget: Duration) {
        self.tick_budget = budget;
    }
}

impl Default for QuickJsScriptRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for QuickJsScriptRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&format!(
            "QuickJsScriptRuntime(hosts: {}, tick_budget: {:?})",
            self.hosts.len(),
            self.tick_budget
        ))
    }
}

impl ScriptRuntime for QuickJsScriptRuntime {
    fn tick(&mut self, world: &mut World, tick: u64, dt: f64) -> ScriptTickOutput {
        // Only the IDs are collected up front; entities are re-borrowed
        // immutably from the world at their use sites, never cloned.
        let runnable: Vec<EntityId> = world
            .entities
            .values()
            .filter(|entity| entity.script.as_ref().is_some_and(|script| script.enabled))
            .map(|entity| entity.id)
            .collect();

        // Drop hosts whose entity disappeared or lost its script entirely.
        // Disabled scripts keep their host so re-enabling preserves state.
        self.hosts.retain(|id, _| {
            world
                .entities
                .get(id)
                .is_some_and(|entity| entity.script.is_some())
        });

        let mut output = ScriptTickOutput::default();
        let mut budget_used = Duration::ZERO;
        for entity_id in runnable {
            // Out of budget: skip the rest of the tick. Skipped entities keep
            // their hosts and enabled state untouched and resume next tick.
            if budget_used >= self.tick_budget {
                output.events.push(Event::ScriptError {
                    entity: entity_id,
                    code: "script_budget_exceeded".to_string(),
                    message: format!(
                        "per-tick script budget of {:?} exhausted; script skipped for this tick",
                        self.tick_budget
                    ),
                });
                continue;
            }
            // Compare the stored host source against the world without
            // cloning the script state.
            let Some(script) = world
                .entities
                .get(&entity_id)
                .and_then(|entity| entity.script.as_ref())
            else {
                continue;
            };
            // Create or replace the host when the source changed; creation
            // failures follow the same disable-and-report path as run errors.
            if self
                .hosts
                .get(&entity_id)
                .is_none_or(|(_, source)| source.as_str() != script.source.as_str())
            {
                match ScriptHost::new(script.source.as_str()) {
                    Ok(host) => {
                        let source = script.source.clone();
                        self.hosts.insert(entity_id, (host, source));
                    }
                    Err(failure) => {
                        self.hosts.remove(&entity_id);
                        disable_script(world, entity_id, &failure);
                        output.events.push(Event::ScriptError {
                            entity: entity_id,
                            code: failure.code,
                            message: failure.message,
                        });
                        continue;
                    }
                }
            }
            // The immutable world borrow is scoped to this block so the
            // mutable borrow taken by the failure handling below is allowed.
            let run_result = {
                let Some(entity) = world.entities.get(&entity_id) else {
                    continue;
                };
                let Some((host, _)) = self.hosts.get_mut(&entity_id) else {
                    continue;
                };
                host.assert_logic_thread();
                let started = (self.now)();
                let result = host.run_tick(entity, tick, dt);
                budget_used += (self.now)().duration_since(started);
                result
            };
            match run_result {
                Ok(hook) => {
                    for message in hook.logs {
                        output.events.push(Event::ScriptLog {
                            entity: entity_id,
                            message,
                        });
                    }
                    for raw in hook.commands {
                        match serde_json::from_str::<Command>(&raw) {
                            Ok(command) => output.commands.push((entity_id, command)),
                            Err(error) => output.events.push(Event::InvalidScriptCommand {
                                entity: entity_id,
                                message: error.to_string(),
                            }),
                        }
                    }
                }
                Err(failure) => {
                    disable_script(world, entity_id, &failure);
                    self.hosts.remove(&entity_id);
                    output.events.push(Event::ScriptError {
                        entity: entity_id,
                        code: failure.code,
                        message: failure.message,
                    });
                }
            }
        }
        output
    }

    fn host_count(&self) -> usize {
        self.hosts.len()
    }

    fn clear(&mut self) {
        self.hosts.clear();
    }
}

fn disable_script(world: &mut World, entity_id: EntityId, failure: &crate::ScriptFailure) {
    if let Some(current) = world.entities.get_mut(&entity_id)
        && let Some(script) = current.script.as_mut()
    {
        script.enabled = false;
        script.last_error = Some(failure.message.clone());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use moni_core::engine::{
        Command, Entity, EntityId, Event, ScriptRuntime, ScriptState, ScriptTickOutput, World,
    };

    use super::QuickJsScriptRuntime;

    /// Top-level counter that keeps increasing across ticks, so tests can
    /// tell a preserved host (state survives) from a recreated one.
    const COUNTER_SOURCE: &str = r#"
let count = 0;
function onTick(ctx, dt) {
  count += 1;
  ctx.log("count-" + count);
}
"#;

    /// Same counter but starting at 100, used as a replacement source: the
    /// first log of the new host is "count-101".
    const REPLACED_COUNTER_SOURCE: &str = r#"
let count = 100;
function onTick(ctx, dt) {
  count += 1;
  ctx.log("count-" + count);
}
"#;

    /// Emits one command and one log per tick; both entities use it to prove
    /// that commands and events flow from every host in a single tick.
    const LOG_AND_TRANSFORM_SOURCE: &str = r#"
function onTick(ctx, dt) {
  ctx.setTransform({ x: ctx.entity.id * 2, y: 0, rotation: 0 });
  ctx.log("tick-" + ctx.entity.id);
}
"#;

    /// Arithmetic busy loop sized to exceed a 1ms tick budget by a wide
    /// A moderate loop that completes well under the 50ms per-hook limit on
    /// any plausible machine (20000 iterations take single-digit
    /// milliseconds even in debug builds on slow runners). Budget
    /// exhaustion is driven by the injected test clock, not by this loop's
    /// real runtime, so the test stays deterministic across hardware. The
    /// log carries the exact sum, proving the loop ran to completion
    /// instead of timing out.
    const BUSY_SOURCE: &str = r#"
function onTick(ctx, dt) {
  let sum = 0;
  for (let i = 0; i < 20000; i++) {
    sum += i;
  }
  ctx.log("busy-" + sum);
}
"#;

    const BUSY_SUM: &str = "busy-199990000";

    fn scripted_entity(id: EntityId, source: &str) -> Entity {
        Entity {
            id,
            name: format!("entity-{id}"),
            transform: Default::default(),
            sprite: None,
            model: None,
            script: Some(ScriptState {
                source: source.to_string(),
                source_path: None,
                enabled: true,
                last_error: None,
            }),
            extensions: BTreeMap::new(),
        }
    }

    fn world_with(entities: Vec<Entity>) -> World {
        let mut world: World =
            serde_json::from_str(r#"{"version": 1}"#).expect("minimal world parses");
        let mut next_entity_id = world.next_entity_id;
        for entity in entities {
            next_entity_id = next_entity_id.max(entity.id + 1);
            world.entities.insert(entity.id, entity);
        }
        world.next_entity_id = next_entity_id;
        world
    }

    fn script_logs(output: &ScriptTickOutput) -> Vec<(EntityId, &str)> {
        output
            .events
            .iter()
            .filter_map(|event| match event {
                Event::ScriptLog { entity, message } => Some((*entity, message.as_str())),
                _ => None,
            })
            .collect()
    }

    fn script_errors(output: &ScriptTickOutput) -> Vec<(EntityId, &str)> {
        output
            .events
            .iter()
            .filter_map(|event| match event {
                Event::ScriptError { entity, code, .. } => Some((*entity, code.as_str())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn both_scripted_entities_run_in_one_tick() {
        let mut world = world_with(vec![
            scripted_entity(1, LOG_AND_TRANSFORM_SOURCE),
            scripted_entity(2, LOG_AND_TRANSFORM_SOURCE),
        ]);
        let mut runtime = QuickJsScriptRuntime::new();

        let output = runtime.tick(&mut world, 1, 0.016);

        assert_eq!(runtime.host_count(), 2);
        assert_eq!(script_logs(&output), vec![(1, "tick-1"), (2, "tick-2")]);
        // Commands from both hosts flow through the output in id order.
        assert_eq!(output.commands.len(), 2);
        for (entity_id, command) in &output.commands {
            match command {
                Command::SetTransform { entity, transform } => {
                    assert_eq!(*entity, *entity_id);
                    // The script sets x to entity.id * 2.
                    assert_eq!(transform.x, *entity_id as f64 * 2.0);
                    assert_eq!(transform.y, 0.0);
                }
                other => panic!("unexpected command: {other:?}"),
            }
        }
        assert!(
            script_errors(&output).is_empty(),
            "no errors expected: {:?}",
            output.events
        );
    }

    #[test]
    fn despawned_entity_host_is_dropped() {
        let mut world = world_with(vec![
            scripted_entity(1, COUNTER_SOURCE),
            scripted_entity(2, COUNTER_SOURCE),
        ]);
        let mut runtime = QuickJsScriptRuntime::new();
        runtime.tick(&mut world, 1, 0.016);
        assert_eq!(runtime.host_count(), 2);

        // Simulate an engine-side despawn; the host reconciles on the next tick.
        world.entities.remove(&2);
        runtime.tick(&mut world, 2, 0.016);

        assert_eq!(runtime.host_count(), 1);
        assert!(world.entities.contains_key(&1));
    }

    #[test]
    fn disabled_script_keeps_host_and_resumes_with_state() {
        let mut world = world_with(vec![scripted_entity(1, COUNTER_SOURCE)]);
        let mut runtime = QuickJsScriptRuntime::new();
        let output = runtime.tick(&mut world, 1, 0.016);
        assert_eq!(script_logs(&output), vec![(1, "count-1")]);

        // Disable directly on the world, as the SetScriptEnabled command does.
        world
            .entities
            .get_mut(&1)
            .and_then(|entity| entity.script.as_mut())
            .expect("scripted entity")
            .enabled = false;
        let output = runtime.tick(&mut world, 2, 0.016);
        assert_eq!(runtime.host_count(), 1, "disabled scripts keep their host");
        assert_eq!(script_logs(&output), vec![], "disabled scripts do not run");

        world
            .entities
            .get_mut(&1)
            .and_then(|entity| entity.script.as_mut())
            .expect("scripted entity")
            .enabled = true;
        let output = runtime.tick(&mut world, 3, 0.016);
        // "count-2" proves the host and its top-level state survived the pause.
        assert_eq!(script_logs(&output), vec![(1, "count-2")]);
    }

    #[test]
    fn source_replacement_recreates_host() {
        let mut world = world_with(vec![scripted_entity(1, COUNTER_SOURCE)]);
        let mut runtime = QuickJsScriptRuntime::new();
        let output = runtime.tick(&mut world, 1, 0.016);
        assert_eq!(script_logs(&output), vec![(1, "count-1")]);

        world
            .entities
            .get_mut(&1)
            .and_then(|entity| entity.script.as_mut())
            .expect("scripted entity")
            .source = REPLACED_COUNTER_SOURCE.to_string();
        let output = runtime.tick(&mut world, 2, 0.016);

        assert_eq!(runtime.host_count(), 1);
        // "count-101" proves a fresh host built from the new source, not the
        // old one continuing from "count-1".
        assert_eq!(script_logs(&output), vec![(1, "count-101")]);
    }

    #[test]
    fn throwing_script_is_disabled_and_reported() {
        let mut world = world_with(vec![scripted_entity(
            1,
            r#"function onTick(ctx, dt) { throw new Error("boom"); }"#,
        )]);
        let mut runtime = QuickJsScriptRuntime::new();

        let output = runtime.tick(&mut world, 1, 0.016);

        assert_eq!(script_errors(&output), vec![(1, "script_error")]);
        let script = world
            .entities
            .get(&1)
            .and_then(|entity| entity.script.as_ref())
            .expect("scripted entity");
        assert!(!script.enabled, "failing scripts are disabled");
        let last_error = script.last_error.as_deref().expect("last_error set");
        assert!(!last_error.is_empty(), "last_error carries the failure");
        assert_eq!(runtime.host_count(), 0, "failed hosts are dropped");

        // A disabled script is not runnable, so no host is recreated later.
        let output = runtime.tick(&mut world, 2, 0.016);
        assert_eq!(runtime.host_count(), 0);
        assert_eq!(script_logs(&output), vec![]);
    }

    /// A stepped fake clock: every call advances the returned instant by
    /// one step, so each measured `run_tick` consumes exactly one step of
    /// budget regardless of how fast the machine really is.
    fn stepped_clock(step_ms: u64) -> Box<dyn Fn() -> Instant> {
        let base = Instant::now();
        let call_index = std::cell::Cell::new(0u64);
        Box::new(move || {
            let index = call_index.get();
            call_index.set(index + 1);
            base + Duration::from_millis(step_ms * index)
        })
    }

    #[test]
    fn budget_exhaustion_skips_remaining_entities_without_disabling() {
        let mut world = world_with(vec![
            scripted_entity(1, BUSY_SOURCE),
            scripted_entity(2, COUNTER_SOURCE),
        ]);
        // Every measured run consumes exactly 5ms of fake budget time.
        let mut runtime = QuickJsScriptRuntime::with_tick_budget(Duration::from_secs(1))
            .with_clock(stepped_clock(5));
        // Generous budget: both scripts run and entity 2 gets a host.
        let output = runtime.tick(&mut world, 1, 0.016);
        assert_eq!(script_logs(&output), vec![(1, BUSY_SUM), (2, "count-1")]);
        assert_eq!(runtime.host_count(), 2);

        // Entity 1 alone consumes the whole 5ms budget, so entity 2 is
        // skipped for this tick: reported, but neither disabled nor evicted.
        runtime.set_tick_budget(Duration::from_millis(5));
        let output = runtime.tick(&mut world, 2, 0.016);
        assert_eq!(script_errors(&output), vec![(2, "script_budget_exceeded")]);
        assert_eq!(runtime.host_count(), 2, "skipped scripts keep their hosts");
        let script = world
            .entities
            .get(&2)
            .and_then(|entity| entity.script.as_ref())
            .expect("scripted entity");
        assert!(script.enabled, "skipped scripts are not disabled");
        assert!(script.last_error.is_none(), "skips are not failures");
        assert_eq!(script_logs(&output), vec![(1, BUSY_SUM)]);

        // With the budget restored, entity 2 resumes on the next tick; the
        // counter continues instead of restarting, proving state survived.
        runtime.set_tick_budget(Duration::from_secs(1));
        let output = runtime.tick(&mut world, 3, 0.016);
        assert_eq!(script_logs(&output), vec![(1, BUSY_SUM), (2, "count-2")]);
    }
}
