//! egui editor panels over the existing wgpu renderer. The editor owns the
//! egui `Context` and the `egui_winit` input bridge; every frame the panels
//! run against the [`EditorDocument`] and the produced `egui::FullOutput` is
//! handed to the render plugin through the type-erased render API, so the
//! wgpu renderer composites the UI as its last pass. All edits go through the
//! document API with the current session revision; rejections surface as an
//! error line instead of breaking selection or history.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use moni_core::components::{PropertyField, PropertySchema, PropertyType};
use moni_core::engine::{EntityId, EntitySpec, MAX_SCRIPT_BYTES};
use moni_core::scene3d::{MeshKind, Model3d, Transform3d};
use serde_json::{Value, json};
use winit::event::WindowEvent;
use winit::window::Window;

use super::document::EditorDocument;

/// The UI frame payload handed to the render plugin through the type-erased
/// render API: the frame's tessellated primitives, its texture deltas, and the
/// pixels-per-point scale for the screen descriptor. Tessellation runs here,
/// on the context that produced the shapes: its font atlas normalizes the
/// glyph UVs, so a renderer-side context cannot tessellate correctly.
pub type EguiFrame = (Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32);

/// The core-registry owner marker; components owned by core are engine
/// reserved and never offered as attachable dynamic components.
const CORE_OWNER: &str = "moni.core";

/// egui context, winit input bridge, and panel state for the editor UI.
pub struct EditorUi {
    /// The window the UI draws onto; egui-winit needs it for input gathering
    /// and platform output (cursor, clipboard, IME).
    window: Arc<Window>,
    context: egui::Context,
    state: egui_winit::State,
    panels: Panels,
}

/// Stateful widget buffers for the panels. Kept separate from the egui
/// machinery so a single egui pass can borrow both at once.
struct Panels {
    /// Last rejected edit, shown as an error line until the next success.
    error: Option<String>,
    /// Buffer for the create-entity name field.
    new_entity_name: String,
    /// Save-as target path buffer.
    save_as_path: String,
    /// Component id currently chosen in the add-component combo box.
    add_component: Option<String>,
    /// Rename buffers per entity, seeded from the entity's current name.
    renames: BTreeMap<EntityId, String>,
    /// Inspector name buffer and the entity it belongs to.
    inspector_name: Option<(EntityId, String)>,
    /// Attachable component ids cached per session revision; component
    /// registration is a revision bump, so the cache cannot go stale.
    registered_components: Option<(u64, Vec<String>)>,
    /// Workspace asset listing cached by the asset browser panel.
    asset_browser: AssetBrowser,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            error: None,
            new_entity_name: "entity".to_string(),
            save_as_path: String::new(),
            add_component: None,
            renames: BTreeMap::new(),
            inspector_name: None,
            registered_components: None,
            asset_browser: AssetBrowser::default(),
        }
    }
}

impl EditorUi {
    /// Creates the egui context and the winit input bridge for `window`.
    pub fn new(window: Arc<Window>) -> Self {
        let context = egui::Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        Self {
            window,
            context,
            state,
            panels: Panels::default(),
        }
    }

    /// Feeds a window event through egui-winit before the app-level handlers
    /// run. The response's `consumed` flag reports that egui is using this
    /// pointer/keyboard interaction; `repaint` asks for a fresh frame.
    pub fn on_window_event(&mut self, event: &WindowEvent) -> egui_winit::EventResponse {
        self.state.on_window_event(&self.window, event)
    }

    /// True when an egui widget currently owns the keyboard (text editing).
    pub fn wants_keyboard_input(&self) -> bool {
        self.context.egui_wants_keyboard_input()
    }

    /// Surfaces an error from outside the panel callbacks (for example a
    /// simulation failure that exits play mode) on the shared error line.
    pub fn set_error(&mut self, message: String) {
        self.panels.error = Some(message);
    }

    /// Runs one egui pass: gathers input, builds the panels against
    /// `document`, applies platform output, and tessellates the produced
    /// shapes on this context. Returns the frame payload for the render
    /// plugin's type-erased hand-off.
    pub fn build_frame(&mut self, document: &mut EditorDocument) -> EguiFrame {
        let raw_input = self.state.take_egui_input(&self.window);
        let full_output = self
            .context
            .run_pass(raw_input, |ctx| self.panels.show(ctx, document));
        self.state
            .handle_platform_output(&self.window, full_output.platform_output.clone());
        let pixels_per_point = full_output.pixels_per_point;
        let paint_jobs = self
            .context
            .tessellate(full_output.shapes, pixels_per_point);
        (paint_jobs, full_output.textures_delta, pixels_per_point)
    }
}

impl Panels {
    fn show(&mut self, ctx: &egui::Context, document: &mut EditorDocument) {
        self.handle_shortcuts(ctx, document);
        ctx.root_ui(|ui| {
            self.show_playback(ui, document);
            self.show_hierarchy(ui, document);
            self.show_inspector(ui, document);
            self.show_asset_browser(ui, document);
        });
    }

    /// Ctrl+Z / Ctrl+Y undo-redo and Ctrl+S save. The keys are consumed so a
    /// focused text widget never sees them as well.
    fn handle_shortcuts(&mut self, ctx: &egui::Context, document: &mut EditorDocument) {
        let undo = ctx.input_mut(|input| {
            let modifiers = input.modifiers;
            let shortcut = modifiers.ctrl || modifiers.command;
            shortcut && input.consume_key(modifiers, egui::Key::Z)
        });
        if undo {
            self.run_history(document, |document| document.undo(), "undo");
        }
        let redo = ctx.input_mut(|input| {
            let modifiers = input.modifiers;
            let shortcut = modifiers.ctrl || modifiers.command;
            shortcut && input.consume_key(modifiers, egui::Key::Y)
        });
        if redo {
            self.run_history(document, |document| document.redo(), "redo");
        }
        let save = ctx.input_mut(|input| {
            let modifiers = input.modifiers;
            let shortcut = modifiers.ctrl || modifiers.command;
            shortcut && input.consume_key(modifiers, egui::Key::S)
        });
        if save {
            self.save(document);
        }
    }

    /// Runs one undo/redo step, surfacing failures without touching
    /// selection or history.
    fn run_history(
        &mut self,
        document: &mut EditorDocument,
        step: impl FnOnce(&mut EditorDocument) -> anyhow::Result<Option<&'static str>>,
        action: &'static str,
    ) {
        match step(document) {
            Ok(Some(label)) => {
                self.error = None;
                let _ = label;
            }
            Ok(None) => {}
            Err(error) => self.error = Some(format!("{action} failed: {error:#}")),
        }
    }

    fn save(&mut self, document: &mut EditorDocument) {
        match document.save() {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("save failed: {error:#}")),
        }
    }

    /// Play-mode toolbar: enter/pause/resume/stop buttons, a state
    /// indicator, and the current tick. Edit operations stay clickable but
    /// the document refuses them while playing; the shared error line in the
    /// inspector explains each rejection.
    fn show_playback(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        egui::Panel::top("playback_panel").show(ui, |ui| {
            ui.horizontal(|ui| {
                let playing = document.is_playing();
                if ui
                    .add_enabled(!playing, egui::Button::new("Play"))
                    .clicked()
                {
                    match document.enter_play() {
                        Ok(()) => self.error = None,
                        Err(error) => self.error = Some(format!("play failed: {error:#}")),
                    }
                }
                if ui
                    .add_enabled(playing && !document.is_paused(), egui::Button::new("Pause"))
                    .clicked()
                {
                    document.pause();
                }
                if ui
                    .add_enabled(document.is_paused(), egui::Button::new("Resume"))
                    .clicked()
                {
                    document.resume();
                }
                if ui.add_enabled(playing, egui::Button::new("Stop")).clicked() {
                    match document.exit_play() {
                        Ok(()) => self.error = None,
                        Err(error) => self.error = Some(format!("stop failed: {error:#}")),
                    }
                }
                if document.is_paused() {
                    ui.colored_label(egui::Color32::YELLOW, "PAUSED");
                } else if playing {
                    ui.colored_label(egui::Color32::from_rgb(110, 220, 110), "PLAYING");
                }
                ui.label(format!("tick {}", document.session().world().tick));
                ui.label("(Space toggles play)");
            });
        });
    }

    /// Entity list with create, per-entity delete, and rename; clicking a row
    /// selects it in the document.
    fn show_hierarchy(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        egui::Panel::left("hierarchy_panel").show(ui, |ui| {
            ui.heading("Hierarchy");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_entity_name)
                        .desired_width(110.0)
                        .hint_text("name"),
                );
                if ui.button("Create entity").clicked() {
                    let name = self.new_entity_name.trim().to_string();
                    let spec = EntitySpec {
                        name: Some(name),
                        ..EntitySpec::default()
                    };
                    match document.create_entity(document.session().revision(), spec) {
                        Ok(id) => {
                            self.error = None;
                            document.select([id]);
                        }
                        Err(error) => self.error = Some(format!("create entity failed: {error:#}")),
                    }
                }
            });
            ui.separator();
            // Snapshot first: applying an edit mutates the world while the
            // rows are being built.
            let entities: Vec<(EntityId, String)> = document
                .session()
                .world()
                .entities
                .values()
                .map(|entity| (entity.id, entity.name.clone()))
                .collect();
            for (id, name) in entities {
                let selected = document.selection().contains(&id);
                ui.horizontal(|ui| {
                    if ui.selectable_label(selected, name.as_str()).clicked() {
                        document.select([id]);
                    }
                    if ui.small_button("Delete").clicked() {
                        match document.delete_entity(document.session().revision(), id) {
                            Ok(()) => self.error = None,
                            Err(error) => {
                                self.error = Some(format!("delete entity {id} failed: {error:#}"))
                            }
                        }
                    }
                    let buffer = self.renames.entry(id).or_insert(name.clone());
                    let response = ui.add(egui::TextEdit::singleline(buffer).desired_width(90.0));
                    if response.lost_focus() && *buffer != name && !buffer.trim().is_empty() {
                        match document.rename_entity(
                            document.session().revision(),
                            id,
                            buffer.trim(),
                        ) {
                            Ok(()) => self.error = None,
                            Err(error) => {
                                self.error = Some(format!("rename entity {id} failed: {error:#}"))
                            }
                        }
                    } else if !response.has_focus() && *buffer != name {
                        // The field is idle and the world disagrees (an undo,
                        // a rename from another path): follow the world.
                        *buffer = name;
                    }
                });
            }
        });
    }

    /// Inspector for the first selected entity: name, transform, and
    /// schema-driven fields for every attached dynamic component.
    fn show_inspector(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        egui::Panel::right("inspector_panel").show(ui, |ui| {
            ui.heading("Inspector");
            let Some(selected) = document.selection().iter().next().copied() else {
                ui.label("Select an entity in the hierarchy.");
                self.show_error_line(ui);
                return;
            };
            // Snapshot the editable state; every commit below re-reads the
            // revision and applies through the document API.
            let snapshot = document
                .session()
                .world()
                .entities
                .get(&selected)
                .map(|entity| {
                    (
                        entity.name.clone(),
                        entity.transform,
                        entity
                            .extensions
                            .iter()
                            .map(|(component, value)| (component.clone(), value.clone()))
                            .collect::<Vec<(String, Value)>>(),
                    )
                });
            let Some((name, transform, extensions)) = snapshot else {
                // The selection points at a despawned entity (e.g. after an
                // undo); the document keeps it, so just report it.
                ui.label("The selected entity no longer exists.");
                self.show_error_line(ui);
                return;
            };

            ui.horizontal(|ui| {
                ui.label("Name");
                if self
                    .inspector_name
                    .as_ref()
                    .is_none_or(|(entity, _)| *entity != selected)
                {
                    self.inspector_name = Some((selected, name.clone()));
                }
                let buffer = self
                    .inspector_name
                    .as_mut()
                    .and_then(|(_, buffer)| Some(buffer))
                    .expect("inspector name buffer was just set");
                let response = ui.add(egui::TextEdit::singleline(buffer).desired_width(120.0));
                if response.lost_focus() && *buffer != name && !buffer.trim().is_empty() {
                    match document.rename_entity(
                        document.session().revision(),
                        selected,
                        buffer.trim(),
                    ) {
                        Ok(()) => self.error = None,
                        Err(error) => {
                            self.error = Some(format!("rename entity {selected} failed: {error:#}"))
                        }
                    }
                } else if !response.has_focus() && *buffer != name {
                    *buffer = name;
                }
            });

            ui.add_space(4.0);
            ui.heading("Transform");
            let mut edit = transform;
            let mut commit = false;
            commit |= drag_field(ui, "x", &mut edit.x);
            commit |= drag_field(ui, "y", &mut edit.y);
            commit |= drag_field(ui, "rotation", &mut edit.rotation);
            if commit && edit != transform {
                match document.set_transform(document.session().revision(), selected, edit) {
                    Ok(()) => self.error = None,
                    Err(error) => self.error = Some(format!("set transform failed: {error:#}")),
                }
            }

            ui.add_space(4.0);
            ui.heading("Components");
            for (component, value) in &extensions {
                let descriptor = document
                    .session()
                    .engine()
                    .registry()
                    .resolve(component)
                    .cloned();
                ui.push_id(component.clone(), |ui| match descriptor {
                    // Unregistered components (e.g. future-version data kept
                    // verbatim by a scene load) stay visible but read-only.
                    None => {
                        ui.collapsing(component.clone(), |ui| {
                            ui.monospace(pretty_json(value));
                        });
                    }
                    Some(descriptor) if !descriptor.is_dynamic() => {
                        ui.collapsing(component.clone(), |ui| {
                            ui.monospace(pretty_json(value));
                        });
                    }
                    Some(descriptor) => {
                        self.show_component(
                            ui,
                            document,
                            selected,
                            component,
                            &descriptor.schema,
                            value,
                        );
                    }
                });
            }
            self.show_add_component(ui, document, selected, &extensions);
            ui.separator();
            self.show_history_and_save(ui, document);
            self.show_error_line(ui);
        });
    }

    /// One attached dynamic component: remove button plus schema-driven
    /// fields. Committing a field attaches the whole value again (attach
    /// replaces), producing exactly one undo step.
    fn show_component(
        &mut self,
        ui: &mut egui::Ui,
        document: &mut EditorDocument,
        entity: EntityId,
        component: &str,
        schema: &PropertySchema,
        value: &Value,
    ) {
        ui.horizontal(|ui| {
            ui.strong(component);
            if ui.small_button("Remove").clicked() {
                match document.remove_component(document.session().revision(), entity, component) {
                    Ok(()) => self.error = None,
                    Err(error) => {
                        self.error = Some(format!("remove {component} failed: {error:#}"))
                    }
                }
            }
        });
        // Fields edit a working copy of the component value; commits apply
        // the whole value through attach (which replaces).
        let mut edit = value.clone();
        let mut commit = false;
        for field in &schema.fields {
            commit |= schema_field(ui, field, &mut edit);
        }
        if commit {
            match document.attach_component(document.session().revision(), entity, component, edit)
            {
                Ok(()) => self.error = None,
                Err(error) => self.error = Some(format!("update {component} failed: {error:#}")),
            }
        }
        ui.separator();
    }

    /// Attach UI for a dynamic component chosen from the session registry.
    fn show_add_component(
        &mut self,
        ui: &mut egui::Ui,
        document: &mut EditorDocument,
        entity: EntityId,
        extensions: &[(String, Value)],
    ) {
        ui.add_space(4.0);
        ui.heading("Add component");
        // Every registered component outside the core owner is attachable;
        // core components are engine reserved and already-attached ones are
        // replaced through the inspector instead. The list is cached per
        // session revision (component registration bumps the revision).
        let revision = document.session().revision();
        let registered = match &mut self.registered_components {
            Some((cached_revision, cached)) if *cached_revision == revision => cached.clone(),
            _ => {
                let mut registered: Vec<String> = document
                    .session()
                    .engine()
                    .registry()
                    .agent_schemas()
                    .as_object()
                    .map(|schemas| {
                        schemas
                            .iter()
                            .filter(|(id, schema)| {
                                let core_owned = schema
                                    .get("x-component")
                                    .and_then(|component| component.get("owner"))
                                    .and_then(Value::as_str)
                                    .is_some_and(|owner| owner == CORE_OWNER);
                                !core_owned
                                    && !extensions.iter().any(|(attached, _)| attached == *id)
                            })
                            .map(|(id, _)| id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                registered.sort();
                self.registered_components = Some((revision, registered.clone()));
                registered
            }
        };
        egui::ComboBox::from_id_salt("add_component_combo")
            .selected_text(
                self.add_component
                    .clone()
                    .unwrap_or_else(|| "select".into()),
            )
            .show_ui(ui, |ui| {
                for id in &registered {
                    ui.selectable_value(&mut self.add_component, Some(id.clone()), id.clone());
                }
            });
        let chosen = self
            .add_component
            .as_ref()
            .filter(|id| registered.contains(*id))
            .cloned();
        if let Some(component) = chosen
            && ui.button("Attach").clicked()
        {
            let Some(descriptor) = document.session().engine().registry().resolve(&component)
            else {
                return;
            };
            let value = default_component_value(&descriptor.schema);
            match document.attach_component(
                document.session().revision(),
                entity,
                &component,
                value,
            ) {
                Ok(()) => self.error = None,
                Err(error) => self.error = Some(format!("attach {component} failed: {error:#}")),
            }
        }
    }

    /// Undo/redo buttons with depths, dirty indicator, save, and save-as.
    fn show_history_and_save(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        ui.horizontal(|ui| {
            if ui
                .button(format!("Undo ({})", document.undo_depth()))
                .clicked()
            {
                self.run_history(document, |document| document.undo(), "undo");
            }
            if ui
                .button(format!("Redo ({})", document.redo_depth()))
                .clicked()
            {
                self.run_history(document, |document| document.redo(), "redo");
            }
        });
        ui.horizontal(|ui| {
            if document.is_dirty() {
                ui.colored_label(egui::Color32::YELLOW, "unsaved changes");
            }
            // Play state is transient and unsavable; the document refuses
            // anyway, so the button just does not offer the broken path.
            if ui
                .add_enabled(
                    document.is_dirty() && !document.is_playing(),
                    egui::Button::new("Save"),
                )
                .clicked()
            {
                self.save(document);
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.save_as_path)
                    .desired_width(120.0)
                    .hint_text("scenes/copy.mnsc"),
            );
            if ui.button("Save as").clicked() {
                match document.save_as(self.save_as_path.trim()) {
                    Ok(()) => self.error = None,
                    Err(error) => self.error = Some(format!("save as failed: {error:#}")),
                }
            }
        });
    }

    /// Bottom asset browser: a flat categorized listing of the workspace's
    /// scenes, models, and scripts with open/spawn/attach actions. The
    /// listing is cached and rebuilt on session revision changes outside
    /// play mode, or from the Refresh button — never per frame.
    fn show_asset_browser(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        egui::Panel::bottom("asset_browser_panel").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Assets");
                if ui.small_button("Refresh").clicked() {
                    self.asset_browser.cached_revision = Some(document.session().revision());
                    self.asset_browser.listing = Some(walk_workspace_assets());
                }
                let truncated = self
                    .asset_browser
                    .listing
                    .as_ref()
                    .is_some_and(|listing| listing.truncated);
                if truncated {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!("listing truncated at {ASSET_WALK_MAX_ENTRIES} entries"),
                    );
                }
            });
            self.refresh_assets(document);
            if self.asset_browser.listing.is_none() {
                return;
            }
            // The listing is owned locally while the closure runs so the
            // action handlers can borrow the panels mutably; it is put back
            // afterwards, keeping the cache across frames.
            let listing = self
                .asset_browser
                .listing
                .take()
                .expect("checked directly above");
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .show(ui, |ui| {
                    if !listing.scenes.is_empty() {
                        ui.label(egui::RichText::new("Scenes").strong());
                    }
                    for entry in &listing.scenes {
                        ui.horizontal(|ui| {
                            ui.label(&entry.name).on_hover_text(&entry.path);
                            if ui.small_button("Open").clicked() {
                                match document.open_scene(&entry.path) {
                                    Ok(()) => self.error = None,
                                    Err(error) => {
                                        self.error = Some(format!("open scene failed: {error:#}"))
                                    }
                                }
                            }
                        });
                    }
                    if !listing.models.is_empty() {
                        ui.label(egui::RichText::new("Models").strong());
                    }
                    for entry in &listing.models {
                        ui.horizontal(|ui| {
                            ui.label(&entry.name).on_hover_text(&entry.path);
                            if ui.small_button("Spawn").clicked() {
                                self.spawn_model(document, &entry.path);
                            }
                        });
                    }
                    if !listing.scripts.is_empty() {
                        ui.label(egui::RichText::new("Scripts").strong());
                    }
                    for entry in &listing.scripts {
                        ui.horizontal(|ui| {
                            ui.label(&entry.name).on_hover_text(&entry.path);
                            if ui.small_button("Attach to selected").clicked() {
                                self.attach_script_asset(document, &entry.path);
                            }
                        });
                    }
                    if listing.scenes.is_empty()
                        && listing.models.is_empty()
                        && listing.scripts.is_empty()
                    {
                        ui.label("no scenes, models, or scripts found in the workspace");
                    }
                });
            self.asset_browser.listing = Some(listing);
            ui.add_space(2.0);
        });
    }

    /// Rebuilds the asset listing when the session revision moved, except
    /// while playing: play ticks bump the revision every frame, so a playing
    /// session keeps its cached listing until stopped or refreshed.
    fn refresh_assets(&mut self, document: &mut EditorDocument) {
        let revision = document.session().revision();
        if self.asset_browser.listing.is_some()
            && (self.asset_browser.cached_revision == Some(revision) || document.is_playing())
        {
            return;
        }
        self.asset_browser.cached_revision = Some(revision);
        self.asset_browser.listing = Some(walk_workspace_assets());
    }

    /// Spawns an entity carrying the model asset as an FBX component. Both
    /// `.fbx` and cooked `.mnec` sources pass the engine's `Model3d`
    /// validation for `kind = fbx` (workspace-relative source ending in
    /// `.fbx` or `.mnec`), so both asset kinds spawn.
    fn spawn_model(&mut self, document: &mut EditorDocument, path: &str) {
        let name = file_stem(basename(path));
        let spec = EntitySpec {
            name: Some(name),
            model: Some(Model3d {
                kind: MeshKind::Fbx,
                source: Some(path.to_string()),
                transform: Transform3d::default(),
                color: "#ffffff".to_string(),
                metallic: 0.0,
                roughness: 0.5,
            }),
            ..EntitySpec::default()
        };
        match document.create_entity(document.session().revision(), spec) {
            Ok(id) => {
                self.error = None;
                document.select([id]);
            }
            Err(error) => self.error = Some(format!("spawn model failed: {error:#}")),
        }
    }

    /// Reads a script asset and attaches it to the first selected entity.
    /// The size is capped before reading so a huge file never reaches
    /// memory; the engine's own `script_too_large` guard stays the
    /// authoritative check.
    fn attach_script_asset(&mut self, document: &mut EditorDocument, path: &str) {
        let Some(selected) = document.selection().iter().next().copied() else {
            self.error = Some("select an entity in the hierarchy first".to_string());
            return;
        };
        let resolved = match moni_core::workspace_path(path) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.error = Some(format!("invalid script path {path}: {error}"));
                return;
            }
        };
        match std::fs::metadata(&resolved) {
            Ok(metadata) if metadata.len() > MAX_SCRIPT_BYTES as u64 => {
                self.error = Some(format!(
                    "script_too_large: {path} exceeds {MAX_SCRIPT_BYTES} bytes"
                ));
                return;
            }
            Err(error) => {
                self.error = Some(format!("could not read script {path}: {error}"));
                return;
            }
            Ok(_) => {}
        }
        let source = match std::fs::read_to_string(&resolved) {
            Ok(source) => source,
            Err(error) => {
                self.error = Some(format!("could not read script {path}: {error}"));
                return;
            }
        };
        match document.attach_script(document.session().revision(), selected, source) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("attach script failed: {error:#}")),
        }
    }

    /// The last rejected edit as a plain error line; it clears on the next
    /// successful edit.
    fn show_error_line(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(255, 110, 110), error);
        }
    }
}

/// One transform field: a drag value whose edit reaches the document on
/// change-commit (drag release or focus loss), not on every drag frame.
fn drag_field(ui: &mut egui::Ui, label: &'static str, value: &mut f64) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui.add(egui::DragValue::new(value).speed(0.1));
        response.drag_stopped() || response.lost_focus()
    })
    .inner
}

/// One schema-driven field row. Returns true when the field's value changed
/// and the edit is committed. Widgets are picked by property type: number
/// ranges become drag bounds, enum variants a combo box, color/path/string
/// text inputs, and object fields raw JSON text.
fn schema_field(ui: &mut egui::Ui, field: &PropertyField, value: &mut Value) -> bool {
    ui.horizontal(|ui| {
        ui.label(field.label).on_hover_text(field.description);
        match &field.kind {
            PropertyType::Number => {
                let mut number = value.as_f64().unwrap_or(0.0);
                let mut drag = egui::DragValue::new(&mut number).speed(0.1);
                if let Some(range) = numeric_range(field) {
                    drag = drag.range(range);
                }
                let response = ui.add(drag);
                if (response.drag_stopped() || response.lost_focus())
                    && number != value.as_f64().unwrap_or(0.0)
                {
                    *value = json!(number);
                    return true;
                }
            }
            PropertyType::Integer => {
                let mut number = value.as_i64().unwrap_or(0);
                let mut drag = egui::DragValue::new(&mut number).speed(1.0);
                if let Some(range) = numeric_range(field) {
                    drag = drag.range(range.start().floor() as i64..=range.end().ceil() as i64);
                }
                let response = ui.add(drag);
                if (response.drag_stopped() || response.lost_focus())
                    && number != value.as_i64().unwrap_or(0)
                {
                    *value = json!(number);
                    return true;
                }
            }
            PropertyType::Enum(variants) => {
                let current = value.as_str().unwrap_or_default().to_string();
                let mut picked = current.clone();
                egui::ComboBox::from_id_salt(field.name)
                    .selected_text(picked.clone())
                    .show_ui(ui, |ui| {
                        for variant in variants {
                            ui.selectable_value(&mut picked, (*variant).to_string(), *variant);
                        }
                    });
                if picked != current {
                    *value = Value::String(picked);
                    return true;
                }
            }
            PropertyType::String | PropertyType::Color | PropertyType::Path => {
                let mut text = value.as_str().unwrap_or_default().to_string();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .desired_width(110.0)
                        .hint_text(if field.required { "required" } else { "value" }),
                );
                if response.lost_focus() && text != value.as_str().unwrap_or_default() {
                    *value = Value::String(text);
                    return true;
                }
            }
            PropertyType::Object => {
                // Object fields edit as raw JSON text; a parse failure simply
                // does not commit.
                let mut text = serde_json::to_string(value).unwrap_or_default();
                let response = ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0));
                if response.lost_focus()
                    && let Ok(parsed) = serde_json::from_str::<Value>(&text)
                    && parsed != *value
                {
                    *value = parsed;
                    return true;
                }
            }
        }
        false
    })
    .inner
}

/// The schema's numeric bounds as a drag range, when any bound is present.
fn numeric_range(field: &PropertyField) -> Option<std::ops::RangeInclusive<f64>> {
    match (field.minimum, field.maximum) {
        (Some(minimum), Some(maximum)) => Some(minimum..=maximum),
        (Some(minimum), None) => Some(minimum..=f64::INFINITY),
        (None, Some(maximum)) => Some(f64::NEG_INFINITY..=maximum),
        (None, None) => None,
    }
}

/// Builds an attach value for `schema`: schema defaults, plus first-variant
/// prefills for required enums and zero prefills for required numbers that
/// carry no default. The engine validates the result; a rejected attach
/// surfaces as the error line.
fn default_component_value(schema: &PropertySchema) -> Value {
    let mut value = Value::Object(serde_json::Map::new());
    let _ = schema.apply_defaults(&mut value);
    if let Some(object) = value.as_object_mut() {
        for field in &schema.fields {
            if !field.required || object.contains_key(field.name) {
                continue;
            }
            let prefill = match &field.kind {
                PropertyType::Enum(variants) => variants
                    .first()
                    .map(|variant| Value::String((*variant).to_string())),
                PropertyType::Number => Some(json!(field.minimum.unwrap_or(0.0))),
                PropertyType::Integer => Some(json!(field.minimum.unwrap_or(0.0) as i64)),
                _ => None,
            };
            if let Some(prefill) = prefill {
                object.insert(field.name.to_string(), prefill);
            }
        }
    }
    value
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The file name portion of a workspace-relative asset path.
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// File name without its extension, used as the spawned entity's name.
fn file_stem(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

/// Maximum directory depth (below the workspace root) the asset walk
/// descends to; files up to three levels deep are listed.
const ASSET_WALK_MAX_DEPTH: usize = 3;

/// Entry cap for the asset walk so a large workspace cannot blow up the
/// panel; the listing reports truncation.
const ASSET_WALK_MAX_ENTRIES: usize = 500;

/// Asset extensions the browser lists, mapped to categories in the walk.
const ASSET_SCENE_EXTENSIONS: [&str; 1] = ["mnsc"];
const ASSET_MODEL_EXTENSIONS: [&str; 2] = ["fbx", "mnec"];
const ASSET_SCRIPT_EXTENSIONS: [&str; 2] = ["js", "ts"];

/// Directory names excluded from the asset walk (build output, package
/// caches, VCS internals).
const ASSET_SKIPPED_DIRECTORIES: [&str; 3] = ["target", "node_modules", ".git"];

/// One listed asset: the file name shown in the panel and the
/// workspace-relative path the actions use.
struct AssetEntry {
    name: String,
    path: String,
}

/// A bounded workspace listing grouped by category.
#[derive(Default)]
struct AssetListing {
    scenes: Vec<AssetEntry>,
    models: Vec<AssetEntry>,
    scripts: Vec<AssetEntry>,
    /// True when the walk stopped at the entry cap.
    truncated: bool,
    /// Unreadable workspace root; individual unreadable directories are
    /// skipped silently.
    error: Option<String>,
}

impl AssetListing {
    fn total(&self) -> usize {
        self.scenes.len() + self.models.len() + self.scripts.len()
    }

    fn push(&mut self, extension: &str, entry: AssetEntry) {
        if self.total() >= ASSET_WALK_MAX_ENTRIES {
            self.truncated = true;
            return;
        }
        let lowered = extension.to_ascii_lowercase();
        if ASSET_SCENE_EXTENSIONS.contains(&lowered.as_str()) {
            self.scenes.push(entry);
        } else if ASSET_MODEL_EXTENSIONS.contains(&lowered.as_str()) {
            self.models.push(entry);
        } else if ASSET_SCRIPT_EXTENSIONS.contains(&lowered.as_str()) {
            self.scripts.push(entry);
        }
    }
}

/// Cached workspace listing for the asset browser. `cached_revision` records
/// the session revision the listing was built at.
#[derive(Default)]
struct AssetBrowser {
    cached_revision: Option<u64>,
    listing: Option<AssetListing>,
}

/// Walks the workspace root (the current directory) for scene, model, and
/// script files up to [`ASSET_WALK_MAX_DEPTH`] levels deep, skipping build
/// and VCS directories. Flat and bounded: directories beyond the depth cap,
/// unreadable directories, and entries past the cap are skipped.
fn walk_workspace_assets() -> AssetListing {
    let mut listing = AssetListing::default();
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            listing.error = Some(format!("could not read the workspace root: {error}"));
            return listing;
        }
    };
    walk_assets(&root, &root, 0, &mut listing);
    listing
}

fn walk_assets(root: &Path, directory: &Path, depth: usize, listing: &mut AssetListing) {
    if listing.truncated {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if listing.truncated {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if file_type.is_dir() {
            if depth >= ASSET_WALK_MAX_DEPTH || ASSET_SKIPPED_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            // `examples/dist` holds built demo bundles; skipping every
            // directory named `dist` would over-exclude, so the path is
            // matched exactly.
            let relative = relative_workspace_path(root, &path);
            if relative == "examples/dist" || relative.starts_with("examples/dist/") {
                continue;
            }
            walk_assets(root, &path, depth + 1, listing);
        } else if file_type.is_file() {
            let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
                continue;
            };
            let lowered = extension.to_ascii_lowercase();
            let listed = ASSET_SCENE_EXTENSIONS.contains(&lowered.as_str())
                || ASSET_MODEL_EXTENSIONS.contains(&lowered.as_str())
                || ASSET_SCRIPT_EXTENSIONS.contains(&lowered.as_str());
            if !listed {
                continue;
            }
            listing.push(
                &lowered,
                AssetEntry {
                    name,
                    path: relative_workspace_path(root, &path),
                },
            );
        }
    }
}

/// The path relative to the workspace root with `/` separators, so entry
/// paths match the workspace-relative contract of scene loading and model
/// sources on every platform.
fn relative_workspace_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
