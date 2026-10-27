use std::{path::PathBuf, time::Duration};

use crate::codex::{ChatGptAccount, CodexClient, CodexEvent, CodexWorkStatus};

struct Message {
    user: bool,
    text: String,
}

pub struct App {
    root: PathBuf,
    characters: crate::characters::CharacterGallery,
    screenplay: crate::screenplay::Screenplay,
    video: crate::video::VideoWorkspace,
    screenplay_fraction: f32,
    codex_started: bool,
    client: Option<CodexClient>,
    account: Option<ChatGptAccount>,
    checking: bool,
    login_pending: bool,
    chat_open: bool,
    opening: bool,
    ready: bool,
    busy: bool,
    cancelling: bool,
    streaming: bool,
    messages: Vec<Message>,
    draft: String,
    status: String,
    error: Option<String>,
    import_error: Option<String>,
    settings_open: bool,
    settings_error: Option<String>,
    pub quit: bool,
}

impl App {
    pub fn new(root: PathBuf) -> Self {
        Self {
            video: crate::video::VideoWorkspace::new(root.clone()),
            screenplay_fraction: 0.5,
            characters: crate::characters::CharacterGallery::load(&root),
            screenplay: crate::screenplay::Screenplay::load(&root),
            codex_started: false,
            root,
            client: None,
            account: None,
            checking: false,
            login_pending: false,
            chat_open: false,
            opening: false,
            ready: false,
            busy: false,
            cancelling: false,
            streaming: false,
            messages: Vec::new(),
            draft: String::new(),
            status: String::new(),
            error: None,
            import_error: None,
            settings_open: false,
            settings_error: None,
            quit: false,
        }
    }

    fn reconnect(&mut self) {
        self.client = None;
        self.account = None;
        self.checking = true;
        self.login_pending = false;
        self.ready = false;
        self.opening = false;
        self.busy = false;
        self.cancelling = false;
        self.streaming = false;
        self.error = None;
        self.status.clear();
        // A new connection starts a new conversation; keep any unsent draft.
        self.messages.clear();
        match CodexClient::new(&self.root, false) {
            Ok(client) => self.client = Some(client),
            Err(error) => {
                self.checking = false;
                self.error = Some(error);
            }
        }
    }

    fn begin_login(&mut self) {
        if let Some(client) = &self.client {
            match client.begin_chatgpt_login() {
                Ok(()) => {
                    self.login_pending = true;
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn open_chat(&mut self) {
        self.chat_open = true;
        if self.ready || self.opening || self.account.is_none() {
            return;
        }
        if let Some(client) = &self.client {
            match client.open_chat() {
                Ok(()) => {
                    self.opening = true;
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn poll(&mut self) {
        while let Some(event) = self.client.as_ref().and_then(CodexClient::try_recv) {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: CodexEvent) {
        match event {
            CodexEvent::AccountStatus(account) => {
                self.checking = false;
                self.account = account;
                if self.chat_open && self.account.is_some() {
                    self.open_chat();
                }
            }
            CodexEvent::LoginCompleted(account) => {
                self.checking = false;
                self.login_pending = false;
                self.account = Some(account);
                self.error = None;
                if self.chat_open {
                    self.open_chat();
                }
            }
            CodexEvent::BrowserOpened => self.status = "Finish signing in in your browser.".into(),
            CodexEvent::ChatReady => {
                self.ready = true;
                self.opening = false;
            }
            CodexEvent::WorkStatus(status) => {
                self.status = match status {
                    CodexWorkStatus::Thinking => "Thinking…",
                    CodexWorkStatus::Editing => "Editing files…",
                    CodexWorkStatus::Checking => "Checking…",
                    CodexWorkStatus::Working => "Working…",
                }
                .into();
            }
            CodexEvent::ActivityDelta(delta) => {
                // Keep the transcript limited to assistant messages.
                if !delta.is_empty() {
                    self.status = "Thinking…".into();
                }
            }
            CodexEvent::AssistantDelta(delta) => {
                if !self.streaming {
                    self.messages.push(Message {
                        user: false,
                        text: String::new(),
                    });
                    self.streaming = true;
                }
                if let Some(message) = self.messages.last_mut() {
                    message.text.push_str(&delta);
                }
            }
            CodexEvent::AssistantMessage(text) => {
                if self.streaming {
                    if let Some(message) = self.messages.last_mut() {
                        message.text = text;
                    }
                } else {
                    self.messages.push(Message { user: false, text });
                }
                self.streaming = false;
            }
            CodexEvent::ChatTurnCompleted | CodexEvent::ChatTurnCancelled => {
                self.busy = false;
                self.cancelling = false;
                self.streaming = false;
                self.status = if matches!(event, CodexEvent::ChatTurnCancelled) {
                    "Stopped.".into()
                } else {
                    String::new()
                };
            }
            CodexEvent::CancelError(error) => {
                self.cancelling = false;
                self.error = Some(error);
            }
            CodexEvent::ChatError(error) | CodexEvent::Error(error) => {
                self.busy = false;
                self.cancelling = false;
                self.streaming = false;
                self.opening = false;
                self.login_pending = false;
                self.checking = false;
                self.error = Some(error);
            }
            CodexEvent::Unavailable(error) => {
                self.client = None;
                self.account = None;
                self.ready = false;
                self.opening = false;
                self.busy = false;
                self.cancelling = false;
                self.login_pending = false;
                self.checking = false;
                self.error = Some(error);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Clear each frame so the native player hides when chat/setup replaces
        // the workspace, and update its clipping rect after scrolling/resizing.
        self.video.playback_target = None;
        if ui
            .ctx()
            .input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Q))
        {
            self.quit = true;
        }
        self.video.poll(ui.ctx());
        if !self.video.is_ready() {
            self.video.setup_ui(ui);
            return;
        }
        if !self.codex_started {
            self.codex_started = true;
            self.reconnect();
        }
        self.poll();
        // Poll the worker without blocking native input or continuously repainting.
        if self.client.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Settings…").clicked() {
                        ui.close();
                        self.settings_open = true;
                        self.settings_error = None;
                    }
                    if ui
                        .add_enabled(
                            self.screenplay.can_import(),
                            egui::Button::new("Import…"),
                        )
                        .on_hover_text("Import a .fountain file. Undo restores the current screenplay and cast.")
                        .on_disabled_hover_text("Reload the screenplay before importing.")
                        .clicked()
                    {
                        ui.close();
                        self.import_error = match select_fountain(&self.root) {
                            Ok(Some(path)) => self.screenplay.import_fountain(&path).err(),
                            Ok(None) => None,
                            Err(error) => Some(error),
                        };
                    }
                    if ui
                        .add_enabled(self.video.can_resume(), egui::Button::new("Resume…"))
                        .on_hover_text("Continue an output directory using its saved shooting script and shots.")
                        .on_disabled_hover_text("Finish setup and wait for the current video job to stop.")
                        .clicked()
                    {
                        ui.close();
                        self.import_error = match select_resume(&self.root.join("outputs")) {
                            Ok(Some(directory)) => {
                                self.video.resume(directory);
                                None
                            }
                            Ok(None) => None,
                            Err(error) => Some(error),
                        };
                    }
                });
                ui.menu_button("Codex", |ui| {
                    self.connection_ui(ui);
                    if ui.checkbox(&mut self.chat_open, "Show chat").changed() && self.chat_open {
                        self.open_chat();
                    }
                });
            });
            if let Some(error) = self.import_error.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.button("Dismiss").clicked() {
                        self.import_error = None;
                    }
                });
            }
        });
        let gallery_width = (ui.available_width() * 0.30).min(160.0);
        egui::Panel::right("characters")
            .exact_size(gallery_width)
            .resizable(false)
            .show(ui, |ui| {
                self.characters.ui(ui);
            });
        if self.chat_open {
            if ui.available_width() < 680.0 {
                egui::CentralPanel::default().show(ui, |ui| self.chat_ui(ui));
            } else {
                egui::Panel::right("chat")
                    .default_size(400.0)
                    .min_size(320.0)
                    .max_size(ui.available_width() * 0.65)
                    .resizable(true)
                    .show(ui, |ui| self.chat_ui(ui));
                egui::CentralPanel::default().show(ui, |ui| self.workspace_ui(ui));
            }
        } else {
            egui::CentralPanel::default().show(ui, |ui| self.workspace_ui(ui));
        }
        self.settings_ui(ui.ctx());
        // The native AVKit view sits above egui; hide it while the modal is open.
        if self.settings_open {
            self.video.playback_target = None;
        }
    }

    fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let response = egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
            ui.set_max_width((ctx.content_rect().width() - 64.0).clamp(200.0, 320.0));
            ui.heading("Settings");
            ui.add_space(8.0);
            ui.label("Render format");
            let mut format = self.video.render_format;
            ui.spacing_mut().interact_size.y = 44.0;
            for option in [
                crate::settings::RenderFormat::Landscape,
                crate::settings::RenderFormat::Portrait,
            ] {
                ui.radio_value(&mut format, option, option.label());
            }
            if format != self.video.render_format {
                self.settings_error = format.save(&self.root).err();
                if self.settings_error.is_none() {
                    self.video.render_format = format;
                }
            }
            ui.add_space(8.0);
            let mut budget_minutes = self.video.planning_budget_seconds as f64 / 60.0;
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Planning budget");
                ui.add(
                    egui::DragValue::new(&mut budget_minutes)
                        .range(1.0..=60.0)
                        .speed(1.0)
                        .max_decimals(1)
                        .suffix(" min"),
                )
                .labelled_by(label.id)
                .on_hover_text("Total time allowed to plan the scene.");
            });
            let budget_seconds = (budget_minutes * 60.0).round() as u64;
            if budget_seconds != self.video.planning_budget_seconds {
                self.settings_error =
                    crate::settings::save_planning_budget(&self.root, budget_seconds).err();
                if self.settings_error.is_none() {
                    self.video.planning_budget_seconds = budget_seconds;
                }
            }
            if self.video.is_rendering() {
                ui.label("Changes apply to the next generation.");
            }
            if let Some(error) = &self.settings_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(8.0);
            if ui.button("Done").clicked() {
                self.settings_open = false;
            }
        });
        if response.should_close() {
            self.settings_open = false;
        }
    }

    fn workspace_ui(&mut self, ui: &mut egui::Ui) {
        let bounds = ui.available_rect_before_wrap();
        let divider_height = 12.0_f32.min(bounds.height());
        let content_height = (bounds.height() - divider_height).max(1.0);
        let divider_top = bounds.top() + content_height * self.screenplay_fraction;
        let divider = egui::Rect::from_min_size(
            egui::pos2(bounds.left(), divider_top),
            egui::vec2(bounds.width(), divider_height),
        );
        let response = ui
            .interact(
                divider,
                ui.id().with("workspace_divider"),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text("Drag to resize (30–70%). Double-click to reset to 50%. Arrow keys resize; Home resets.");
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        if response.dragged() {
            self.screenplay_fraction += response.drag_delta().y / content_height;
        }
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
            ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.screenplay_fraction -= 0.05;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.screenplay_fraction += 0.05;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                    self.screenplay_fraction = 0.5;
                }
            });
        }
        if response.double_clicked() {
            self.screenplay_fraction = 0.5;
        }
        self.screenplay_fraction = self.screenplay_fraction.clamp(0.3, 0.7);
        response.widget_info(|| {
            egui::WidgetInfo::slider(
                true,
                f64::from(self.screenplay_fraction * 100.0),
                "Screenplay height (%)",
            )
        });

        // Use the updated fraction immediately, and clip each independently scrolling pane.
        let split = bounds.top() + content_height * self.screenplay_fraction;
        let divider = egui::Rect::from_min_size(
            egui::pos2(bounds.left(), split),
            egui::vec2(bounds.width(), divider_height),
        );
        let top = egui::Rect::from_min_max(bounds.min, egui::pos2(bounds.right(), split));
        let bottom = egui::Rect::from_min_max(divider.left_bottom(), bounds.max);
        let mut screenplay = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("screenplay")
                .max_rect(top)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        screenplay.set_clip_rect(ui.clip_rect().intersect(top));
        self.screenplay
            .ui(&mut screenplay, &self.characters.cast_choices());
        let mut context = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("video_context")
                .max_rect(bottom)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        context.set_clip_rect(ui.clip_rect().intersect(bottom));
        self.video.ui(&mut context, &self.screenplay);

        let visuals = ui.style().interact(&response);
        ui.painter().rect_filled(divider, 0.0, visuals.bg_fill);
        ui.painter().line_segment(
            [
                divider.center() - egui::vec2(20.0, 0.0),
                divider.center() + egui::vec2(20.0, 0.0),
            ],
            visuals.fg_stroke,
        );
        if response.has_focus() {
            ui.painter()
                .rect_stroke(divider, 0.0, visuals.fg_stroke, egui::StrokeKind::Inside);
        }
        ui.allocate_rect(bounds, egui::Sense::hover());
    }

    pub fn playback_target(&self) -> Option<&crate::playback::PlaybackTarget> {
        self.video.playback_target.as_ref()
    }

    pub fn poll_background(&mut self, ctx: &egui::Context) {
        self.video.poll(ctx);
    }

    pub fn has_video_job(&self) -> bool {
        self.video.has_job()
    }

    pub fn is_generating_video(&self) -> bool {
        self.video.is_rendering()
    }

    pub fn take_generation_failure(&mut self) -> Option<String> {
        self.video.take_failure_alert()
    }

    pub fn set_playback_error(&mut self, error: Option<String>) {
        self.video.playback_error = error;
    }

    fn connection_ui(&mut self, ui: &mut egui::Ui) {
        if self.checking {
            ui.label("Connecting to Codex…");
        } else if self.login_pending {
            ui.label("Finish signing in in your browser.");
            if ui.button("Cancel sign-in").clicked() {
                self.reconnect();
            }
        } else if let Some(account) = &self.account {
            ui.label(account.email.as_deref().unwrap_or("ChatGPT connected"));
            if let Some(plan) = &account.plan_type {
                ui.label(format!("Plan: {plan}"));
            }
        } else if self.client.is_some() && ui.button("Sign in with ChatGPT").clicked() {
            self.begin_login();
        }
        if self.client.is_none() || self.error.is_some() {
            if let Some(error) = &self.error {
                ui.add(egui::Label::new(error).wrap());
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new("Reconnect"))
                .clicked()
            {
                self.reconnect();
            }
        }
    }

    fn chat_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Codex");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.chat_open = false;
                }
            });
        });
        ui.add(egui::Label::new(self.root.display().to_string()).truncate())
            .on_hover_text(self.root.display().to_string());
        if self.account.is_none() {
            self.connection_ui(ui);
            return;
        }
        // Reserve the composer first so long transcripts and errors cannot push it offscreen.
        egui::Panel::bottom("composer").show(ui, |ui| {
            if let Some(error) = &self.error {
                egui::ScrollArea::vertical()
                    .id_salt("chat_error")
                    .max_height(64.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(format!("Error: {error}")).wrap());
                    });
            }
            if !self.status.is_empty() && (self.busy || self.status == "Stopped.") {
                ui.label(&self.status);
            }
            if self.opening {
                ui.label("Starting chat…");
            } else if !self.ready && ui.button("Start chat").clicked() {
                self.open_chat();
            }
            ui.label("Message");
            let response = egui::ScrollArea::vertical()
                .id_salt("draft")
                .max_height(88.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("Ask Codex…"),
                    )
                })
                .inner;
            let shortcut = response.has_focus()
                && ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                let can_send = self.ready && !self.busy && !self.draft.trim().is_empty();
                if ui
                    .add_enabled(
                        can_send,
                        egui::Button::new("Send").min_size(egui::vec2(64.0, 44.0)),
                    )
                    .on_hover_text("Send (⌘/Ctrl+Enter)")
                    .clicked()
                    || (can_send && shortcut)
                {
                    self.send();
                }
                if self.busy
                    && ui
                        .add_enabled(
                            !self.cancelling,
                            egui::Button::new("Stop").min_size(egui::vec2(64.0, 44.0)),
                        )
                        .clicked()
                    && let Some(client) = &self.client
                {
                    match client.cancel_chat_message() {
                        Ok(()) => self.cancelling = true,
                        Err(error) => self.error = Some(error),
                    }
                }
            });
        });
        egui::ScrollArea::vertical()
            .id_salt("messages")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for message in &self.messages {
                    ui.strong(if message.user { "You" } else { "Codex" });
                    ui.add(egui::Label::new(&message.text).wrap().selectable(true));
                    ui.add_space(8.0);
                }
            });
    }

    fn send(&mut self) {
        let Some(client) = &self.client else { return };
        let message = self.draft.trim().to_owned();
        // Empty overrides leave model and reasoning selection to the user's Codex config.
        match client.send_chat_message(message.clone(), String::new(), String::new()) {
            Ok(()) => {
                self.messages.push(Message {
                    user: true,
                    text: message,
                });
                self.draft.clear();
                self.busy = true;
                self.streaming = false;
                self.error = None;
                self.status = "Working…".into();
            }
            Err(error) => self.error = Some(error),
        }
    }
}

#[cfg(target_os = "macos")]
fn select_fountain(root: &std::path::Path) -> Result<Option<PathBuf>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString, NSURL};

    let mtm = MainThreadMarker::new().ok_or("Import requires the main thread.")?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setTitle(Some(&NSString::from_str("Import screenplay")));
    panel.setPrompt(Some(&NSString::from_str("Import")));
    panel.setMessage(Some(&NSString::from_str(
        "Choose a Fountain screenplay. Undo restores the current screenplay and cast.",
    )));
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setAllowsOtherFileTypes(false);
    // Fountain has no system UTI. AppKit also accepts an extension directly.
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str(
        "fountain",
    )])));
    if let Some(directory) = NSURL::from_file_path(root) {
        panel.setDirectoryURL(Some(&directory));
    }
    let response = panel.runModal();
    if response == NSModalResponseOK {
        panel
            .URL()
            .and_then(|url| url.to_file_path())
            .map(Some)
            .ok_or_else(|| "Could not open the selected file.".into())
    } else if response == NSModalResponseCancel {
        Ok(None)
    } else {
        Err("Could not open the file chooser. Please try again.".into())
    }
}

#[cfg(target_os = "macos")]
fn select_resume(outputs: &std::path::Path) -> Result<Option<PathBuf>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSString, NSURL};

    let mtm = MainThreadMarker::new().ok_or("Resume requires the main thread.")?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setTitle(Some(&NSString::from_str("Resume video")));
    panel.setPrompt(Some(&NSString::from_str("Resume")));
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(false);
    if let Some(directory) = NSURL::from_file_path(outputs) {
        panel.setDirectoryURL(Some(&directory));
    }
    let response = panel.runModal();
    if response == NSModalResponseOK {
        panel
            .URL()
            .and_then(|url| url.to_file_path())
            .map(Some)
            .ok_or_else(|| "Could not open the selected output directory.".into())
    } else if response == NSModalResponseCancel {
        Ok(None)
    } else {
        Err("Could not open the directory chooser. Please try again.".into())
    }
}

#[cfg(not(target_os = "macos"))]
fn select_resume(_outputs: &std::path::Path) -> Result<Option<PathBuf>, String> {
    Err("The output directory chooser requires macOS.".into())
}

#[cfg(not(target_os = "macos"))]
fn select_fountain(_root: &std::path::Path) -> Result<Option<PathBuf>, String> {
    Err("The screenplay file chooser requires macOS.".into())
}

#[cfg(test)]
mod tests;
