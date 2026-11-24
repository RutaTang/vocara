//! The menu bar (port of the Flutter Runner's `Base.lproj/MainMenu.xib`,
//! item for item: titles, key equivalents, actions, the Find items' tags, and
//! the Services, Window and Help menus' roles).
//!
//! winit installs a minimal menu of its own when the app finishes launching
//! (the app menu only). [`install`] replaces it with this one, once, from the
//! main thread; the app calls it when its window has opened, which is after
//! winit's.
//!
//! Every item is nil-targeted, as in the xib: its action goes to the first
//! responder chain, and AppKit enables the item only while something there
//! implements it. The Quit item sends `terminate:`, so it goes through the
//! quit guard ([`crate::quit_request`]). The app-menu titles carry the app's
//! name where the xib had `APP_NAME`, which `FlutterAppDelegate` replaced
//! with `CFBundleDisplayName`, else `CFBundleName`; an unbundled run (`cargo
//! run`) uses the process name.
//!
//! One deliberate difference: Cut, Copy, Paste and Select All carry no key
//! equivalents (the xib had ⌘X, ⌘C, ⌘V, ⌘A). iced's text inputs handle those
//! keys themselves, from the key events winit's view receives, and nothing in
//! the responder chain of the app's window implements `cut:`, `copy:`,
//! `paste:` or `selectAll:` (winit's view, window, window delegate and
//! application delegate do not), so the items are disabled there. AppKit is
//! understood to pass a disabled item's key equivalent on to the window, but
//! that could not be checked on a Mac, and if it did not, text editing would
//! lose all four keys; without the equivalents the keys reach iced whatever
//! AppKit does. The other Edit equivalents (⌘Z, ⇧⌘Z, ⌥⇧⌘V, the Find and
//! Spelling keys) are kept: iced uses none of them.
//!
//! Two of the Runner's settings stay different, deliberately:
//! - `applicationSupportsSecureRestorableState`, which the Runner's
//!   `AppDelegate` answered YES, stays undeclared (winit's application
//!   delegate has no such method). Its only effect is a console warning on
//!   macOS 14 and later.
//! - The minimum macOS version is 11.0 (`LSMinimumSystemVersion` in
//!   `packaging/macos/Info.plist.in`), where the Flutter app's was 10.15:
//!   the native file panels (rfd) are built on UniformTypeIdentifiers,
//!   whose framework the binary links and which exists only from macOS 11,
//!   so the app cannot even load on 10.15.

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

/// One menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Separator,
    /// An item sending `action` to the first responder (no action: always
    /// disabled, as the xib's Preferences… was).
    Action {
        title: String,
        action: Option<&'static CStr>,
        shortcut: Option<Shortcut>,
        /// The tag `performFindPanelAction:` reads (0 for the others).
        tag: isize,
    },
    /// An item opening `menu`.
    Submenu {
        title: String,
        menu: Menu,
    },
}

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
    }
}

fn find(title: &str, tag: isize, shortcut: Option<Shortcut>) -> Item {
    Item::Action {
        title: title.into(),
        action: Some(c"performFindPanelAction:"),
        shortcut,
        tag,
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
            item("Undo", c"undo:", cmd("z")),
            item("Redo", c"redo:", cmd("Z")),
            Separator,
            // No key equivalents on these four: iced's text inputs handle
            // ⌘X, ⌘C, ⌘V and ⌘A (see the module doc).
            item("Cut", c"cut:", None),
            item("Copy", c"copy:", None),
            item("Paste", c"paste:", None),
            item("Paste and Match Style", c"pasteAsPlainText:", opt_cmd("V")),
            item("Delete", c"delete:", None),
            item("Select All", c"selectAll:", None),
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2::rc::Retained;
    use objc2::runtime::Sel;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{ns_string, NSBundle, NSProcessInfo, NSString};

    use super::{Item, Menu, Role};

    static INSTALLED: AtomicBool = AtomicBool::new(false);

    pub fn install() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = NSApplication::sharedApplication(mtm);
        let mut roles = Vec::new();
        let bar = build(mtm, &super::main_menu(&app_name()), &mut roles);
        app.setMainMenu(Some(&bar));
        for (role, menu) in roles {
            match role {
                Role::Services => app.setServicesMenu(Some(&menu)),
                Role::Windows => app.setWindowsMenu(Some(&menu)),
                Role::Help => app.setHelpMenu(Some(&menu)),
            }
        }
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
                } => {
                    let key = shortcut.map_or("", |s| s.key);
                    // SAFETY: each action names an action method
                    // (`-(void)name:(id)sender`), sent with no target to the
                    // responder chain, which AppKit only does to an object
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
                    ns_item
                }
                Item::Submenu { title, menu } => {
                    let ns_item = NSMenuItem::new(mtm);
                    ns_item.setTitle(&NSString::from_str(title));
                    let submenu = build(mtm, menu, roles);
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
    /// `title | key | action [| tag n]`, `title > menu title [role]`, `---`.
    /// Dumped from the xib, `APP_NAME` read as "Vocara", with the four keys
    /// iced's text inputs handle taken off Cut, Copy, Paste and Select All.
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
  Undo | ⌘z | undo:
  Redo | ⌘Z | redo:
  ---
  Cut | - | cut:
  Copy | - | copy:
  Paste | - | paste:
  Paste and Match Style | ⌥⌘V | pasteAsPlainText:
  Delete | - | delete:
  Select All | - | selectAll:
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

    #[test]
    fn no_item_can_take_the_keys_iceds_text_inputs_handle() {
        fn shortcuts(menu: &Menu, out: &mut Vec<Shortcut>) {
            for item in &menu.items {
                match item {
                    Item::Action {
                        shortcut: Some(s), ..
                    } => out.push(*s),
                    Item::Submenu { menu, .. } => shortcuts(menu, out),
                    _ => {}
                }
            }
        }
        let mut all = Vec::new();
        shortcuts(&main_menu("Vocara"), &mut all);
        for key in ["x", "c", "v", "a"] {
            let plain = Shortcut {
                key,
                option: false,
                control: false,
            };
            assert!(!all.contains(&plain), "⌘{key} belongs to the text inputs");
        }
    }
}
