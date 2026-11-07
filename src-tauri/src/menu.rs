// SPDX-License-Identifier: MIT
//! macOS menu bar for C.16.
//!
//! Menu structure: `CodeSense | Edit | View | Go`. The app (`CodeSense`)
//! submenu hosts `Settings…` between About and Services, per macOS
//! HIG. The hardcoded item ids in `MENU_ITEMS` mirror `SHORTCUT_ACTIONS`
//! in `src/shared/lib/shortcuts/registry.ts` and MUST stay in sync.
//!
//! The `Go` submenu additionally carries a registry-driven nav section:
//! the frontend derives the curated nav commands from `NAV_AXES` and
//! pushes them (id + label + effective accelerator) on every
//! `rebuild_menu`, mirroring how `View → Panels` is pushed. These are
//! NOT in `MENU_ITEMS` — there is no hardcoded Rust duplicate of a nav
//! label or keystroke — so rebinding/relabelling a nav command in
//! Settings updates the menu on the next rebuild with no Rust change.
//! The pushed nav group renders above the hardcoded org/team switchers.
//!
//! Accelerators ARE displayed — each item shows the user's current
//! effective keystroke. The menu is rebuilt whenever the user rebinds
//! a shortcut in Settings via the `rebuild_menu` Tauri command, so
//! the displayed accelerators never go stale.
//!
//! Click handling: every custom item emits the same `menu:action`
//! event with the item's id as payload. The frontend has a single
//! listener that dispatches via the TS registry's `run()`.

use std::collections::HashMap;
use tauri::{AppHandle, Runtime};

#[cfg(target_os = "macos")]
use tauri::{
    menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuEvent, MenuItemBuilder, SubmenuBuilder},
    Emitter,
};

/// SW-RP-1: one entry in the registry-driven `View → Panels` submenu. The
/// frontend panel registry supplies these on every `rebuild_menu` call;
/// `checked` reflects whether the panel is currently present in the layout.
/// The menu item id is `panel:<id>` so `on_event`/the frontend router can
/// distinguish a panel toggle from a shortcut action.
#[derive(serde::Deserialize)]
pub struct PanelMenuEntry {
    pub id: String,
    pub label: String,
    pub checked: bool,
}

/// One entry in the registry-driven `Go` nav section. The frontend derives these
/// from `NAV_AXES` on every `rebuild_menu` call and supplies each id's live
/// effective `accelerator` (already in Tauri accelerator grammar), so rebinding
/// or relabelling a nav command in Settings updates the menu with no Rust change.
/// The item id is the plain registry action id (e.g. `viewer.cycle-workspace-next`),
/// so clicking it dispatches through the same `menu:action` → registry path as any
/// other shortcut item.
#[derive(serde::Deserialize)]
pub struct NavMenuEntry {
    pub id: String,
    pub label: String,
    pub accelerator: String,
}

/// Canonical (id, label, default_accelerator) triples for every
/// menu-mirrored shortcut. Mirrors `SHORTCUT_ACTIONS` in
/// `src/shared/lib/shortcuts/registry.ts`. The Rust unit test asserts
/// the full set + uniqueness.
///
/// The `CmdOrCtrl+…` strings are Tauri's cross-platform accelerator
/// form (macOS renders `⌘`, elsewhere `Ctrl`).
pub const MENU_ITEMS: &[(&str, &str, &str)] = &[
    ("settings.open", "Settings…", "CmdOrCtrl+,"),
    ("viewer.toggle-terminal", "Toggle Terminal", "CmdOrCtrl+\\"),
    (
        "viewer.toggle-right-pane",
        "Toggle Right Pane",
        "CmdOrCtrl+J",
    ),
    ("viewer.toggle-pane-full-screen", "Toggle Pane Full Screen", ""),
    // SW-EW-35: full-screen / console toggle (rebindable). Replaced the dead
    // `nav.switch-workspace` entry (retired from the TS registry in SW-EW-33 —
    // it emitted a `menu:action` with no `run()` handler).
    (
        "viewer.toggle-console",
        "Toggle Console",
        "CmdOrCtrl+Shift+\\",
    ),
    ("nav.switch-org", "Switch Context Repo", "CmdOrCtrl+Shift+O"),
    ("nav.switch-team", "Switch Team", "CmdOrCtrl+Alt+Shift+T"),
];

/// Look up a MENU_ITEMS entry by id. Used by `build_item` to resolve
/// label + default accelerator from a single source.
#[cfg(target_os = "macos")]
fn lookup(id: &str) -> (&'static str, &'static str) {
    for (candidate, label, default_accel) in MENU_ITEMS {
        if *candidate == id {
            return (label, default_accel);
        }
    }
    // This panic is unreachable in practice — all callers pass ids
    // from MENU_ITEMS. A missing id indicates a coding mistake.
    panic!("menu: unknown item id {id:?}");
}

/// Build a single menu item with the user's effective accelerator.
/// Falls back to the default when the override fails to parse (Tauri
/// validates accelerator strings at build time).
#[cfg(target_os = "macos")]
fn build_item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    overrides: &HashMap<String, String>,
) -> tauri::Result<tauri::menu::MenuItem<R>> {
    let (label, default_accel) = lookup(id);
    let user_accel = overrides.get(id).map(String::as_str);

    // An empty override means the person cleared the chord (or a dashboard
    // command took it), so the menu must not keep firing it.
    if user_accel == Some("") {
        return MenuItemBuilder::with_id(id, label).build(app);
    }
    if let Some(accel) = user_accel {
        match MenuItemBuilder::with_id(id, label)
            .accelerator(accel)
            .build(app)
        {
            Ok(item) => return Ok(item),
            Err(e) => {
                log::warn!(
                    target: "menu",
                    "menu: override accelerator {accel:?} for {id} failed ({e}); falling back to default"
                );
            }
        }
    }

    let item = MenuItemBuilder::with_id(id, label);
    if default_accel.is_empty() {
        item.build(app)
    } else {
        item.accelerator(default_accel).build(app)
    }
}

/// SW-RP-1: build the registry-driven `Panels` submenu of checkable items.
/// Item id is `panel:<id>`; toggling emits `menu:action` like any other item.
#[cfg(target_os = "macos")]
fn build_panels_submenu<R: Runtime>(
    app: &AppHandle<R>,
    panels: &[PanelMenuEntry],
) -> tauri::Result<tauri::menu::Submenu<R>> {
    let items = panels
        .iter()
        .map(|p| {
            CheckMenuItemBuilder::with_id(format!("panel:{}", p.id), &p.label)
                .checked(p.checked)
                .build(app)
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let mut builder = SubmenuBuilder::new(app, "Panels");
    for item in &items {
        builder = builder.item(item);
    }
    builder.build()
}

/// Build the registry-driven nav items for the `Go` submenu. Each item carries
/// the effective accelerator the frontend pushed. A pushed accelerator that
/// fails to parse is dropped for that item (built label-only) rather than
/// failing the whole menu — the keyboard handler remains authoritative for
/// dispatch regardless of whether the menu renders the hint.
#[cfg(target_os = "macos")]
fn build_nav_items<R: Runtime>(
    app: &AppHandle<R>,
    nav: &[NavMenuEntry],
) -> tauri::Result<Vec<tauri::menu::MenuItem<R>>> {
    nav.iter()
        .map(|n| {
            if n.accelerator.is_empty() {
                return MenuItemBuilder::with_id(&n.id, &n.label).build(app);
            }
            match MenuItemBuilder::with_id(&n.id, &n.label)
                .accelerator(&n.accelerator)
                .build(app)
            {
                Ok(item) => Ok(item),
                Err(e) => {
                    log::warn!(
                        target: "menu",
                        "menu: nav accelerator {:?} for {} failed ({e}); building without accelerator",
                        n.accelerator, n.id
                    );
                    MenuItemBuilder::with_id(&n.id, &n.label).build(app)
                }
            }
        })
        .collect()
}

/// Build the full menu tree from `MENU_ITEMS` + the user's overrides + the
/// registry-driven panel and nav lists. Used by both the startup builder (empty
/// overrides + no panels/nav) and the `rebuild_menu` command.
#[cfg(target_os = "macos")]
fn build_menu_internal<R: Runtime>(
    app: &AppHandle<R>,
    overrides: &HashMap<String, String>,
    panels: &[PanelMenuEntry],
    nav: &[NavMenuEntry],
) -> tauri::Result<Menu<R>> {
    // CodeSense submenu: About, (sep), Settings…, (sep), Services,
    // (sep), Hide/HideOthers/ShowAll, (sep), Quit. Matches macOS HIG.
    let settings_item = build_item(app, "settings.open", overrides)?;
    let app_sub = SubmenuBuilder::new(app, "CodeSense")
        .about(None)
        .separator()
        .item(&settings_item)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    // Standard macOS Edit menu. Tauri's predefined roles wire each item
    // to the responder-chain action (`undo:`, `copy:`, `paste:`, etc.)
    // so the OS forwards the matching keystroke to whichever view has
    // first-responder status — typically the webview. Without this
    // submenu the webview never receives `copy:` / `paste:` and Cmd+C /
    // Cmd+V silently no-op.
    //
    // `select_all()` is included so Cmd+A reaches input elements (search
    // fields, text inputs) via the responder chain. Without it, macOS
    // swallows the keystroke before the webview sees it. When focus is
    // inside an <input>, the browser scopes Select All to that input's
    // text — it does not select the whole document.
    let edit_sub = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let mut view_builder = SubmenuBuilder::new(app, "View")
        .item(&build_item(app, "viewer.toggle-terminal", overrides)?)
        .item(&build_item(app, "viewer.toggle-right-pane", overrides)?)
        .item(&build_item(app, "viewer.toggle-pane-full-screen", overrides)?)
        .item(&build_item(app, "viewer.toggle-console", overrides)?);
    // SW-RP-1: append the registry-driven Panels submenu (close/re-open panels)
    // when the frontend has supplied panels. Built wholesale on each rebuild.
    let panels_sub = if panels.is_empty() {
        None
    } else {
        Some(build_panels_submenu(app, panels)?)
    };
    if let Some(ref sub) = panels_sub {
        view_builder = view_builder.separator().item(sub);
    }
    let view_sub = view_builder.build()?;

    // Go submenu: the registry-driven nav representatives (pushed from the
    // frontend) grouped above the org/team switchers. The switchers stay
    // hardcoded in MENU_ITEMS so they render at first menu build, before the
    // frontend's first post-rehydrate push; the nav group appears once that
    // push arrives (empty at startup — no leading separator then).
    let nav_items = build_nav_items(app, nav)?;
    let mut go_builder = SubmenuBuilder::new(app, "Go");
    for item in &nav_items {
        go_builder = go_builder.item(item);
    }
    if !nav_items.is_empty() {
        go_builder = go_builder.separator();
    }
    let go_sub = go_builder
        .item(&build_item(app, "nav.switch-org", overrides)?)
        .item(&build_item(app, "nav.switch-team", overrides)?)
        .build()?;

    MenuBuilder::new(app)
        .item(&app_sub)
        .item(&edit_sub)
        .item(&view_sub)
        .item(&go_sub)
        .build()
}

/// Startup menu builder. No disk reads, no user overrides — just the
/// defaults from MENU_ITEMS. The frontend calls `rebuild_menu` once
/// after the shortcuts store rehydrates, so any live overrides appear
/// within the first render cycle after startup.
#[cfg(target_os = "macos")]
pub fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    build_menu_internal(app, &HashMap::new(), &[], &[])
}

/// Menu event forwarder. Pure dispatch — emits `menu:action` with the
/// item id as payload. Business logic lives on the frontend side
/// (`SHORTCUT_ACTIONS[].run()`).
#[cfg(target_os = "macos")]
pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let id = event.id().0.clone();
    // SV-8: the menu-bar item's entries are its own to handle.
    if id.starts_with(crate::commands::terminal_server_tray::MENU_ID_PREFIX) {
        return;
    }
    log::info!(target: "menu", "menu item clicked: {id}");
    if let Err(e) = app.emit("menu:action", id) {
        log::warn!(target: "menu", "failed to emit menu:action: {e}");
    }
}

/// Rebuild the macOS menu with the user's current shortcut overrides.
/// Called by the frontend after the shortcuts store rehydrates and on
/// every successful `setShortcut` / `resetShortcut` / `resetAll`.
///
/// Windows/Linux: no-op (macOS-only menu).
#[tauri::command]
#[allow(unused_variables)]
pub async fn rebuild_menu<R: Runtime>(
    app: AppHandle<R>,
    overrides: HashMap<String, String>,
    panels: Vec<PanelMenuEntry>,
    nav: Vec<NavMenuEntry>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let menu =
            build_menu_internal(&app, &overrides, &panels, &nav).map_err(|e| format!("{e}"))?;
        app.set_menu(menu).map_err(|e| format!("{e}"))?;
        log::info!(
            target: "menu",
            "rebuild_menu: applied with {} override(s), {} panel(s), {} nav item(s)",
            overrides.len(),
            panels.len(),
            nav.len()
        );
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // MENU_ITEMS holds ONLY the statically-built items (app/view toggles + the
    // Go org/team switchers). The curated nav commands are pushed dynamically as
    // `NavMenuEntry`s and are intentionally NOT here — the frontend
    // `buildNavMenuEntries` test pins that set.
    #[test]
    fn menu_items_cover_the_expected_set() {
        let ids: Vec<&str> = MENU_ITEMS.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(
            ids,
            vec![
                "settings.open",
                "viewer.toggle-terminal",
                "viewer.toggle-right-pane",
                "viewer.toggle-pane-full-screen",
                "viewer.toggle-console",
                "nav.switch-org",
                "nav.switch-team",
            ],
        );
    }

    // Pins the frontend↔Rust contract for the pushed Go nav section: the
    // `{ id, label, accelerator }` shape `rebuild_menu` receives. A drift on
    // either side (renamed/removed field) fails here.
    #[test]
    fn nav_menu_entry_deserializes_from_the_pushed_shape() {
        let entry: NavMenuEntry = serde_json::from_value(serde_json::json!({
            "id": "viewer.cycle-workspace-next",
            "label": "Cycle Story Workspace Next",
            "accelerator": "CmdOrCtrl+Alt+]",
        }))
        .expect("nav entry deserializes");
        assert_eq!(entry.id, "viewer.cycle-workspace-next");
        assert_eq!(entry.label, "Cycle Story Workspace Next");
        assert_eq!(entry.accelerator, "CmdOrCtrl+Alt+]");
    }

    #[test]
    fn menu_item_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (id, _, _) in MENU_ITEMS {
            assert!(seen.insert(*id), "duplicate id: {id}");
        }
    }

    #[test]
    fn pane_full_screen_has_no_default_accelerator() {
        assert!(MENU_ITEMS.contains(&(
            "viewer.toggle-pane-full-screen",
            "Toggle Pane Full Screen",
            "",
        )));
    }

    #[test]
    fn every_bound_default_accelerator_uses_cmd_or_ctrl() {
        for (id, _, accel) in MENU_ITEMS {
            assert!(
                accel.is_empty() || accel.starts_with("CmdOrCtrl"),
                "{id}: accelerator {accel:?} should start with CmdOrCtrl",
            );
        }
    }

    #[test]
    fn settings_label_uses_macos_ellipsis() {
        let (_, label, _) = MENU_ITEMS
            .iter()
            .find(|(id, _, _)| *id == "settings.open")
            .expect("settings.open exists");
        assert!(
            label.ends_with('…'),
            "settings label should end with a single-character ellipsis per macOS HIG, got {label:?}"
        );
    }
}
