//! Routing: the routes (port of `routing/routes.dart`), the page stack with
//! go_router's push / pop / go semantics (`routing/router.dart`), and
//! `popOrGo` (`routing/navigation.dart`).
//!
//! The stack holds every page that is open, bottom first, each under a
//! [`PageId`]. As in Flutter, a page beneath the top stays alive (its state,
//! its view model, its in-flight work), it is just not drawn. Messages carry
//! the id of the page they belong to, so a late result for a page that has
//! left the stack is dropped, and one for a covered page reaches it.
//!
//! - **push** adds a page on top. The page beneath learns that the push
//!   completed only when the pushed page is popped (`Screen::returned`), as
//!   go_router completed a push future only on pop.
//! - **pop** removes the top page; with nothing beneath, there is nothing to
//!   pop (go_router threw) and the stack stays as it is.
//! - **go** replaces the whole stack with the location's page (every route is
//!   top-level, so that is one page): the pages it drops are disposed and
//!   their pushes never complete — exactly why [`pop_or_go`] pops when it
//!   can.

use std::fmt;

/// `Routes.home` — the collection of song practices.
pub const HOME: &str = "/";
/// `Routes.songPracticePattern` — a song practice (`/song/:id`).
pub const SONG_PRACTICE_PATTERN: &str = "/song/:id";
/// `Routes.settings` — global app settings (audio devices).
pub const SETTINGS: &str = "/settings";
/// `Routes.techniqueLibrary` — the flat, tag-organized pool of techniques.
pub const TECHNIQUE_LIBRARY: &str = "/techniques";
/// `Routes.techniqueDetailPattern` — one technique (`/techniques/:id`).
pub const TECHNIQUE_DETAIL_PATTERN: &str = "/techniques/:id";

/// `Routes.songPractice(id)`.
pub fn song_practice(id: &str) -> String {
    format!("/song/{id}")
}

/// `Routes.techniqueDetail(id)`.
pub fn technique_detail(id: &str) -> String {
    format!("/techniques/{id}")
}

/// A page the app can show.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Route {
    Home,
    Song { id: String },
    Settings,
    Techniques,
    Technique { id: String },
}

/// Why a location matched no route (`GoException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteError(pub String);

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GoException: {}", self.0)
    }
}

impl std::error::Error for RouteError {}

impl Route {
    /// The route's location (path).
    pub fn location(&self) -> String {
        match self {
            Route::Home => HOME.to_string(),
            Route::Song { id } => song_practice(id),
            Route::Settings => SETTINGS.to_string(),
            Route::Techniques => TECHNIQUE_LIBRARY.to_string(),
            Route::Technique { id } => technique_detail(id),
        }
    }

    /// Matches a location as go_router did: the query and fragment are not
    /// part of the match, a trailing slash is dropped (except the root's),
    /// literal segments match case-insensitively, a `:id` parameter is one
    /// non-empty segment, percent-decoded, and the whole path must be
    /// consumed.
    pub fn parse(location: &str) -> Result<Route, RouteError> {
        if location.is_empty() {
            return Err(RouteError("Location cannot be empty.".into()));
        }
        let path = location.split(['?', '#']).next().unwrap_or_default();
        let path = if path.len() > 1 && path.ends_with('/') {
            &path[..path.len() - 1]
        } else {
            path
        };
        let segments: Vec<&str> = path.strip_prefix('/').map_or_else(Vec::new, |p| {
            if p.is_empty() {
                Vec::new()
            } else {
                p.split('/').collect()
            }
        });
        let literal = |s: &str, want: &str| s.eq_ignore_ascii_case(want);
        let param = |s: &str| (!s.is_empty()).then(|| percent_decode(s));
        let route = if !path.starts_with('/') {
            None
        } else {
            match segments.as_slice() {
                [] => Some(Route::Home),
                [s] if literal(s, "settings") => Some(Route::Settings),
                [s] if literal(s, "techniques") => Some(Route::Techniques),
                [s, id] if literal(s, "song") => param(id).map(|id| Route::Song { id }),
                [s, id] if literal(s, "techniques") => param(id).map(|id| Route::Technique { id }),
                _ => None,
            }
        };
        route.ok_or_else(|| RouteError(format!("no routes for location: {location}")))
    }

    /// The location [`pop_or_go`] falls back to from this page when nothing
    /// lies beneath it (each screen's back affordance in Dart).
    pub fn parent(&self) -> Route {
        match self {
            Route::Technique { .. } => Route::Techniques,
            _ => Route::Home,
        }
    }
}

/// `Uri.decodeComponent`: `%XX` escapes decoded as UTF-8; a malformed one is
/// kept as written.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let decoded = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(b) = decoded {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// The identity of one page on the stack: never reused, so a message for a
/// page that has gone cannot reach the page that replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageId(u64);

/// What a screen asks the router to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Navigation {
    /// `context.push(location)`.
    Push(Route),
    /// `context.go(location)`.
    Go(Route),
    /// `context.pop()`.
    Pop,
    /// `popOrGo(context, fallback)`: pop when a page lies beneath, otherwise
    /// go to `fallback`.
    PopOrGo(Route),
}

/// The page stack (`P` is the app's page type).
#[derive(Debug)]
pub struct Router<P> {
    pages: Vec<(PageId, P)>,
    next_id: u64,
}

/// What a navigation did to the stack.
#[derive(Debug)]
pub enum Change<P> {
    /// A page was pushed on top.
    Pushed(PageId),
    /// The top page was popped; the page now on top is `revealed`.
    Popped {
        popped: (PageId, P),
        revealed: PageId,
    },
    /// The stack was replaced by one page; `dropped` are the pages it held,
    /// bottom first.
    Replaced {
        page: PageId,
        dropped: Vec<(PageId, P)>,
    },
    /// Nothing changed (a pop with nothing beneath).
    Nothing,
}

impl<P> Router<P> {
    /// A stack holding `initial` (go_router's `initialLocation`).
    pub fn new(initial: P) -> Self {
        Self {
            pages: vec![(PageId(0), initial)],
            next_id: 1,
        }
    }

    fn mint(&mut self) -> PageId {
        let id = PageId(self.next_id);
        self.next_id += 1;
        id
    }

    /// The page on top.
    pub fn top(&self) -> (PageId, &P) {
        let (id, page) = self.pages.last().expect("the stack is never empty");
        (*id, page)
    }

    /// The page on top, mutably.
    pub fn top_mut(&mut self) -> (PageId, &mut P) {
        let (id, page) = self.pages.last_mut().expect("the stack is never empty");
        (*id, page)
    }

    /// The page `id`, while it is on the stack.
    pub fn get_mut(&mut self, id: PageId) -> Option<&mut P> {
        self.pages
            .iter_mut()
            .find(|(i, _)| *i == id)
            .map(|(_, p)| p)
    }

    /// Whether page `id` is on the stack.
    pub fn contains(&self, id: PageId) -> bool {
        self.pages.iter().any(|(i, _)| *i == id)
    }

    /// The pages, bottom first.
    pub fn pages(&self) -> impl Iterator<Item = (PageId, &P)> {
        self.pages.iter().map(|(id, p)| (*id, p))
    }

    /// The pages, bottom first, mutably.
    pub fn pages_mut(&mut self) -> impl Iterator<Item = (PageId, &mut P)> {
        self.pages.iter_mut().map(|(id, p)| (*id, p))
    }

    /// How many pages are open.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Always false: the stack is never empty.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// `context.canPop()`: whether a page lies beneath the top one.
    pub fn can_pop(&self) -> bool {
        self.pages.len() > 1
    }

    /// `context.push`: `page` on top.
    pub fn push(&mut self, page: P) -> Change<P> {
        let id = self.mint();
        self.pages.push((id, page));
        Change::Pushed(id)
    }

    /// `context.pop`: the top page off, when a page lies beneath it.
    pub fn pop(&mut self) -> Change<P> {
        if !self.can_pop() {
            return Change::Nothing;
        }
        let popped = self.pages.pop().expect("checked above");
        Change::Popped {
            popped,
            revealed: self.top().0,
        }
    }

    /// `context.go`: the stack replaced by `page`.
    pub fn go(&mut self, page: P) -> Change<P> {
        let id = self.mint();
        let dropped = std::mem::replace(&mut self.pages, vec![(id, page)]);
        Change::Replaced { page: id, dropped }
    }
}

/// `popOrGo(context, fallback)`: leaves the current page — pops it when a
/// page lies beneath, otherwise goes to `fallback` (built by `make`).
///
/// Pop is the normal case, and the one that matters: a screen that pushed
/// this page awaits the push (Home and the technique library reload their
/// lists and release their double-open guards when it completes), and only
/// a pop completes it — a go drops the pushed page without ever completing
/// it, which left Home's open guard set for good. The fallback covers a page
/// that is the root of the stack (a `VOCARA_START` deep link), where there is
/// nothing to pop and the back affordance would do nothing at all.
pub fn pop_or_go<P>(router: &mut Router<P>, make: impl FnOnce() -> P) -> Change<P> {
    if router.can_pop() {
        router.pop()
    } else {
        router.go(make())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_round_trip() {
        for route in [
            Route::Home,
            Route::Song {
                id: "01J0000000000000000000000A".into(),
            },
            Route::Settings,
            Route::Techniques,
            Route::Technique {
                id: "T1000000000000000000000000".into(),
            },
        ] {
            assert_eq!(Route::parse(&route.location()), Ok(route));
        }
        assert_eq!(song_practice("x"), "/song/x");
        assert_eq!(technique_detail("y"), "/techniques/y");
    }

    #[test]
    fn locations_match_as_go_router_matched_them() {
        assert_eq!(Route::parse("/Settings/"), Ok(Route::Settings));
        assert_eq!(Route::parse("/techniques?tag=a"), Ok(Route::Techniques));
        assert_eq!(
            Route::parse("/song/a%20b#x"),
            Ok(Route::Song { id: "a b".into() })
        );
        assert!(Route::parse("/song").is_err());
        assert!(Route::parse("/song/a/b").is_err());
        assert!(Route::parse("settings").is_err());
        let err = Route::parse("/nope").unwrap_err();
        assert_eq!(
            err.to_string(),
            "GoException: no routes for location: /nope"
        );
        assert_eq!(
            Route::parse("").unwrap_err().to_string(),
            "GoException: Location cannot be empty."
        );
    }

    #[test]
    fn push_and_pop_reveal_the_page_beneath() {
        let mut r = Router::new("home");
        assert!(!r.can_pop());
        let Change::Pushed(settings) = r.push("settings") else {
            panic!("pushed")
        };
        assert_eq!(r.top(), (settings, &"settings"));
        let Change::Popped { popped, revealed } = r.pop() else {
            panic!("popped")
        };
        assert_eq!(popped, (settings, "settings"));
        assert_eq!(r.top().0, revealed);
        assert!(
            matches!(r.pop(), Change::Nothing),
            "nothing to pop at the root"
        );
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn go_replaces_the_stack_and_never_reuses_an_id() {
        let mut r = Router::new("home");
        let Change::Pushed(a) = r.push("library") else {
            panic!()
        };
        r.push("technique");
        let Change::Replaced { page, dropped } = r.go("home") else {
            panic!("replaced")
        };
        assert_eq!(dropped.len(), 3);
        assert_eq!(r.len(), 1);
        assert!(!r.contains(a));
        assert_ne!(page, a);
    }

    // deep_link_back_test: a page opened by a deep link is the ROOT of the
    // stack; its back affordance goes to the page's parent instead of popping.
    #[test]
    fn a_deep_linked_page_goes_to_its_parent_and_a_pushed_one_pops() {
        let mut r = Router::new(Route::Technique { id: "T1".into() });
        let parent = r.top().1.parent();
        assert!(matches!(
            pop_or_go(&mut r, || parent),
            Change::Replaced { .. }
        ));
        assert_eq!(r.top().1, &Route::Techniques);

        let mut r = Router::new(Route::Settings);
        let parent = r.top().1.parent();
        pop_or_go(&mut r, || parent);
        assert_eq!(r.top().1, &Route::Home);

        // A pushed technique still pops back to the library it came from —
        // popped, not replaced: the library underneath is the same page.
        let mut r = Router::new(Route::Techniques);
        let library = r.top().0;
        r.push(Route::Technique { id: "T1".into() });
        let Change::Popped { revealed, .. } = pop_or_go(&mut r, || Route::Techniques) else {
            panic!("popped")
        };
        assert_eq!(revealed, library);
    }
}
