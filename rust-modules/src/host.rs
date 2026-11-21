//! The cross-crate construction interface: what a platform crate needs to build its `static` [`Platform`]
//! and hand it to the application (`docs/platform-ports.md`, Target shape 8).
//!
//! The application's modules are private (`mod app;`), so a crate that is a sibling of this one cannot
//! name `app::Platform` or implement a crate-private `Shell`. This is the one narrow public face: re-exports
//! of the types a platform builds or implements, an [`EventSink`] through which it reports input, the two SDL
//! helpers an SDL platform's `Shell` is built from ([`sdl_init`], [`sdl_open`], with [`InitHooks`]), [`run`], the
//! one function defined in here (it hands the platform to the application's entry), and the ready media values
//! described below. No other module of the
//! application is public to a television build. A `hostsim` build also keeps the simulator binary's own doors,
//! each behind `feature = "hostsim"` in `lib.rs` (`grep -n 'feature = "hostsim"' rust-modules/src/lib.rs`):
//! `hud_cli`, `sim_runtime_dir`, `sim_events_log`, `publish_sim_webos_major` and
//! `synthetic_home_initial`, the one `app` item re-exported at the crate root. `HostEvent`, the loop's own event type, stays
//! private: a platform reports input through [`EventSink`] and the application converts it. The owner's ruling (2026-11-21, Decision 3
//! amended): the eight application-crate items `Platform`, `Shell`, `Surface`, `FrameProbe`, `MediaBoot`,
//! `Composition`, `Exit` and `Release` are reachable only through this module. `Services` is not one of the eight:
//! it is `plx_os::tv::Services`, already a `pub` type of a lower crate, which this module re-exports beside them.
//! A platform crate names the application crate's types through `host` alone; the hook tables a `Services` is
//! built from (`plx_os::tv`) come from their own crate, which it depends on directly.
//!
//! **The media values.** The application keeps the playback composition (`app::adapters::backends`) until W11a
//! moves it into the webOS platform, so `host` hands a platform READY [`MediaBoot`] values, and a platform crate names
//! no `PlayerAdapter` (the type `MediaBoot::compose` takes): [`WEBOS_MEDIA`] is the television's (the plane owner's
//! boot, then the Feed and the product URI driver), compiled in every build of the application because it carries no `cfg` (the application cannot know which platform names it), and, under
//! `hostsim`, `FEED_MEDIA` is the desktop's (the Feed over the simulator's sink, no URI driver) and `HOST_SINK`
//! is that sink itself (also `play-feed`, the module it lives in). A television build never has the desktop pair.
//! Each retires at W11a, when the composition moves into the platform that owns it
//! (`docs/platform-ports.md`, Target shape 8 and W7). The names this module publishes are a known list that a
//! pin in this file's tests holds: growing it fails until the addition and its retirement point are recorded there.
//!
//! The split is by thread. A [`Platform`] is registration data in a `static`: function pointers and
//! `&'static dyn` of `Sync` types, [`Shell`] included as a factory. The thread-affine state (the window,
//! the GL context, a toolkit's handles) lives only in the [`Surface`] that [`Shell::open`] returns on the
//! loop thread and that the application owns from then on.
//!
//! Beside the platform's own types, the vocabulary their signatures name is re-exported from the lower
//! layers it lives in ([`MainThread`], [`Action`], [`Edge`], [`RawKey`], [`Lifecycle`], [`PointerEdge`],
//! [`OpenAsk`], [`Placement`], [`InitHooks`]), so a platform crate names the application crate's types, and the vocabulary they use, through `host` alone.
//! [`sdl_open`] stays in the application, not in a platform crate: the `Surface` it returns is implemented for
//! `plx_shell_sdl::Window` there, which the orphan rule allows nowhere else.
use std::ffi::CStr;
use std::os::raw::c_int;

pub use crate::app::platform::{sdl_init, sdl_open, Composition, EventSink, FrameProbe, MediaBoot, Platform, Shell, Surface};
pub use crate::app::{Exit, Release};
pub use plx_base::task::MainThread;
pub use plx_machine::machine::Edge;
pub use plx_os::tv::Services;
pub use plx_shell_sdl::event::{Lifecycle, PointerEdge};
pub use plx_shell_sdl::{InitHooks, OpenAsk, Placement};
pub use plx_ui::consts::{Action, RawKey};
/// The simulator's video sink, the Feed backend's host sink built with the simulator's decoded picture: what the desktop
/// platform's `Services` hold as their video sink and display binding. Under the cfg of the module it lives in, so a
/// television build never has it. Retires at W11a with `FEED_MEDIA`.
#[cfg(all(feature = "hostsim", feature = "play-feed"))]
pub use crate::app::adapters::hostsim::HOST_SINK;

/// The television's [`MediaBoot`]: the plane owner's boot (`backends::acb_init`, once per process, on a boot that touches
/// the television) and the composition of the Feed plus the product URI driver (`backends::compose_native`). Compiled in every
/// build, because it carries no `cfg`: the application cannot know which platform names it. Retires at W11a, when the composition moves into the webOS platform.
pub const WEBOS_MEDIA: MediaBoot = MediaBoot {
    early_init: crate::app::adapters::backends::acb_init,
    compose: crate::app::adapters::backends::compose_native,
};

/// The desktop's [`MediaBoot`]: nothing to boot, and the Feed over the simulator's sink as the only driver, with no URI
/// driver and so no resolver. `hostsim` builds only. Retires at W11a with [`WEBOS_MEDIA`].
#[cfg(feature = "hostsim")]
pub const FEED_MEDIA: MediaBoot = MediaBoot {
    early_init: crate::app::adapters::backends::feed_early_init,
    compose: crate::app::adapters::backends::compose_feed_alone,
};

/// Boot the application on `platform` and return the status it ended with: the application's entry
/// (`run_application`), which installs the platform's services, brings the window up through its
/// [`Shell`], runs the loop and tears down. Ending the process is the caller's, as for the television's
/// and the desktop's own entries: a process that hosts the application never does.
///
/// `pms_host` and `pms_port` are the pair `plex_run` hands the entry and `plx_platform_desktop::run` takes.
pub fn run(platform: &'static Platform, pms_host: &CStr, pms_port: u16) -> Exit {
    crate::app::run_application(platform, pms_host.as_ptr(), c_int::from(pms_port))
}

/// **The owner's ruling, held in the source.** `tests/host_interface.rs` is a separate crate and reads only its own
/// spelling, so nothing there stops `lib.rs` from growing `pub mod app;` or `pub use app::platform::Shell;`: every
/// test would still pass. These pins read the crate's own source, in the style of the `include_str!` pins of
/// the webOS crate's `port.rs` and `app/boot.rs`, and fail on that edit. The reason is Decision 3's amendment of 2026-11-21
/// (`docs/platform-ports.md`): `host` is the only way a platform crate reaches the application's types.
#[cfg(test)]
mod surface_pin_tests {
    use std::path::Path;

    /// What the crate root may make `pub`, by the head of the item: `host`, and the simulator binary's own doors
    /// (a separate crate, so they must be `pub`), each existing only under `feature = "hostsim"`. `host` alone is
    /// open to a television build.
    const ROOT_ITEMS: [(&str, bool); 6] = [
        ("pub mod host", false),
        ("pub mod hud_cli", true),
        ("pub fn sim_runtime_dir", true),
        ("pub fn publish_sim_webos_major", true),
        ("pub fn sim_events_log", true),
        ("pub use app::synthetic_home_initial", true),
    ];

    const HOW_TO_PROCEED: &str = "The application is reachable only through `host`, because the owner's 2026-11-21 amendment to \
         Decision 3 (docs/platform-ports.md) makes `host` the only way a platform crate reaches it. To publish an \
         application item, re-export it from host.rs, which the second pin allows and the third pin (HOST_ITEMS) \
         lists. Only a new simulator door that is NOT an application item, existing only under `feature = \"hostsim\"`, \
         extends ROOT_ITEMS in this test, and the reason is recorded in that amendment.";

    /// The text of an item's head: up to its first `;`, `{`, `(` or `<`.
    fn head(line: &str) -> &str {
        let end = line.find([';', '{', '(', '<']).unwrap_or(line.len());
        line[..end].trim()
    }

    /// `(can be true, can be false)` for the cfg predicate at the start of `s` (whitespace removed), in a build with
    /// neither `hostsim` nor `test` and whatever else set; also returns the rest of `s`. Any other predicate may be
    /// either, so an `any(...)` naming another feature (`devtriggers` is on by default) can be true.
    fn can_be(s: &str) -> ((bool, bool), &str) {
        let end = s.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(s.len());
        let (name, rest) = s.split_at(end);
        if let Some(mut rest) = rest.strip_prefix('(') {
            let mut parts = Vec::new();
            while !rest.starts_with(')') {
                assert!(!rest.is_empty(), "an unbalanced cfg predicate");
                let (part, after) = can_be(rest);
                parts.push(part);
                rest = after.strip_prefix(',').unwrap_or(after);
            }
            let result = match name {
                "any" => (parts.iter().any(|p| p.0), parts.iter().all(|p| p.1)),
                "all" => (parts.iter().all(|p| p.0), parts.iter().any(|p| p.1)),
                "not" => (parts[0].1, parts[0].0),
                other => panic!("an unknown cfg combinator `{other}`"),
            };
            return (result, &rest[1..]);
        }
        if let Some(value) = rest.strip_prefix("=\"") {
            let (value, after) = value.split_once('"').expect("a closed string");
            let never = name == "feature" && value == "hostsim";
            return ((!never, true), after);
        }
        (((name != "test"), true), rest)
    }

    /// Whether the attribute `attr` (whitespace removed) is a `cfg` under which the item cannot exist without
    /// `hostsim` or `test`: `feature = "hostsim"`, an `any` of those two, or an `all` with `hostsim` among its members.
    fn confines_to_hostsim(attr: &str) -> bool {
        let predicate = attr.strip_prefix("#[cfg(").and_then(|a| a.strip_suffix(")]"));
        predicate.is_some_and(|p| !can_be(p).0 .0)
    }

    /// The index just past the `]` that closes the attribute opened at the start of `text` (`#[`), or `None` while it is
    /// still open. A bracket inside a string literal does not count.
    fn attribute_end(text: &str) -> Option<usize> {
        let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
        for (at, c) in text.char_indices() {
            if in_string {
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_string = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(at + 1);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The attributes that open `first` (each whitespace-removed), and what is left of the line after the last one.
    /// An attribute may span lines, so each is read from `more` to its closing bracket, and several may share a line.
    /// When the brackets balance mid-line, the text after the final `]` is the item's own line
    /// (`#[doc(hidden)] pub mod app;`), not something to skip.
    fn split_attributes<'a>(first: &str, more: &mut impl Iterator<Item = (usize, &'a str)>) -> (Vec<String>, String) {
        let mut attrs = Vec::new();
        let mut rest = first.trim().to_string();
        while rest.starts_with("#[") {
            let end = loop {
                if let Some(end) = attribute_end(&rest) {
                    break end;
                }
                match more.next() {
                    Some((_, next)) => {
                        rest.push(' ');
                        rest.push_str(next.trim());
                    }
                    None => break rest.len(),
                }
            };
            attrs.push(rest[..end].split_whitespace().collect());
            rest = rest[end..].trim_start().to_string();
        }
        (attrs, rest)
    }

    /// One `pub` item of a source: where it starts, the attributes before it (each whitespace-removed, those that share its
    /// line included), and its text: the item's own line, or a `pub use` read to its `;`, with single spaces.
    struct Public {
        at: usize,
        attrs: Vec<String>,
        text: String,
    }

    /// Every `pub` item of `source` (not `pub(crate)`, which has no space after `pub`).
    fn public_statements(source: &str) -> Vec<Public> {
        let mut found = Vec::new();
        let mut attrs: Vec<String> = Vec::new();
        let mut lines = source.lines().enumerate();
        while let Some((at, line)) = lines.next() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let (read, mut text) = split_attributes(line, &mut lines);
            attrs.extend(read);
            if text.is_empty() || text.starts_with("//") {
                continue;
            }
            if text.starts_with("pub ") {
                // A statement runs to its `;`, so a `{ ... }` list over several lines is one.
                while text.starts_with("pub use ") && !text.contains(';') {
                    match lines.next() {
                        Some((_, more)) => {
                            text.push(' ');
                            text.push_str(more.trim());
                        }
                        None => break,
                    }
                }
                found.push(Public { at: at + 1, attrs: std::mem::take(&mut attrs), text });
            }
            attrs.clear();
        }
        found
    }

    /// The `pub` items of `source` as `(line, head, confined)`.
    fn public_items(source: &str) -> Vec<(usize, String, bool)> {
        public_statements(source)
            .into_iter()
            .map(|p| (p.at, head(&p.text).to_string(), p.attrs.iter().any(|a| confines_to_hostsim(a))))
            .collect()
    }

    #[test]
    fn the_crate_root_publishes_host_and_the_simulators_hostsim_doors_and_nothing_else() {
        let source = include_str!("lib.rs");
        let items = public_items(source);
        for (at, item, confined) in &items {
            let Some(&(_, hostsim_only)) = ROOT_ITEMS.iter().find(|&&(known, _)| known == item.as_str()) else {
                panic!("lib.rs line {at} makes `{item}` public. {HOW_TO_PROCEED}");
            };
            assert!(
                !hostsim_only || *confined,
                "lib.rs line {at}: `{item}` is a simulator door and must exist only under `feature = \"hostsim\"` (with \
                 `test` or `all(..)` at most): an `any(..)` naming another feature, `devtriggers` being on by default, \
                 ships it in a television build. {HOW_TO_PROCEED}"
            );
        }
        for (known, _) in ROOT_ITEMS {
            assert!(
                items.iter().any(|(_, item, _)| item == known),
                "lib.rs no longer makes `{known}` public: update ROOT_ITEMS to match. {HOW_TO_PROCEED}"
            );
        }
        // An exported macro is a public path into the crate that a `pub ` line scan cannot see.
        assert!(!source.contains(concat!("#[macro_", "export")), "lib.rs exports a macro. {HOW_TO_PROCEED}");
    }

    /// `pub use` statements of `text`, each as `(line number, its text after `pub use` with whitespace removed)`.
    /// A statement runs to its `;`, so a `{ ... }` list over several lines is one, and an attribute on its line
    /// (`#[allow(unused_imports)] pub use ...;`) does not hide it.
    fn pub_uses(text: &str) -> Vec<(usize, String)> {
        public_statements(text)
            .into_iter()
            .filter_map(|p| {
                let tree = p.text.strip_prefix("pub use ")?;
                Some((p.at, tree.split_whitespace().collect()))
            })
            .collect()
    }

    /// Whether a `pub use` tree has `app` as a path segment anywhere: `crate::app::X`, `super::app::X` (from a module
    /// directly under `src/`), `self::app::X` (from `lib.rs`), `app::X` and `crate::{host, app::X}` all do.
    fn names_the_application(statement: &str) -> bool {
        statement.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|segment| segment == "app")
    }

    fn rust_sources(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).expect("the source directory").map(|e| e.expect("an entry").path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                rust_sources(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path.strip_prefix(root).expect("under the root").to_string_lossy().into_owned();
                out.push((name, std::fs::read_to_string(&path).expect("a source file")));
            }
        }
    }

    #[test]
    fn host_is_the_only_place_that_re_exports_the_application() {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut files = Vec::new();
        rust_sources(root, root, &mut files);
        assert!(files.iter().any(|(name, _)| name == "host.rs"), "the scan did not read this crate's own sources");
        let mut in_host = 0;
        for (name, text) in &files {
            for (at, statement) in pub_uses(text) {
                if !names_the_application(&statement) {
                    continue;
                }
                if name == "host.rs" {
                    in_host += 1;
                } else {
                    // The one other: the root's `hostsim`-only `synthetic_home_initial`, which the simulator binary calls.
                    assert!(
                        name == "lib.rs" && statement == "app::synthetic_home_initial;",
                        "{name} line {at} re-exports from the application (`pub use {statement}`). {HOW_TO_PROCEED}"
                    );
                }
            }
        }
        assert!(in_host >= 2, "host.rs re-exports the application's types: the scan found {in_host} `pub use` of `app`");
    }

    const HOW_TO_GROW_HOST: &str = "`host` is the application's whole public face to the platform crates, so what it carries is a \
         recorded list. To add a name, record the addition and its retirement point in docs/platform-ports.md \
         (Target shape 8's interface bullet and the W7 entry), then add it to HOST_ITEMS with the `cfg` its item carries.";

    /// Every name `host.rs` re-exports or defines `pub`, each with the predicate of the `cfg` its item must carry (whitespace
    /// removed; empty for none). The first groups are the interface of a platform's own types and entry; the media values
    /// and the simulator's sink are there because the application keeps the playback composition until W11a, which retires
    /// each of them (`docs/platform-ports.md`, Target shape 8 and W7).
    const HOST_ITEMS: [(&str, &str); 25] = [
        // What a platform builds, implements and reports input through, and the entry that boots it.
        ("Platform", ""),
        ("Shell", ""),
        ("Surface", ""),
        ("FrameProbe", ""),
        ("EventSink", ""),
        ("MediaBoot", ""),
        ("Composition", ""),
        ("Exit", ""),
        ("Release", ""),
        ("Services", ""),
        ("run", ""),
        // The vocabulary their signatures name.
        ("MainThread", ""),
        ("Edge", ""),
        ("Action", ""),
        ("RawKey", ""),
        ("Lifecycle", ""),
        ("PointerEdge", ""),
        ("OpenAsk", ""),
        ("Placement", ""),
        // The SDL helpers an SDL platform's `Shell` is built from; `sdl_open` stays in the application (the orphan rule).
        ("sdl_init", ""),
        ("sdl_open", ""),
        ("InitHooks", ""),
        // The media values: each retires at W11a.
        ("WEBOS_MEDIA", ""),
        ("FEED_MEDIA", r#"feature="hostsim""#),
        ("HOST_SINK", r#"all(feature="hostsim",feature="play-feed")"#),
    ];

    /// The names a `pub use` tree brings in: a group is expanded, `a as b` is `b`, and a glob is `*` (never on the list).
    fn use_leaves(tree: &str) -> Vec<String> {
        let tree = tree.trim();
        let Some(open) = tree.find('{') else {
            let leaf = tree.rsplit_once(" as ").map_or(tree, |(_, alias)| alias);
            return vec![leaf.rsplit("::").next().unwrap_or(leaf).trim().to_string()];
        };
        let inner = &tree[open + 1..tree.rfind('}').expect("a closed use group")];
        let (mut parts, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
        for (at, c) in inner.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&inner[start..at]);
                    start = at + 1;
                }
                _ => {}
            }
        }
        parts.push(&inner[start..]);
        parts.into_iter().filter(|part| !part.trim().is_empty()).flat_map(use_leaves).collect()
    }

    /// The name an item's head declares (`pub const NAME: T = T`, `pub fn name`, `pub static NAME`).
    fn item_name(head: &str) -> String {
        const KEYWORDS: [&str; 14] =
            ["pub", "unsafe", "const", "fn", "static", "mut", "struct", "enum", "trait", "type", "mod", "use", "async", "extern"];
        head.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .find(|token| !token.is_empty() && !KEYWORDS.contains(token))
            .expect("an item name")
            .to_string()
    }

    /// The third pin: the names `host.rs` makes `pub` are exactly HOST_ITEMS, each under exactly its `cfg`. The first two pins
    /// hold the crate root and every other file's re-exports out of the application; this holds `host` itself, which is
    /// allowed to re-export the application, to a list a reviewer reads.
    #[test]
    fn host_makes_public_exactly_the_recorded_names_under_their_recorded_cfgs() {
        let mut seen = Vec::new();
        for public in public_statements(include_str!("host.rs")) {
            let cfg = public
                .attrs
                .iter()
                .filter_map(|attr| attr.strip_prefix("#[cfg(").and_then(|attr| attr.strip_suffix(")]")))
                .collect::<Vec<_>>()
                .join(";");
            let names = match public.text.strip_prefix("pub use ") {
                Some(tree) => use_leaves(tree.trim_end_matches(';')),
                None => vec![item_name(head(&public.text))],
            };
            for name in names {
                let Some(&(_, required)) = HOST_ITEMS.iter().find(|&&(known, _)| known == name) else {
                    panic!("host.rs line {} makes `{name}` public, and it is not in HOST_ITEMS. {HOW_TO_GROW_HOST}", public.at);
                };
                assert_eq!(
                    cfg, required,
                    "host.rs line {}: `{name}` must carry exactly `cfg({required})` (empty: none), as HOST_ITEMS records it: a \
                     television build must have no desktop name. {HOW_TO_GROW_HOST}",
                    public.at
                );
                seen.push(name);
            }
        }
        for (known, _) in HOST_ITEMS {
            assert!(
                seen.iter().any(|name| name == known),
                "host.rs no longer makes `{known}` public: remove it from HOST_ITEMS, and record its retirement in \
                 docs/platform-ports.md. {HOW_TO_GROW_HOST}"
            );
        }
    }

    /// The scanners read an attribute and its item on one line, and an attribute that spans lines, with the item after its
    /// closing `]`. Before, a line starting `#[` was only ever an attribute, so `#[doc(hidden)] pub mod app;` and
    /// `#[allow(unused_imports)] pub use app::platform::Shell;` passed every pin above. Each fixture line is its own
    /// string, so no line of this file starts with the items it holds (the pins read this file too).
    #[test]
    fn the_scanners_read_an_attribute_and_its_item_on_one_line() {
        let source = [
            "#[doc(hidden)] pub mod app;",
            "#[allow(unused_imports)] pub use app::platform::Shell;",
            "#[cfg(feature = \"hostsim\")] pub fn sim_runtime_dir() {}",
            "#[cfg(any(",
            "    test,",
            "    feature = \"hostsim\"",
            "))] pub mod hud_cli;",
            "#[cfg(feature = \"hostsim\")] #[doc(hidden)] pub fn two_on_a_line() {}",
            "#[cfg(feature = \"hostsim\")]",
            "#[allow(dead_code)] pub fn two_groups() {}",
            "#[doc = \"a ] in a string\"] pub fn bracket_in_a_string() {}",
            "#[allow(unused)] fn private() {}",
            "pub fn plain() {}",
        ]
        .join("\n");
        let items = public_items(&source);
        let heads: Vec<(&str, bool)> = items.iter().map(|(_, head, confined)| (head.as_str(), *confined)).collect();
        assert_eq!(
            heads,
            [
                ("pub mod app", false),
                ("pub use app::platform::Shell", false),
                ("pub fn sim_runtime_dir", true),
                ("pub mod hud_cli", true),
                ("pub fn two_on_a_line", true),
                ("pub fn two_groups", true),
                ("pub fn bracket_in_a_string", false),
                ("pub fn plain", false),
            ]
        );
        assert_eq!(items[1].0, 2, "the line is the one the attribute starts on");
        assert_eq!(pub_uses(&source), [(2, "app::platform::Shell;".to_string())]);
        // The attribute above a multi-line `pub use` list still reaches the statement, and the attribute is not the item.
        let list = ["#[cfg(feature = \"hostsim\")]", "pub use a::{", "    B,", "    C,", "};"].join("\n");
        assert_eq!(pub_uses(&list), [(2, "a::{B,C,};".to_string())]);
        assert!(public_statements(&list)[0].attrs.iter().any(|a| confines_to_hostsim(a)));
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod media_boot_tests {
    use super::*;

    /// `FEED_MEDIA` is the media boot the desktop platform states (`plx_platform_desktop`'s `MEDIA`): it composes the
    /// Feed over the host sink and no URI driver, so the session holds no resolver and offers no `Uri`.
    #[test]
    fn the_desktop_feed_boot_composes_the_feed_and_no_resolver() {
        let mt = unsafe { MainThread::assume() };
        (FEED_MEDIA.early_init)(&mt);
        let mut adapter = plx_media::player::adapter::PlayerAdapter::new(mt);
        let composed = (FEED_MEDIA.compose)(&mut adapter, false);
        assert!(composed.resolver.is_none());
        assert!(!adapter.composed_backends().uri);
        #[cfg(feature = "play-feed")]
        assert!(adapter.composed_backends().feed, "the Feed is the desktop's driver");
    }
}
