use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSString, ns_string};
use winit::event_loop::EventLoopProxy;

use crate::DesktopEvent;
use crate::app::{App, AppCommand};

struct MenuTargetIvars {
    proxy: EventLoopProxy<DesktopEvent>,
    enabled: Cell<[bool; 3]>,
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
    items: [(AppCommand, Retained<NSMenuItem>); 3],
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
            enabled: Cell::new([false; 3]),
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

        let file_menu = NSMenu::new(main_thread);
        file_menu.setTitle(ns_string!("File"));
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

        Some(Self {
            target,
            items: [
                (AppCommand::Settings, settings),
                (AppCommand::Import, import),
                (AppCommand::Resume, resume),
            ],
        })
    }

    pub(crate) fn update(&self, app: &App) {
        let mut enabled = [false; 3];
        for (command, item) in &self.items {
            let available = app.command_enabled(*command);
            enabled[*command as usize] = available;
            item.setEnabled(available);
        }
        self.target.ivars().enabled.set(enabled);
    }
}

fn command_for_tag(tag: isize) -> Option<AppCommand> {
    match tag {
        0 => Some(AppCommand::Settings),
        1 => Some(AppCommand::Import),
        2 => Some(AppCommand::Resume),
        _ => None,
    }
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
