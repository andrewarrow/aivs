use winit::window::Window;

#[derive(Default)]
pub struct GenerationAlerts {
    pending: bool,
    attention_request: Option<isize>,
}

impl GenerationAlerts {
    pub fn failed(&mut self, window: &Window, error: &str) {
        self.clear(window);
        self.pending = true;
        platform::set_badge(true);
        platform::bell();
        self.attention_request = platform::request_attention(window);
        platform::notify(error);
    }

    pub fn clear(&mut self, window: &Window) {
        if self.pending {
            self.pending = false;
            platform::cancel_attention(window, self.attention_request.take());
            platform::set_badge(false);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSBeep, NSRequestUserAttentionType};
    use objc2_foundation::NSString;
    use std::process::{Command, Stdio};
    use winit::window::Window;

    // Pass the error as data, never interpolate renderer output into script code.
    // AppleScript also works for the unbundled executable used by `cargo run`.
    const NOTIFICATION_SCRIPT: &str = r#"on run argv
    display notification (item 1 of argv) with title "AIVS" subtitle "Video generation failed"
end run"#;

    pub fn request_attention(_window: &Window) -> Option<isize> {
        let mtm = MainThreadMarker::new()?;
        Some(
            NSApplication::sharedApplication(mtm)
                .requestUserAttention(NSRequestUserAttentionType::CriticalRequest),
        )
    }

    pub fn cancel_attention(_window: &Window, request: Option<isize>) {
        // Winit's macOS implementation ignores request_user_attention(None).
        // Retain AppKit's request ID so retrying also stops a background bounce.
        if let (Some(mtm), Some(request)) = (MainThreadMarker::new(), request) {
            NSApplication::sharedApplication(mtm).cancelUserAttentionRequest(request);
        }
    }

    pub fn set_badge(visible: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let dock = NSApplication::sharedApplication(mtm).dockTile();
        // A native red badge with an exclamation mark also conveys the error
        // without relying on color. It needs no notification permission.
        let label = visible.then(|| NSString::from_str("!"));
        dock.setBadgeLabel(label.as_deref());
    }

    pub fn bell() {
        NSBeep();
    }

    pub fn notify(error: &str) {
        // A notification service can be slow or unavailable. Keep it off the
        // event loop, reap the child, and retain the Dock/bell alerts on failure.
        let body = notification_body(error);
        std::thread::spawn(move || {
            match Command::new("/usr/bin/osascript")
                .args(["-e", NOTIFICATION_SCRIPT, "--", &body])
                .stdin(Stdio::null())
                .output()
            {
                Ok(output) if output.status.success() => {}
                Ok(output) => log::warn!(
                    "Could not post generation failure notification: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                Err(error) => log::warn!("Could not start failure notification: {error}"),
            }
        });
    }

    fn notification_body(error: &str) -> String {
        let summary = error.split_whitespace().collect::<Vec<_>>().join(" ");
        if summary.is_empty() {
            return "Open AIVS to review the error in Activity.".into();
        }
        let mut body: String = summary.chars().take(240).collect();
        if summary.chars().count() > 240 {
            body.push('…');
        }
        body.push_str(" Open AIVS for details.");
        body
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn notification_body_handles_multiline_unicode_and_script_like_errors() {
            assert_eq!(
                notification_body("GPU failed\n\t\"quoted\" \\ 日本語; do shell script"),
                "GPU failed \"quoted\" \\ 日本語; do shell script Open AIVS for details."
            );
            assert_eq!(
                notification_body("\n  "),
                "Open AIVS to review the error in Activity."
            );
            assert_eq!(
                notification_body(&"視".repeat(300)),
                format!("{}… Open AIVS for details.", "視".repeat(240))
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use winit::window::{UserAttentionType, Window};

    pub fn request_attention(window: &Window) -> Option<isize> {
        window.request_user_attention(Some(UserAttentionType::Critical));
        None
    }

    pub fn cancel_attention(window: &Window, _request: Option<isize>) {
        window.request_user_attention(None);
    }

    pub fn set_badge(_visible: bool) {}

    pub fn bell() {
        use std::io::Write;
        let _ = std::io::stderr().write_all(b"\x07");
    }

    pub fn notify(error: &str) {
        log::error!("Video generation failed: {error}");
    }
}
