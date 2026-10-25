use std::path::PathBuf;

#[derive(Clone)]
pub struct PlaybackTarget {
    pub path: PathBuf,
    pub rect: egui::Rect,
    pub clip_rect: egui::Rect,
}

#[cfg(target_os = "macos")]
mod native {
    use super::PlaybackTarget;
    use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
    use objc2_app_kit::NSView;
    use objc2_av_foundation::{AVPlayer, AVPlayerItemStatus, AVPlayerStatus};
    use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSURL};
    use std::{ffi::CString, os::unix::ffi::OsStrExt, path::PathBuf, ptr::NonNull};
    use winit::{raw_window_handle::HasWindowHandle, window::Window};

    #[derive(Default)]
    pub struct Playback {
        views: Option<Views>,
    }

    struct Views {
        clip: Retained<NSView>,
        view: Retained<AVPlayerView>,
        player: Option<Retained<AVPlayer>>,
        path: Option<PathBuf>,
    }

    impl Playback {
        pub fn sync(
            &mut self,
            window: &Window,
            target: Option<&PlaybackTarget>,
            ctx: &egui::Context,
        ) -> Result<(), String> {
            let target = target.filter(|target| {
                target.rect.intersects(target.clip_rect) && !egui::Popup::is_any_open(ctx)
            });
            let Some(target) = target else {
                if let Some(views) = &self.views {
                    views.hide();
                }
                return Ok(());
            };
            let mtm = MainThreadMarker::new().ok_or("Playback requires the main thread.")?;
            let handle = window.window_handle().map_err(|error| error.to_string())?;
            let winit::raw_window_handle::RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return Err("Could not attach the video player to this window.".into());
            };
            // Winit owns this NSView for the lifetime of `window`. All calls
            // here, including teardown, run on winit's main event-loop thread.
            let parent = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
            let views = self.views.get_or_insert_with(|| {
                let clip = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
                clip.setWantsLayer(true);
                clip.setClipsToBounds(true);
                // AVKit owns decoding, audio synchronization, accessibility,
                // seeking, and the standard keyboard/playback controls.
                let view = unsafe {
                    let view = AVPlayerView::initWithFrame(AVPlayerView::alloc(mtm), NSRect::ZERO);
                    view.setControlsStyle(AVPlayerViewControlsStyle::Inline);
                    view.setShowsFullScreenToggleButton(true);
                    view.setShowsSharingServiceButton(false);
                    view
                };
                clip.addSubview(&view);
                parent.addSubview(&clip);
                Views {
                    clip,
                    view,
                    player: None,
                    path: None,
                }
            });
            if views.path.as_ref() != Some(&target.path) {
                let path = CString::new(target.path.as_os_str().as_bytes())
                    .map_err(|_| "The video path is invalid.".to_owned())?;
                // This is a local filesystem URL, so spaces, Unicode, and URL
                // punctuation in workspace paths retain their exact meaning.
                unsafe {
                    let url = NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                        NonNull::new(path.as_ptr().cast_mut()).unwrap(),
                        false,
                        None,
                    );
                    if let Some(player) = &views.player {
                        player.pause();
                    }
                    let player = AVPlayer::playerWithURL(&url, mtm);
                    views.view.setPlayer(Some(&player));
                    views.player = Some(player);
                }
                views.path = Some(target.path.clone());
            }
            let clip_rect = target.rect.intersect(target.clip_rect);
            // Egui points also include the user's zoom. AppKit uses logical
            // window points; convert explicitly rather than assuming Retina 2×.
            let scale = f64::from(ctx.pixels_per_point()) / window.scale_factor();
            views.clip.setFrame(frame(
                clip_rect,
                parent.bounds().size.height,
                parent.isFlipped(),
                scale,
            ));
            let local = target.rect.translate(-clip_rect.min.to_vec2());
            // AVKit may temporarily move the view into its fullscreen window.
            // Only lay it out while our clipping view owns it.
            if unsafe { views.view.superview() }.as_ref() == Some(&views.clip) {
                views.view.setFrame(frame(
                    local,
                    f64::from(clip_rect.height()) * scale,
                    views.clip.isFlipped(),
                    scale,
                ));
            }
            views.clip.setHidden(false);
            if let Some(player) = &views.player {
                // Loading is asynchronous. Poll failures on normal UI repaints
                // so a damaged or subsequently deleted file gets inline feedback.
                unsafe {
                    let item = player.currentItem();
                    if player.status() == AVPlayerStatus::Failed
                        || item
                            .as_ref()
                            .is_some_and(|item| item.status() == AVPlayerItemStatus::Failed)
                    {
                        let error = item
                            .and_then(|item| item.error())
                            .or_else(|| player.error());
                        let message = error.map_or_else(
                            || "The video could not be loaded.".into(),
                            |error| error.localizedDescription().to_string(),
                        );
                        views.hide();
                        return Err(format!("Could not play video: {message}"));
                    }
                }
            }
            Ok(())
        }
    }

    fn frame(rect: egui::Rect, parent_height: f64, flipped: bool, scale: f64) -> NSRect {
        NSRect::new(
            NSPoint::new(
                f64::from(rect.left()) * scale,
                if flipped {
                    f64::from(rect.top()) * scale
                } else {
                    parent_height - f64::from(rect.bottom()) * scale
                },
            ),
            NSSize::new(
                f64::from(rect.width()) * scale,
                f64::from(rect.height()) * scale,
            ),
        )
    }

    impl Views {
        fn hide(&self) {
            if let Some(player) = &self.player {
                unsafe { player.pause() };
            }
            self.clip.setHidden(true);
        }
    }

    impl Drop for Views {
        fn drop(&mut self) {
            self.hide();
            unsafe { self.view.setPlayer(None) };
            self.clip.removeFromSuperview();
        }
    }
}

#[cfg(target_os = "macos")]
pub use native::Playback;

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
pub struct Playback;

#[cfg(not(target_os = "macos"))]
impl Playback {
    pub fn sync(
        &mut self,
        _window: &winit::window::Window,
        target: Option<&PlaybackTarget>,
        _ctx: &egui::Context,
    ) -> Result<(), String> {
        if target.is_some() {
            Err("In-app playback requires macOS.".into())
        } else {
            Ok(())
        }
    }
}
