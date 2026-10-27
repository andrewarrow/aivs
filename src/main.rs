#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod attention;
mod characters;
mod codex;
mod playback;
mod screenplay;
mod settings;
mod video;

use std::{error::Error, num::NonZeroU32, sync::Arc, time::Instant};

use egui::ViewportId;
use egui_wgpu::winit::Painter;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

// The native input/render loop is adapted from DeployerCoaster Studio.
struct DesktopWindow {
    context: egui::Context,
    window: Arc<Window>,
    input: egui_winit::State,
    painter: Painter,
    first_frame: bool,
    playback: playback::Playback,
}

impl DesktopWindow {
    fn new(
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Instant>,
    ) -> Result<Self, Box<dyn Error>> {
        let context = egui::Context::default();
        context.set_request_repaint_callback(move |request| {
            if let Some(deadline) = Instant::now().checked_add(request.delay) {
                let _ = proxy.send_event(deadline);
            }
        });
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("AIVS")
                    .with_inner_size(LogicalSize::new(1280.0, 800.0))
                    .with_min_inner_size(LogicalSize::new(390.0, 360.0))
                    .with_visible(false),
            )?,
        );
        let mut painter = pollster::block_on(Painter::new(
            context.clone(),
            egui_wgpu::WgpuConfiguration::default(),
            false,
            egui_wgpu::RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))?;
        let input = egui_winit::State::new(
            context.clone(),
            ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        window.request_redraw();
        Ok(Self {
            context,
            window,
            input,
            painter,
            first_frame: true,
            playback: playback::Playback::default(),
        })
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop, app: &mut app::App) -> Option<Instant> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        egui_winit::update_viewport_info(
            self.input
                .egui_input_mut()
                .viewports
                .entry(ViewportId::ROOT)
                .or_default(),
            &self.context,
            &self.window,
            self.first_frame,
        );
        let output = self
            .context
            .run_ui(self.input.take_egui_input(&self.window), |ui| app.ui(ui));
        self.input.handle_platform_output_with_event_loop(
            &self.window,
            event_loop,
            output.platform_output,
        );
        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        self.painter.paint_and_update_textures(
            ViewportId::ROOT,
            output.pixels_per_point,
            self.context
                .global_style()
                .visuals
                .panel_fill
                .to_normalized_gamma_f32(),
            &primitives,
            &output.textures_delta,
            Vec::new(),
            &self.window,
        );
        let playback_error = self
            .playback
            .sync(&self.window, app.playback_target(), &self.context)
            .err();
        app.set_playback_error(playback_error);
        if app.playback_target().is_some() {
            self.context
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        if self.first_frame {
            self.window.set_visible(true);
            self.window.focus_window();
            self.first_frame = false;
        }
        if app.quit {
            event_loop.exit();
        }
        output
            .viewport_output
            .get(&ViewportId::ROOT)
            .and_then(|viewport| Instant::now().checked_add(viewport.repaint_delay))
    }
}

impl Drop for DesktopWindow {
    fn drop(&mut self) {
        self.playback = playback::Playback::default();
        self.painter.destroy();
    }
}

struct Desktop {
    app: app::App,
    window: Option<DesktopWindow>,
    next_repaint: Option<Instant>,
    startup_error: Option<Box<dyn Error>>,
    proxy: EventLoopProxy<Instant>,
    alerts: attention::GenerationAlerts,
}

impl Desktop {
    fn schedule_repaint(&mut self, deadline: Instant) {
        self.next_repaint = Some(
            self.next_repaint
                .map_or(deadline, |next| next.min(deadline)),
        );
    }
}

impl ApplicationHandler<Instant> for Desktop {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            match DesktopWindow::new(event_loop, self.proxy.clone()) {
                Ok(window) => self.window = Some(window),
                Err(error) => {
                    self.startup_error = Some(error);
                    event_loop.exit();
                }
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, deadline: Instant) {
        self.schedule_repaint(deadline);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(desktop) = self.window.as_mut().filter(|w| w.window.id() == window_id) else {
            return;
        };
        if desktop
            .input
            .on_window_event(&desktop.window, &event)
            .repaint
        {
            desktop.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(true) => self.alerts.clear(&desktop.window),
            WindowEvent::Resized(size) => {
                if let (Some(width), Some(height)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    desktop
                        .painter
                        .on_window_resized(ViewportId::ROOT, width, height);
                    desktop.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(deadline) = desktop.redraw(event_loop, &mut self.app) {
                    self.schedule_repaint(deadline);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(desktop) = &self.window {
            // Hidden/minimized windows may receive no RedrawRequested events.
            // Drain worker events on the native loop so a failed background
            // render still alerts immediately.
            self.app.poll_background(&desktop.context);
            if let Some(error) = self.app.take_generation_failure() {
                self.alerts.failed(&desktop.window, &error);
            }
            if self.app.is_generating_video() {
                self.alerts.clear(&desktop.window);
            }
        }
        match self.next_repaint {
            Some(deadline) if deadline <= Instant::now() => {
                self.next_repaint = None;
                if let Some(desktop) = &self.window {
                    desktop.window.request_redraw();
                }
            }
            _ => {}
        }
        let mut deadline = self.next_repaint;
        if self.app.has_video_job() {
            let poll = Instant::now() + std::time::Duration::from_millis(100);
            deadline = Some(deadline.map_or(poll, |repaint| repaint.min(poll)));
        }
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.window = None;
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().nth(1).as_deref() == Some("--production") {
        std::process::exit(aivs_core::production::cli::main(std::env::args().skip(2).collect()));
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--engine")) {
        aivs_core::serve(
            std::io::BufReader::new(std::io::stdin()),
            std::io::stdout().lock(),
        )?;
        return Ok(());
    }
    env_logger::init();
    let root = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err("The workspace must be a directory.".into());
    }
    let mut builder = EventLoop::<Instant>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_default_menu(true);
        builder.with_activation_policy(winit::platform::macos::ActivationPolicy::Regular);
    }
    let event_loop = builder.build()?;
    let mut desktop = Desktop {
        app: app::App::new(root),
        window: None,
        next_repaint: None,
        startup_error: None,
        proxy: event_loop.create_proxy(),
        alerts: attention::GenerationAlerts::default(),
    };
    event_loop.run_app(&mut desktop)?;
    if let Some(error) = desktop.startup_error {
        return Err(error);
    }
    Ok(())
}
