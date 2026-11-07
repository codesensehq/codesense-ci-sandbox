// SPDX-License-Identifier: MIT
//! The X1 guard — no unattended writers.
//!
//! Only three things may write into a workspace or team content: Step 1 for
//! that workspace, Step 2 for an agent being launched in it, and an explicit
//! operation started by an MCP call or a human action. This test parses the
//! crate with `syn`, builds a **transitive** call graph, and fails if any
//! entry point that is not a reviewed explicit operation reaches a write
//! primitive, printing the call path.
//!
//! **What is parsed.** The crate as the compiler sees it in a normal build:
//! the walk starts at `src/lib.rs` and follows `mod` declarations (honouring
//! `#[path]`), skipping every item whose `#[cfg]` is false in a default-feature,
//! non-test build (`#[cfg(test)]`, `#[cfg(all(test, …))]`,
//! `#[cfg(any(test, feature = "off-by-default"))]`, `#[cfg(doc)]`) and every
//! `#[test]`. Platform cfgs (`unix`, `target_os`) count as on, so both arms of a
//! platform split are in the graph. No filename decides what is test code.
//!
//! **Roots — deny by default.** Every one of these is checked:
//! - the app's own seams in `lib.rs` ([`APP_ROOTS`]);
//! - every notify-watcher handler (a function that registers a dispatch
//!   handler or builds one);
//! - every **background body**: each closure, async block or call handed to
//!   `spawn` / `spawn_blocking` (it runs on its own), and each one handed to a
//!   registration — `listen`, `subscribe`, `register_handler`, `on_*_event`,
//!   `setup`, the app's `run` callback, a `notify` watcher's constructor
//!   ([`TASK_CALLS`], [`EVENT_CALLS`]). A registered callback is **not**
//!   credited to the function that registers it (registering does not run
//!   it); a spawned task is also reachable from its spawner, since the spawner
//!   does start it;
//! - every `#[tauri::command]`;
//! - every HTTP route (`.route(PATH, handler)`).
//!
//! A command, route or background body may reach a sink only if it is on the
//! reviewed [`EXPLICIT`] list, with its reason: it is an explicit operation
//! (X1 (c)) or the Step 1 / Step 2 of a launch (X1 (a)/(b)). Anything else
//! that reaches a sink fails, naming it. MCP tools (`#[tool]` methods behind
//! the `/mcp` service) are explicit by definition and are not roots; their
//! spawned tasks are.
//!
//! - **Sinks** ([`SINKS`]): the Step 2 engine, the explicit hydrate and
//!   publish/route verbs, the structural team-content op, the selected-context
//!   `execute`, and the remote batch Apply (`workspace_context_batch_within`);
//!   plus the retired names (`materialize_*`, `receive_startup`,
//!   `execute_reverse_sync`, `claim_now`) should they ever come back.
//! - **Allowed paths** ([`ALLOWED`]): `(via, target)` — once a walk has passed
//!   through `via`, it does not enter `target`. Each has its reason; an entry
//!   no root crosses fails the test.
//!
//! Resolution is by name and deliberately conservative. A path call reaches
//! every function whose module or type ends with the path's qualifier,
//! following `pub use` re-exports (named and glob) of that module. A method
//! call on `self` reaches that type's method; any other method call reaches
//! every method of that name when only one type defines it (and the name is
//! not also a common std/tokio/tauri method), else the methods of the types
//! the calling function names (its signature, generic bounds, body, `self`
//! type and that type's fields, and the traits they implement — so a
//! `&dyn Trait` or `T: Trait` call reaches every impl). A call through a
//! struct field (`(self.on_change)(…)`) reaches every closure or function
//! stored into a field of that name. Calls inside macro invocations are read
//! from their tokens. A spurious path is cut with an [`ALLOWED`] entry and a
//! reason, never by weakening the walk.
//!
//! Known blind spots, kept honest by the reach floors and the unresolved
//! count the main test prints: a closure kept in a variable and passed to a
//! registration from another function; calls through a function pointer
//! held in a local that came from elsewhere; `#[path]` inside inline modules.
//!
//! The planted self-tests prove each dispatch form (direct chain, trait
//! object, generic bound, field fn, subscribe callback, spawn, re-export)
//! is followed. Point `CODESENSE_GUARD_ROOT` at another checkout's `src-tauri`
//! to run it there.

use codesense_guards::guard_support::app_root as root;
use proc_macro2::{Delimiter, TokenStream, TokenTree};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

// ---- The policy ----------------------------------------------------------------

/// A function the graph names: `module::path::[Type::]name`, matched as a
/// suffix of a node's full label (`commands::watch::lazy_attach_workspace`);
/// `crate::name` names a crate-root (`lib.rs`) function exactly. A background
/// body is labelled after the function it is written in:
/// `routines::scheduler::spawn_scheduler::{spawn}`, `crate::run::{setup}`.
type Name = &'static str;

/// The app's own unattended seams in `lib.rs`, each with the least number of
/// functions its walk must reach (so a parser regression cannot pass by
/// seeing nothing). Everything else is discovered.
const APP_ROOTS: &[(Name, &str, usize)] = &[
    (
        "crate::run",
        "app start: builder, plugins, the managed state it constructs",
        200,
    ),
    ("crate::on_window_focus_changed", "window focus handler", 1),
    (
        "crate::on_osc133_command_end",
        "OSC-133 command-end handler",
        1,
    ),
    ("crate::spawn_sync_timers", "app setup timers", 1),
];

/// Write primitives (X1), as suffixes of node labels. A `*` suffix
/// matches a name prefix.
const SINKS: &[Name] = &[
    "agents::prepare::prepare",
    "agents::prepare::step3::hydrate",
    "agents::prepare::step3::hydrate_everything_into_a_workspace",
    "agents::prepare::publish::publish",
    "agents::prepare::publish::route",
    "agents::prepare::publish::unroute",
    "install::story_linker::apply_structural_source_op",
    "mcp::context_operations::explicit::execute",
    "mcp::context_operations::explicit::execute_local",
    // Discard of a new file deletes it from the workspace.
    "agents::prepare::publish::discard_new_files",
    "terminal::session_server::SessionServerClient::workspace_context_batch_within",
    // Retired writers; each is a sink again if it ever comes back.
    "materialize_*",
    "receive_startup",
    "execute_reverse_sync",
    "claim_now",
];

/// The sinks that must exist (the rest are retired names). Keeps the guard
/// from passing vacuously after a rename.
const LIVE_SINKS: &[Name] = &[
    "agents::prepare::prepare",
    "agents::prepare::step3::hydrate",
    "agents::prepare::publish::publish",
    "install::story_linker::apply_structural_source_op",
    "mcp::context_operations::explicit::execute",
    "agents::prepare::publish::discard_new_files",
    "terminal::session_server::SessionServerClient::workspace_context_batch_within",
];

/// The reviewed entry points that may write: `#[tauri::command]`s (label
/// suffix), HTTP routes (`route:PATH`) and background bodies (label suffix),
/// each pinned to the sinks it is reviewed to reach (label suffixes). Each
/// must exist, must still reach a sink (else it is stale) and must reach no
/// sink outside its pin.
const EXPLICIT: &[(Name, &[Name], &str)] = &[
    // The person confirmed a terminal server update (or asked to
    // reopen an unfinished update's tabs): every one of these is attended.
    // Shells carry no launch, Agent or prepared workspace, so Step 2 is
    // skipped at runtime (`restore::reopen_request`, tested). A saved
    // Agent that opted in (`resumeAfterRestart`) is relaunched with its
    // resume brief (`resume::resume_request`, tested), and that relaunch's
    // Step 2 is the launch the person's confirmation started — X1 (b)'s shape.
    (
        "commands::host_connection_manager::restart_machine_terminal_server",
        &["agents::prepare::prepare"],
        "SW-AGR-19: a confirmed update reopens shells (no Step 2) and relaunches opted-in Agents (their Step 2, attended)",
    ),
    (
        "commands::machines::reinstall::reinstall_machine_terminal_server",
        &["agents::prepare::prepare"],
        "SW-AGR-19: a confirmed reinstall reopens shells (no Step 2) and relaunches opted-in Agents (their Step 2, attended)",
    ),
    (
        "commands::machines::restore::reopen_machine_pending_tabs",
        &["agents::prepare::prepare"],
        "SW-AGR-19: the person's Reopen opens shells (no Step 2) and relaunches opted-in Agents (their Step 2, attended)",
    ),
    (
        "commands::machines::local_update::update_local_terminal_server",
        &["agents::prepare::prepare"],
        "SW-AGR-19: This Mac's confirmed update reopens shells (no Step 2) and relaunches opted-in Agents (their Step 2, attended)",
    ),
    // Update all, the same confirmed updates one machine at a
    // time — started by the person's confirmation (the Tauri command) or an
    // agent's `update_terminal_servers` (Interactive gate, confirmEndSessions,
    // its reviewed `expected`), and run on the task it spawns. Its reopens
    // are the per-machine updates' above.
    (
        "commands::machines::update_all::start_terminal_servers_update",
        &["agents::prepare::prepare"],
        "SW-AGR-19: the person's confirmed Update all reopens shells (no Step 2) and relaunches opted-in Agents (their Step 2, attended)",
    ),
    (
        "commands::machines::update_all::start::{spawn}",
        &["agents::prepare::prepare"],
        "SW-AGR-19: the run a confirmed Update all (or update_terminal_servers) started; its reopens are the confirmed updates'",
    ),
    (
        "commands::machines::update_all::start::{spawn}::{spawn}",
        &["agents::prepare::prepare"],
        "SW-AGR-19: the run a confirmed Update all (or update_terminal_servers) started (its worker, watched by the run's task so a panic still ends the run); its reopens are the confirmed updates'",
    ),
    // X1 (b): the user launches or opens an agent; the Step 2 is that launch's.
    (
        "commands::agents_launch::agent_launch_prepare",
        &["agents::prepare::prepare"],
        "X1 (b): the launch dialog's Step 2",
    ),
    (
        "commands::workspace_open::agent_launch_open",
        &["agents::prepare::prepare"],
        "X1 (b): the user opens an agent; its Step 2",
    ),
    (
        "commands::agents::agent_open_authoring_agent",
        &["agents::prepare::prepare"],
        "X1 (b): the user opens the authoring agent; its Step 2",
    ),
    (
        "commands::routines::routine_open_authoring_agent",
        &["agents::prepare::prepare"],
        "X1 (b): the user opens a routine's authoring agent; its Step 2",
    ),
    (
        "commands::pty_layout::terminal_remote_shell_tab",
        &["agents::prepare::prepare"],
        "X1 (b): the user opens an agent tab on a machine; its Step 2",
    ),
    (
        "weft::dispatch_bridge::wv_dispatch_worker",
        &["agents::prepare::prepare"],
        "X1 (b): a dispatch launches a worker agent; its Step 2",
    ),
    // A machine-armed routine's workspace is prepared only by the
    // user's arm/edit/restore/resume, or by an explicit Refresh on machine
    // (all through `machine_arm::refresh_on_machine`); never by the arm sync.
    (
        "commands::routines::routine_refresh_on_machine",
        &["agents::prepare::prepare"],
        "X1 (c): the user's Refresh on machine",
    ),
    (
        "commands::routines::routine_arm_on",
        &["agents::prepare::prepare"],
        "X1 (c): the user arms a routine on a machine; its agent is prepared there",
    ),
    (
        "commands::routines::routine_save",
        &["agents::prepare::prepare"],
        "X1 (c): the user edits an armed routine; its agent is re-prepared",
    ),
    (
        "commands::routines::routine_restore_revision",
        &["agents::prepare::prepare"],
        "X1 (c): the user restores a routine revision; an armed agent is re-prepared",
    ),
    (
        "commands::routines::routine_handover_release",
        &["agents::prepare::prepare"],
        "X1 (c): the user releases a handover; the armed agent is re-prepared",
    ),
    (
        "commands::routines::routine_set_enabled",
        &["agents::prepare::prepare"],
        "X1 (c): the user resumes an armed routine; its agent is re-prepared",
    ),
    // X1 (c): explicit operations the user starts.
    (
        "commands::agent_inbox::trigger_hydrate_from_inbox",
        &["mcp::context_operations::explicit::execute"],
        "X1 (c): the user hydrates from an inbox item",
    ),
    (
        "commands::context_operation::discard_context_edits",
        &["mcp::context_operations::explicit::execute"],
        "X1 (c): the user discards context edits",
    ),
    (
        "commands::team_file_edits::publish_context_edits",
        &["mcp::context_operations::explicit::execute"],
        "X1 (c): the user publishes team-file edits from the list (SW-XS-27)",
    ),
    (
        "commands::team_file_edits::discard_team_file_edits",
        &["mcp::context_operations::explicit::execute"],
        "X1 (c): the user discards team-file edits from the list (SW-XS-27)",
    ),
    (
        "mcp::context_operations::explicit::run_bound::{spawn_blocking}",
        &["mcp::context_operations::explicit::execute"],
        "X1 (c): the worker of one explicit context operation (MCP call or the two commands above)",
    ),
    (
        "commands::files::delete_content_file",
        &["install::story_linker::apply_structural_source_op"],
        "X1 (c): the user deletes a team-content file",
    ),
    (
        "commands::files::move_content_file",
        &["install::story_linker::apply_structural_source_op"],
        "X1 (c): the user moves a team-content file",
    ),
    (
        "commands::route_to_context::route_file_to_context",
        &["agents::prepare::publish::route"],
        "X1 (c): the user routes a file to a context",
    ),
    (
        "commands::route_to_context::run_route_for::{spawn_blocking}",
        &["agents::prepare::publish::route"],
        "X1 (c): the worker of one route_file_to_context call",
    ),
    (
        "commands::route_to_context::unroute_file_from_context",
        &["agents::prepare::publish::unroute"],
        "X1 (c): the user unroutes a file",
    ),
    (
        "commands::route_to_context::run_unroute::{spawn_blocking}",
        &["agents::prepare::publish::unroute"],
        "X1 (c): the worker of one unroute_file_from_context call",
    ),
    (
        "commands::file_surface::file_surface_call",
        &["install::story_linker::apply_structural_source_op"],
        "X1 (c): a page's files.delete / files.move; allowed by default (no gesture model) and \
         confirm-gateable per dashboard (files.delete and files.move are confirmable)",
    ),
];

/// Reviewed `(via, target)` paths: once a walk has passed through `via`, it
/// does not enter `target`. Both are label suffixes.
const ALLOWED: &[(Name, Name, &str)] = &[
    (
        "routines::dispatch::launch",
        "agents::prepare::prepare",
        "X1 (b): a routine fire launches its agent; its Step 2 is that launch's",
    ),
    (
        "routines::wake::open_remote",
        "agents::prepare::prepare",
        "X1 (b): a routine fire (timer, wake, queued redispatch, sprint claim) starts its \
         agent on a machine; its Step 2 is that launch's",
    ),
    (
        "routines::workspace::prepare",
        "agents::prepare::prepare",
        "X1 (b): SV-PBF-159: a local routine fire (timer, wake, queued redispatch after an \
         archive or stop, sprint claim) prepares the launched agent's disposable working \
         directory (`routines::dispatch::prepare`); its Step 2 is that launch's",
    ),
];

/// Calls whose closure / async-block / call arguments run on their own: each
/// argument is its own background root, and the spawner reaches it too.
const TASK_CALLS: &[&str] = &[
    "spawn",
    "spawn_blocking",
    "spawn_local",
    "run_on_main_thread",
];

/// Registrations: each argument runs later, on a stimulus, so it is its own
/// background root and is not credited to the registering function.
const EVENT_CALLS: &[&str] = &[
    "listen",
    "listen_any",
    "once",
    "once_any",
    "subscribe",
    "register_handler",
    "on_window_event",
    "on_menu_event",
    "on_tray_icon_event",
    "on_page_load",
    "on_webview_event",
    "setup",
    "register_uri_scheme_protocol",
    "register_asynchronous_uri_scheme_protocol",
];

/// Method names std, tokio, tauri or serde types also define: a call to one
/// is resolved only to the crate types the caller names, even when a single
/// crate type defines it (`mutex.lock()` is not `PrivateRegistry::lock`).
const COMMON_METHODS: &[&str] = &[
    "apply",
    "as_ref",
    "build",
    "call",
    "cancel",
    "clear",
    "clone",
    "close",
    "contains",
    "default",
    "emit",
    "entry",
    "extend",
    "finish",
    "flush",
    "from",
    "get",
    "get_mut",
    "insert",
    "into",
    "iter",
    "join",
    "kill",
    "len",
    "listen",
    "load",
    "lock",
    "map",
    "new",
    "next",
    "notify",
    "open",
    "parse",
    "pop",
    "push",
    "read",
    "recv",
    "remove",
    "reserve",
    "run",
    "send",
    "set",
    "spawn",
    "start",
    "state",
    "stop",
    "store",
    "subscribe",
    "take",
    "try_recv",
    "update",
    "wait",
    "write",
];

// ---- cfg -----------------------------------------------------------------------

/// The crate's default features (`[features] default = [...]` in Cargo.toml).
fn default_features() -> HashSet<String> {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap_or_default();
    manifest
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("default").map(str::trim))
        .and_then(|rest| rest.strip_prefix('='))
        .map(|list| {
            list.trim()
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|f| f.trim().trim_matches('"').to_string())
                .filter(|f| !f.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A `cfg` predicate in a default-feature, non-test, non-doc build:
/// `Some(false)` is off, `None` is unknown (platform, `debug_assertions`) and
/// counts as on.
fn eval_cfg(meta: &syn::Meta, features: &HashSet<String>) -> Option<bool> {
    match meta {
        syn::Meta::Path(path) => {
            if path.is_ident("test") || path.is_ident("doc") || path.is_ident("doctest") {
                Some(false)
            } else {
                None
            }
        }
        syn::Meta::NameValue(nv) => {
            if !nv.path.is_ident("feature") {
                return None;
            }
            match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(features.contains(&s.value())),
                _ => None,
            }
        }
        syn::Meta::List(list) => {
            let nested = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .ok()?;
            let values: Vec<Option<bool>> = nested.iter().map(|m| eval_cfg(m, features)).collect();
            if list.path.is_ident("not") {
                values.first().copied().flatten().map(|v| !v)
            } else if list.path.is_ident("all") {
                if values.contains(&Some(false)) {
                    Some(false)
                } else if values.iter().all(|v| *v == Some(true)) {
                    Some(true)
                } else {
                    None
                }
            } else if list.path.is_ident("any") {
                if values.contains(&Some(true)) {
                    Some(true)
                } else if values.iter().all(|v| *v == Some(false)) {
                    Some(false)
                } else {
                    None
                }
            } else {
                None
            }
        }
    }
}

/// Whether an item with these attributes is compiled in a normal build.
fn compiled(attrs: &[syn::Attribute], features: &HashSet<String>) -> bool {
    !attrs.iter().any(|attr| {
        let path = attr.path();
        if path.segments.last().is_some_and(|s| s.ident == "test") {
            return true; // #[test], #[tokio::test]
        }
        path.is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| eval_cfg(&predicate, features) == Some(false))
    })
}

// ---- The graph -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    /// `a::b::c(…)`, or `a::b::c` passed as a value when `value`.
    Path { segments: Vec<String>, value: bool },
    /// `x.name(…)`; `on_self` when `x` is `self`. `args` is the argument
    /// count (`usize::MAX` when read from macro tokens), `sql` whether there
    /// is exactly one argument and it is SQL text (see [`is_sql_text`]).
    Method {
        name: String,
        on_self: bool,
        args: usize,
        sql: bool,
    },
    /// `(x.name)(…)`: a function or closure stored in a struct field.
    Field { name: String },
    /// A body extracted from this one that this one starts (a spawned task):
    /// an index into [`Body::children`] while parsing, a node id after.
    Child(usize),
}

#[derive(Debug)]
struct Func {
    file: String,
    module: Vec<String>,
    self_ty: Option<String>,
    /// The trait an `impl Trait for Type` method implements.
    trait_name: Option<String>,
    name: String,
    calls: Vec<Call>,
    /// The types (and traits) the function names.
    types: HashSet<String>,
}

impl Func {
    fn label(&self) -> String {
        let mut parts = self.module.clone();
        parts.extend(self.self_ty.clone());
        parts.push(self.name.clone());
        parts.join("::")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootKind {
    App,
    Watcher,
    Task,
    Event,
    Command,
    Route,
}

impl RootKind {
    fn describe(self) -> &'static str {
        match self {
            RootKind::App => "app seam",
            RootKind::Watcher => "notify-watcher handler",
            RootKind::Task => "spawned background task",
            RootKind::Event => "registered callback",
            RootKind::Command => "#[tauri::command]",
            RootKind::Route => "HTTP route",
        }
    }
    /// Whether [`EXPLICIT`] may name a root of this kind.
    fn reviewable(self) -> bool {
        matches!(
            self,
            RootKind::Task | RootKind::Event | RootKind::Command | RootKind::Route
        )
    }
}

#[derive(Default)]
struct Graph {
    funcs: Vec<Func>,
    by_name: HashMap<String, Vec<usize>>,
    /// Per file: `use` alias → the full path it names.
    uses: HashMap<String, HashMap<String, Vec<String>>>,
    /// Per module (`a::b`): public re-exports, alias → path (`*` for a glob).
    reexports: HashMap<String, Vec<(String, Vec<String>)>>,
    /// Every module segment in the crate (`commands`, `watch`, …).
    modules: HashSet<String>,
    /// Per struct: the types its fields name.
    fields: HashMap<String, HashSet<String>>,
    /// Per trait: its supertraits (`trait WakeEnv: WakeNotify`).
    supertraits: HashMap<String, Vec<String>>,
    /// Per field name: the closures and functions stored into it.
    field_fns: HashMap<String, Vec<usize>>,
    /// Discovered roots: background bodies, commands and routes.
    discovered: Vec<(usize, RootKind, String)>,
    /// The source files the module walk reached.
    files: BTreeSet<String>,
    features: HashSet<String>,
}

fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

fn type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        syn::Type::Reference(r) => type_name(&r.elem),
        _ => None,
    }
}

/// The last segment of a call's callee, if it is a path.
fn callee_name(func: &syn::Expr) -> Option<(String, Vec<String>)> {
    match func {
        syn::Expr::Path(p) => {
            let segments = path_segments(&p.path);
            segments.last().cloned().map(|last| (last, segments))
        }
        _ => None,
    }
}

/// A closure, an async block, or one wrapped in `Box::new` / `Arc::new` /
/// `Some` / parentheses.
fn is_closure_like(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Closure(_) | syn::Expr::Async(_) => true,
        syn::Expr::Paren(p) => is_closure_like(&p.expr),
        syn::Expr::Call(call) => {
            call.args.len() == 1
                && callee_name(&call.func)
                    .is_some_and(|(name, _)| matches!(name.as_str(), "new" | "Some" | "from"))
                && is_closure_like(&call.args[0])
        }
        _ => false,
    }
}

/// Whether `expr` is SQL text: a string literal, a `format!` / `concat!` /
/// `include_str!` / `indoc!`, a constant (`SELECT_RUNS`), or a binding whose
/// name says so (`sql`, `query`, `stmt_sql`); through `&`, parentheses and
/// `.as_str()` / `.as_ref()`. Used to tell rusqlite's `prepare(sql)` from a
/// crate method that happens to be called `prepare`.
fn is_sql_text(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(_),
            ..
        }) => true,
        syn::Expr::Macro(m) => m.mac.path.segments.last().is_some_and(|s| {
            matches!(
                s.ident.to_string().as_str(),
                "format" | "concat" | "include_str" | "indoc" | "formatdoc"
            )
        }),
        syn::Expr::Reference(r) => is_sql_text(&r.expr),
        syn::Expr::Paren(p) => is_sql_text(&p.expr),
        syn::Expr::MethodCall(m) if m.args.is_empty() => {
            matches!(m.method.to_string().as_str(), "as_str" | "as_ref") && is_sql_text(&m.receiver)
        }
        syn::Expr::Path(p) => p.path.segments.last().is_some_and(|s| {
            let name = s.ident.to_string();
            let lower = name.to_ascii_lowercase();
            (name.len() > 1
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()))
                || lower.contains("sql")
                || lower.contains("query")
        }),
        _ => false,
    }
}

/// A route literal or constant: `"/api/v1/x"` or `VIEWER_ROUTE`.
fn route_name(expr: &syn::Expr) -> String {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => s.value(),
        syn::Expr::Path(p) => path_segments(&p.path).join("::"),
        _ => "<computed>".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildKind {
    Task,
    Event,
    Field,
    Route,
}

/// A body extracted from a function: a spawned task, a registered callback,
/// a closure stored in a struct field, or a route handler.
#[derive(Debug, Clone)]
struct Child {
    kind: ChildKind,
    /// The callee (`spawn`, `listen`), the field name, or the route.
    tag: String,
    body: Body,
}

/// The calls, local bindings and named types in one function body.
#[derive(Debug, Clone, Default)]
struct Body {
    calls: Vec<Call>,
    locals: HashSet<String>,
    types: HashSet<String>,
    children: Vec<Child>,
    /// Locals bound to a closure or async block: `let tick = move || …;`.
    bound: HashMap<String, Body>,
    /// Bound closures called in place, and ones handed to a spawn or registration.
    called: HashSet<String>,
    registered: HashSet<String>,
    features: HashSet<String>,
}

impl Body {
    fn new(features: &HashSet<String>) -> Self {
        Body {
            features: features.clone(),
            ..Body::default()
        }
    }
    fn note_types(&mut self, path: &syn::Path) {
        for segment in &path.segments {
            let ident = segment.ident.to_string();
            if ident.starts_with(|c: char| c.is_ascii_uppercase()) {
                self.types.insert(ident);
            }
        }
    }
    /// Fold `other` into this body (its child indices shift).
    fn absorb(&mut self, other: Body) {
        let offset = self.children.len();
        self.calls
            .extend(other.calls.into_iter().map(|call| match call {
                Call::Child(i) => Call::Child(i + offset),
                other => other,
            }));
        self.children.extend(other.children);
        self.locals.extend(other.locals);
        self.types.extend(other.types);
        self.bound.extend(other.bound);
    }
    /// `expr` as its own body.
    fn sub(&mut self, expr: &syn::Expr) -> Body {
        if let syn::Expr::Path(p) = expr {
            if let Some(ident) = p.path.get_ident() {
                let name = ident.to_string();
                if let Some(bound) = self.bound.get(&name) {
                    let bound = bound.clone();
                    self.registered.insert(name);
                    return bound;
                }
            }
        }
        let mut body = Body::new(&self.features);
        body.visit_expr(expr);
        body.finish()
    }
    /// Credits each bound closure to this body unless its only use is a
    /// spawn or a registration (which made it a body of its own).
    fn finish(mut self) -> Body {
        let bound = std::mem::take(&mut self.bound);
        let mut names: Vec<&String> = bound.keys().collect();
        names.sort();
        for name in names {
            if self.called.contains(name) || !self.registered.contains(name) {
                self.absorb(bound[name].clone());
            }
        }
        self
    }
    fn child(&mut self, kind: ChildKind, tag: &str, expr: &syn::Expr) {
        let body = self.sub(expr);
        if kind == ChildKind::Task {
            self.calls.push(Call::Child(self.children.len()));
        }
        self.children.push(Child {
            kind,
            tag: tag.to_string(),
            body,
        });
    }
    /// The arguments of a spawn or a registration, as children. Literals carry
    /// no calls and are read in place.
    fn extract_args<'a>(&mut self, name: &str, args: impl Iterator<Item = &'a syn::Expr>) -> bool {
        let kind = if TASK_CALLS.contains(&name) {
            ChildKind::Task
        } else if EVENT_CALLS.contains(&name) {
            ChildKind::Event
        } else {
            return false;
        };
        for arg in args {
            if matches!(arg, syn::Expr::Lit(_)) {
                continue;
            }
            self.child(kind, name, arg);
        }
        true
    }
}

impl<'ast> Visit<'ast> for Body {
    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        let attrs: &[syn::Attribute] = match stmt {
            syn::Stmt::Local(local) => &local.attrs,
            syn::Stmt::Macro(mac) => &mac.attrs,
            syn::Stmt::Expr(expr, _) => match expr {
                syn::Expr::Call(e) => &e.attrs,
                syn::Expr::MethodCall(e) => &e.attrs,
                syn::Expr::Block(e) => &e.attrs,
                syn::Expr::If(e) => &e.attrs,
                syn::Expr::Macro(e) => &e.attrs,
                syn::Expr::Assign(e) => &e.attrs,
                _ => &[],
            },
            syn::Stmt::Item(_) => &[],
        };
        if compiled(attrs, &self.features) {
            visit::visit_stmt(self, stmt);
        }
    }
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let (syn::Pat::Ident(pat), Some(init)) = (&local.pat, &local.init) {
            if is_closure_like(&init.expr) {
                // Kept for a later spawn/registration of the binding; credited
                // here at the end unless that is its only use (`finish`).
                let body = self.sub(&init.expr);
                self.bound.insert(pat.ident.to_string(), body);
                self.locals.insert(pat.ident.to_string());
                return;
            }
        }
        visit::visit_local(self, local);
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.note_types(path);
        visit::visit_path(self, path);
    }
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        match &*call.func {
            syn::Expr::Path(path) => {
                let segments = path_segments(&path.path);
                self.visit_path(&path.path);
                let name = segments.last().cloned().unwrap_or_default();
                if segments.len() == 1 && self.bound.contains_key(&name) {
                    self.called.insert(name.clone());
                }
                // `notify::RecommendedWatcher::new(handler, config)`.
                let watcher_new = name == "new"
                    && segments.len() >= 2
                    && (segments[segments.len() - 2].ends_with("Watcher")
                        || segments[segments.len() - 2].ends_with("Debouncer"));
                self.calls.push(Call::Path {
                    segments,
                    value: false,
                });
                if watcher_new {
                    for arg in &call.args {
                        if is_closure_like(arg) {
                            self.child(ChildKind::Event, "watcher", arg);
                        } else {
                            self.visit_expr(arg);
                        }
                    }
                    return;
                }
                if self.extract_args(&name, call.args.iter()) {
                    return;
                }
            }
            syn::Expr::Paren(syn::ExprParen { expr, .. })
            | syn::Expr::Group(syn::ExprGroup { expr, .. })
                if matches!(&**expr, syn::Expr::Field(_)) =>
            {
                if let syn::Expr::Field(field) = &**expr {
                    if let syn::Member::Named(ident) = &field.member {
                        self.calls.push(Call::Field {
                            name: ident.to_string(),
                        });
                    }
                    self.visit_expr(&field.base);
                }
            }
            syn::Expr::Field(field) => {
                if let syn::Member::Named(ident) = &field.member {
                    self.calls.push(Call::Field {
                        name: ident.to_string(),
                    });
                }
                self.visit_expr(&field.base);
            }
            other => self.visit_expr(other),
        }
        for arg in &call.args {
            self.visit_expr(arg);
        }
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let on_self = matches!(&*call.receiver, syn::Expr::Path(p) if p.path.is_ident("self"));
        let name = call.method.to_string();
        self.calls.push(Call::Method {
            name: name.clone(),
            on_self,
            args: call.args.len(),
            sql: call.args.len() == 1 && is_sql_text(&call.args[0]),
        });
        self.visit_expr(&call.receiver);
        if name == "route" && call.args.len() == 2 {
            let route = route_name(&call.args[0]);
            self.child(ChildKind::Route, &route, &call.args[1]);
            return;
        }
        // The app's `run(|app, event| …)`: a closure there is the run-event
        // callback. Elsewhere `run` takes no callback worth extracting.
        if name == "run"
            && call
                .args
                .iter()
                .any(|a| matches!(a, syn::Expr::Closure(c) if c.inputs.len() == 2))
        {
            for arg in &call.args {
                self.child(ChildKind::Event, "run", arg);
            }
            return;
        }
        if self.extract_args(&name, call.args.iter()) {
            return;
        }
        for arg in &call.args {
            self.visit_expr(arg);
        }
    }
    fn visit_expr_struct(&mut self, item: &'ast syn::ExprStruct) {
        self.visit_path(&item.path);
        for field in &item.fields {
            match &field.member {
                syn::Member::Named(ident) if is_closure_like(&field.expr) => {
                    self.child(ChildKind::Field, &ident.to_string(), &field.expr);
                }
                _ => self.visit_expr(&field.expr),
            }
        }
        if let Some(rest) = &item.rest {
            self.visit_expr(rest);
        }
    }
    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        self.visit_path(&path.path);
        let segments = path_segments(&path.path);
        if segments
            .last()
            .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_lowercase()))
        {
            self.calls.push(Call::Path {
                segments,
                value: true,
            });
        }
    }
    fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
        self.locals.insert(pat.ident.to_string());
        visit::visit_pat_ident(self, pat);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        scan_tokens(mac.tokens.clone(), &mut self.calls, &mut self.types);
    }
}

/// The types a field list names.
#[derive(Default)]
struct TypeNames(HashSet<String>);

impl<'ast> Visit<'ast> for TypeNames {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        for segment in &path.segments {
            let ident = segment.ident.to_string();
            if ident.starts_with(|c: char| c.is_ascii_uppercase()) {
                self.0.insert(ident);
            }
        }
        visit::visit_path(self, path);
    }
}

/// Calls inside a macro invocation, read from its tokens: `a::b(` is a path
/// call, `.m(` a method call.
fn scan_tokens(tokens: TokenStream, out: &mut Vec<Call>, types: &mut HashSet<String>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let punct =
        |i: usize, c: char| matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == c);
    let paren = |i: usize| matches!(toks.get(i), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Group(group) => {
                scan_tokens(group.stream(), out, types);
                i += 1;
            }
            TokenTree::Punct(p) if p.as_char() == '.' => {
                if let Some(TokenTree::Ident(method)) = toks.get(i + 1) {
                    if paren(i + 2) {
                        out.push(Call::Method {
                            name: method.to_string(),
                            on_self: matches!(i.checked_sub(1).map(|k| &toks[k]), Some(TokenTree::Ident(r)) if r == "self"),
                            args: usize::MAX,
                            sql: false,
                        });
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            TokenTree::Ident(first) => {
                if first
                    .to_string()
                    .starts_with(|c: char| c.is_ascii_uppercase())
                {
                    types.insert(first.to_string());
                }
                let mut segments = vec![first.to_string()];
                let mut j = i + 1;
                while punct(j, ':') && punct(j + 1, ':') {
                    match toks.get(j + 2) {
                        Some(TokenTree::Ident(next)) => {
                            if next
                                .to_string()
                                .starts_with(|c: char| c.is_ascii_uppercase())
                            {
                                types.insert(next.to_string());
                            }
                            segments.push(next.to_string());
                            j += 3;
                        }
                        _ => break,
                    }
                }
                let after_fn = i > 0 && matches!(&toks[i - 1], TokenTree::Ident(k) if k == "fn");
                if paren(j) && !after_fn {
                    out.push(Call::Path {
                        segments,
                        value: false,
                    });
                }
                i = j;
            }
            _ => i += 1,
        }
    }
}

/// Visits one file's items. `dir` is where this module's `mod x;` files live
/// (`None` for a planted source, whose `mod x;` declarations are ignored).
struct Items<'g> {
    graph: &'g mut Graph,
    file: String,
    module: Vec<String>,
    dir: Option<PathBuf>,
    /// The directory of the current source file (`#[path]` is relative to it).
    file_dir: Option<PathBuf>,
    src_root: Option<PathBuf>,
}

fn is_command(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let segments = path_segments(attr.path());
        segments.last().is_some_and(|s| s == "command")
            && (segments.len() == 1 || segments[0] == "tauri")
    })
}

impl Items<'_> {
    fn add(
        &mut self,
        self_ty: Option<String>,
        trait_name: Option<String>,
        name: String,
        attrs: &[syn::Attribute],
        sig: &syn::Signature,
        block: &syn::Block,
    ) {
        let mut body = Body::new(&self.graph.features);
        body.visit_signature(sig);
        body.visit_block(block);
        let body = body.finish();
        let id = self.add_body(
            self_ty,
            trait_name,
            name.clone(),
            body,
            &HashSet::new(),
            &HashSet::new(),
        );
        self.graph.by_name.entry(name.clone()).or_default().push(id);
        if is_command(attrs) {
            self.graph
                .discovered
                .push((id, RootKind::Command, self.graph.funcs[id].label()));
        }
    }

    /// One node for `body`, and one per child (recursively), named after it.
    fn add_body(
        &mut self,
        self_ty: Option<String>,
        trait_name: Option<String>,
        name: String,
        body: Body,
        outer_locals: &HashSet<String>,
        outer_types: &HashSet<String>,
    ) -> usize {
        let mut locals = outer_locals.clone();
        locals.extend(body.locals.iter().cloned());
        let mut types = outer_types.clone();
        types.extend(body.types.iter().cloned());
        let mut child_ids = Vec::new();
        for child in body.children {
            let tag = match child.kind {
                ChildKind::Task | ChildKind::Event => format!("{{{}}}", child.tag),
                ChildKind::Field => format!("{{field:{}}}", child.tag),
                ChildKind::Route => format!("{{route:{}}}", child.tag),
            };
            let id = self.add_body(
                self_ty.clone(),
                trait_name.clone(),
                format!("{name}::{tag}"),
                child.body,
                &locals,
                &types,
            );
            let label = self.graph.funcs[id].label();
            match child.kind {
                ChildKind::Task => self.graph.discovered.push((id, RootKind::Task, label)),
                ChildKind::Event => self.graph.discovered.push((id, RootKind::Event, label)),
                ChildKind::Route => self.graph.discovered.push((
                    id,
                    RootKind::Route,
                    format!("route:{}", child.tag),
                )),
                ChildKind::Field => self
                    .graph
                    .field_fns
                    .entry(child.tag.clone())
                    .or_default()
                    .push(id),
            }
            child_ids.push(id);
        }
        let calls = body
            .calls
            .into_iter()
            .filter_map(|call| match call {
                Call::Child(i) => Some(Call::Child(child_ids[i])),
                Call::Path { ref segments, .. }
                    if segments.len() == 1 && locals.contains(&segments[0]) =>
                {
                    None
                }
                other => Some(other),
            })
            .collect();
        let id = self.graph.funcs.len();
        self.graph.funcs.push(Func {
            file: self.file.clone(),
            module: self.module.clone(),
            self_ty,
            trait_name,
            name,
            calls,
            types,
        });
        id
    }

    fn compiled(&self, attrs: &[syn::Attribute]) -> bool {
        compiled(attrs, &self.graph.features)
    }

    /// `mod name;` → its file, following the compiler's rules.
    fn module_file(&self, item: &syn::ItemMod) -> Option<PathBuf> {
        let explicit = item.attrs.iter().find_map(|attr| match &attr.meta {
            syn::Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        });
        if let Some(path) = explicit {
            return Some(self.file_dir.as_ref()?.join(path));
        }
        let dir = self.dir.as_ref()?;
        let name = item.ident.to_string();
        [
            dir.join(format!("{name}.rs")),
            dir.join(&name).join("mod.rs"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    }
}

impl<'ast> Visit<'ast> for Items<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !self.compiled(&item.attrs) {
            return;
        }
        let ident = item.ident.to_string();
        self.graph.modules.insert(ident.clone());
        if let Some((_, items)) = &item.content {
            self.module.push(ident.clone());
            let saved = self.dir.clone();
            self.dir = self.dir.as_ref().map(|d| d.join(&ident));
            for item in items {
                self.visit_item(item);
            }
            self.dir = saved;
            self.module.pop();
            return;
        }
        let Some(path) = self.module_file(item) else {
            if self.src_root.is_some() {
                panic!(
                    "mod {ident} in {} names no file: the guard's module walk is out of date",
                    self.file
                );
            }
            return;
        };
        let src_root = self.src_root.clone().unwrap();
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let parsed =
            syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        if !self.compiled(&parsed.attrs) {
            return; // `#![cfg(test)]` at the top of the file
        }
        let rel = path
            .strip_prefix(&src_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        self.graph.files.insert(rel.clone());
        let file_dir = path.parent().unwrap().to_path_buf();
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let dir = if stem == "mod" {
            file_dir.clone()
        } else {
            file_dir.join(&stem)
        };
        let mut module = self.module.clone();
        module.push(ident);
        let mut nested = Items {
            graph: self.graph,
            file: rel,
            module,
            dir: Some(dir),
            file_dir: Some(file_dir),
            src_root: Some(src_root),
        };
        for item in &parsed.items {
            nested.visit_item(item);
        }
    }
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if self.compiled(&item.attrs) {
            self.add(
                None,
                None,
                item.sig.ident.to_string(),
                &item.attrs,
                &item.sig,
                &item.block,
            );
        }
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if !self.compiled(&item.attrs) {
            return;
        }
        let self_ty = type_name(&item.self_ty);
        let trait_name = item
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .map(|s| s.ident.to_string());
        for impl_item in &item.items {
            if let syn::ImplItem::Fn(method) = impl_item {
                if self.compiled(&method.attrs) {
                    self.add(
                        self_ty.clone(),
                        trait_name.clone(),
                        method.sig.ident.to_string(),
                        &method.attrs,
                        &method.sig,
                        &method.block,
                    );
                }
            }
        }
    }
    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if !self.compiled(&item.attrs) {
            return;
        }
        let supers: Vec<String> = item
            .supertraits
            .iter()
            .filter_map(|bound| match bound {
                syn::TypeParamBound::Trait(t) => {
                    t.path.segments.last().map(|s| s.ident.to_string())
                }
                _ => None,
            })
            .collect();
        self.graph
            .supertraits
            .entry(item.ident.to_string())
            .or_default()
            .extend(supers);
        for trait_item in &item.items {
            if let syn::TraitItem::Fn(method) = trait_item {
                if let (Some(block), true) = (&method.default, self.compiled(&method.attrs)) {
                    self.add(
                        Some(item.ident.to_string()),
                        Some(item.ident.to_string()),
                        method.sig.ident.to_string(),
                        &method.attrs,
                        &method.sig,
                        block,
                    );
                }
            }
        }
    }
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if !self.compiled(&item.attrs) {
            return;
        }
        let mut names = TypeNames::default();
        names.visit_fields(&item.fields);
        self.graph
            .fields
            .entry(item.ident.to_string())
            .or_default()
            .extend(names.0);
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !self.compiled(&item.attrs) {
            return;
        }
        let mut out = Vec::new();
        flatten_use(&item.tree, Vec::new(), &mut out);
        if !matches!(item.vis, syn::Visibility::Inherited) {
            let reexports = self
                .graph
                .reexports
                .entry(self.module.join("::"))
                .or_default();
            reexports.extend(out.iter().cloned());
        }
        let uses = self.graph.uses.entry(self.file.clone()).or_default();
        for (alias, path) in out {
            if alias != "*" {
                uses.insert(alias, path);
            }
        }
    }
}

fn flatten_use(tree: &syn::UseTree, prefix: Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
    match tree {
        syn::UseTree::Path(p) => {
            let mut next = prefix;
            next.push(p.ident.to_string());
            flatten_use(&p.tree, next, out);
        }
        syn::UseTree::Name(n) => {
            let mut path = prefix;
            let name = n.ident.to_string();
            if name == "self" {
                if let Some(last) = path.last().cloned() {
                    out.push((last, path));
                }
            } else {
                path.push(name.clone());
                out.push((name, path));
            }
        }
        syn::UseTree::Rename(r) => {
            let mut path = prefix;
            if r.ident != "self" {
                path.push(r.ident.to_string());
            }
            out.push((r.rename.to_string(), path));
        }
        syn::UseTree::Group(g) => {
            for tree in &g.items {
                flatten_use(tree, prefix.clone(), out);
            }
        }
        syn::UseTree::Glob(_) => out.push(("*".to_string(), prefix)),
    }
}

fn strip_relative(path: &[String]) -> Vec<String> {
    path.iter()
        .skip_while(|s| matches!(s.as_str(), "crate" | "self" | "super"))
        .cloned()
        .collect()
}

/// A `use` path or call path made module-absolute from the module `module` it was
/// written in: `super::publish` in `live_hub::machine_stats` is `live_hub::publish`,
/// not every `publish` in the crate (stripping the prefix alone loses the module).
/// A path that climbs past the module root keeps the plain stripped form.
fn absolute_in(path: &[String], module: &[String]) -> Vec<String> {
    let rest = strip_relative(path);
    let mut base: Vec<String> = match path.first().map(String::as_str) {
        Some("self") => module.to_vec(),
        Some("super") => {
            let ups = path.iter().take_while(|s| s.as_str() == "super").count();
            match module.len().checked_sub(ups) {
                Some(keep) => module[..keep].to_vec(),
                None => return rest,
            }
        }
        _ => Vec::new(),
    };
    base.extend(rest);
    base
}

/// What the main test reports about resolution.
#[derive(Debug, Default)]
struct Resolution {
    /// Successors per node.
    adj: Vec<Vec<usize>>,
    /// In-crate path calls (`crate::…`, a crate module or type) that name a
    /// crate function but resolve to nothing.
    unresolved_paths: Vec<String>,
    /// Method calls several crate types define that resolve to none of them
    /// (the caller names none of those types).
    unresolved_methods: usize,
}

impl Graph {
    fn new(features: HashSet<String>) -> Self {
        Graph {
            features,
            ..Graph::default()
        }
    }

    /// A planted source (self-tests): `mod x;` declarations are ignored.
    fn add_source(&mut self, file: &str, module: Vec<String>, src: &str) {
        let parsed = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {file}: {e}"));
        for segment in &module {
            self.modules.insert(segment.clone());
        }
        let mut items = Items {
            graph: self,
            file: file.to_string(),
            module,
            dir: None,
            file_dir: None,
            src_root: None,
        };
        for item in &parsed.items {
            items.visit_item(item);
        }
    }

    /// The crate as compiled: `src/lib.rs` and the `mod` tree under it.
    fn from_tree(src_root: &Path) -> Self {
        let mut graph = Graph::new(default_features());
        let lib = src_root.join("lib.rs");
        let src = std::fs::read_to_string(&lib).unwrap();
        let parsed = syn::parse_file(&src).unwrap_or_else(|e| panic!("parse lib.rs: {e}"));
        graph.files.insert("lib.rs".into());
        let mut items = Items {
            graph: &mut graph,
            file: "lib.rs".into(),
            module: Vec::new(),
            dir: Some(src_root.to_path_buf()),
            file_dir: Some(src_root.to_path_buf()),
            src_root: Some(src_root.to_path_buf()),
        };
        for item in &parsed.items {
            items.visit_item(item);
        }
        graph
    }

    /// The functions named `name` that `quals` qualifies, following the
    /// re-exports of the module `quals` ends in.
    fn qualified(&self, name: &str, quals: &[String], depth: usize) -> Vec<usize> {
        let direct: Vec<usize> = self
            .by_name
            .get(name)
            .map(|ids| {
                ids.iter()
                    .copied()
                    .filter(|&id| self.qualifies(&self.funcs[id], quals))
                    .collect()
            })
            .unwrap_or_default();
        if !direct.is_empty() || depth > 4 {
            return direct;
        }
        let Some(last) = quals.last() else {
            return direct;
        };
        let mut out = Vec::new();
        for (module, reexports) in &self.reexports {
            if module.rsplit("::").next() != Some(last.as_str()) {
                continue;
            }
            for (alias, path) in reexports {
                let path = strip_relative(path);
                if alias == name {
                    if let Some((target, target_quals)) = path.split_last() {
                        out.extend(self.qualified(target, target_quals, depth + 1));
                    }
                } else if alias == "*" {
                    out.extend(self.qualified(name, &path, depth + 1));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The nodes `call` (made in `from`) may reach.
    fn resolve(&self, from: &Func, call: &Call) -> Vec<usize> {
        match call {
            Call::Child(id) => vec![*id],
            Call::Field { name } => self.field_fns.get(name).cloned().unwrap_or_default(),
            Call::Method { name, on_self, .. } => {
                let methods: Vec<usize> = self
                    .by_name
                    .get(name)
                    .map(|ids| {
                        ids.iter()
                            .copied()
                            .filter(|&id| self.funcs[id].self_ty.is_some())
                            .collect()
                    })
                    .unwrap_or_default();
                if *on_self && from.self_ty.is_some() {
                    let own: Vec<usize> = methods
                        .iter()
                        .copied()
                        .filter(|&id| {
                            self.funcs[id].self_ty == from.self_ty
                                || (from.trait_name.is_some()
                                    && self.funcs[id].self_ty == from.trait_name)
                        })
                        .collect();
                    if !own.is_empty() {
                        return own;
                    }
                }
                let owners: HashSet<&String> = methods
                    .iter()
                    .filter_map(|&id| self.funcs[id].self_ty.as_ref())
                    .collect();
                if owners.len() == 1 && !COMMON_METHODS.contains(&name.as_str()) {
                    return methods;
                }
                let mut named: HashSet<&String> = from.types.iter().collect();
                if let Some(self_ty) = &from.self_ty {
                    named.insert(self_ty);
                    if let Some(fields) = self.fields.get(self_ty) {
                        named.extend(fields.iter());
                    }
                }
                // A `dyn Sub` / `T: Sub` value also has every supertrait's
                // methods (`WakeEnv: WakeNotify` -> `env.redispatch(..)`).
                let mut pending: Vec<&String> = named.iter().copied().collect();
                while let Some(name) = pending.pop() {
                    for sup in self.supertraits.get(name).into_iter().flatten() {
                        if named.insert(sup) {
                            pending.push(sup);
                        }
                    }
                }
                methods
                    .into_iter()
                    .filter(|&id| {
                        let func = &self.funcs[id];
                        func.self_ty.as_ref().is_some_and(|t| named.contains(t))
                            || func.trait_name.as_ref().is_some_and(|t| named.contains(t))
                    })
                    .collect()
            }
            Call::Path { segments, value } => {
                let relative = matches!(
                    segments.first().map(String::as_str),
                    Some("crate" | "self" | "super")
                );
                let mut segments = absolute_in(segments, &from.module);
                let Some(name) = segments.pop() else {
                    return Vec::new();
                };
                let Some(ids) = self.by_name.get(&name) else {
                    return Vec::new();
                };
                let uses = self.uses.get(&from.file);
                if segments.is_empty() {
                    let free: Vec<usize> = ids
                        .iter()
                        .copied()
                        .filter(|&id| self.funcs[id].self_ty.is_none())
                        .collect();
                    if relative {
                        // `crate::f()`, or `super::f()` from a top-level module: `f` is
                        // in the crate root.
                        return free
                            .into_iter()
                            .filter(|&id| self.funcs[id].module.is_empty())
                            .collect();
                    }
                    let local: Vec<usize> = free
                        .iter()
                        .copied()
                        .filter(|&id| self.funcs[id].module == from.module)
                        .collect();
                    if !local.is_empty() {
                        return local;
                    }
                    if let Some(path) = uses.and_then(|u| u.get(&name)) {
                        let path = absolute_in(path, &from.module);
                        let Some((target, quals)) = path.split_last() else {
                            return Vec::new();
                        };
                        return self
                            .qualified(target, quals, 0)
                            .into_iter()
                            .filter(|&id| self.funcs[id].self_ty.is_none())
                            .collect();
                    }
                    return if *value { Vec::new() } else { free };
                }
                if segments[0] == "Self" {
                    return ids
                        .iter()
                        .copied()
                        .filter(|&id| {
                            self.funcs[id].self_ty.is_some()
                                && self.funcs[id].self_ty == from.self_ty
                        })
                        .collect();
                }
                if let Some(path) = uses.and_then(|u| u.get(&segments[0])) {
                    let mut expanded = strip_relative(path);
                    expanded.extend(segments.drain(1..));
                    segments = expanded;
                }
                let Some(first) = segments.first() else {
                    return Vec::new();
                };
                let is_type = first.starts_with(|c: char| c.is_ascii_uppercase());
                if !is_type && !self.modules.contains(first) {
                    return Vec::new(); // another crate (`std::fs::write`, `tokio::spawn`)
                }
                self.qualified(&name, &segments, 0)
            }
        }
    }

    /// Whether `func` is named by the qualifier `quals` (its last segment is
    /// the function's type, the trait it implements — `Trait::method(x)` —
    /// or its module).
    fn qualifies(&self, func: &Func, quals: &[String]) -> bool {
        let Some(last) = quals.last() else {
            return true;
        };
        func.self_ty.as_ref() == Some(last)
            || func.trait_name.as_ref() == Some(last)
            || func.module.last() == Some(last)
    }

    /// Every node's successors, and what did not resolve.
    fn resolution(&self) -> Resolution {
        let mut out = Resolution::default();
        let crate_types: HashSet<&String> = self
            .funcs
            .iter()
            .filter_map(|f| f.self_ty.as_ref())
            .collect();
        for func in &self.funcs {
            let mut next = Vec::new();
            for call in &func.calls {
                let targets = self.resolve(func, call);
                if targets.is_empty() {
                    match call {
                        Call::Path {
                            segments,
                            value: false,
                        } if segments.len() > 1 => {
                            let first = &segments[0];
                            let in_crate = matches!(first.as_str(), "crate" | "self" | "super")
                                || self.modules.contains(first)
                                || crate_types.contains(first);
                            let defined = self.by_name.contains_key(segments.last().unwrap());
                            if in_crate && defined && !self.is_external_alias(func, first) {
                                out.unresolved_paths.push(format!(
                                    "{} calls {}",
                                    func.label(),
                                    segments.join("::")
                                ));
                            }
                        }
                        Call::Method { name, .. } if !COMMON_METHODS.contains(&name.as_str()) => {
                            let owners: HashSet<&String> = self
                                .by_name
                                .get(name)
                                .into_iter()
                                .flatten()
                                .filter_map(|&id| self.funcs[id].self_ty.as_ref())
                                .collect();
                            if owners.len() > 1 {
                                out.unresolved_methods += 1;
                                if std::env::var_os("X1_PRINT_UNRESOLVED_METHODS").is_some() {
                                    println!("X1-method|{}|{}", func.label(), name);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                next.extend(targets);
            }
            next.sort_unstable();
            next.dedup();
            out.adj.push(next);
        }
        out.unresolved_paths.sort();
        out.unresolved_paths.dedup();
        out
    }

    /// `first` is a `use` alias for another crate's item in `func`'s file.
    fn is_external_alias(&self, func: &Func, first: &str) -> bool {
        self.uses
            .get(&func.file)
            .and_then(|u| u.get(first))
            .is_some_and(|path| {
                path.first().is_some_and(|head| {
                    !matches!(head.as_str(), "crate" | "self" | "super")
                        && !self.modules.contains(head)
                })
            })
    }

    fn matching(&self, suffix: &str) -> Vec<usize> {
        if let Some(exact) = suffix.strip_prefix("crate::") {
            return (0..self.funcs.len())
                .filter(|&id| self.funcs[id].label() == exact)
                .collect();
        }
        let (suffix, prefix_match) = match suffix.strip_suffix('*') {
            Some(stem) => (stem, true),
            None => (suffix, false),
        };
        (0..self.funcs.len())
            .filter(|&id| {
                let label = self.funcs[id].label();
                if prefix_match {
                    let (head, name) = suffix.rsplit_once("::").unwrap_or(("", suffix));
                    self.funcs[id].name.starts_with(name)
                        && (head.is_empty() || label.contains(&format!("{head}::")))
                } else {
                    label == suffix || label.ends_with(&format!("::{suffix}"))
                }
            })
            .collect()
    }

    /// The notify-watcher handlers, found rather than listed: every function
    /// that registers a dispatch handler or builds one (names
    /// `DispatchHandler`).
    fn watcher_roots(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = (0..self.funcs.len())
            .filter(|&id| {
                let func = &self.funcs[id];
                func.types.contains("DispatchHandler")
                    || func.calls.iter().any(|call| match call {
                        Call::Method { name, .. } => name == "register_handler",
                        Call::Path { segments, .. } => {
                            segments.last().is_some_and(|s| s == "register_handler")
                        }
                        _ => false,
                    })
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Every root the policy checks.
    fn roots(&self, policy: &Policy) -> Vec<Root> {
        let mut roots = Vec::new();
        for (name, why, min_reach) in policy.app_roots {
            let ids = self.matching(name);
            assert!(
                !ids.is_empty(),
                "app root {name} ({why}) names no function: update APP_ROOTS"
            );
            for id in ids {
                roots.push(Root {
                    id,
                    kind: RootKind::App,
                    name: format!("{name} ({why})"),
                    explicit: None,
                    min_reach: *min_reach,
                });
            }
        }
        for id in self.watcher_roots() {
            roots.push(Root {
                id,
                kind: RootKind::Watcher,
                name: self.funcs[id].label(),
                explicit: None,
                min_reach: 0,
            });
        }
        for (id, kind, name) in &self.discovered {
            let explicit = policy.explicit.iter().position(|(entry, _, _)| {
                match entry.strip_prefix("route:") {
                    Some(route) => *kind == RootKind::Route && name == &format!("route:{route}"),
                    None => {
                        kind.reviewable()
                            && *kind != RootKind::Route
                            && self.label_matches(*id, entry)
                    }
                }
            });
            roots.push(Root {
                id: *id,
                kind: *kind,
                name: name.clone(),
                explicit,
                min_reach: 0,
            });
        }
        roots
    }

    fn label_matches(&self, id: usize, suffix: &str) -> bool {
        let label = self.funcs[id].label();
        label == suffix || label.ends_with(&format!("::{suffix}"))
    }

    /// Walks every root; see [`Report`].
    fn check(&self, policy: &Policy) -> Report {
        let res = self.resolution();
        let sink_ids: HashSet<usize> = policy.sinks.iter().flat_map(|s| self.matching(s)).collect();
        // Nodes that can reach a sink at all (ignoring ALLOWED): the walk
        // skips the rest.
        let mut rev: Vec<Vec<usize>> = vec![Vec::new(); self.funcs.len()];
        for (from, next) in res.adj.iter().enumerate() {
            for &to in next {
                rev[to].push(from);
            }
        }
        let mut live = vec![false; self.funcs.len()];
        let mut queue: VecDeque<usize> = sink_ids.iter().copied().collect();
        for &id in &sink_ids {
            live[id] = true;
        }
        while let Some(at) = queue.pop_front() {
            for &prev in &rev[at] {
                if !live[prev] {
                    live[prev] = true;
                    queue.push_back(prev);
                }
            }
        }
        let allowed: Vec<(HashSet<usize>, HashSet<usize>)> = policy
            .allowed
            .iter()
            .map(|(via, target, _)| {
                (
                    self.matching(via).into_iter().collect(),
                    self.matching(target).into_iter().collect(),
                )
            })
            .collect();
        assert!(allowed.len() <= 64, "ALLOWED holds at most 64 entries");
        let via_mask = |id: usize| -> u64 {
            allowed
                .iter()
                .enumerate()
                .filter(|(_, (via, _))| via.contains(&id))
                .fold(0, |m, (i, _)| m | (1 << i))
        };

        let roots = self.roots(policy);
        let mut report = Report {
            roots_by_kind: BTreeMap::new(),
            unresolved_paths: res.unresolved_paths.clone(),
            unresolved_methods: res.unresolved_methods,
            nodes: self.funcs.len(),
            ..Report::default()
        };
        report.explicit_sinks = vec![0; policy.explicit.len()];
        report.explicit_reached = vec![BTreeSet::new(); policy.explicit.len()];
        report.explicit_roots = vec![0; policy.explicit.len()];
        // A root id checked under any unreviewed kind is checked.
        let checked: HashSet<usize> = roots
            .iter()
            .filter(|r| r.explicit.is_none())
            .map(|r| r.id)
            .collect();
        for root in &roots {
            *report
                .roots_by_kind
                .entry(root.kind.describe())
                .or_default() += 1;
            if let Some(index) = root.explicit {
                report.explicit_roots[index] += 1;
            }
            let full_walk = root.min_reach > 0;
            if !full_walk && !live[root.id] {
                continue;
            }
            // BFS over (node, the ALLOWED vias passed).
            let start = (root.id, via_mask(root.id));
            let mut parent: HashMap<(usize, u64), (usize, u64)> = HashMap::new();
            let mut seen: HashSet<(usize, u64)> = HashSet::from([start]);
            let mut reached: HashSet<usize> = HashSet::from([root.id]);
            let mut queue: VecDeque<(usize, u64)> = VecDeque::from([start]);
            let mut sinks_hit = 0;
            let mut sinks_seen: BTreeSet<String> = BTreeSet::new();
            while let Some(state) = queue.pop_front() {
                let (at, mask) = state;
                if sink_ids.contains(&at) {
                    sinks_hit += 1;
                    sinks_seen.insert(self.funcs[at].label());
                    if root.explicit.is_none() || checked.contains(&root.id) {
                        let mut path = vec![self.funcs[at].label()];
                        let mut cur = state;
                        while let Some(&prev) = parent.get(&cur) {
                            path.push(self.funcs[prev.0].label());
                            cur = prev;
                        }
                        path.reverse();
                        report.violations.push(format!(
                            "{} {} reaches a write primitive:\n    {}\n    (at {})",
                            root.kind.describe(),
                            root.name,
                            path.join("\n    -> "),
                            self.funcs[at].file
                        ));
                    }
                    continue;
                }
                for &next in &res.adj[at] {
                    if !full_walk && !live[next] {
                        continue;
                    }
                    if let Some(index) = allowed
                        .iter()
                        .enumerate()
                        .position(|(i, (_, target))| mask & (1 << i) != 0 && target.contains(&next))
                    {
                        report.used_allowed.insert(index);
                        continue;
                    }
                    let state = (next, mask | via_mask(next));
                    if seen.insert(state) {
                        parent.insert(state, (at, mask));
                        reached.insert(next);
                        queue.push_back(state);
                    }
                }
            }
            if let Some(index) = root.explicit {
                report.explicit_sinks[index] += sinks_hit;
                report.explicit_reached[index].extend(sinks_seen);
            }
            if full_walk {
                report
                    .reach
                    .push((root.name.clone(), reached.len(), root.min_reach));
            }
        }
        report.violations.sort();
        report.violations.dedup();
        report
    }
}

struct Policy<'a> {
    app_roots: &'a [(Name, &'a str, usize)],
    sinks: &'a [Name],
    allowed: &'a [(Name, Name, &'a str)],
    explicit: &'a [(Name, &'a [Name], &'a str)],
}

const POLICY: Policy<'static> = Policy {
    app_roots: APP_ROOTS,
    sinks: SINKS,
    allowed: ALLOWED,
    explicit: EXPLICIT,
};

struct Root {
    id: usize,
    kind: RootKind,
    name: String,
    /// The [`EXPLICIT`] entry that reviews it.
    explicit: Option<usize>,
    /// For an app root: the least number of functions its walk must reach.
    min_reach: usize,
}

#[derive(Debug, Default)]
struct Report {
    violations: Vec<String>,
    used_allowed: BTreeSet<usize>,
    /// Per [`EXPLICIT`] entry: the roots it names and the sinks they reach.
    explicit_roots: Vec<usize>,
    explicit_sinks: Vec<usize>,
    /// Per [`EXPLICIT`] entry: the sinks (labels) its roots reach.
    explicit_reached: Vec<BTreeSet<String>>,
    /// Per app root: (name, functions reached, floor).
    reach: Vec<(String, usize, usize)>,
    roots_by_kind: BTreeMap<&'static str, usize>,
    unresolved_paths: Vec<String>,
    unresolved_methods: usize,
    nodes: usize,
}

// ---- The tests -----------------------------------------------------------------

/// Reviewed unresolved method calls that name a method some sink-reaching
/// type defines: `(method, scope, reason)`. Each is a call the resolver
/// cannot pin to a type and that, read by hand, does not reach the
/// sink-reaching candidate. A scope is an exact caller (a label suffix,
/// never a module) or `sql(1)`: a call with one SQL-text argument, judged
/// per call, not per file.
const REVIEWED_UNRESOLVED: &[(&str, &str, &str)] = &[
    (
        "prepare",
        "sql(1)",
        "rusqlite's `prepare(sql)`: a one-argument call whose argument is SQL text (a \
         literal, format!/concat!, an SQL constant or an `sql`/`query` binding). The \
         arm's `ArmPrepare::prepare` takes three arguments, so it never matches",
    ),
    (
        "apply_batch",
        "agents::prepare::publish::exclude_added",
        "publish's own remote adapter (`io.apply_batch`, publish.rs exclude_added), \
         reached only from publish, itself a write primitive",
    ),
    (
        "apply_batch",
        "agents::prepare::publish::discard_new_files",
        "SW-XS-27: Discard of new files on a machine (`io.apply_batch` of compare-and-swapped \
         removals), inside the discard_new_files write primitive",
    ),
    (
        "reconcile_pending",
        "agents::prepare::publish::discard_new_files",
        "SW-XS-27: discard_new_files' own remote adapter (`io.reconcile_pending`), inside the \
         discard_new_files write primitive",
    ),
    (
        "manifest_reading",
        "agents::prepare::publish::publish",
        "publish's own remote adapter (`io.manifest_reading`), inside the publish \
         write primitive",
    ),
    (
        "reconcile_pending",
        "agents::prepare::publish::publish",
        "publish's own remote adapter (`io.reconcile_pending`), inside the publish \
         write primitive",
    ),
    (
        "open_remote_tab",
        "commands::pty_backend::open_tab_pinned",
        "the receiver is the backend's SessionServerClient (its open_remote_tab), not the \
         MCP handler's tool method",
    ),
    (
        "recover",
        "principal_lifecycle::lifecycle::LifecycleRegistry::recover_all",
        "the receiver is a `Box<dyn LifecycleOwner>` (a principal restore), not the remote \
         destination adapter",
    ),
    (
        "prepare",
        "live_hub::run_review::prepare",
        "SV-PBF-159: the receiver is the run-review projection's `Owner` behind its mutex \
         (`run_review.lock().unwrap().prepare(topic, key, binding, story, run, id)`), an in-memory \
         view; the arm environment's `prepare` takes three arguments",
    ),
    (
        "prepare",
        "commands::host_connection_manager::HostConnectionManager::adopt_current",
        "the receiver is a RemotePublicationPermit (`publication_permit().prepare()`), not \
         the arm environment",
    ),
];

/// Whether `call`, made in `func`, is in a [`REVIEWED_UNRESOLVED`] scope.
fn in_scope(func: &Func, call: &Call, scope: &str) -> bool {
    if scope == "sql(1)" {
        return matches!(
            call,
            Call::Method {
                args: 1,
                sql: true,
                ..
            }
        );
    }
    let label = func.label();
    label == scope || label.ends_with(&format!("::{scope}"))
}

/// Unresolved method calls (several crate types define the name, the caller
/// names none of them) where some candidate can reach a sink: the walk's
/// blind spot, where a real writer could hide. `(caller id, call, live candidates)`.
fn dangerous_unresolved(graph: &Graph, sinks: &[Name]) -> Vec<(usize, Call, Vec<String>)> {
    let res = graph.resolution();
    let sink_ids: HashSet<usize> = sinks.iter().flat_map(|s| graph.matching(s)).collect();
    let mut rev: Vec<Vec<usize>> = vec![Vec::new(); graph.funcs.len()];
    for (from, next) in res.adj.iter().enumerate() {
        for &to in next {
            rev[to].push(from);
        }
    }
    let mut live = vec![false; graph.funcs.len()];
    let mut queue: VecDeque<usize> = sink_ids.iter().copied().collect();
    for &id in &sink_ids {
        live[id] = true;
    }
    while let Some(at) = queue.pop_front() {
        for &prev in &rev[at] {
            if !live[prev] {
                live[prev] = true;
                queue.push_back(prev);
            }
        }
    }
    let mut out = Vec::new();
    for (id, func) in graph.funcs.iter().enumerate() {
        for call in &func.calls {
            let Call::Method { name, .. } = call else {
                continue;
            };
            if COMMON_METHODS.contains(&name.as_str()) || !graph.resolve(func, call).is_empty() {
                continue;
            }
            let candidates: Vec<usize> = graph
                .by_name
                .get(name)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&id| graph.funcs[id].self_ty.is_some())
                .collect();
            let owners: HashSet<&String> = candidates
                .iter()
                .filter_map(|&id| graph.funcs[id].self_ty.as_ref())
                .collect();
            if owners.len() <= 1 {
                continue;
            }
            let live_candidates: Vec<String> = candidates
                .iter()
                .filter(|&&id| live[id])
                .map(|&id| graph.funcs[id].label())
                .collect();
            if !live_candidates.is_empty() {
                out.push((id, call.clone(), live_candidates));
            }
        }
    }
    out.dedup();
    out
}

/// No unresolved method call may name a sink-reaching method unless it
/// is reviewed (ceiling 0). [`MAX_UNRESOLVED_METHODS`] stays as a growth
/// tripwire; this is the part of that blind spot that matters.
#[test]
fn no_unresolved_call_may_hide_a_writer() {
    let graph = Graph::from_tree(&root().join("src"));
    let dangerous = dangerous_unresolved(&graph, SINKS);
    let (unreviewed, used) = unreviewed_calls(&graph, &dangerous, REVIEWED_UNRESOLVED);
    println!(
        "{} unresolved method calls name a sink-reaching method; {} reviewed by {} entries",
        dangerous.len(),
        dangerous.len() - unreviewed.len(),
        REVIEWED_UNRESOLVED.len()
    );
    assert!(
        unreviewed.is_empty(),
        "unresolved method calls that may reach a write primitive. Name the receiver's \
         type in the caller (a type annotation), or review it into REVIEWED_UNRESOLVED:\n{}",
        unreviewed.join("\n")
    );
    let stale: Vec<&str> = REVIEWED_UNRESOLVED
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|((_, scope, _), _)| *scope)
        .collect();
    assert!(
        stale.is_empty(),
        "stale REVIEWED_UNRESOLVED entries: {stale:?}"
    );
}

/// The dangerous calls no `reviewed` entry covers (as messages), and which
/// entries were used.
fn unreviewed_calls(
    graph: &Graph,
    dangerous: &[(usize, Call, Vec<String>)],
    reviewed: &[(&str, &str, &str)],
) -> (Vec<String>, Vec<bool>) {
    let mut used = vec![false; reviewed.len()];
    let mut out = Vec::new();
    for (caller, call, live) in dangerous {
        let Call::Method { name, args, .. } = call else {
            continue;
        };
        let func = &graph.funcs[*caller];
        match reviewed
            .iter()
            .position(|(method, scope, _)| method == name && in_scope(func, call, scope))
        {
            Some(index) => used[index] = true,
            None => out.push(format!(
                "{} calls .{name}(..{args} args), which may be {live:?}",
                func.label()
            )),
        }
    }
    (out, used)
}

/// The planted graph for the two hidden-writer self-tests: a sink-reaching `prepare`
/// (three arguments, like `ArmPrepare::prepare`) and a harmless one, and a
/// sink-reaching `apply_batch` and a harmless one, all on crate types the
/// callers do not name.
fn planted_ambiguous(caller_module: &[&str], caller_src: &str) -> Graph {
    planted(&[
        PLANTED_SINK,
        (
            "routines/machine_arm_app.rs",
            &["routines", "machine_arm_app"],
            r#"
            pub struct AppArmEnv;
            impl AppArmEnv {
                pub fn prepare(&self, r: u8, m: &str, p: u8) { crate::agents::prepare::prepare(r); }
            }
            pub struct Ledger;
            impl Ledger { pub fn prepare(&self, sql: &str) {} }
            pub struct Adapter;
            impl Adapter { pub fn apply_batch(&self) { crate::agents::prepare::prepare(1); } }
            pub struct LocalSink;
            impl LocalSink { pub fn apply_batch(&self) {} }
            "#,
        ),
        ("caller.rs", caller_module, caller_src),
    ])
}

/// The `sql(1)` review covers rusqlite's one-argument
/// `prepare(sql)`, judged per call: the arm's three-argument `prepare` in the
/// same file (here in a spawned thread) is still a finding.
#[test]
fn planted_three_argument_prepare_beside_sqlite_is_caught() {
    let graph = planted_ambiguous(
        &["routines", "tick"],
        r#"
        use rusqlite::Connection;
        const SELECT_RUNS: &str = "SELECT 1";
        pub fn read(conn: &Connection, sql: &str) {
            conn.prepare("SELECT id FROM runs");
            conn.prepare(&format!("SELECT {}", 1));
            conn.prepare(SELECT_RUNS);
            conn.prepare(sql);
        }
        pub fn tick(env: Env) {
            std::thread::spawn(move || { env.prepare(1, "m", 2); });
        }
        "#,
    );
    let dangerous = dangerous_unresolved(&graph, &["agents::prepare::prepare"]);
    let (unreviewed, _) = unreviewed_calls(&graph, &dangerous, REVIEWED_UNRESOLVED);
    assert_eq!(
        unreviewed.len(),
        1,
        "only the three-argument prepare is a finding: {unreviewed:?}"
    );
    assert!(
        unreviewed[0].starts_with("routines::tick::tick::{spawn} calls .prepare(..3 args)"),
        "{unreviewed:?}"
    );
}

/// The publish adapter reviews name the exact functions that make
/// those calls; the same call anywhere else in publish (here `preview`) is a
/// finding.
#[test]
fn planted_adapter_call_in_preview_is_caught() {
    let graph = planted_ambiguous(
        &["agents", "prepare", "publish"],
        r#"
        fn exclude_added(io: Io) { io.apply_batch(); }
        pub fn preview(io: Io) { io.apply_batch(); }
        "#,
    );
    let dangerous = dangerous_unresolved(&graph, &["agents::prepare::prepare"]);
    let (unreviewed, _) = unreviewed_calls(&graph, &dangerous, REVIEWED_UNRESOLVED);
    assert_eq!(unreviewed.len(), 1, "{unreviewed:?}");
    assert!(
        unreviewed[0].starts_with("agents::prepare::publish::preview calls .apply_batch"),
        "{unreviewed:?}"
    );
}

/// The retired duplicate preview path has no production entry point or writer cut.
#[test]
fn the_retired_preview_has_no_writer_exception() {
    let graph = Graph::from_tree(&root().join("src"));
    for name in ["preview_reverse_sync", "preview_reverse_sync_core"] {
        assert!(
            graph.matching(name).is_empty(),
            "the retired {name} path has returned"
        );
        assert!(
            ALLOWED.iter().all(|(via, _, _)| !via.ends_with(name)),
            "the retired {name} path must not bypass the writer guard"
        );
    }
}

/// The most in-crate path calls that may resolve to nothing. Nearly all are
/// derived or std-trait functions the crate never writes out (`Type::default`,
/// `String::new`, `Type::deserialize`, `Key::parse` via `FromStr`) or
/// external-crate constructors and parsers: none has a body in this app-source
/// graph, so none reaches a write primitive. A rise means the resolver lost track
/// of something: run with `X1_PRINT_UNRESOLVED=1` and read the list. The ceiling
/// is a growth tripwire; raise it only after reviewing every added site.
const MAX_UNRESOLVED_PATHS: usize = 277;
/// The most method calls that several crate types define and that resolve to
/// none of them, because the caller names none of those types (`.name()`, `.id()`
/// on a value whose type is inferred). This is the walk's main blind spot; the
/// ceiling stops it growing. Most of the count is std and library receivers
/// (`ok`, `is_empty`, `as_str`, ...), which are outside this graph; std method
/// names belong in COMMON_METHODS. The hidden-writer check uses the same
/// unresolved-call, COMMON_METHODS and multiple-owner filters as this count,
/// then checks every candidate's transitive reach to SINKS: every counted call
/// with a sink-reaching candidate must pass its review (rusqlite `.prepare`
/// collisions are covered by the SQL review). This cap is a growth tripwire:
/// raise it only after reviewing the added sites, never by adding sink
/// exceptions or slack.
const MAX_UNRESOLVED_METHODS: usize = 6_413;

/// X1: no unattended root reaches a write primitive; every reviewed entry is
/// still needed; the walk still sees the app.
#[test]
fn no_unattended_root_reaches_a_write_primitive() {
    let graph = Graph::from_tree(&root().join("src"));
    assert!(
        graph.funcs.len() > 5_000,
        "the graph is too small to be the app: {}",
        graph.funcs.len()
    );
    for sink in LIVE_SINKS {
        assert!(
            !graph.matching(sink).is_empty(),
            "sink {sink} names no function: update SINKS"
        );
    }
    let watchers: Vec<String> = graph
        .watcher_roots()
        .into_iter()
        .map(|id| graph.funcs[id].label())
        .collect();
    for expected in [
        "build_context_handler",
        "team_dir_handler",
        "attach_recursive_watch",
    ] {
        assert!(
            watchers
                .iter()
                .any(|label| label.ends_with(&format!("::{expected}"))),
            "the watcher roots include {expected}: {watchers:?}"
        );
    }
    // Test-only modules are not in the graph; production ones are.
    for test_only in [
        "routines/machine_arm_seams.rs",
        "routines/boundary_guard.rs",
        "commands/ri2_backstop.rs",
        "commands/machines/rm4f_support.rs",
        "mcp/remote_agent_task/claude_fixture.rs",
    ] {
        assert!(
            !graph.files.contains(test_only),
            "{test_only} is #[cfg(test)] and must not be in the graph"
        );
    }
    for production in [
        "routines/machine_arm.rs",
        "commands/watch.rs",
        "agents/prepare/mod.rs",
    ] {
        assert!(
            graph.files.contains(production),
            "the module walk reaches {production}"
        );
    }

    let report = graph.check(&POLICY);
    println!(
        "X1 guard: {} files, {} nodes; roots {:?}; {} in-crate path calls and {} ambiguous \
         method calls unresolved; {} ALLOWED, {} EXPLICIT",
        graph.files.len(),
        report.nodes,
        report.roots_by_kind,
        report.unresolved_paths.len(),
        report.unresolved_methods,
        ALLOWED.len(),
        EXPLICIT.len()
    );
    for (name, reached, floor) in &report.reach {
        println!("  reach {name}: {reached} (floor {floor})");
    }

    if std::env::var_os("X1_PRINT_UNRESOLVED").is_some() {
        for line in &report.unresolved_paths {
            println!("  unresolved: {line}");
        }
    }

    // A parser regression must not pass by seeing nothing.
    for (kind, floor) in [
        (RootKind::Command, 300),
        (RootKind::Route, 10),
        (RootKind::Task, 150),
        (RootKind::Event, 20),
    ] {
        let found = report
            .roots_by_kind
            .get(kind.describe())
            .copied()
            .unwrap_or(0);
        assert!(
            found >= floor,
            "only {found} {} roots found (floor {floor}): the walk lost them",
            kind.describe()
        );
    }
    for (name, reached, floor) in &report.reach {
        assert!(
            reached >= floor,
            "app root {name} reaches only {reached} functions (floor {floor}): the walk is blind"
        );
    }
    eprintln!(
        "X1 census: {}/{} (ceilings {MAX_UNRESOLVED_PATHS}/{MAX_UNRESOLVED_METHODS})",
        report.unresolved_paths.len(),
        report.unresolved_methods
    );
    assert!(
        report.unresolved_paths.len() <= MAX_UNRESOLVED_PATHS,
        "{} in-crate path calls resolve to nothing (ceiling {MAX_UNRESOLVED_PATHS}); \
         X1_PRINT_UNRESOLVED=1 lists them",
        report.unresolved_paths.len()
    );
    assert!(
        report.unresolved_methods <= MAX_UNRESOLVED_METHODS,
        "{} ambiguous method calls resolve to nothing (ceiling {MAX_UNRESOLVED_METHODS})",
        report.unresolved_methods
    );

    assert!(
        report.violations.is_empty(),
        "X1: an entry point that is not a reviewed explicit operation reaches a write \
         primitive. Make the write explicit (an MCP call or a human action) and add the \
         command / route / task to EXPLICIT with its reason; or, if this path is Step 1 / \
         Step 2 for a launch, add the reviewed (via, target) to ALLOWED.\n\n{}",
        report.violations.join("\n\n")
    );
    let stale: Vec<String> = ALLOWED
        .iter()
        .enumerate()
        .filter(|(index, _)| !report.used_allowed.contains(index))
        .map(|(_, (via, target, why))| format!("{via} -> {target} ({why})"))
        .collect();
    assert!(
        stale.is_empty(),
        "stale ALLOWED entries (no root crosses them):\n{}",
        stale.join("\n")
    );
    let stale: Vec<String> = EXPLICIT
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            report.explicit_roots[*index] == 0 || report.explicit_sinks[*index] == 0
        })
        .map(|(index, (name, _, why))| {
            format!(
                "{name} ({why}): names {} roots reaching {} sinks",
                report.explicit_roots[index], report.explicit_sinks[index]
            )
        })
        .collect();
    assert!(
        stale.is_empty(),
        "stale EXPLICIT entries (each must name a root that reaches a write primitive):\n{}",
        stale.join("\n")
    );
    let unpinned: Vec<String> = EXPLICIT
        .iter()
        .zip(&report.explicit_reached)
        .flat_map(|((name, pinned, _), reached)| {
            reached
                .iter()
                .filter(|sink| {
                    !pinned
                        .iter()
                        .any(|p| *sink == p || sink.ends_with(&format!("::{p}")))
                })
                .map(move |sink| format!("{name} now reaches {sink} (pinned to {pinned:?})"))
        })
        .collect();
    assert!(
        unpinned.is_empty(),
        "an EXPLICIT entry reaches a write primitive it was not reviewed for:\n{}",
        unpinned.join("\n")
    );
}

/// The machine-arm sync (its 30 s timer thread, and the reconnect
/// callback with the thread it spawns) reaches the sync but never a prepare:
/// a drifted arm is marked `content_stale`, not re-prepared.
#[test]
fn the_machine_arm_sync_never_prepares() {
    let graph = Graph::from_tree(&root().join("src"));
    let roots: &[(Name, &str, usize)] = &[
        (
            "routines::machine_arm_app::install::{spawn}",
            "30 s arm-sync timer",
            1,
        ),
        (
            "routines::machine_arm_app::install::{subscribe}",
            "reconnect arm sync",
            1,
        ),
    ];
    let reaches = |sinks: &[Name]| {
        graph
            .check(&Policy {
                app_roots: roots,
                sinks,
                allowed: &[],
                explicit: &[],
            })
            .violations
            .into_iter()
            .filter(|v| v.starts_with("app seam"))
            .collect::<Vec<_>>()
    };
    // The walk is not blind: both roots reach the sync, its handover settle
    // and its definition push.
    for reached in [
        "routines::machine_arm::sync_machine",
        "routines::machine_arm::end_handover",
        "routines::machine_arm::push_definition",
    ] {
        let found = reaches(&[reached]);
        assert_eq!(found.len(), 2, "both roots reach {reached}: {found:?}");
    }
    let mut never: Vec<Name> = SINKS.to_vec();
    never.extend([
        "routines::machine_arm::refresh_on_machine",
        "routines::machine_arm_app::refresh",
        "routines::machine_arm_app::refresh_bounded",
    ]);
    let prepared = reaches(&never);
    assert!(
        prepared.is_empty(),
        "the arm sync reaches a write primitive or a refresh:\n{}",
        prepared.join("\n\n")
    );
}

/// The refresh thread (`refresh_within`'s spawn) only runs the
/// work it is handed; the work (`refresh`) is credited to `refresh_bounded`,
/// whose callers are the EXPLICIT routine commands and MCP tools (the main
/// test fails if any other root reaches it). The thread itself reaches no
/// write primitive.
#[test]
fn the_refresh_thread_writes_only_for_its_caller() {
    let graph = Graph::from_tree(&root().join("src"));
    let report = graph.check(&Policy {
        app_roots: &[(
            "routines::machine_arm_app::refresh_within::{spawn}",
            "routine-arm-refresh thread",
            1,
        )],
        sinks: SINKS,
        allowed: &[],
        explicit: &[],
    });
    let own: Vec<&String> = report
        .violations
        .iter()
        .filter(|v| v.starts_with("app seam"))
        .collect();
    assert!(
        own.is_empty(),
        "the refresh thread writes by itself: {own:?}"
    );
    let bounded = graph.check(&Policy {
        app_roots: &[(
            "routines::machine_arm_app::refresh_bounded",
            "refresh_bounded",
            1,
        )],
        sinks: &["agents::prepare::prepare"],
        allowed: &[],
        explicit: &[],
    });
    assert!(
        bounded.violations.iter().any(|v| v.starts_with("app seam")),
        "refresh_bounded reaches prepare (the walk sees the handed-in work)"
    );
}

/// Counting a workspace's unpublished team-file edits only reads.
/// From the count (local and machine) and the `get_sync_status` assembly, no
/// walk reaches a write primitive, settles a cut remote batch
/// (`reconcile_pending` / `recover`, which resume an Apply on the machine),
/// journals one, or loads/saves a managed record (`load` moves an unreadable
/// record aside). The walk is not blind: it does reach the read-only manifest.
#[test]
fn the_unpublished_count_only_reads() {
    let graph = Graph::from_tree(&root().join("src"));
    let roots: &[(Name, &str, usize)] = &[
        (
            "agents::prepare::edits::count",
            "the count of one workspace",
            1,
        ),
        (
            "mcp::team_file_edits::unpublished",
            "get_sync_status.unpublished",
            1,
        ),
        (
            "commands::team_file_edits::team_file_edits_status",
            "the app's count of unsaved team edits",
            1,
        ),
        (
            "commands::team_file_edits::worktree_unpublished_context",
            "the remove confirmation's count",
            1,
        ),
    ];
    let reaches = |sinks: &[Name]| {
        graph
            .check(&Policy {
                app_roots: roots,
                sinks,
                allowed: &[],
                explicit: &[],
            })
            .violations
            .into_iter()
            .filter(|v| v.starts_with("app seam"))
            .collect::<Vec<_>>()
    };
    let mut never: Vec<Name> = SINKS.to_vec();
    never.extend([
        "reconcile_pending",
        "mcp::remote_destination::Adapter::recover",
        "mcp::remote_destination::Adapter::journal",
        "mcp::remote_destination::Adapter::settled_journal",
        "mcp::remote_destination::Adapter::manifest_call",
        "agents::prepare::record::ManagedRecord::load",
        "agents::prepare::record::ManagedRecord::save",
        "agents::prepare::publish::observe_remote",
        "agents::prepare::publish::preview",
        // The machine's preparation reconcile (a count resolves receipt-only).
        "terminal::session_server::SessionServerClient::reconcile_remote_workspace",
        "mcp::remote_workspace_target::confirm_prepared",
        // The install store is only read (never created or quarantined).
        "content_engine::sync_state_store::SyncStateStore::open",
        "content_engine::sync_state_store::quarantine_corrupt_db",
    ]);
    let wrote = reaches(&never);
    assert!(
        wrote.is_empty(),
        "the unpublished count reaches a writer:\n{}",
        wrote.join("\n\n")
    );
    for reached in [
        "mcp::remote_destination::Adapter::manifest_observe",
        "agents::prepare::publish::observe_local",
        // The remote lookup is walked (so the confirm_prepared ban means
        // something).
        "mcp::remote_workspace_target::admit",
        "mcp::remote_workspace_target::candidate_receipts",
    ] {
        assert!(
            !reaches(&[reached]).is_empty(),
            "the walk must see the count reach {reached}"
        );
    }
}

/// The sinks are reachable from where they should be: the explicit MCP
/// operations reach the engine (so the walk is not blind to them).
#[test]
fn explicit_operations_do_reach_the_write_primitives() {
    let graph = Graph::from_tree(&root().join("src"));
    let report = graph.check(&Policy {
        app_roots: &[(
            "mcp::context_operations::explicit::run",
            "explicit Step 3 operation",
            1,
        )],
        sinks: &["agents::prepare::prepare"],
        allowed: &[],
        explicit: &[],
    });
    assert!(
        report.violations.iter().any(|p| p
            .starts_with("app seam mcp::context_operations::explicit::run")
            && p.contains("agents::prepare::prepare")),
        "the walk must find explicit run -> prepare"
    );
}

fn planted(sources: &[(&str, &[&str], &str)]) -> Graph {
    let mut graph = Graph::new(HashSet::new());
    for (file, module, src) in sources {
        graph.add_source(file, module.iter().map(|s| s.to_string()).collect(), src);
    }
    graph
}

const PLANTED_SINK: (&str, &[&str], &str) = (
    "agents/prepare/mod.rs",
    &["agents", "prepare"],
    "pub fn prepare(request: u8) -> u8 { request }",
);

fn planted_policy<'a>(
    app_roots: &'a [(Name, &'a str, usize)],
    allowed: &'a [(Name, Name, &'a str)],
    explicit: &'a [(Name, &'a [Name], &'a str)],
) -> Policy<'a> {
    Policy {
        app_roots,
        sinks: &["agents::prepare::prepare"],
        allowed,
        explicit,
    }
}

/// Self-test: a planted root → helper → sink chain fails with its path; the
/// same chain through an allowed path does not; test-only code is not a
/// caller; calls inside macros and through `use` aliases are followed.
#[test]
fn planted_violation_is_caught_with_its_path() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "commands/watch.rs",
            &["commands", "watch"],
            r#"
            use crate::agents::prepare as engine;
            pub fn build_planted_handler() { helper(); }
            fn helper() { tracing::info!("x {}", relay(1)); }
            fn relay(n: u8) -> u8 { engine::prepare(n) }
            pub fn quiet_handler() { let prepare = |x: u8| x; prepare(1); std::fs::write("a", "b").ok(); }
            #[cfg(test)]
            mod tests { fn t() { crate::agents::prepare::prepare(1); } }
            #[cfg(any(test, feature = "off"))]
            fn fixture() { crate::agents::prepare::prepare(1); }
            "#,
        ),
    ]);
    assert!(
        graph.matching("commands::watch::fixture").is_empty(),
        "cfg(any(test, off-feature)) is not compiled"
    );
    let roots = [
        (
            "commands::watch::build_planted_handler",
            "planted watcher",
            1,
        ),
        ("commands::watch::quiet_handler", "quiet", 1),
    ];
    let report = graph.check(&planted_policy(&roots, &[], &[]));
    assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
    let found = &report.violations[0];
    for hop in [
        "commands::watch::build_planted_handler",
        "commands::watch::helper",
        "commands::watch::relay",
        "agents::prepare::prepare",
    ] {
        assert!(found.contains(hop), "the printed path names {hop}: {found}");
    }
    assert!(found.contains("planted watcher"), "{found}");

    let allowed = [(
        "commands::watch::helper",
        "agents::prepare::prepare",
        "planted launch",
    )];
    let report = graph.check(&planted_policy(&roots, &allowed, &[]));
    assert!(report.violations.is_empty(), "{:?}", report.violations);
    assert_eq!(report.used_allowed.into_iter().collect::<Vec<_>>(), vec![0]);
}

/// The planted dispatch forms the resolver must follow, each from an
/// unreviewed entry point to the sink.
fn assert_caught(graph: &Graph, root_kind: &str, hop: &str) {
    let report = graph.check(&planted_policy(&[], &[], &[]));
    assert!(
        report.violations.iter().any(|v| v.starts_with(root_kind)
            && v.contains(hop)
            && v.contains("agents::prepare::prepare")),
        "expected a {root_kind} violation through {hop}: {:?}",
        report.violations
    );
}

#[test]
fn planted_trait_object_dispatch_is_followed() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "commands/sync.rs",
            &["commands", "sync"],
            r#"
            pub trait Writer { fn write_it(&self); }
            pub struct Quiet;
            pub struct Loud;
            impl Writer for Quiet { fn write_it(&self) {} }
            impl Writer for Loud { fn write_it(&self) { crate::agents::prepare::prepare(1); } }
            #[tauri::command]
            pub fn on_focus(w: &dyn Writer) { w.write_it(); }
            "#,
        ),
    ]);
    assert_caught(
        &graph,
        "#[tauri::command] commands::sync::on_focus",
        "Loud::write_it",
    );
}

#[test]
fn planted_supertrait_dispatch_is_followed() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "routines/wake.rs",
            &["routines", "wake"],
            r#"
            pub trait Notify: Send { fn redispatch(&self); }
            pub trait Env: Notify { fn home(&self) -> u8; }
            pub struct Quiet;
            pub struct App;
            impl Notify for Quiet { fn redispatch(&self) {} }
            impl Notify for App { fn redispatch(&self) { crate::agents::prepare::prepare(1); } }
            #[tauri::command]
            pub fn defer(env: &dyn Env) { env.redispatch(); }
            #[tauri::command]
            pub fn defer_generic<E: Env>(env: E) { env.redispatch(); }
            "#,
        ),
    ]);
    assert_caught(
        &graph,
        "#[tauri::command] routines::wake::defer ",
        "App::redispatch",
    );
    assert_caught(
        &graph,
        "#[tauri::command] routines::wake::defer_generic",
        "App::redispatch",
    );
}

#[test]
fn planted_generic_dispatch_is_followed() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "commands/sync.rs",
            &["commands", "sync"],
            r#"
            pub trait Writer { fn write_it(&self); }
            pub struct Quiet;
            pub struct Loud;
            impl Writer for Quiet { fn write_it(&self) {} }
            impl Writer for Loud { fn write_it(&self) { crate::agents::prepare::prepare(1); } }
            #[tauri::command]
            pub fn on_focus<W>(w: W) where W: Writer { w.write_it(); }
            "#,
        ),
    ]);
    assert_caught(
        &graph,
        "#[tauri::command] commands::sync::on_focus",
        "Loud::write_it",
    );
}

#[test]
fn planted_field_fn_dispatch_is_followed() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "commands/sync.rs",
            &["commands", "sync"],
            r#"
            pub struct Hooks { on_change: Box<dyn Fn(u8) + Send> }
            pub fn hooks() -> Hooks {
                Hooks { on_change: Box::new(|n| { crate::agents::prepare::prepare(n); }) }
            }
            impl Hooks {
                pub fn fire(&self) { (self.on_change)(1); }
            }
            #[tauri::command]
            pub fn switch_team(h: &Hooks) { h.fire(); }
            "#,
        ),
    ]);
    assert_caught(
        &graph,
        "#[tauri::command] commands::sync::switch_team",
        "hooks::{field:on_change}",
    );
    // Building the hooks does not run them.
    let report = graph.check(&planted_policy(
        &[("commands::sync::hooks", "builder", 1)],
        &[],
        &[],
    ));
    assert!(
        !report.violations.iter().any(|v| v.starts_with("app seam")),
        "{:?}",
        report.violations
    );
}

#[test]
fn planted_subscribe_callback_is_its_own_root() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "terminal/osc.rs",
            &["terminal", "osc"],
            r#"
            pub struct Bus;
            pub fn wire(bus: &Bus) {
                let on_end = move |id: u8| { relay(id); };
                bus.subscribe(on_end);
                bus.listen("x", |e| { crate::agents::prepare::prepare(e); });
            }
            fn relay(n: u8) { crate::agents::prepare::prepare(n); }
            pub fn starter() {
                tokio::spawn(async move { crate::agents::prepare::prepare(2); });
            }
            "#,
        ),
    ]);
    let report = graph.check(&planted_policy(&[], &[], &[]));
    let roots: Vec<&str> = report
        .violations
        .iter()
        .map(|v| v.lines().next().unwrap())
        .collect();
    for expected in [
        "registered callback terminal::osc::wire::{subscribe}",
        "registered callback terminal::osc::wire::{listen}",
        "spawned background task terminal::osc::starter::{spawn}",
    ] {
        assert!(
            roots.iter().any(|r| r.starts_with(expected)),
            "{expected} is a root that reaches the sink: {roots:?}"
        );
    }
    // A registration is not credited to the registering function...
    let report = graph.check(&planted_policy(
        &[("terminal::osc::wire", "registrar", 1)],
        &[],
        &[],
    ));
    assert!(
        !report.violations.iter().any(|v| v.starts_with("app seam")),
        "{:?}",
        report.violations
    );
    // ...but a spawner does start its task.
    let report = graph.check(&planted_policy(
        &[("terminal::osc::starter", "spawner", 1)],
        &[],
        &[],
    ));
    assert!(
        report.violations.iter().any(|v| v.starts_with("app seam")),
        "{:?}",
        report.violations
    );
    // A reviewed background body passes; the entry must still be needed.
    let sink: &[Name] = &["agents::prepare::prepare"];
    let explicit = [
        ("osc::wire::{subscribe}", sink, "planted"),
        ("osc::wire::{listen}", sink, "planted"),
        ("osc::starter::{spawn}", sink, "planted"),
    ];
    let report = graph.check(&planted_policy(&[], &[], &explicit));
    assert!(report.violations.is_empty(), "{:?}", report.violations);
    assert!(report.explicit_sinks.iter().all(|&n| n > 0));
}

#[test]
fn planted_routes_and_reexports_are_followed() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "agents/mod.rs",
            &["agents"],
            "pub use self::inner::kick; pub use self::glob::*;",
        ),
        (
            "agents/inner.rs",
            &["agents", "inner"],
            "pub fn kick() { crate::agents::prepare::prepare(1); }",
        ),
        (
            "agents/glob.rs",
            &["agents", "glob"],
            "pub fn nudge() { crate::agents::prepare::prepare(1); }",
        ),
        (
            "http/routes/hook.rs",
            &["http", "routes", "hook"],
            r#"
            pub fn router() -> Router {
                Router::new()
                    .route("/api/v1/hook", post(hook))
                    .route("/api/v1/nudge", get(|| async { crate::agents::nudge(); }))
            }
            async fn hook() { crate::agents::kick(); }
            "#,
        ),
    ]);
    assert_caught(
        &graph,
        "HTTP route route:/api/v1/hook",
        "agents::inner::kick",
    );
    assert_caught(
        &graph,
        "HTTP route route:/api/v1/nudge",
        "agents::glob::nudge",
    );
    let report = graph.check(&planted_policy(
        &[],
        &[],
        &[
            (
                "route:/api/v1/hook",
                &["agents::prepare::prepare"],
                "planted",
            ),
            (
                "route:/api/v1/nudge",
                &["agents::prepare::prepare"],
                "planted",
            ),
        ],
    ));
    assert!(report.violations.is_empty(), "{:?}", report.violations);
}

/// `super::f()` and `use super::f` name the parent module's `f`, not
/// every `f` in the crate. A hub-local `publish` must not become the sink-reaching
/// `agents::prepare::publisher::publish`.
#[test]
fn planted_relative_paths_name_their_own_module() {
    let graph = planted(&[
        PLANTED_SINK,
        (
            "agents/prepare/publisher.rs",
            &["agents", "prepare", "publisher"],
            "pub fn publish() { crate::agents::prepare::prepare(1); }",
        ),
        ("hub/mod.rs", &["hub"], "pub fn publish(x: u8) {}"),
        (
            "hub/stats.rs",
            &["hub", "stats"],
            r#"
            use super::publish;
            pub fn by_use() { publish(1); }
            pub fn by_path() { super::publish(2); }
            pub fn the_real_one() { crate::agents::prepare::publisher::publish(); }
            "#,
        ),
    ]);
    let violations = |root: &'static str| {
        graph
            .check(&planted_policy(&[(root, "planted", 1)], &[], &[]))
            .violations
    };
    assert!(violations("hub::stats::by_use").is_empty());
    assert!(violations("hub::stats::by_path").is_empty());
    assert!(
        !violations("hub::stats::the_real_one").is_empty(),
        "the walk still follows a qualified call to the sink-reaching publish"
    );
}

/// A maintainer's probe, not a gate: print every path the walk finds from
/// `X1_ROOT` (a label suffix, or `ALL` for the full policy) to `X1_SINK`,
/// honouring [`ALLOWED`] unless `X1_NO_ALLOW` is set. `X1_ROOT=crate::run
/// X1_SINK=agents::prepare::prepare cargo test -p codesense --test
/// explicit_sync_unattended_writer_guard explain_a_path -- --ignored --nocapture`
#[test]
#[ignore = "a probe: set X1_ROOT and X1_SINK"]
fn explain_a_path() {
    let (Ok(from), Ok(to)) = (std::env::var("X1_ROOT"), std::env::var("X1_SINK")) else {
        panic!("set X1_ROOT and X1_SINK");
    };
    let graph = Graph::from_tree(&root().join("src"));
    let leak = |s: String| -> Name { Box::leak(s.into_boxed_str()) };
    let mut allowed: Vec<(Name, Name, &str)> = if std::env::var_os("X1_NO_ALLOW").is_some() {
        Vec::new()
    } else {
        ALLOWED.to_vec()
    };
    // `X1_ALLOW="via>target;…"`: try more allowed paths without a rebuild.
    for edge in std::env::var("X1_ALLOW").unwrap_or_default().split(';') {
        if let Some((a, b)) = edge.split_once('>') {
            allowed.push((
                leak(a.trim().to_string()),
                leak(b.trim().to_string()),
                "probe",
            ));
        }
    }
    let sinks: Vec<Name> = if to == "ALL" {
        SINKS.to_vec()
    } else {
        vec![leak(to)]
    };
    let app_roots: Vec<(Name, &str, usize)> = if from == "ALL" {
        APP_ROOTS.to_vec()
    } else {
        vec![(leak(from.clone()), "probe", 1)]
    };
    let report = graph.check(&Policy {
        app_roots: &app_roots,
        sinks: &sinks,
        allowed: &allowed,
        explicit: if from == "ALL" { EXPLICIT } else { &[] },
    });
    let paths: Vec<&String> = report
        .violations
        .iter()
        .filter(|v| from == "ALL" || v.starts_with("app seam"))
        .collect();
    if paths.is_empty() {
        println!("no path");
    }
    for path in paths {
        println!("{path}\n");
    }
}
