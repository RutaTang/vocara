//! The menu bar (port of the Flutter Runner's `Base.lproj/MainMenu.xib`,
//! item for item: titles, key equivalents, actions, the Find items' tags, and
//! the Services, Window and Help menus' roles).
//!
//! winit installs a minimal menu of its own when the app finishes launching
//! (the app menu only). [`install`] replaces it with this one, once, from the
//! main thread; the app calls it when its window has opened, which is after
//! winit's.
//!
//! Every item but six is nil-targeted, as in the xib: its action goes to
//! the first responder chain, and AppKit enables the item only while
//! something there implements it. The Quit item sends `terminate:`, so it
//! goes through the quit guard ([`crate::quit_request`]). The app-menu titles
//! carry the app's name where the xib had `APP_NAME`, which
//! `FlutterAppDelegate` replaced with `CFBundleDisplayName`, else
//! `CFBundleName`; an unbundled run (`cargo run`) uses the process name.
//!
//! The six are Cut, Copy, Paste, Select All, Undo and Redo, whose ⌘X, ⌘C,
//! ⌘V, ⌘A, ⌘Z and ⇧⌘Z are also the keys the app's text fields handle, from
//! the key events winit's view receives. Nothing in the responder chain of
//! the app's window implements `cut:`, `copy:`, `paste:` or `selectAll:`
//! (winit's view, window, window delegate and application delegate do not),
//! nor can undo anything there, so nil-targeted, the six would be disabled
//! there, and whether the keys reached the fields would rest on what AppKit
//! does with a disabled item's key equivalent. Instead they target an
//! object of ours, the edit target, which keeps them enabled and answers
//! each of the six actions in two steps:
//! 1. it sends the action down the responder chain, as a nil-targeted item
//!    would (`-[NSApplication sendAction:to:from:]` with no target), so a
//!    native panel's field editor or an `NSTextField` cuts, copies, pastes,
//!    selects all or undoes, as in the Flutter app; Undo and Redo only to a
//!    responder that, asked (`validateMenuItem:`, else
//!    `validateUserInterfaceItem:`), can take them now, since a window's
//!    own undo with nothing to undo would otherwise keep ⌘Z from the field;
//! 2. when nothing there takes it, it hands the keystroke to the key window,
//!    whose `sendEvent:` passes a keyDown to its first responder (winit's
//!    view) and not back through the menu, so iced's focused text input gets
//!    the key as if no item had it: the keyDown that chose the item when
//!    there is one (`NSApp.currentEvent`), else, for a click on the item, a
//!    ⌘-keyDown and a keyUp made for the key (its ANSI key code, see
//!    [`EditKey`]). The keyUp made carries the modifiers held at that moment,
//!    so winit does not keep ⌘ as held after a click.
//!
//! `tests/macos_gui.rs` checks this on a Mac (CI's macOS runner): the four
//! keys and clicks on the four items reach a winit-like view once each, and
//! in an `NSTextField` ⌘A with ⌘C, ⌘V and ⌘X select, copy, paste and cut.
//!
//! The Runner's other settings: its deployment target, macOS 10.15, is the
//! app's minimum too (`LSMinimumSystemVersion` in
//! `packaging/macos/Info.plist.in`), and every AppKit call here is in 10.15
//! (the newest, `NSApp.helpMenu` and `NSEvent.modifierFlags`, are 10.6).
//! Its `applicationSupportsSecureRestorableState` is
//! [`crate::restorable_state`].

use std::ffi::CStr;

/// A key equivalent. Every one in the menu includes ⌘; an uppercase key
/// implies ⇧, as in the xib.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key: &'static str,
    pub option: bool,
    pub control: bool,
}

/// A menu AppKit is told about (`systemMenu` in the xib).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `NSApp.servicesMenu`: AppKit fills it with the services.
    Services,
    /// `NSApp.windowsMenu`: AppKit lists the windows in it.
    Windows,
    /// `NSApp.helpMenu`: AppKit adds the help search to it.
    Help,
}

/// A menu: its title, its role, its items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    pub role: Option<Role>,
    pub items: Vec<Item>,
}

/// Where an item sends its action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The first responder chain (nil-targeted, as every item in the xib).
    FirstResponder,
    /// The edit target: the responder chain, else the keystroke to the key
    /// window (see the module doc). Only the items of [`EDIT_KEYS`].
    EditTarget,
}

/// One menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Separator,
    /// An item sending `action` to `target` (no action: always disabled, as
    /// the xib's Preferences… was).
    Action {
        title: String,
        action: Option<&'static CStr>,
        shortcut: Option<Shortcut>,
        /// The tag `performFindPanelAction:` reads (0 for the others).
        tag: isize,
        target: Target,
    },
    /// An item opening `menu`.
    Submenu {
        title: String,
        menu: Menu,
    },
}

/// An Edit item whose key the app's text fields handle: its action, its key
/// (with ⌘; an uppercase key with ⇧ too), and that key's virtual key code
/// on the ANSI layout, which the edit target gives the keystroke it makes
/// for a click on the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditKey {
    pub action: &'static CStr,
    pub key: &'static str,
    pub key_code: u16,
    /// Whether the action goes down the responder chain only to a
    /// responder that says it can take it now (Undo and Redo, see the
    /// module doc).
    pub only_if_valid: bool,
}

/// Cut, ⌘X (`kVK_ANSI_X`).
pub const CUT: EditKey = EditKey {
    action: c"cut:",
    key: "x",
    key_code: 7,
    only_if_valid: false,
};
/// Copy, ⌘C (`kVK_ANSI_C`).
pub const COPY: EditKey = EditKey {
    action: c"copy:",
    key: "c",
    key_code: 8,
    only_if_valid: false,
};
/// Paste, ⌘V (`kVK_ANSI_V`).
pub const PASTE: EditKey = EditKey {
    action: c"paste:",
    key: "v",
    key_code: 9,
    only_if_valid: false,
};
/// Select All, ⌘A (`kVK_ANSI_A`).
pub const SELECT_ALL: EditKey = EditKey {
    action: c"selectAll:",
    key: "a",
    key_code: 0,
    only_if_valid: false,
};
/// Undo, ⌘Z (`kVK_ANSI_Z`): the text fields' own history.
pub const UNDO: EditKey = EditKey {
    action: c"undo:",
    key: "z",
    key_code: 6,
    only_if_valid: true,
};
/// Redo, ⇧⌘Z.
pub const REDO: EditKey = EditKey {
    action: c"redo:",
    key: "Z",
    key_code: 6,
    only_if_valid: true,
};

/// The items that target the edit target.
pub const EDIT_KEYS: [EditKey; 6] = [CUT, COPY, PASTE, SELECT_ALL, UNDO, REDO];

const fn cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut {
        key,
        option: false,
        control: false,
    })
}

const fn opt_cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut {
        key,
        option: true,
        control: false,
    })
}

const fn ctrl_cmd(key: &'static str) -> Option<Shortcut> {
    Some(Shortcut {
        key,
        option: false,
        control: true,
    })
}

fn item(title: impl Into<String>, action: &'static CStr, shortcut: Option<Shortcut>) -> Item {
    Item::Action {
        title: title.into(),
        action: Some(action),
        shortcut,
        tag: 0,
        target: Target::FirstResponder,
    }
}

fn find(title: &str, tag: isize, shortcut: Option<Shortcut>) -> Item {
    Item::Action {
        title: title.into(),
        action: Some(c"performFindPanelAction:"),
        shortcut,
        tag,
        target: Target::FirstResponder,
    }
}

/// The Edit item of `key`: its action and ⌘ key, sent to the edit target.
fn edit_key(title: &str, key: EditKey) -> Item {
    Item::Action {
        title: title.into(),
        action: Some(key.action),
        shortcut: cmd(key.key),
        tag: 0,
        target: Target::EditTarget,
    }
}

fn submenu(title: &str, menu_title: &str, role: Option<Role>, items: Vec<Item>) -> Item {
    Item::Submenu {
        title: title.into(),
        menu: Menu {
            title: menu_title.into(),
            role,
            items,
        },
    }
}

/// The menu bar for an app named `app_name` (`MainMenu.xib`'s main menu).
pub fn main_menu(app_name: &str) -> Menu {
    use Item::Separator;
    let app = submenu(
        app_name,
        app_name,
        None,
        vec![
            item(
                format!("About {app_name}"),
                c"orderFrontStandardAboutPanel:",
                None,
            ),
            Separator,
            // No action in the xib either: always disabled.
            Item::Action {
                title: "Preferences…".into(),
                action: None,
                shortcut: cmd(","),
                tag: 0,
                target: Target::FirstResponder,
            },
            Separator,
            submenu("Services", "Services", Some(Role::Services), Vec::new()),
            Separator,
            item(format!("Hide {app_name}"), c"hide:", cmd("h")),
            item("Hide Others", c"hideOtherApplications:", opt_cmd("h")),
            item("Show All", c"unhideAllApplications:", None),
            Separator,
            item(format!("Quit {app_name}"), c"terminate:", cmd("q")),
        ],
    );
    let find_menu = submenu(
        "Find",
        "Find",
        None,
        vec![
            find("Find…", 1, cmd("f")),
            find("Find and Replace…", 12, opt_cmd("f")),
            find("Find Next", 2, cmd("g")),
            find("Find Previous", 3, cmd("G")),
            find("Use Selection for Find", 7, cmd("e")),
            item(
                "Jump to Selection",
                c"centerSelectionInVisibleArea:",
                cmd("j"),
            ),
        ],
    );
    let spelling = submenu(
        "Spelling and Grammar",
        "Spelling",
        None,
        vec![
            item("Show Spelling and Grammar", c"showGuessPanel:", cmd(":")),
            item("Check Document Now", c"checkSpelling:", cmd(";")),
            Separator,
            item(
                "Check Spelling While Typing",
                c"toggleContinuousSpellChecking:",
                None,
            ),
            item(
                "Check Grammar With Spelling",
                c"toggleGrammarChecking:",
                None,
            ),
            item(
                "Correct Spelling Automatically",
                c"toggleAutomaticSpellingCorrection:",
                None,
            ),
        ],
    );
    let substitutions = submenu(
        "Substitutions",
        "Substitutions",
        None,
        vec![
            item("Show Substitutions", c"orderFrontSubstitutionsPanel:", None),
            Separator,
            item("Smart Copy/Paste", c"toggleSmartInsertDelete:", None),
            item("Smart Quotes", c"toggleAutomaticQuoteSubstitution:", None),
            item("Smart Dashes", c"toggleAutomaticDashSubstitution:", None),
            item("Smart Links", c"toggleAutomaticLinkDetection:", None),
            item("Data Detectors", c"toggleAutomaticDataDetection:", None),
            item("Text Replacement", c"toggleAutomaticTextReplacement:", None),
        ],
    );
    let transformations = submenu(
        "Transformations",
        "Transformations",
        None,
        vec![
            item("Make Upper Case", c"uppercaseWord:", None),
            item("Make Lower Case", c"lowercaseWord:", None),
            item("Capitalize", c"capitalizeWord:", None),
        ],
    );
    let speech = submenu(
        "Speech",
        "Speech",
        None,
        vec![
            item("Start Speaking", c"startSpeaking:", None),
            item("Stop Speaking", c"stopSpeaking:", None),
        ],
    );
    let edit = submenu(
        "Edit",
        "Edit",
        None,
        vec![
            // To the edit target: the text fields handle ⌘Z, ⇧⌘Z, ⌘X, ⌘C,
            // ⌘V and ⌘A too (see the module doc).
            edit_key("Undo", UNDO),
            edit_key("Redo", REDO),
            Separator,
            edit_key("Cut", CUT),
            edit_key("Copy", COPY),
            edit_key("Paste", PASTE),
            item("Paste and Match Style", c"pasteAsPlainText:", opt_cmd("V")),
            item("Delete", c"delete:", None),
            edit_key("Select All", SELECT_ALL),
            Separator,
            find_menu,
            spelling,
            substitutions,
            transformations,
            speech,
        ],
    );
    let view = submenu(
        "View",
        "View",
        None,
        vec![item(
            "Enter Full Screen",
            c"toggleFullScreen:",
            ctrl_cmd("f"),
        )],
    );
    let window = submenu(
        "Window",
        "Window",
        Some(Role::Windows),
        vec![
            item("Minimize", c"performMiniaturize:", cmd("m")),
            item("Zoom", c"performZoom:", None),
            Separator,
            item("Bring All to Front", c"arrangeInFront:", None),
        ],
    );
    let help = submenu("Help", "Help", Some(Role::Help), Vec::new());
    Menu {
        title: "Main Menu".into(),
        role: None,
        items: vec![app, edit, view, window, help],
    }
}

/// Replaces winit's menu bar with [`main_menu`], once. Only from the main
/// thread, after the app finished launching (winit sets its own menu then):
/// elsewhere, and off macOS, it does nothing.
pub fn install() {
    #[cfg(target_os = "macos")]
    macos::install();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem,
        NSMenuItemValidation, NSWindow,
    };
    use objc2_foundation::{ns_string, NSBundle, NSPoint, NSProcessInfo, NSString};

    use super::{EditKey, Item, Menu, Role, Target, COPY, CUT, PASTE, REDO, SELECT_ALL, UNDO};

    static INSTALLED: AtomicBool = AtomicBool::new(false);

    define_class!(
        /// The target of Cut, Copy, Paste and Select All: the module doc's
        /// edit target. Main thread only, as AppKit's menus are. Its ivar is
        /// set while it hands a keystroke to the key window.
        // SAFETY: NSObject has no subclassing requirements, and `EditTarget`
        // does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = Cell<bool>]
        struct EditTarget;

        // SAFETY: each is an action method, `-(void)name:(id)sender`, which
        // AppKit sends with the menu item (or nil) as the sender.
        impl EditTarget {
            #[unsafe(method(cut:))]
            fn cut_action(&self, sender: Option<&AnyObject>) {
                self.perform(&CUT, sender);
            }

            #[unsafe(method(copy:))]
            fn copy_action(&self, sender: Option<&AnyObject>) {
                self.perform(&COPY, sender);
            }

            #[unsafe(method(paste:))]
            fn paste_action(&self, sender: Option<&AnyObject>) {
                self.perform(&PASTE, sender);
            }

            #[unsafe(method(selectAll:))]
            fn select_all_action(&self, sender: Option<&AnyObject>) {
                self.perform(&SELECT_ALL, sender);
            }

            #[unsafe(method(undo:))]
            fn undo_action(&self, sender: Option<&AnyObject>) {
                self.perform(&UNDO, sender);
            }

            #[unsafe(method(redo:))]
            fn redo_action(&self, sender: Option<&AnyObject>) {
                self.perform(&REDO, sender);
            }
        }

        unsafe impl NSObjectProtocol for EditTarget {}

        // SAFETY: `-(BOOL)validateMenuItem:(NSMenuItem *)item`, the
        // protocol's one method.
        unsafe impl NSMenuItemValidation for EditTarget {
            /// Always enabled: when nothing in the responder chain takes
            /// the action, the keystroke goes to the key window.
            #[unsafe(method(validateMenuItem:))]
            fn validate_menu_item(&self, _item: &NSMenuItem) -> bool {
                true
            }
        }
    );

    impl EditTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(Cell::new(false));
            // SAFETY: `init` is NSObject's designated initializer, and the
            // ivars are set.
            unsafe { msg_send![super(this), init] }
        }

        /// Answers `key`'s action: down the responder chain, else as the
        /// keystroke, to the key window (see the module doc).
        fn perform(&self, key: &EditKey, sender: Option<&AnyObject>) {
            // The key window's `sendEvent:` hands a keyDown to its first
            // responder, not to the menu. Were a keystroke handed on to come
            // back here all the same, it stops here rather than loop.
            let forwarding = self.ivars();
            if forwarding.get() {
                return;
            }
            let app = NSApplication::sharedApplication(self.mtm());
            let action = Sel::register(key.action);
            // SAFETY: `key.action` names an action method
            // (`-(void)name:(id)sender`). With no target, AppKit sends it
            // only to an object in the responder chain that implements it,
            // with `sender` (the menu item that sent it here, or nil), as it
            // sends a nil-targeted item's action. The edit target is not in
            // any responder chain, so this does not come back here.
            if (!key.only_if_valid || can_take(&app, action, sender))
                && unsafe { app.sendAction_to_from(action, None, sender) }
            {
                return;
            }
            let Some(window) = app.keyWindow() else {
                return;
            };
            let _forwarding = Forwarding::start(forwarding);
            match app.currentEvent() {
                Some(event) if chose(&event, key) => window.sendEvent(&event),
                _ => {
                    for event in keystroke(key, &window).into_iter().flatten() {
                        window.sendEvent(&event);
                    }
                }
            }
        }
    }

    /// Whether the responder `action` would go to down the responder chain
    /// can take it now, by its own answer for `sender` (the menu item):
    /// `validateMenuItem:`, else `validateUserInterfaceItem:`, else yes.
    fn can_take(app: &NSApplication, action: Sel, sender: Option<&AnyObject>) -> bool {
        // SAFETY: `targetForAction:to:from:` only looks the chain up.
        let Some(target) = (unsafe { app.targetForAction_to_from(action, None, sender) }) else {
            return false;
        };
        let Some(item) = sender else {
            return true;
        };
        let class = target.class();
        if class.responds_to(sel!(validateMenuItem:)) {
            // SAFETY: `-(BOOL)validateMenuItem:(NSMenuItem *)`, which the
            // target answers (checked), sent the menu item that chose the
            // action.
            return unsafe { msg_send![&*target, validateMenuItem: item] };
        }
        if class.responds_to(sel!(validateUserInterfaceItem:)) {
            // SAFETY: `-(BOOL)validateUserInterfaceItem:(id)`, likewise.
            return unsafe { msg_send![&*target, validateUserInterfaceItem: item] };
        }
        true
    }

    /// Sets the edit target's ivar while it lives.
    struct Forwarding<'a>(&'a Cell<bool>);

    impl<'a> Forwarding<'a> {
        fn start(flag: &'a Cell<bool>) -> Self {
            flag.set(true);
            Self(flag)
        }
    }

    impl Drop for Forwarding<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }

    /// Whether `event` is the keystroke that chose `key`'s item: a keyDown
    /// with ⌘ whose characters, with or without the modifiers, are the key.
    fn chose(event: &NSEvent, key: &EditKey) -> bool {
        // The type first: `characters` is only for key events.
        event.r#type() == NSEventType::KeyDown
            && event
                .modifierFlags()
                .contains(NSEventModifierFlags::Command)
            && [event.characters(), event.charactersIgnoringModifiers()]
                .into_iter()
                .flatten()
                .any(|chars| chars.to_string().eq_ignore_ascii_case(key.key))
    }

    /// The keystroke of `key`'s item, for `window`: a keyDown of the key
    /// with ⌘ (and ⇧, for an uppercase key), then its keyUp with the
    /// modifiers held now. After a click those are none, and winit, which
    /// takes the modifiers from every key event, does not keep ⌘ as held.
    fn keystroke(key: &EditKey, window: &NSWindow) -> [Option<Retained<NSEvent>>; 2] {
        let chars = NSString::from_str(key.key);
        let time = NSProcessInfo::processInfo().systemUptime();
        let number = window.windowNumber();
        let held =
            NSEvent::modifierFlags_class() & NSEventModifierFlags::DeviceIndependentFlagsMask;
        let mut down = NSEventModifierFlags::Command;
        if key.key.bytes().any(|b| b.is_ascii_uppercase()) {
            down |= NSEventModifierFlags::Shift;
        }
        [(NSEventType::KeyDown, down), (NSEventType::KeyUp, held)]
        .map(|(kind, flags)| {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                kind,
                NSPoint::ZERO,
                flags,
                time,
                number,
                None,
                &chars,
                &chars,
                false,
                key.key_code,
            )
        })
    }

    pub fn install() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = NSApplication::sharedApplication(mtm);
        let edit_target = EditTarget::new(mtm);
        let mut roles = Vec::new();
        let bar = build(
            mtm,
            &super::main_menu(&app_name()),
            &edit_target,
            &mut roles,
        );
        app.setMainMenu(Some(&bar));
        for (role, menu) in roles {
            match role {
                Role::Services => app.setServicesMenu(Some(&menu)),
                Role::Windows => app.setWindowsMenu(Some(&menu)),
                Role::Help => app.setHelpMenu(Some(&menu)),
            }
        }
        // A menu item holds its target weakly: the edit target, made once,
        // lives as long as the app.
        std::mem::forget(edit_target);
    }

    /// `CFBundleDisplayName`, else `CFBundleName`, else the process name.
    fn app_name() -> String {
        let bundle = NSBundle::mainBundle();
        for key in [
            ns_string!("CFBundleDisplayName"),
            ns_string!("CFBundleName"),
        ] {
            let name = bundle
                .objectForInfoDictionaryKey(key)
                .and_then(|value| value.downcast::<NSString>().ok())
                .map(|name| name.to_string())
                .filter(|name| !name.is_empty());
            if let Some(name) = name {
                return name;
            }
        }
        NSProcessInfo::processInfo().processName().to_string()
    }

    fn build(
        mtm: MainThreadMarker,
        menu: &Menu,
        edit_target: &EditTarget,
        roles: &mut Vec<(Role, Retained<NSMenu>)>,
    ) -> Retained<NSMenu> {
        let ns_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(&menu.title));
        for item in &menu.items {
            let ns_item = match item {
                Item::Separator => NSMenuItem::separatorItem(mtm),
                Item::Action {
                    title,
                    action,
                    shortcut,
                    tag,
                    target,
                } => {
                    let key = shortcut.map_or("", |s| s.key);
                    // SAFETY: each action names an action method
                    // (`-(void)name:(id)sender`). AppKit sends it to the
                    // item's target, which for the edit target's items
                    // implements it (`EditTarget`), and with no target down
                    // the responder chain, which it only does to an object
                    // that responds to it.
                    let ns_item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            mtm.alloc(),
                            &NSString::from_str(title),
                            action.map(Sel::register),
                            &NSString::from_str(key),
                        )
                    };
                    if let Some(shortcut) = shortcut {
                        let mut mask = NSEventModifierFlags::Command;
                        if shortcut.option {
                            mask |= NSEventModifierFlags::Option;
                        }
                        if shortcut.control {
                            mask |= NSEventModifierFlags::Control;
                        }
                        ns_item.setKeyEquivalentModifierMask(mask);
                    }
                    if *tag != 0 {
                        ns_item.setTag(*tag);
                    }
                    if *target == Target::EditTarget {
                        let edit_target: &AnyObject = edit_target;
                        // SAFETY: the edit target is an object, which
                        // implements the four actions it is given and
                        // `validateMenuItem:`, and lives as long as the app
                        // (the item holds it weakly).
                        unsafe { ns_item.setTarget(Some(edit_target)) };
                    }
                    ns_item
                }
                Item::Submenu { title, menu } => {
                    let ns_item = NSMenuItem::new(mtm);
                    ns_item.setTitle(&NSString::from_str(title));
                    let submenu = build(mtm, menu, edit_target, roles);
                    ns_item.setSubmenu(Some(&submenu));
                    ns_item
                }
            };
            ns_menu.addItem(&ns_item);
        }
        if let Some(role) = menu.role {
            roles.push((role, ns_menu.clone()));
        }
        ns_menu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MainMenu.xib`'s main menu, item for item, as [`describe`] writes it:
    /// `title | key | action [| tag n] [| edit target]`, `title > menu title
    /// [role]`, `---`. Dumped from the xib, `APP_NAME` read as "Vocara",
    /// with Undo, Redo, Cut, Copy, Paste and Select All sent to the edit
    /// target.
    const XIB: &str = "\
Vocara > Vocara
  About Vocara | - | orderFrontStandardAboutPanel:
  ---
  Preferences… | ⌘, | -
  ---
  Services > Services [Services]
  ---
  Hide Vocara | ⌘h | hide:
  Hide Others | ⌥⌘h | hideOtherApplications:
  Show All | - | unhideAllApplications:
  ---
  Quit Vocara | ⌘q | terminate:
Edit > Edit
  Undo | ⌘z | undo: | edit target
  Redo | ⌘Z | redo: | edit target
  ---
  Cut | ⌘x | cut: | edit target
  Copy | ⌘c | copy: | edit target
  Paste | ⌘v | paste: | edit target
  Paste and Match Style | ⌥⌘V | pasteAsPlainText:
  Delete | - | delete:
  Select All | ⌘a | selectAll: | edit target
  ---
  Find > Find
    Find… | ⌘f | performFindPanelAction: | tag 1
    Find and Replace… | ⌥⌘f | performFindPanelAction: | tag 12
    Find Next | ⌘g | performFindPanelAction: | tag 2
    Find Previous | ⌘G | performFindPanelAction: | tag 3
    Use Selection for Find | ⌘e | performFindPanelAction: | tag 7
    Jump to Selection | ⌘j | centerSelectionInVisibleArea:
  Spelling and Grammar > Spelling
    Show Spelling and Grammar | ⌘: | showGuessPanel:
    Check Document Now | ⌘; | checkSpelling:
    ---
    Check Spelling While Typing | - | toggleContinuousSpellChecking:
    Check Grammar With Spelling | - | toggleGrammarChecking:
    Correct Spelling Automatically | - | toggleAutomaticSpellingCorrection:
  Substitutions > Substitutions
    Show Substitutions | - | orderFrontSubstitutionsPanel:
    ---
    Smart Copy/Paste | - | toggleSmartInsertDelete:
    Smart Quotes | - | toggleAutomaticQuoteSubstitution:
    Smart Dashes | - | toggleAutomaticDashSubstitution:
    Smart Links | - | toggleAutomaticLinkDetection:
    Data Detectors | - | toggleAutomaticDataDetection:
    Text Replacement | - | toggleAutomaticTextReplacement:
  Transformations > Transformations
    Make Upper Case | - | uppercaseWord:
    Make Lower Case | - | lowercaseWord:
    Capitalize | - | capitalizeWord:
  Speech > Speech
    Start Speaking | - | startSpeaking:
    Stop Speaking | - | stopSpeaking:
View > View
  Enter Full Screen | ⌃⌘f | toggleFullScreen:
Window > Window [Windows]
  Minimize | ⌘m | performMiniaturize:
  Zoom | - | performZoom:
  ---
  Bring All to Front | - | arrangeInFront:
Help > Help [Help]
";

    fn describe(menu: &Menu, depth: usize, out: &mut String) {
        for item in &menu.items {
            out.push_str(&"  ".repeat(depth));
            match item {
                Item::Separator => out.push_str("---\n"),
                Item::Action {
                    title,
                    action,
                    shortcut,
                    tag,
                    target,
                } => {
                    let key = shortcut.map_or("-".to_string(), |s| {
                        format!(
                            "{}{}⌘{}",
                            if s.control { "⌃" } else { "" },
                            if s.option { "⌥" } else { "" },
                            s.key
                        )
                    });
                    let action = action.map_or("-", |a| a.to_str().expect("ascii"));
                    out.push_str(&format!("{title} | {key} | {action}"));
                    if *tag != 0 {
                        out.push_str(&format!(" | tag {tag}"));
                    }
                    if *target == Target::EditTarget {
                        out.push_str(" | edit target");
                    }
                    out.push('\n');
                }
                Item::Submenu { title, menu } => {
                    out.push_str(&format!("{title} > {}", menu.title));
                    if let Some(role) = menu.role {
                        out.push_str(&format!(" [{role:?}]"));
                    }
                    out.push('\n');
                    describe(menu, depth + 1, out);
                }
            }
        }
    }

    #[test]
    fn the_menu_bar_is_the_xibs_item_for_item() {
        let mut described = String::new();
        describe(&main_menu("Vocara"), 0, &mut described);
        assert_eq!(described, XIB);
    }

    #[test]
    fn the_app_menu_names_the_app_it_runs_as() {
        let mut described = String::new();
        describe(&main_menu("Vocara Dev"), 0, &mut described);
        for line in [
            "Vocara Dev > Vocara Dev\n",
            "  About Vocara Dev | - | orderFrontStandardAboutPanel:\n",
            "  Hide Vocara Dev | ⌘h | hide:\n",
            "  Quit Vocara Dev | ⌘q | terminate:\n",
        ] {
            assert!(described.contains(line), "{line}");
        }
    }

    /// Every item that takes ⌘Z, ⇧⌘Z, ⌘X, ⌘C, ⌘V or ⌘A, the keys the text
    /// fields handle, sends its action to the edit target, which hands the
    /// key on to the window when nothing in the responder chain takes the
    /// action; and the edit target gets no other item.
    #[test]
    fn the_keys_iceds_text_inputs_handle_go_through_the_edit_target() {
        fn actions(menu: &Menu, out: &mut Vec<(Option<&'static CStr>, Option<Shortcut>, Target)>) {
            for item in &menu.items {
                match item {
                    Item::Action {
                        action,
                        shortcut,
                        target,
                        ..
                    } => out.push((*action, *shortcut, *target)),
                    Item::Submenu { menu, .. } => actions(menu, out),
                    Item::Separator => {}
                }
            }
        }
        let mut all = Vec::new();
        actions(&main_menu("Vocara"), &mut all);
        for key in EDIT_KEYS {
            let plain = cmd(key.key);
            let taking: Vec<_> = all.iter().filter(|(_, s, _)| *s == plain).collect();
            assert_eq!(
                taking,
                [&(Some(key.action), plain, Target::EditTarget)],
                "⌘{}",
                key.key
            );
        }
        let to_edit_target = all
            .iter()
            .filter(|(_, _, target)| *target == Target::EditTarget)
            .count();
        assert_eq!(to_edit_target, EDIT_KEYS.len());
    }
}
