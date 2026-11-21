//! Composition-root plugin factory. This is the ONLY place that names
//! concrete plugin types: the binary wires the effective profile's plugins
//! into the host, split into a registration phase (schemas, services,
//! commands — no World dependency) and a start phase (GPU, script runtime).

use std::sync::Arc;

use winit::window::Window;

use moni_core::engine::Engine;
use moni_core::host::{Plugin, PluginError, PluginHandle, PluginHost, SystemStage};
use moni_core::plugins::SimulationPlugin;
use moni_core::profile::{Profile, Target};
use moni_core::session::RuntimeSession;
use moni_render::plugin::RenderPlugin;
use moni_script::QuickJsScriptRuntime;

/// Builds the agent/CLI control session: QuickJS plus the `serve` profile.
/// Registration runs first, then the scene-free startup path: start the
/// simulation plugin and run the Startup stage, so capabilities reflect the
/// real running session. This is the same plugin lifecycle the windowed
/// client and editor run through.
pub fn serve_session() -> anyhow::Result<RuntimeSession> {
    let mut engine = Engine::new();
    engine.set_script_runtime(Box::new(QuickJsScriptRuntime::new()));
    let profile = Profile::named("serve")?;
    let mut session = RuntimeSession::new(engine, profile.clone());
    register_profile(&profile, session.host_mut(), None)?;
    // The serve profile exists to drive ticks; older profiles could patch
    // simulation out, in which case the session falls back to engine ticks.
    if session
        .host_mut()
        .has_plugin(moni_core::host::CORE_SIMULATION)
    {
        session
            .host_mut()
            .start_plugin(moni_core::host::CORE_SIMULATION)?;
        session.host_mut().run_stage(SystemStage::Startup)?;
    }
    Ok(session)
}

/// Builds the MCP-server session: the same composition as
/// [`serve_session`] (Engine + QuickJS + the `serve` profile, simulation
/// started so ticks run through the plugin host), plus an optional initial
/// scene loaded through `RuntimeSession::load_scene` before the simulation
/// starts, mirroring the windowed client's composition order. The session
/// lives for the whole MCP server lifetime; tools operate on it
/// sequentially from the dedicated engine thread.
pub fn mcp_session(scene_path: Option<&str>) -> anyhow::Result<RuntimeSession> {
    let mut engine = Engine::new();
    engine.set_script_runtime(Box::new(QuickJsScriptRuntime::new()));
    let profile = Profile::named("serve")?;
    let mut session = RuntimeSession::new(engine, profile.clone());
    register_profile(&profile, session.host_mut(), None)?;
    // Load the scene before starting simulation so script startup and the
    // Startup stage observe the loaded world, like the windowed client.
    if let Some(scene_path) = scene_path {
        session.load_scene(scene_path)?;
    }
    if session
        .host_mut()
        .has_plugin(moni_core::host::CORE_SIMULATION)
    {
        session
            .host_mut()
            .start_plugin(moni_core::host::CORE_SIMULATION)?;
        session.host_mut().run_stage(SystemStage::Startup)?;
    }
    Ok(session)
}

/// Builds the windowed-client session following the directive composition
/// order: register the profile's plugins (no window — `core.render` stays
/// registered-but-unstarted), load and validate the scene against the
/// effective registry, start the simulation plugin, then run the Startup
/// stage. The render plugin is started later once a window exists.
///
/// `scene_path` is workspace-relative as required by
/// `RuntimeSession::load_scene`; `None` builds an empty world.
pub fn client_session(
    scene_path: Option<&str>,
    profile: Profile,
    hot_reload: bool,
) -> anyhow::Result<RuntimeSession> {
    // Forward the hot-reload flag through an ordered profile patch. A config
    // patch enables its plugin, so only add it when core.render is actually
    // in the effective tree.
    let profile = if hot_reload && profile.resolve(&Target::current())?.contains("core.render") {
        profile.with_config("core.render", serde_json::json!({ "hotReload": true }))
    } else {
        profile
    };
    let mut engine = Engine::new();
    engine.set_script_runtime(Box::new(QuickJsScriptRuntime::new()));
    let mut session = RuntimeSession::new(engine, profile.clone());
    register_profile(&profile, session.host_mut(), None)?;
    if let Some(scene_path) = scene_path {
        session.load_scene(scene_path)?;
    }
    // Start simulation if the profile provides it; render deliberately stays
    // unstarted until the windowed frontend attaches a surface.
    if session
        .host_mut()
        .has_plugin(moni_core::host::CORE_SIMULATION)
    {
        session
            .host_mut()
            .start_plugin(moni_core::host::CORE_SIMULATION)?;
    }
    session.host_mut().run_stage(SystemStage::Startup)?;
    Ok(session)
}

/// Builds a headless inspection session: register the (typically empty)
/// profile, load the scene into the validated session world, and start
/// nothing. Forbidden runtime providers therefore stay registered-only at
/// most, and `has_plugin` checks in the caller keep their meaning.
pub fn headless_session(profile_name: &str, scene_path: &str) -> anyhow::Result<RuntimeSession> {
    let profile = Profile::named(profile_name)?;
    let engine = Engine::new();
    let mut session = RuntimeSession::new(engine, profile.clone());
    register_profile(&profile, session.host_mut(), None)?;
    session.load_scene(scene_path)?;
    Ok(session)
}

/// Registration phase only: constructs every instance plugin in the profile's
/// effective tree, in dependency order, and registers it with
/// [`PluginHost::register_plugin`]. No plugin is started. Capability
/// providers (core.fbx, core.editor) contribute only permissions. If a window
/// is supplied it is attached to the render plugin via `set_window`; without
/// one the render plugin registers headlessly and fails only when started.
///
/// On any failure the host is restored to its pre-call state: constructed
/// handles are disposed in reverse order and capability grants are revoked,
/// preserving the old instantiate contract.
pub fn register_profile(
    profile: &Profile,
    host: &mut PluginHost,
    window: Option<Arc<Window>>,
) -> anyhow::Result<Vec<PluginHandle>> {
    let effective = profile.resolve(&Target::current())?;
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
    // state behind. core.render is constructed window-free; the profile
    // config (hotReload, collectGpuTimestamps) is applied through configure.
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
                let mut instance = RenderPlugin::new(false, effective.contains("core.fbx"), false);
                if let Some(window) = &window {
                    instance.set_window(Arc::clone(window));
                }
                let mut instance = Box::new(instance) as Box<dyn Plugin>;
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
        match host.register_plugin(plugin, &permissions) {
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

/// Startup phase: starts every registered handle in order via
/// [`PluginHost::start_plugin`]. On failure the error propagates unchanged;
/// rollback is the caller's decision (see [`instantiate`], which unregisters
/// everything it registered when a start fails).
// Part of the composition-root contract and exercised by bootstrap tests;
// the session builders call `start_plugin` directly for per-plugin control.
#[allow(dead_code)]
pub fn start_all(handles: &[PluginHandle], host: &mut PluginHost) -> anyhow::Result<()> {
    for handle in handles {
        host.start_plugin(handle.id())?;
    }
    Ok(())
}

/// Combined convenience path: [`register_profile`] then [`start_all`], with
/// full rollback on start failure — every registered handle is disposed in
/// reverse order and capability grants are revoked — so existing bootstrap
/// tests keep their all-or-nothing semantics. Prefer the two-phase functions
/// when composition must run between registration and start.
///
/// `collect_gpu_timestamps` and `hot_reload` are forwarded to the render
/// plugin as ordered profile patches; they are ignored when core.render is
/// not in the effective tree (a config patch would re-enable it).
// Kept as the documented combined path so existing bootstrap tests and
// simple callers keep their all-or-nothing semantics.
#[allow(dead_code)]
pub fn instantiate(
    profile: &Profile,
    host: &mut PluginHost,
    window: Option<Arc<Window>>,
    collect_gpu_timestamps: bool,
    hot_reload: bool,
) -> anyhow::Result<Vec<PluginHandle>> {
    let mut profile = profile.clone();
    if profile.resolve(&Target::current())?.contains("core.render") {
        profile = profile.with_config(
            "core.render",
            serde_json::json!({
                "collectGpuTimestamps": collect_gpu_timestamps,
                "hotReload": hot_reload,
            }),
        );
    }
    let handles = register_profile(&profile, host, window)?;
    if let Err(error) = start_all(&handles, host) {
        for handle in handles.into_iter().rev() {
            let _ = handle.dispose(host);
        }
        return Err(error);
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

    #[test]
    fn register_profile_leaves_plugins_registered_but_unstarted() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default")
            .expect("default profile")
            .with_patch("core.render", false);
        let handles = register_profile(&profile, &mut host, None).expect("register profile");
        assert_eq!(handles.len(), 1);
        assert!(host.has_plugin("core.simulation"));
        assert!(
            !host.is_started("core.simulation"),
            "register_profile must not start plugins"
        );
        // The unstarted simulation system does not exist yet, so a stage run
        // is a no-op instead of ticking the world.
        host.run_stage(SystemStage::FixedUpdate)
            .expect("empty stage");
        assert_eq!(host.world().tick, 0);
        start_all(&handles, &mut host).expect("start all");
        assert!(host.is_started("core.simulation"));
        host.run_stage(SystemStage::FixedUpdate).expect("ticking");
        assert_eq!(host.world().tick, 1);
    }

    #[test]
    fn start_all_failure_propagates_without_rollback() {
        let mut host = PluginHost::new(Engine::new());
        let profile = Profile::named("default")
            .expect("default profile")
            .with_patch("core.fbx", false);
        // core.render registers headlessly but cannot start without a window.
        // Dependency order starts it first, so the error propagates before
        // core.simulation is reached and nothing is unregistered: callers
        // that need all-or-nothing semantics use instantiate.
        let handles = register_profile(&profile, &mut host, None).expect("register profile");
        assert_eq!(handles.len(), 2);
        let error = start_all(&handles, &mut host).expect_err("render start needs a window");
        assert!(error.to_string().contains("requires a window"));
        assert!(host.has_plugin("core.simulation"));
        assert!(host.has_plugin("core.render"));
        assert!(!host.is_started("core.render"));
    }
}
