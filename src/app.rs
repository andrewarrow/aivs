use std::{path::PathBuf, time::Duration};

use crate::codex::{ChatGptAccount, CodexClient, CodexEvent, CodexWorkStatus};

struct Message {
    user: bool,
    text: String,
}

pub struct App {
    root: PathBuf,
    characters: crate::characters::CharacterGallery,
    video: crate::video::VideoWorkspace,
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
    pub quit: bool,
}

impl App {
    pub fn new(root: PathBuf) -> Self {
        Self {
            video: crate::video::VideoWorkspace::new(root.clone()),
            characters: crate::characters::CharacterGallery::load(&root),
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
                ui.menu_button("Codex", |ui| {
                    self.connection_ui(ui);
                    if ui.checkbox(&mut self.chat_open, "Show chat").changed() && self.chat_open {
                        self.open_chat();
                    }
                });
            });
        });
        let gallery_width = ui.available_width() * 0.30;
        egui::Panel::right("characters")
            .exact_size(gallery_width)
            .resizable(false)
            .show(ui, |ui| self.characters.ui(ui));
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
                egui::CentralPanel::default().show(ui, |ui| self.video.ui(ui));
            }
        } else {
            egui::CentralPanel::default().show(ui, |ui| self.video.ui(ui));
        }
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

#[cfg(test)]
mod tests;
