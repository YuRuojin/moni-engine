//! The core engine's `ScriptRuntime` implementation over QuickJS. Owns one
//! script host per scripted entity and reconciles host state against the
//! world every tick, so despawns, script replacement, and world reloads need
//! no explicit engine hooks.

use std::collections::{BTreeMap, BTreeSet};

use moni_core::engine::{Command, Entity, EntityId, Event, ScriptRuntime, ScriptTickOutput, World};

use crate::ScriptHost;

/// Per-entity QuickJS hosts paired with the source they were created from,
/// enabling change detection without engine-side notifications.
pub struct QuickJsScriptRuntime {
    hosts: BTreeMap<EntityId, (ScriptHost, String)>,
}

impl QuickJsScriptRuntime {
    pub fn new() -> Self {
        Self {
            hosts: BTreeMap::new(),
        }
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
            "QuickJsScriptRuntime(hosts: {})",
            self.hosts.len()
        ))
    }
}

impl ScriptRuntime for QuickJsScriptRuntime {
    fn tick(&mut self, world: &mut World, tick: u64, dt: f64) -> ScriptTickOutput {
        let runnable: Vec<Entity> = world
            .entities
            .values()
            .filter(|entity| entity.script.as_ref().is_some_and(|script| script.enabled))
            .cloned()
            .collect();

        // Drop hosts whose entity disappeared or lost its script entirely.
        // Disabled scripts keep their host so re-enabling preserves state.
        let live: BTreeSet<EntityId> = world
            .entities
            .values()
            .filter(|entity| entity.script.is_some())
            .map(|entity| entity.id)
            .collect();
        self.hosts.retain(|id, _| live.contains(id));

        let mut output = ScriptTickOutput::default();
        for entity in runnable {
            let Some(script) = entity.script.as_ref() else {
                continue;
            };
            // Create or replace the host when the source changed; creation
            // failures follow the same disable-and-report path as run errors.
            if self
                .hosts
                .get(&entity.id)
                .is_none_or(|(_, source)| *source != script.source)
            {
                match ScriptHost::new(script.source.as_str()) {
                    Ok(host) => {
                        self.hosts.insert(entity.id, (host, script.source.clone()));
                    }
                    Err(failure) => {
                        self.hosts.remove(&entity.id);
                        disable_script(world, entity.id, &failure);
                        output.events.push(Event::ScriptError {
                            entity: entity.id,
                            code: failure.code,
                            message: failure.message,
                        });
                        continue;
                    }
                }
            }
            let Some((host, _)) = self.hosts.get_mut(&entity.id) else {
                continue;
            };
            host.assert_logic_thread();
            match host.run_tick(&entity, tick, dt) {
                Ok(hook) => {
                    for message in hook.logs {
                        output.events.push(Event::ScriptLog {
                            entity: entity.id,
                            message,
                        });
                    }
                    for raw in hook.commands {
                        match serde_json::from_str::<Command>(&raw) {
                            Ok(command) => output.commands.push((entity.id, command)),
                            Err(error) => output.events.push(Event::InvalidScriptCommand {
                                entity: entity.id,
                                message: error.to_string(),
                            }),
                        }
                    }
                }
                Err(failure) => {
                    disable_script(world, entity.id, &failure);
                    self.hosts.remove(&entity.id);
                    output.events.push(Event::ScriptError {
                        entity: entity.id,
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
