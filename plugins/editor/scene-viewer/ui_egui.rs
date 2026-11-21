//! egui editor panels over the existing wgpu renderer. The editor owns the
//! egui `Context` and the `egui_winit` input bridge; every frame the panels
//! run against the [`EditorDocument`] and the produced `egui::FullOutput` is
//! handed to the render plugin through the type-erased render API, so the
//! wgpu renderer composites the UI as its last pass. All edits go through the
//! document API with the current session revision; rejections surface as an
//! error line instead of breaking selection or history.

use std::collections::BTreeMap;
use std::sync::Arc;

use moni_core::components::{PropertyField, PropertySchema, PropertyType};
use moni_core::engine::{EntityId, EntitySpec};
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
            self.show_hierarchy(ui, document);
            self.show_inspector(ui, document);
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
            if ui
                .add_enabled(document.is_dirty(), egui::Button::new("Save"))
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
