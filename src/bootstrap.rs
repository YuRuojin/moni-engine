//! Composition-root plugin factory. This is the ONLY place that names
//! concrete plugin types: the binary wires the effective profile's plugins
//! into the host, with rollback on partial failure.

use std::sync::Arc;

use winit::window::Window;

use moni_core::host::{Plugin, PluginError, PluginHandle, PluginHost};
use moni_core::plugins::SimulationPlugin;
use moni_core::profile::Profile;
use moni_render::plugin::RenderPlugin;

/// Constructs and registers every instance plugin in the profile's effective
/// tree, in dependency order. Capability providers (core.fbx, core.editor)
/// contribute only permissions. On any failure the host is restored to its
/// pre-call state: constructed handles are disposed in reverse order and
/// capability grants are revoked.
pub fn instantiate(
    profile: &Profile,
    host: &mut PluginHost,
    window: Option<Arc<Window>>,
    collect_gpu_timestamps: bool,
    hot_reload: bool,
) -> anyhow::Result<Vec<PluginHandle>> {
    let effective = profile.resolve(&moni_core::profile::Target::current())?;
    // Capability providers such as core.fbx contribute permissions without a
    // plugin instance. Grant them first so dependent plugins can start, and
    // revoke them again if any later step fails.
    let mut granted = Vec::new();
    for plugin in effective.plugins() {
        if plugin.id() == "core.fbx" {
            host.grant_permissions(plugin.id(), plugin.permissions());
            granted.push(plugin.id().to_string());
        }
    }
    // Construct every plugin instance before registering anything so a
    // factory or configuration failure cannot leave partially registered
    // state behind.
    let mut pending: Vec<(Vec<String>, Box<dyn Plugin>)> = Vec::new();
    let mut construction = Ok(());
    for plugin in effective.plugins() {
        match plugin.id() {
            "core.simulation" => {
                let mut instance = Box::new(SimulationPlugin::new()) as Box<dyn Plugin>;
                if let Some(config) = effective.config_for("core.simulation")
                    && let Err(error) = instance.configure(config)
                {
                    construction = Err(error);
                    break;
                }
                pending.push((plugin.permissions().to_vec(), instance));
            }
            "core.render" => {
                let Some(window) = window.clone() else {
                    construction = Err(PluginError::new(
                        "core.render requires a window but headless was requested",
                    ));
                    break;
                };
                let mut instance = Box::new(RenderPlugin::new(
                    window,
                    collect_gpu_timestamps,
                    effective.contains("core.fbx"),
                    hot_reload,
                )) as Box<dyn Plugin>;
                if let Some(config) = effective.config_for("core.render")
                    && let Err(error) = instance.configure(config)
                {
                    construction = Err(error);
                    break;
                }
                pending.push((plugin.permissions().to_vec(), instance));
            }
            "core.fbx" | "core.editor" => {
                if effective.config_for(plugin.id()).is_some() {
                    construction = Err(PluginError::new(format!(
                        "plugin '{}' has no runtime instance to configure",
                        plugin.id()
                    )));
                    break;
                }
            }
            id => {
                construction = Err(PluginError::new(format!(
                    "no static factory registered for plugin '{id}'"
                )));
                break;
            }
        }
    }
    if let Err(error) = construction {
        for id in &granted {
            host.revoke_permissions(id);
        }
        return Err(error.into());
    }
    // Register in dependency order; on failure dispose the handles created
    // so far in reverse order so the host is left exactly as it started.
    let mut handles = Vec::new();
    for (permissions, plugin) in pending {
        match host.register_plugin_with_permissions(plugin, &permissions) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                while let Some(handle) = handles.pop() {
                    let _ = handle.dispose(host);
                }
                for id in &granted {
                    host.revoke_permissions(id);
                }
                return Err(error.into());
            }
        }
    }
    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moni_core::engine::Engine;
    use serde_json::json;

    #[test]
    fn headless_default_instantiate_fails_and_leaves_host_clean() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default").expect("default profile");
        let error = instantiate(&profile, &mut host, None, false, false)
            .expect_err("core.render requires a window");
        assert!(error.to_string().contains("requires a window"));
        assert!(!host.has_plugin("core.simulation"));
        assert!(!host.has_plugin("core.render"));
    }

    #[test]
    fn instantiate_registers_only_patched_plugins() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default")
            .expect("default profile")
            .with_patch("core.render", false);
        let handles = instantiate(&profile, &mut host, None, false, false)
            .expect("simulation-only instantiate");
        assert_eq!(handles.len(), 1);
        assert!(host.has_plugin("core.simulation"));
        assert!(!host.has_plugin("core.render"));
        for handle in handles {
            handle
                .dispose(&mut host)
                .expect("dispose simulation plugin");
        }
        assert!(!host.has_plugin("core.simulation"));
    }

    #[test]
    fn minimal_profile_instantiates_nothing() {
        let mut host = PluginHost::new(Engine::new());
        let handles = instantiate(
            &Profile::named("minimal").expect("minimal profile"),
            &mut host,
            None,
            false,
            false,
        )
        .expect("minimal instantiate");
        assert!(handles.is_empty());
        assert!(!host.has_plugin("core.simulation"));
    }

    #[test]
    fn patch_config_applies_to_plugins_in_order() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default")
            .expect("default profile")
            .with_patch("core.render", false)
            .with_config("core.simulation", json!({ "tickRate": 100 }))
            .with_config("core.simulation", json!({ "tickRate": 250 }));
        instantiate(&profile, &mut host, None, false, false).expect("configured instantiate");
        assert_eq!(host.world().tick_rate, 250);
    }

    #[test]
    fn patch_config_can_enable_a_plugin() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("minimal")
            .expect("minimal profile")
            .with_config("core.simulation", json!({ "tickRate": 30 }));
        let handles = instantiate(&profile, &mut host, None, false, false)
            .expect("config-enabled instantiate");
        assert_eq!(handles.len(), 1);
        assert_eq!(host.world().tick_rate, 30);
    }

    #[test]
    fn patch_config_rejects_unknown_fields_and_capability_plugins() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default")
            .expect("default profile")
            .with_patch("core.render", false)
            .with_config("core.simulation", json!({ "bogus": 1 }));
        let error = instantiate(&profile, &mut host, None, false, false)
            .expect_err("unknown config field must fail");
        assert!(error.to_string().contains("unknown field"));

        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default")
            .expect("default profile")
            .with_config("core.fbx", json!({ "anything": true }));
        let error = instantiate(&profile, &mut host, None, false, false)
            .expect_err("capability plugin config must fail");
        assert!(error.to_string().contains("no runtime instance"));
    }
}
