//! egui editor panels over the existing wgpu renderer. The editor owns the
//! egui `Context` and the `egui_winit` input bridge; every frame the panels
//! run against the [`EditorDocument`] and the produced `egui::FullOutput` is
//! handed to the render plugin through the type-erased render API, so the
//! wgpu renderer composites the UI as its last pass. All edits go through the
//! document API with the current session revision; rejections surface as an
//! error line instead of breaking selection or history.
//!
//! Viewport interaction (click-to-select, the move gizmo, snapping, the
//! renderer grid toggle, and mouse camera operations) runs at the
//! [`Panels::show`] seam over the free central area between the panels and
//! drives the same document API: drags preview through
//! `EditorDocument::preview_apply` and commit exactly one undo step through
//! `EditorDocument::record_external_edit`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use moni_core::components::{PropertyField, PropertySchema, PropertyType};
use moni_core::engine::{Command, EntityId, EntitySpec, MAX_SCRIPT_BYTES, Transform};
use moni_core::host::CORE_RENDER;
use moni_core::scene3d::{Camera3d, MeshKind, Model3d, Transform3d};
use serde_json::{Value, json};
use winit::event::WindowEvent;
use winit::window::Window;

use super::document::EditorDocument;
use super::project::{self, EditorProject, RecentProject};
use super::scripts::{self, ScriptBuild};
use super::viewport::{
    GIZMO_MODEL_AXIS_LENGTH, GIZMO_SPRITE_AXIS_LENGTH, GizmoTarget, ORBIT_RADIANS_PER_POINT,
    ViewportSpace, closest_gizmo_axis, effective_snap_step, entity_gizmo_origin,
    gizmo_axis_segment, orbit_camera, pan_camera, pick_entity, pointer_axis_world_offset,
    quantize_offset, sprite_axis_segment,
};

/// The core-registry owner marker; components owned by core are engine
/// reserved and never offered as attachable dynamic components.
const CORE_OWNER: &str = "moni.core";

/// Move-gizmo axis colors: X red, Y green, Z blue.
const GIZMO_COLORS: [egui::Color32; 3] = [
    egui::Color32::from_rgb(255, 90, 90),
    egui::Color32::from_rgb(90, 220, 90),
    egui::Color32::from_rgb(100, 150, 255),
];

/// The transform a gizmo drag moves, tagged by entity kind so previews and
/// the commit use the command that actually renders the entity (sprites
/// follow the entity's 2D transform, models their own 3D transform).
#[derive(Debug, Clone, Copy, PartialEq)]
enum GizmoTransform {
    /// Sprite entity: moves the engine 2D transform via `SetTransform`.
    Sprite(Transform),
    /// Model entity: moves the model's 3D transform via `SetModelTransform`.
    Model(Transform3d),
}

impl GizmoTransform {
    /// The value shifted by `offset` along the gizmo axis (0 = x, 1 = y,
    /// 2 = z). Sprite entities have no z; the caller only passes axes the
    /// target exposes.
    fn moved_along_axis(self, axis: usize, offset: f64) -> Self {
        match self {
            GizmoTransform::Sprite(mut transform) => {
                if axis == 0 {
                    transform.x += offset;
                } else {
                    transform.y += offset;
                }
                GizmoTransform::Sprite(transform)
            }
            GizmoTransform::Model(mut transform) => {
                let component = match axis {
                    0 => &mut transform.position.x,
                    1 => &mut transform.position.y,
                    _ => &mut transform.position.z,
                };
                *component += offset;
                GizmoTransform::Model(transform)
            }
        }
    }

    /// The preview/forward command carrying this transform value.
    fn command(self, entity: EntityId) -> Command {
        match self {
            GizmoTransform::Sprite(transform) => Command::SetTransform { entity, transform },
            GizmoTransform::Model(transform) => Command::SetModelTransform { entity, transform },
        }
    }
}

/// One in-progress move-gizmo drag: the grabbed axis handle, the transform
/// snapshot at drag start, the accumulated (unsnapped) offset along the
/// axis, and the last preview applied — used to skip no-op applies and to
/// decide the release commit.
struct GizmoDrag {
    entity: EntityId,
    axis: usize,
    target: GizmoTarget,
    start: GizmoTransform,
    offset: f64,
    applied: Option<GizmoTransform>,
}

/// A camera mouse drag started inside the viewport (right-drag orbit,
/// middle-drag pan). Never coexists with a gizmo drag.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CameraDrag {
    Orbit,
    Pan,
}

/// Viewport-interaction state: the gizmo drag, camera mouse drag, grid
/// visibility, and snapping.
struct ViewportState {
    drag: Option<GizmoDrag>,
    camera_drag: Option<CameraDrag>,
    /// Renderer grid visibility; pushed to the render API when toggled and
    /// enabled once at startup (golden mode never constructs this UI, so
    /// golden captures never touch the flag by construction).
    grid_visible: bool,
    grid_initialized: bool,
    /// Snap toggle and step for gizmo movement; snapping off quantizes to
    /// whole units (step 1.0).
    snap_enabled: bool,
    snap_step: f64,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            drag: None,
            camera_drag: None,
            grid_visible: false,
            grid_initialized: false,
            snap_enabled: true,
            snap_step: 0.25,
        }
    }
}

/// The UI frame payload handed to the render plugin through the type-erased
/// render API: the frame's tessellated primitives, its texture deltas, and the
/// pixels-per-point scale for the screen descriptor. Tessellation runs here,
/// on the context that produced the shapes: its font atlas normalizes the
/// glyph UVs, so a renderer-side context cannot tessellate correctly.
pub type EguiFrame = (Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32);

/// A deferred action waiting on the unsaved-changes confirmation modal.
#[derive(Debug, Clone, PartialEq)]
enum PendingAction {
    /// Switch the document to another scene.
    OpenScene(String),
    /// Move the working directory to a project root and open its main scene.
    OpenProject(PathBuf),
    /// Close the editor window (Discard or successful-Save verdict).
    QuitApp,
}

/// Project panel state: the current project, the open/new path inputs, and
/// the cached recent list.
struct ProjectPanel {
    current: Option<EditorProject>,
    open_path: String,
    new_path: String,
    recents: Vec<RecentProject>,
}

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
    /// Project bar state (current project, open/new inputs, recents).
    project: ProjectPanel,
    /// TypeScript build state behind the Scripts panel.
    scripts: ScriptBuild,
    /// Viewport interaction: gizmo drag, camera mouse drag, grid, snapping.
    viewport: ViewportState,
    /// Action waiting on the unsaved-changes confirmation modal.
    pending: Option<PendingAction>,
    /// Exit verdict produced by the confirmation modal; consumed by the
    /// app loop after the frame is submitted.
    quit_requested: bool,
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
            project: ProjectPanel {
                current: None,
                open_path: String::new(),
                new_path: String::new(),
                recents: Vec::new(),
            },
            scripts: ScriptBuild::new(false),
            viewport: ViewportState::default(),
            pending: None,
            quit_requested: false,
        }
    }
}

impl EditorUi {
    /// Creates the egui context and the winit input bridge for `window`.
    /// `project` is the startup project detected by the composition root
    /// (`None` is workspace mode); with one present the recent list is
    /// seeded and the one-shot auto-build is scheduled when `auto_build`
    /// says the run is interactive (automated `--frames` runs never spawn
    /// builds).
    pub fn new(window: Arc<Window>, project: Option<EditorProject>, auto_build: bool) -> Self {
        let context = egui::Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let mut panels = Panels::default();
        if let Some(project) = &project {
            panels.project.current = Some(project.clone());
            panels.record_recent(project);
            panels.scripts.auto_requested = auto_build;
        }
        if panels.project.recents.is_empty()
            && let Some(config_dir) = project::user_config_dir()
        {
            panels.project.recents = project::load_recent_projects(&config_dir);
        }
        Self {
            window,
            context,
            state,
            panels,
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

    /// Raises the unsaved-changes confirmation for a window close request.
    /// A no-op when a confirmation is already pending; the open modal
    /// handles the close as one of its outcomes.
    pub fn request_close_confirmation(&mut self) {
        self.panels.request_quit();
    }

    /// Consumes the confirmation modal's exit verdict (`Discard`, or `Save`
    /// after a successful save). True exactly once, checked by the app loop
    /// after the frame that showed the modal is submitted.
    pub fn take_quit_request(&mut self) -> bool {
        std::mem::take(&mut self.panels.quit_requested)
    }

    /// The project currently open, if any; follows in-app project opens.
    pub fn project(&self) -> Option<&EditorProject> {
        self.panels.project.current.as_ref()
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
        // Drain the build completion channel every frame; a finished build
        // updates status, output tail, and diagnostics before the panels
        // render.
        self.scripts.poll();
        // The renderer grid is enabled once, on the first UI frame, and
        // toggled from the toolbar afterwards. Golden mode never constructs
        // this UI, so golden captures never touch the grid flag by
        // construction.
        if !self.viewport.grid_initialized {
            self.viewport.grid_initialized = true;
            self.viewport.grid_visible = true;
            self.apply_grid_visibility(document);
        }
        ctx.root_ui(|ui| {
            self.show_playback(ui, document);
            self.show_project(ui, document);
            self.show_hierarchy(ui, document);
            self.show_inspector(ui, document);
            self.show_asset_browser(ui, document);
            self.show_scripts(ui, document);
            // Viewport interaction at the panel seam: picking, the move
            // gizmo, and mouse camera ops drive the same document API from
            // the free central area the panels leave open.
            self.show_viewport(ui, document);
        });
        // The confirmation modal is a floating egui window, so it renders
        // outside root_ui and can take over after any panel raised a
        // pending action this frame.
        self.show_confirmation_modal(ctx, document);
    }

    /// Raises the unsaved-changes modal for a window close request. A no-op
    /// when a confirmation is already pending (the open modal handles the
    /// close as one of its outcomes).
    fn request_quit(&mut self) {
        self.pending.get_or_insert(PendingAction::QuitApp);
    }

    /// Runs the action the user confirmed in the modal. The pending entry
    /// is consumed first so a failure inside the action cannot loop the
    /// modal; failures surface on the error line and the document stays as
    /// it was.
    fn perform_pending(&mut self, document: &mut EditorDocument) {
        let Some(action) = self.pending.take() else {
            return;
        };
        match action {
            PendingAction::OpenScene(path) => self.perform_scene_open(document, &path),
            PendingAction::OpenProject(root) => self.perform_project_open(document, &root),
            PendingAction::QuitApp => self.quit_requested = true,
        }
    }

    /// The unsaved-changes confirmation modal (a plain egui window):
    /// Save runs the document save and only proceeds on success, Discard
    /// proceeds without saving, Cancel aborts the pending action. A failed
    /// save keeps the modal open with the error surfaced.
    fn show_confirmation_modal(&mut self, ctx: &egui::Context, document: &mut EditorDocument) {
        if self.pending.is_none() {
            return;
        }
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("The current scene has unsaved changes.");
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        match document.save() {
                            Ok(()) => {
                                self.error = None;
                                self.perform_pending(document);
                            }
                            Err(error) => {
                                self.error = Some(format!(
                                    "save failed: {error:#} — fix it or choose Discard/Cancel"
                                ));
                            }
                        }
                    }
                    if ui.button("Discard").clicked() {
                        self.perform_pending(document);
                    }
                    if ui.button("Cancel").clicked() {
                        self.pending = None;
                    }
                });
            });
    }

    /// Scene switches from the asset browser: while the document is dirty
    /// the unsaved-changes modal intercepts the attempt and proceeds (or
    /// aborts) through the modal's Save/Discard/Cancel; while playing the
    /// document's play lock refuses through the guarded path.
    fn request_scene_open(&mut self, document: &mut EditorDocument, path: &str) {
        if document.is_playing() {
            self.perform_scene_open(document, path);
            return;
        }
        if document.is_dirty() {
            self.pending
                .get_or_insert(PendingAction::OpenScene(path.to_string()));
            return;
        }
        self.perform_scene_open(document, path);
    }

    fn perform_scene_open(&mut self, document: &mut EditorDocument, path: &str) {
        match document.open_scene(path) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("open scene failed: {error:#}")),
        }
    }

    /// Opens a project from the UI (input field, New flow, or recents).
    /// Playing documents refuse first (play state is transient); dirty
    /// documents route through the unsaved-changes modal first.
    fn request_project_open(&mut self, document: &mut EditorDocument, root: &PathBuf) {
        if document.is_playing() {
            self.error = Some("stop play mode before opening a project".to_string());
            return;
        }
        if document.is_dirty() {
            self.pending
                .get_or_insert(PendingAction::OpenProject(root.clone()));
            return;
        }
        self.perform_project_open(document, root);
    }

    /// The verified open flow: load and validate the project, move the
    /// process working directory to the project root (process-wide: every
    /// workspace-relative path — documents, assets, scripts, saves —
    /// resolves against the root from here on), then rebuild the document
    /// through the guarded `open_scene` path with the project's main scene.
    /// On success the project bar, recent list, and one auto-build request
    /// are updated. A failed scene open after the chdir surfaces the error
    /// and keeps the document on its previous scene.
    fn perform_project_open(&mut self, document: &mut EditorDocument, root: &PathBuf) {
        let project = match project::EditorProject::load_at(root) {
            Ok(project) => project,
            Err(error) => {
                self.error = Some(format!("open project failed: {error:#}"));
                return;
            }
        };
        if let Err(error) = std::env::set_current_dir(&project.root) {
            self.error = Some(format!(
                "could not enter the project root {}: {error}",
                project.root.display()
            ));
            return;
        }
        match document.open_scene(&project.main_scene) {
            Ok(()) => {
                self.error = None;
                self.project.current = Some(project.clone());
                self.project.open_path.clear();
                self.record_recent(&project);
                self.scripts.auto_requested = true;
            }
            Err(error) => {
                self.error = Some(format!("open project scene failed: {error:#}"));
            }
        }
    }

    /// Creates a project through the isolated `moni new` child process and
    /// opens it. The path is absolutized against the current directory
    /// before the child runs, so the created location is unambiguous
    /// regardless of the child's working directory.
    fn perform_new_project(&mut self, document: &mut EditorDocument, raw: &str) {
        if raw.is_empty() {
            self.error = Some("enter a directory path for the new project".to_string());
            return;
        }
        let root = match std::env::current_dir() {
            Ok(cwd) => cwd.join(raw),
            Err(error) => {
                self.error = Some(format!("could not read the working directory: {error}"));
                return;
            }
        };
        if let Err(error) = project::spawn_new_project(&root) {
            self.error = Some(format!("new project failed: {error:#}"));
            return;
        }
        self.request_project_open(document, &root);
    }

    /// Records a successfully opened project in the persisted recent list
    /// and refreshes the panel cache. Failures (no user config directory,
    /// unwritable file) surface on the error line without blocking.
    fn record_recent(&mut self, project: &EditorProject) {
        let Some(config_dir) = project::user_config_dir() else {
            return;
        };
        match project::record_recent_project(
            &config_dir,
            &project.root.to_string_lossy(),
            &project.name,
        ) {
            Ok(recents) => {
                self.project.recents = recents;
                self.error = None;
            }
            Err(error) => {
                self.error = Some(format!("could not record the recent project: {error:#}"))
            }
        }
    }

    /// Project bar: the current project (or workspace mode), open/new
    /// project inputs, and the recent list.
    fn show_project(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        egui::Panel::top("project_panel").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Project");
                match &self.project.current {
                    Some(current) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({})",
                                current.name, current.profile_name
                            ))
                            .strong(),
                        );
                        ui.label(current.root.display().to_string()).on_hover_text(
                            "project mode: workspace-relative paths resolve against this root",
                        );
                    }
                    None => {
                        ui.label("no project — workspace mode");
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.project.open_path)
                        .desired_width(220.0)
                        .hint_text("path to a project directory"),
                );
                if ui.button("Open project...").clicked() {
                    let path = self.project.open_path.trim().to_string();
                    if path.is_empty() {
                        self.error = Some("enter a project directory path".to_string());
                    } else {
                        self.request_project_open(document, &PathBuf::from(path));
                    }
                }
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(&mut self.project.new_path)
                        .desired_width(220.0)
                        .hint_text("new project directory"),
                );
                if ui.button("New project...").clicked() {
                    let path = self.project.new_path.trim().to_string();
                    self.perform_new_project(document, &path);
                }
            });
            if !self.project.recents.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Recent:");
                    for entry in self.project.recents.clone() {
                        if ui
                            .small_button(&entry.name)
                            .on_hover_text(&entry.path)
                            .clicked()
                        {
                            self.request_project_open(document, &PathBuf::from(&entry.path));
                        }
                    }
                });
            }
        });
    }

    /// Scripts panel: one `npm run build` per request in the project or
    /// workspace root with non-blocking output capture, the parsed tsc
    /// diagnostics with Locate buttons, and a bounded raw-output tail. A
    /// failed build is never fatal; the engine also validates scene scripts
    /// at load time, so a failed build explains scenes whose scripts refuse
    /// to load.
    fn show_scripts(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        egui::Panel::bottom("scripts_panel").show(ui, |ui| {
            // Consume the auto-build request raised when a project opened.
            if self.scripts.auto_requested {
                self.scripts.auto_requested = false;
                match std::env::current_dir() {
                    Ok(root) => self.scripts.request(&root),
                    Err(error) => {
                        self.scripts.status =
                            Some(format!("could not read the working directory: {error}"))
                    }
                }
            }
            ui.horizontal(|ui| {
                ui.heading("Scripts");
                if ui
                    .add_enabled(!self.scripts.is_running(), egui::Button::new("Build"))
                    .clicked()
                {
                    match std::env::current_dir() {
                        Ok(root) => self.scripts.request(&root),
                        Err(error) => {
                            self.error =
                                Some(format!("could not read the working directory: {error}"))
                        }
                    }
                }
                if self.scripts.is_running() {
                    ui.colored_label(egui::Color32::YELLOW, "building...");
                } else if let Some(status) = &self.scripts.status {
                    ui.label(status);
                }
                ui.label(format!("{} diagnostic(s)", self.scripts.diagnostics.len()));
            });
            if !self.scripts.diagnostics.is_empty() {
                ui.label(egui::RichText::new("Diagnostics").strong());
                // Cloned per frame: the Locate handler mutates panel state
                // while the list is being shown.
                let diagnostics = self.scripts.diagnostics.clone();
                egui::ScrollArea::vertical()
                    .max_height(90.0)
                    .show(ui, |ui| {
                        for (index, diagnostic) in diagnostics.iter().enumerate() {
                            ui.push_id(format!("diagnostic-{index}"), |ui| {
                                ui.horizontal(|ui| {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(255, 110, 110),
                                        format!(
                                            "{}:{}:{} {}: {}",
                                            diagnostic.file,
                                            diagnostic.line,
                                            diagnostic.col,
                                            diagnostic.code,
                                            diagnostic.message
                                        ),
                                    );
                                    // Locate appears only when an entity's
                                    // script source references the file;
                                    // ownerless errors stay non-clickable.
                                    if let Some(entity) = scripts::find_entity_by_script_source(
                                        document.session().world(),
                                        &diagnostic.file,
                                    ) && ui.small_button("Locate").clicked()
                                    {
                                        document.select([entity]);
                                        self.error = None;
                                    }
                                });
                            });
                        }
                    });
            }
            if !self.scripts.output_tail.is_empty() {
                ui.label(egui::RichText::new("Build output").strong());
                egui::ScrollArea::vertical()
                    .max_height(90.0)
                    .show(ui, |ui| {
                        ui.monospace(&self.scripts.output_tail);
                    });
            }
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
                ui.separator();
                if ui
                    .checkbox(&mut self.viewport.grid_visible, "Grid")
                    .on_hover_text("toggle the renderer's floor grid")
                    .changed()
                {
                    self.apply_grid_visibility(document);
                }
                ui.separator();
                ui.checkbox(&mut self.viewport.snap_enabled, "Snap")
                    .on_hover_text(
                        "quantize gizmo movement to the snap step; off quantizes to whole units",
                    );
                ui.add(
                    egui::DragValue::new(&mut self.viewport.snap_step)
                        .speed(0.05)
                        .range(0.05..=8.0)
                        .prefix("step "),
                )
                .on_hover_text("snap step in world units");
            });
        });
    }

    /// Pushes the grid visibility flag to the render provider. A missing
    /// render provider simply keeps the UI flag; the next toggle retries.
    fn apply_grid_visibility(&mut self, document: &mut EditorDocument) {
        let visible = self.viewport.grid_visible;
        if let Some(render) = document
            .session_mut()
            .host_mut()
            .render_api_mut(CORE_RENDER)
        {
            render.set_grid_visible(visible);
        }
    }

    /// Viewport interaction over the free central area between the panels:
    /// click-to-select, the move gizmo with snapping, and mouse camera
    /// operations. The scene fills the whole window behind the panels, so
    /// 3D projection uses the egui screen size while the sprite overlay
    /// maps through the renderer's `world.viewport` space. A gizmo drag
    /// holds the pointer exclusively; camera drags and picking never run
    /// alongside it.
    fn show_viewport(&mut self, ui: &mut egui::Ui, document: &mut EditorDocument) {
        let Some(screen_rect) = ui.ctx().input(|input| input.raw.screen_rect) else {
            return;
        };
        let screen_size = (screen_rect.width(), screen_rect.height());
        let viewport_rect = ui.available_rect_before_wrap();
        let space = ViewportSpace {
            screen_size,
            sprite_size: (
                document.session().world().viewport.width as f32,
                document.session().world().viewport.height as f32,
            ),
        };
        let camera = document.session().world().camera.clone();
        let playing = document.is_playing();

        if self.viewport.drag.is_some() {
            // An active gizmo drag owns the pointer until release.
            self.update_gizmo_drag(ui.ctx(), document, &camera, screen_size, space);
        } else {
            self.update_camera_drag(ui.ctx(), document, &camera, screen_size, viewport_rect);
            self.update_click_selection(
                ui.ctx(),
                document,
                &camera,
                screen_size,
                space,
                viewport_rect,
                playing,
            );
        }
        self.draw_gizmo(
            ui,
            document,
            &camera,
            screen_size,
            space,
            viewport_rect,
            playing,
        );
    }

    /// Continues the active move-gizmo drag: the pointer delta is converted
    /// into movement along the dragged axis (snap-quantized on the
    /// accumulated offset), previewed through the document with history
    /// untouched, and on release committed as exactly one undo step — or,
    /// when the drag ended where it began, restored exactly and recorded
    /// not at all.
    fn update_gizmo_drag(
        &mut self,
        ctx: &egui::Context,
        document: &mut EditorDocument,
        camera: &Camera3d,
        screen_size: (f32, f32),
        space: ViewportSpace,
    ) {
        let pointer_delta = ctx.input(|input| input.pointer.delta());
        let released = ctx.input(|input| {
            input.pointer.button_released(egui::PointerButton::Primary) || !input.pointer.any_down()
        });
        let Some(drag) = self.viewport.drag.as_mut() else {
            return;
        };
        let (entity, axis, target) = (drag.entity, drag.axis, drag.target);

        // The entity (or its rendered transform) may have vanished mid-drag
        // through delete or undo; the drag then ends without a commit.
        let Some((origin, current_target)) = document
            .session()
            .world()
            .entities
            .get(&entity)
            .and_then(entity_gizmo_origin)
        else {
            self.viewport.drag = None;
            return;
        };
        if current_target != target {
            self.viewport.drag = None;
            return;
        }

        if !released {
            // World-axis movement for models, sprite-space movement for
            // sprites: both convert this frame's pointer delta into units
            // of the transform the gizmo moves.
            let delta_world = match target {
                GizmoTarget::Sprite => {
                    let (dx, dy) = space.sprite_delta(pointer_delta.x, pointer_delta.y);
                    if axis == 0 { dx as f64 } else { dy as f64 }
                }
                GizmoTarget::Model => pointer_axis_world_offset(
                    camera,
                    screen_size,
                    origin,
                    axis,
                    GIZMO_MODEL_AXIS_LENGTH,
                    (pointer_delta.x, pointer_delta.y),
                ) as f64,
            };
            drag.offset += delta_world;
            let step = effective_snap_step(self.viewport.snap_enabled, self.viewport.snap_step);
            let snapped = quantize_offset(drag.offset as f32, step) as f64;
            let candidate = drag.start.moved_along_axis(axis, snapped);
            // Apply only when the candidate differs from what the world
            // currently shows (the last preview, or the start): a silent
            // frame must not bump the revision.
            let current = drag.applied.unwrap_or(drag.start);
            if candidate != current {
                match document.preview_apply(candidate.command(entity)) {
                    Ok(()) => drag.applied = Some(candidate),
                    Err(error) => self.error = Some(format!("move preview failed: {error:#}")),
                }
            }
            return;
        }

        // Release: exactly one undo step, or an exact restore with nothing
        // recorded when the drag ended where it began.
        let drag = self.viewport.drag.take().expect("drag checked above");
        let final_transform = drag.applied.unwrap_or(drag.start);
        if final_transform != drag.start {
            let undo_commands = vec![drag.start.command(entity)];
            let redo_commands = vec![final_transform.command(entity)];
            match document.record_external_edit("move entity", undo_commands, redo_commands) {
                Ok(()) => self.error = None,
                Err(error) => self.error = Some(format!("move commit failed: {error:#}")),
            }
        } else if drag.applied.is_some() {
            if let Err(error) = document.preview_apply(drag.start.command(entity)) {
                self.error = Some(format!("move restore failed: {error:#}"));
            }
        }
    }

    /// Mouse camera operations: right-drag orbits around the camera target,
    /// middle-drag pans in the camera plane — applied through the same
    /// session command path as the keyboard camera ops. Drags start only on
    /// a fresh press inside the viewport rect and end when their button is
    /// released; the caller guarantees no gizmo drag is active.
    fn update_camera_drag(
        &mut self,
        ctx: &egui::Context,
        document: &mut EditorDocument,
        camera: &Camera3d,
        screen_size: (f32, f32),
        viewport_rect: egui::Rect,
    ) {
        let pointer_pos = ctx.input(|input| input.pointer.interact_pos());
        let delta = ctx.input(|input| input.pointer.delta());
        let orbit_started =
            ctx.input(|input| input.pointer.button_pressed(egui::PointerButton::Secondary));
        let pan_started =
            ctx.input(|input| input.pointer.button_pressed(egui::PointerButton::Middle));
        let orbit_held =
            ctx.input(|input| input.pointer.button_down(egui::PointerButton::Secondary));
        let pan_held = ctx.input(|input| input.pointer.button_down(egui::PointerButton::Middle));

        if let Some(mode) = self.viewport.camera_drag {
            let held = match mode {
                CameraDrag::Orbit => orbit_held,
                CameraDrag::Pan => pan_held,
            };
            if !held {
                self.viewport.camera_drag = None;
            }
        }
        if self.viewport.camera_drag.is_none()
            && (orbit_started || pan_started)
            && pointer_pos.is_some_and(|pos| viewport_rect.contains(pos))
        {
            self.viewport.camera_drag = Some(if orbit_started {
                CameraDrag::Orbit
            } else {
                CameraDrag::Pan
            });
        }
        let Some(mode) = self.viewport.camera_drag else {
            return;
        };
        if delta.x == 0.0 && delta.y == 0.0 {
            return;
        }
        // Dragging right turns the view right and dragging up raises the
        // camera (grab conventions), so orbit deltas negate the pointer.
        let moved = match mode {
            CameraDrag::Orbit => orbit_camera(
                camera,
                -delta.x * ORBIT_RADIANS_PER_POINT,
                -delta.y * ORBIT_RADIANS_PER_POINT,
            ),
            CameraDrag::Pan => pan_camera(camera, delta.x, delta.y, screen_size.1),
        };
        if let Some(moved) = moved
            && let Err(error) = document
                .session_mut()
                .apply(Command::SetCamera { camera: moved })
        {
            self.error = Some(format!("camera move failed: {error:#}"));
        }
    }

    /// Left-click inside the viewport: grab a gizmo axis handle of the
    /// single selected entity when the pointer is near one, otherwise pick
    /// the entity under the pointer. Selection is document state, so the
    /// hierarchy tree follows automatically; empty space clears it.
    fn update_click_selection(
        &mut self,
        ctx: &egui::Context,
        document: &mut EditorDocument,
        camera: &Camera3d,
        screen_size: (f32, f32),
        space: ViewportSpace,
        viewport_rect: egui::Rect,
        playing: bool,
    ) {
        let pressed = ctx.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary));
        if !pressed {
            return;
        }
        let Some(pos) = ctx.input(|input| input.pointer.interact_pos()) else {
            return;
        };
        if !viewport_rect.contains(pos) {
            return;
        }
        // A widget holding the pointer (a dragged DragValue moved over the
        // viewport, an open combo box) keeps its interaction.
        if ctx.egui_wants_pointer_input() {
            return;
        }
        // Gizmo handles win over picking: grabbing an axis must not
        // deselect or reselect.
        if !playing
            && self.try_start_gizmo_drag(document, camera, screen_size, space, (pos.x, pos.y))
        {
            return;
        }
        match pick_entity(document.session().world(), camera, space, pos.x, pos.y) {
            Some(entity) => document.select([entity]),
            None => document.clear_selection(),
        }
    }

    /// Starts a move-gizmo drag when the pointer presses near an axis
    /// handle of the single selected entity. Returns true when a drag
    /// started.
    fn try_start_gizmo_drag(
        &mut self,
        document: &mut EditorDocument,
        camera: &Camera3d,
        screen_size: (f32, f32),
        space: ViewportSpace,
        pointer: (f32, f32),
    ) -> bool {
        if document.selection().len() != 1 {
            return false;
        }
        let Some(&entity) = document.selection().iter().next() else {
            return false;
        };
        let Some((origin, target)) = document
            .session()
            .world()
            .entities
            .get(&entity)
            .and_then(entity_gizmo_origin)
        else {
            return false;
        };
        let segments = gizmo_screen_segments(target, origin, camera, screen_size, space);
        let Some(axis) = closest_gizmo_axis(&segments, pointer) else {
            return false;
        };
        let Some(start) = gizmo_transform_of(document, entity, target) else {
            return false;
        };
        self.viewport.drag = Some(GizmoDrag {
            entity,
            axis,
            target,
            start,
            offset: 0.0,
            applied: None,
        });
        true
    }

    /// Draws the move gizmo for the single selected entity: colored axis
    /// segments with arrowheads over the viewport, brighter while hovered
    /// or dragged. Hidden while playing (drags are refused there), for
    /// empty or multi selections, and when the entity has no rendered
    /// transform.
    fn draw_gizmo(
        &mut self,
        ui: &mut egui::Ui,
        document: &mut EditorDocument,
        camera: &Camera3d,
        screen_size: (f32, f32),
        space: ViewportSpace,
        viewport_rect: egui::Rect,
        playing: bool,
    ) {
        if playing || document.selection().len() != 1 {
            return;
        }
        let Some(&entity) = document.selection().iter().next() else {
            return;
        };
        let Some((origin, target)) = document
            .session()
            .world()
            .entities
            .get(&entity)
            .and_then(entity_gizmo_origin)
        else {
            return;
        };
        let segments = gizmo_screen_segments(target, origin, camera, screen_size, space);
        let pointer_pos = ui.ctx().input(|input| input.pointer.interact_pos());
        let hovered_axis = match (&self.viewport.drag, pointer_pos) {
            (Some(drag), _) => Some(drag.axis),
            (None, Some(pos)) if viewport_rect.contains(pos) => {
                closest_gizmo_axis(&segments, (pos.x, pos.y))
            }
            _ => None,
        };
        let painter = ui.painter_at(viewport_rect);
        for &(axis, (a, b)) in &segments {
            let active = self
                .viewport
                .drag
                .as_ref()
                .is_some_and(|drag| drag.entity == entity && drag.axis == axis);
            let hovered = hovered_axis == Some(axis) && !active;
            let width = if active {
                3.5
            } else if hovered {
                3.0
            } else {
                2.0
            };
            let color = GIZMO_COLORS[axis];
            painter.line_segment(
                [egui::pos2(a.0, a.1), egui::pos2(b.0, b.1)],
                egui::Stroke::new(width, color),
            );
            draw_arrowhead(&painter, a, b, color);
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
                                // Dirty documents route through the
                                // unsaved-changes modal before the switch.
                                self.request_scene_open(document, &entry.path);
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

/// Screen-space segments of the gizmo axes for `target`: models project
/// through the camera, sprites map from the renderer's sprite space.
fn gizmo_screen_segments(
    target: GizmoTarget,
    origin: [f32; 3],
    camera: &Camera3d,
    screen_size: (f32, f32),
    space: ViewportSpace,
) -> Vec<(usize, ((f32, f32), (f32, f32)))> {
    match target {
        GizmoTarget::Model => target
            .axes()
            .iter()
            .filter_map(|&axis| {
                gizmo_axis_segment(camera, screen_size, origin, axis, GIZMO_MODEL_AXIS_LENGTH)
                    .map(|segment| (axis, segment))
            })
            .collect(),
        GizmoTarget::Sprite => {
            let origin_screen = space.screen_point(origin[0], origin[1]);
            target
                .axes()
                .iter()
                .map(|&axis| {
                    (
                        axis,
                        sprite_axis_segment(origin_screen, axis, GIZMO_SPRITE_AXIS_LENGTH),
                    )
                })
                .collect()
        }
    }
}

/// The transform a gizmo drag moves for `entity`, as the target kind says.
fn gizmo_transform_of(
    document: &EditorDocument,
    entity: EntityId,
    target: GizmoTarget,
) -> Option<GizmoTransform> {
    let entity = document.session().world().entities.get(&entity)?;
    match target {
        GizmoTarget::Model => Some(GizmoTransform::Model(entity.model.as_ref()?.transform)),
        GizmoTarget::Sprite => Some(GizmoTransform::Sprite(entity.transform)),
    }
}

/// Two short segments forming an arrowhead at the tip of the `a` -> `b`
/// segment.
fn draw_arrowhead(painter: &egui::Painter, a: (f32, f32), b: (f32, f32), color: egui::Color32) {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1.0 {
        return;
    }
    let unit = (dx / length, dy / length);
    let perp = (-unit.1, unit.0);
    let base = (b.0 - unit.0 * 10.0, b.1 - unit.1 * 10.0);
    let stroke = egui::Stroke::new(2.0, color);
    painter.line_segment(
        [
            egui::pos2(b.0, b.1),
            egui::pos2(base.0 + perp.0 * 4.0, base.1 + perp.1 * 4.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(b.0, b.1),
            egui::pos2(base.0 - perp.0 * 4.0, base.1 - perp.1 * 4.0),
        ],
        stroke,
    );
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
