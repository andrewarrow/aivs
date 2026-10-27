use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSString, ns_string};
use winit::event_loop::EventLoopProxy;

use crate::DesktopEvent;
use crate::app::{App, AppCommand};

struct MenuTargetIvars {
    proxy: EventLoopProxy<DesktopEvent>,
    enabled: Cell<[bool; AppCommand::ALL.len()]>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(performAppMenuAction:))]
        fn perform_app_menu_action(&self, sender: &NSMenuItem) {
            if let Some(command) = command_for_tag(sender.tag()) {
                // Wake Winit even when the window is idle. The shared command
                // runs after AppKit has finished handling the menu selection.
                let _ = self.ivars().proxy.send_event(DesktopEvent::MenuCommand(command));
            }
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            command_for_tag(item.tag())
                .is_some_and(|command| self.ivars().enabled.get()[command as usize])
        }
    }
);

pub(crate) struct NativeMenu {
    // AppKit does not retain a menu item's target.
    target: Retained<MenuTarget>,
    items: Vec<(AppCommand, Retained<NSMenuItem>)>,
    codex_status: [Retained<NSMenuItem>; 3],
}

impl NativeMenu {
    pub(crate) fn install(proxy: EventLoopProxy<DesktopEvent>) -> Option<Self> {
        let main_thread = MainThreadMarker::new().expect("Menus require the main thread");
        let application = NSApplication::sharedApplication(main_thread);
        // Winit installs its default application menu before resumed(). Keep
        // its native About, Services, Hide, and Quit actions intact.
        let Some((main_menu, application_item, application_menu)) =
            application.mainMenu().and_then(|main_menu| {
                let item = main_menu.itemAtIndex(0)?;
                let submenu = item.submenu()?;
                Some((main_menu, item, submenu))
            })
        else {
            log::warn!("macOS menu installation skipped: Winit's application menu is unavailable");
            return None;
        };

        let this = MenuTarget::alloc(main_thread).set_ivars(MenuTargetIvars {
            proxy,
            enabled: Cell::new([false; AppCommand::ALL.len()]),
        });
        // SAFETY: NSObject's initializer is valid for this initialized subclass.
        let target: Retained<MenuTarget> = unsafe { msg_send![super(this), init] };

        application_item.setTitle(ns_string!("AIVS"));
        application_menu.setTitle(ns_string!("AIVS"));
        // Rename Winit's process-derived titles for unbundled cargo run builds.
        for (index, title) in [
            (0, ns_string!("About AIVS")),
            (3, ns_string!("Hide AIVS")),
            (7, ns_string!("Quit AIVS")),
        ] {
            if let Some(item) = application_menu.itemAtIndex(index) {
                item.setTitle(title);
            }
        }

        let settings = command_item(
            main_thread,
            &target,
            AppCommand::Settings,
            ns_string!("Settings…"),
            ns_string!(","),
        );
        application_menu.insertItem_atIndex(&settings, 2);
        application_menu.insertItem_atIndex(&NSMenuItem::separatorItem(main_thread), 3);

        let quit = command_item(
            main_thread,
            &target,
            AppCommand::Quit,
            ns_string!("Quit AIVS"),
            ns_string!("q"),
        );
        // Route Quit through the same unsaved-project check as window closing.
        let last = application_menu.numberOfItems() - 1;
        application_menu.removeItemAtIndex(last);
        application_menu.addItem(&quit);

        let file_menu = NSMenu::new(main_thread);
        file_menu.setTitle(ns_string!("File"));
        let mut items = vec![(AppCommand::Settings, settings), (AppCommand::Quit, quit)];
        for (command, title, key) in [
            (
                AppCommand::NewProject,
                ns_string!("New Project"),
                ns_string!("n"),
            ),
            (
                AppCommand::OpenProject,
                ns_string!("Open Project…"),
                ns_string!("o"),
            ),
            (AppCommand::SaveProject, ns_string!("Save"), ns_string!("s")),
            (
                AppCommand::SaveProjectAs,
                ns_string!("Save As…"),
                ns_string!("s"),
            ),
        ] {
            let item = command_item(main_thread, &target, command, title, key);
            if matches!(command, AppCommand::SaveProjectAs) {
                item.setKeyEquivalentModifierMask(
                    NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
                );
            }
            file_menu.addItem(&item);
            items.push((command, item));
        }
        file_menu.addItem(&NSMenuItem::separatorItem(main_thread));
        let import = command_item(
            main_thread,
            &target,
            AppCommand::Import,
            ns_string!("Import…"),
            ns_string!(""),
        );
        let resume = command_item(
            main_thread,
            &target,
            AppCommand::Resume,
            ns_string!("Resume…"),
            ns_string!(""),
        );
        file_menu.addItem(&import);
        file_menu.addItem(&resume);
        file_menu.addItem(&NSMenuItem::separatorItem(main_thread));
        file_menu.addItem(&menu_item(
            main_thread,
            ns_string!("Close"),
            sel!(performClose:),
            ns_string!("w"),
        ));
        let file_item = NSMenuItem::new(main_thread);
        file_item.setTitle(ns_string!("File"));
        file_item.setSubmenu(Some(&file_menu));
        main_menu.insertItem_atIndex(&file_item, 1);

        items.push((AppCommand::Import, import));
        items.push((AppCommand::Resume, resume));
        let motion_menu = NSMenu::new(main_thread);
        motion_menu.setTitle(ns_string!("Motion"));
        for (command, title) in [
            (
                AppCommand::Previsualization,
                ns_string!("Previsualization…"),
            ),
            (AppCommand::FinalRender, ns_string!("Final Render…")),
        ] {
            let item = command_item(main_thread, &target, command, title, ns_string!(""));
            motion_menu.addItem(&item);
            items.push((command, item));
        }
        let motion_item = NSMenuItem::new(main_thread);
        motion_item.setTitle(ns_string!("Motion"));
        motion_item.setSubmenu(Some(&motion_menu));
        main_menu.insertItem_atIndex(&motion_item, 2);

        let codex_menu = NSMenu::new(main_thread);
        codex_menu.setTitle(ns_string!("Codex"));
        let codex_status = std::array::from_fn(|_| {
            let item = NSMenuItem::new(main_thread);
            item.setEnabled(false);
            item.setHidden(true);
            codex_menu.addItem(&item);
            item
        });
        for (command, title) in [
            (AppCommand::SignIn, ns_string!("Sign in with ChatGPT")),
            (AppCommand::CancelSignIn, ns_string!("Cancel Sign-In")),
            (AppCommand::Reconnect, ns_string!("Reconnect")),
            (AppCommand::ToggleChat, ns_string!("Show Chat")),
        ] {
            if matches!(command, AppCommand::ToggleChat) {
                codex_menu.addItem(&NSMenuItem::separatorItem(main_thread));
            }
            let item = command_item(main_thread, &target, command, title, ns_string!(""));
            codex_menu.addItem(&item);
            items.push((command, item));
        }
        let codex_item = NSMenuItem::new(main_thread);
        codex_item.setTitle(ns_string!("Codex"));
        codex_item.setSubmenu(Some(&codex_menu));
        main_menu.insertItem_atIndex(&codex_item, 3);

        Some(Self {
            target,
            items,
            codex_status,
        })
    }

    pub(crate) fn update(&self, app: &App) {
        let mut enabled = [false; AppCommand::ALL.len()];
        for (command, item) in &self.items {
            let available = app.command_enabled(*command);
            enabled[*command as usize] = available;
            item.setEnabled(available);
            item.setHidden(!app.command_visible(*command));
            item.setState(if app.command_checked(*command) { 1 } else { 0 });
        }
        self.target.ivars().enabled.set(enabled);
        for (item, text) in self.codex_status.iter().zip(app.codex_menu_status()) {
            item.setHidden(text.is_none());
            if let Some(text) = text {
                item.setTitle(&NSString::from_str(&text));
            }
        }
    }
}

fn command_for_tag(tag: isize) -> Option<AppCommand> {
    usize::try_from(tag)
        .ok()
        .and_then(|index| AppCommand::ALL.get(index).copied())
}

fn command_item(
    main_thread: MainThreadMarker,
    target: &MenuTarget,
    command: AppCommand,
    title: &NSString,
    key: &NSString,
) -> Retained<NSMenuItem> {
    let item = menu_item(main_thread, title, sel!(performAppMenuAction:), key);
    item.setTag(command as isize);
    item.setEnabled(false);
    // SAFETY: MenuTarget registers this action with the standard menu signature,
    // and NativeMenu retains the target for the lifetime of the menu.
    unsafe { item.setTarget(Some(target)) };
    item
}

fn menu_item(
    main_thread: MainThreadMarker,
    title: &NSString,
    action: Sel,
    key: &NSString,
) -> Retained<NSMenuItem> {
    // SAFETY: Each selector uses the standard one-argument menu action signature.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(main_thread),
            title,
            Some(action),
            key,
        )
    }
}

/// Use AppKit for standard alerts so layout, appearance, and keyboard behavior
/// follow the user's macOS settings, just like the native file panels.
pub(crate) fn confirm_save_changes(
    project_name: &str,
    can_save: bool,
) -> crate::app::projects::SaveChangesChoice {
    use crate::app::projects::SaveChangesChoice;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn};

    let main_thread = MainThreadMarker::new().expect("Alerts require the main thread");
    let alert = NSAlert::new(main_thread);
    alert.setMessageText(&NSString::from_str(&format!(
        "Save changes to “{project_name}”?"
    )));
    let message = if can_save {
        "Your unsaved changes will be lost if you don’t save them."
    } else {
        "Your unsaved changes will be lost if you don’t save them. To save, cancel and stop the current job first."
    };
    alert.setInformativeText(&NSString::from_str(message));
    let save = alert.addButtonWithTitle(ns_string!("Save"));
    save.setEnabled(can_save);
    alert.addButtonWithTitle(ns_string!("Don’t Save"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    match alert.runModal() {
        response if response == NSAlertFirstButtonReturn && can_save => SaveChangesChoice::Save,
        response if response == NSAlertSecondButtonReturn => SaveChangesChoice::Discard,
        _ => SaveChangesChoice::Cancel,
    }
}

pub(crate) fn show_error(title: &str, message: &str) {
    use objc2_app_kit::{NSAlert, NSAlertStyle};

    let main_thread = MainThreadMarker::new().expect("Alerts require the main thread");
    let alert = NSAlert::new(main_thread);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    alert.addButtonWithTitle(ns_string!("OK"));
    alert.runModal();
}
