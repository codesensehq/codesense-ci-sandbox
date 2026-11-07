/**
 * Tauri IPC mock for Playwright E2E tests.
 *
 * This module exports a function string that can be passed to page.addInitScript().
 * The function runs IN THE BROWSER before any app JS loads, setting up
 * window.__TAURI_INTERNALS__ with mock IPC responses so the app can
 * initialize fully without a real Tauri backend.
 *
 * IMPORTANT: The function body is pure browser JS — no Node.js APIs, no imports,
 * no TypeScript, no closures over external variables.
 */

/**
 * Returns the serializable init script function to inject via addInitScript.
 * Call it with optional overrides for specific commands.
 */
export interface TauriMockCall {
  cmd: string;
  args?: Record<string, unknown>;
}

const TERMINAL_PROCESS_COMMANDS = new Set([
  "terminal_open_tab",
  "terminal_close_tab",
  "terminal_close_workspace",
  "write_pty",
  "resize_pty",
  "reconfigure_warm_pool",
  "submit_pty_line",
  "subscribe_terminal",
  "unsubscribe_terminal",
  "read_terminal_text",
  "session_has_live_agent",
  "sessions_have_live_agent",
  "session_is_idle",
  "read_run_transcript",
  "dashboard_foreground_session",
  "dashboard_list_sessions",
  "dashboard_probe_sessions_alive",
  "dashboard_register_session",
  "dashboard_resolve_session",
  "open_terminal",
  "open_external_terminal",
  "run_shell_command",
]);

export const isTerminalProcessCommand = (command: string): boolean =>
  TERMINAL_PROCESS_COMMANDS.has(command);

export function getTauriMockInitScript(overrides?: Record<string, unknown>): string {
  // Serialize overrides into the script as a JSON string
  const overridesJson = JSON.stringify(overrides ?? {});

  return `
(function () {
  // === Command overrides from test ===
  var commandOverrides = ${overridesJson};
  var mockCalls = [];
  var humanStatusByTeam = {};
  var configurationViews = {};

  function owns(object, key) {
    return Object.prototype.hasOwnProperty.call(object, key);
  }

  function redactedArgs(value) {
    if (!value || typeof value !== "object") return value;
    if (Array.isArray(value)) return value.map(redactedArgs);
    var copy = {};
    Object.keys(value).forEach(function (key) {
      copy[key] = /token|secret|password|credential|authorization|api[-_]?key/i.test(key)
        ? "[REDACTED]"
        : redactedArgs(value[key]);
    });
    return copy;
  }

  window.__TAURI_MOCK__ = { calls: mockCalls };

  // === Mock app state ===
  var MOCK_APP_STATE = JSON.stringify({
    repos: [
      {
        repoPath: "/mock/repos/my-project",
        repoName: "my-project",
        contextRepoPath: "/mock/repos/my-project/.context",
        hasRemote: false,
        remoteUrl: null
      }
    ],
    installBundleReady: true,
    viewer: {
      openTabs: {},
      activeTab: {},
      expandedFolders: {},
      terminalVisible: false,
      settings: {
        terminal: "terminal",
        editor: "cursor",
        sidebarWidth: 240,
        treeFontSize: "normal",
        terminalFontSize: 13,
        terminalTheme: "github-dark",
        statusLineStyle: "normal"
      },
      claudeSessionIds: {},
      terminalTabs: {},
      activeTerminalTab: {},
      shellTabCounter: {}
    }
  });

  // === Mock local profile status ===
  var MOCK_LOCAL_PROFILE = {
    principal: "12345",
    displayName: "Local profile",
    machineOwner: { kind: "local", id: "mock-machine-owner" }
  };
  var MOCK_LOCAL_PROFILE_STATUS = {
    active: MOCK_LOCAL_PROFILE,
    profiles: [MOCK_LOCAL_PROFILE],
    privateAuthority: null
  };

  // === Mock org (satisfies the C.2 first-launch gate so tests land on
  // the repo picker instead of the org picker). discover_local_orgs
  // returns this summary, load_org_cmd returns a matching LoadedOrg,
  // and the useOrgDiscovery hook populates the store. ===
  var MOCK_ORG = {
    slug: "test-workspace",
    display_name: "Test Workspace",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    org_root: "/mock/codesense/users/12345/orgs/test-workspace",
    people: [
      {
        github_user_id: 12345,
        github_username: "test-user",
        display_name: "Test User",
        avatar_url: null
      }
    ],
    teams: [
      {
        name: "default",
        member_ids: [12345],
        lead_ids: [12345],
        source_repos: []
      }
    ]
  };

  var MOCK_ORG_SUMMARY = {
    slug: "test-workspace",
    display_name: "Test Workspace",
    has_remote: false,
    remote_url: null,
    org_root: "/mock/codesense/users/12345/orgs/test-workspace"
  };

  var MOCK_LOADED_ORG = {
    slug: "test-workspace",
    root: "/mock/codesense/users/12345/orgs/test-workspace",
    org: MOCK_ORG,
    partial: { org_yml: null, people_yml: null, teams_yml: null },
    file_errors: []
  };

  // === Mock file entries ===
  var MOCK_FILE_ENTRIES = [
    { name: "README.md", path: "/mock/repos/my-project/.context/README.md", is_dir: false, extension: "md" },
    { name: "docs", path: "/mock/repos/my-project/.context/docs", is_dir: true, extension: null },
    { name: "architecture.md", path: "/mock/repos/my-project/.context/architecture.md", is_dir: false, extension: "md" },
    { name: "product-brief.md", path: "/mock/repos/my-project/.context/product-brief.md", is_dir: false, extension: "md" }
  ];

  // === Mock context files ===
  var MOCK_CONTEXT_FILES = [
    { name: "README.md", relativePath: "README.md", fullPath: "/mock/repos/my-project/.context/README.md" },
    { name: "architecture.md", relativePath: "architecture.md", fullPath: "/mock/repos/my-project/.context/architecture.md" },
    { name: "product-brief.md", relativePath: "product-brief.md", fullPath: "/mock/repos/my-project/.context/product-brief.md" }
  ];

  // === Mock markdown content ===
  var MOCK_MARKDOWN = "# My Project\\n\\nThis is a mock project for E2E testing.\\n\\n## Architecture\\n\\nThe project uses a standard structure.\\n";

  // === Set up __TAURI_INTERNALS__ ===
  window.__TAURI_INTERNALS__ = window.__TAURI_INTERNALS__ || {};
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = window.__TAURI_EVENT_PLUGIN_INTERNALS__ || {};

  // === Window metadata (required for getCurrentWindow) ===
  window.__TAURI_INTERNALS__.metadata = {
    currentWindow: { label: "main" },
    currentWebview: { windowLabel: "main", label: "main" }
  };

  // === Callback management ===
  var callbacks = new Map();

  function registerCallback(callback, once) {
    var identifier = window.crypto.getRandomValues(new Uint32Array(1))[0];
    callbacks.set(identifier, function (data) {
      if (once) {
        callbacks.delete(identifier);
      }
      return callback && callback(data);
    });
    return identifier;
  }

  function unregisterCallback(id) {
    callbacks.delete(id);
  }

  function runCallback(id, data) {
    var callback = callbacks.get(id);
    if (callback) {
      callback(data);
    }
  }

  // === Event listener management ===
  var eventListeners = new Map();

  function handleListen(args) {
    if (!eventListeners.has(args.event)) {
      eventListeners.set(args.event, []);
    }
    eventListeners.get(args.event).push(args.handler);
    return args.handler;
  }

  function handleUnlisten(args) {
    var listeners = eventListeners.get(args.event);
    if (listeners) {
      var idx = listeners.indexOf(args.id);
      if (idx !== -1) listeners.splice(idx, 1);
    }
  }

  function handleEmit(args) {
    var listeners = eventListeners.get(args.event) || [];
    for (var i = 0; i < listeners.length; i++) {
      runCallback(listeners[i], args);
    }
    return null;
  }

  // === Command handler ===
  function handleCommand(cmd, args) {
    // Check overrides first
    if (owns(commandOverrides, cmd)) {
      var override = commandOverrides[cmd];
      if (
        override &&
        typeof override === "object" &&
        owns(override, "__tauriReject")
      ) {
        throw override.__tauriReject;
      }
      return override;
    }

    switch (cmd) {
      // Local profiles: a fresh install enters as one selected local profile (the
      // principal matches the mock org root's users/<id> namespace).
      case "local_profile_status":
        return MOCK_LOCAL_PROFILE_STATUS;
      case "routine_scheduler_status":
        return { missed_count: 0 };
      case "routine_needs_you_count":
        return 0;
      case "wv_list_attention_board":
        return { youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] };
      case "wv_list_pending_approvals":
        return [];
      case "list_machines":
        return [];
      case "list_context_attachments":
        return [];
      // The source-free team landing renders the nav, which scopes this set.
      case "get_publish_set":
        return { buckets: [], general: { files: [], count: 0 } };
      case "get_team_nav_pins":
        return [];
      case "avatar_packs_list_for_team":
        return { packs: [], default_pack: null, agent_avatars: {}, warnings: [] };
      // Boot seeds the prepared configuration snapshot, so every topic answers a
      // HubSnapshot with the view key the host derives and no rows.
      case "configuration_prepare": {
        var topic = args && args.topic;
        var principal = args && args.expectedPrincipal;
        var viewKey =
          topic === "prefs"
            ? args.scope === "global"
              ? "prefs:global"
              : "prefs:" + principal + ":native"
            : topic === "profile"
              ? "profile:" + principal + ":" + args.org
              : topic + ":" + principal + ":native";
        configurationViews[viewKey] = { topic: topic, viewKey: viewKey, rev: 0, snapshot: [] };
        return configurationViews[viewKey];
      }
      case "live_hub_subscribe":
      case "live_hub_unsubscribe":
        return args && configurationViews[args.viewKey];
      // App state
      case "load_app_state":
        return MOCK_APP_STATE;
      case "save_app_state":
        return undefined;

      // File watcher
      case "start_watching":
        return undefined;
      case "stop_watching":
        return undefined;
      case "add_watch":
        return undefined;
      case "remove_watch":
        return undefined;

      // Dependency manager
      case "get_dependency_status":
        return {
          git: { available: true, path: "/mock/codesense/bin/git", version: "2.51.0" },
          agents: {
            claude: { available: true, path: "/usr/local/bin/claude", resolving: false }
          }
        };

      // Tool detection
      case "detect_tools":
        return {
          terminals: [{ id: "terminal", label: "Terminal" }],
          editors: [{ id: "cursor", label: "Cursor" }],
          default_terminal: "terminal",
          default_editor: "cursor"
        };

      // File operations
      case "list_directory":
        return MOCK_FILE_ENTRIES;
      case "read_file":
        return MOCK_MARKDOWN;

      // Pipeline scanner (story 30.7) — return an empty derived tree so
      // the pipeline viewer renders the empty state without crashing.
      case "scan_pipeline_tree":
        return {
          dirPath: "",
          dirName: "",
          absoluteDirPath: (args && args.contextRepoPath ? args.contextRepoPath : "/mock") +
            "/_bmad-output/implementation-artifacts",
          stories: [],
          subEpics: [],
          order: null,
          orderError: null,
          mainDoc: null
        };
      case "find_nearest_valid_scope":
        return args && args.scopePath ? args.scopePath : "";
      case "reconcile_required_providers":
        // Healthy default: no provider gaps. Without this the ProviderReconcilePanel
        // reads the gaps array off an undefined report and the whole app crashes to
        // the ErrorBoundary, timing out every assembled-app fixture.
        return { gaps: [], comingSoon: [], unsupported: [] };
      // The inbox header's presence chip reads .status off this view; an
      // undefined answer crashes it to the view's ErrorBoundary. A set is
      // remembered per team, so a later get for that team reads it back.
      case "get_human_status":
        return humanStatusByTeam[JSON.stringify(args && args.team)] || { status: "online", updatedAtMs: 0 };
      case "set_human_status": {
        var view = { status: (args && args.status) || "online", updatedAtMs: Date.now() };
        if (args && args.note) view.note = args.note;
        humanStatusByTeam[JSON.stringify(args && args.team)] = view;
        return view;
      }
      case "auto_commit_and_push":
        return { status: "ok" };
      case "list_context_files":
        return MOCK_CONTEXT_FILES;
      case "check_path_exists":
        return true;
      // A team's landing page (#2097). The mock org's team content folder holds
      // nothing, which the backend answers as "unavailable" (not cloned/synced
      // yet): nothing auto-opens. A test wanting a page overrides it with
      // { kind: "found", path: "<content root>/README.md" }.
      case "resolve_team_landing_page":
        return { kind: "unavailable" };

      // Unsaved team edits (SW-XS-27, #2101/#2103): nothing unsaved anywhere.
      case "team_file_edits_status":
        return { total: 0, cleanWorkspaces: 0, workspaces: [] };
      case "publish_context_edits":
        return { status: "published", failedCount: 0, conflicts: [] };
      case "discard_team_file_edits":
        return { status: "ready", failedCount: 0 };
      case "worktree_unpublished_context":
        return { count: 0, files: [] };

      // Inbox "Needs you" read marks (#2096/#2098): how many were marked.
      case "mark_inbox_messages_read":
        return Array.isArray(args && args.messageIds) ? args.messageIds.length : 0;

      // Notes' "Send to…" (#2093): every mock agent runs on this Mac.
      case "send_prompt_to_agent":
        return { status: "submitted", submitted: true, machine: null, code: null, message: null };
      case "create_file":
        return undefined;
      case "rename_path":
        return undefined;
      case "delete_path":
        return undefined;

      // Context repo
      case "create_context_repo":
        return { path: "/mock/repos/my-project/.context", created: true };
      case "get_context_repo_path":
        return "/mock/repos/my-project/.context";
      case "check_context_repo_remote":
        return null;
      case "connect_context_remote":
        return { path: "/mock/repos/my-project/.context", created: false };
      case "reveal_context_repo":
        return undefined;

      // Install operations
      case "ensure_directory":
        return undefined;

      // PTY operations (return void/undefined)
      case "write_pty":
        return undefined;
      case "resize_pty":
        return undefined;

      // Claude session detection
      case "watch_for_claude_session":
        return undefined;
      case "cancel_session_watcher":
        return undefined;

      // Terminal/editor launch
      case "open_terminal":
        return undefined;
      case "open_editor":
        return undefined;

      // GitHub API
      case "get_authenticated_user":
        return { id: 12345, login: "test-user", name: "Test User", avatarUrl: "" };
      case "list_user_orgs":
        return [{ login: "test-org" }];
      case "check_repo_exists":
        return true;
      case "create_pull_request":
        return { number: 1, htmlUrl: "https://github.com/test-org/test-repo/pull/1", state: "open" };
      case "merge_pull_request":
        return true;
      case "get_git_credential_override":
        return [];

      // GitHub device flow (not needed for authenticated state, but safe defaults)
      case "start_device_flow":
        return { deviceCode: "mock", userCode: "MOCK-CODE", verificationUri: "https://github.com/login/device", expiresIn: 900, interval: 5 };
      case "poll_device_flow":
        return { status: "pending", interval: 5 };
      case "open_auth_url":
        return undefined;

      // SW-LD-4 removed the embedded Atlassian OAuth 3LO commands.

      // Window plugin commands (from @tauri-apps/api/window)
      case "plugin:window|show":
        return undefined;
      case "plugin:window|hide":
        return undefined;
      case "plugin:window|start_dragging":
        return undefined;
      case "plugin:window|get_all_windows":
        return ["main"];
      case "plugin:window|close":
        return undefined;
      case "plugin:window|minimize":
        return undefined;
      case "plugin:window|maximize":
        return undefined;
      case "plugin:window|set_focus":
        return undefined;

      // Webview plugin commands
      case "plugin:webview|webview_close":
        return undefined;

      // Org management — satisfy the C.2 first-launch gate by
      // returning one pre-existing workspace, which useOrgDiscovery
      // loads and populates into useOrgStore. orgCount > 0 then
      // bypasses the picker and tests land on the repo picker.
      case "discover_local_orgs":
        return [MOCK_ORG_SUMMARY];
      case "load_org_cmd":
        return MOCK_LOADED_ORG;
      case "create_workspace":
        return MOCK_LOADED_ORG;
      case "connect_to_org_cmd":
        return {
          org_slug: MOCK_ORG_SUMMARY.slug,
          org_root: MOCK_ORG_SUMMARY.org_root,
          clone_outcome: "already_existed",
          loaded_org: MOCK_LOADED_ORG
        };

      // Updater plugin — avoid real update checks
      default:
        // Safe default for any unhandled command
        return undefined;
    }
  }

  // === Main invoke handler ===
  async function invoke(cmd, args, options) {
    mockCalls.push({ cmd: cmd, args: redactedArgs(args) });

    // Handle event plugin commands
    if (cmd === "plugin:event|listen") {
      return handleListen(args);
    }
    if (cmd === "plugin:event|unlisten") {
      return handleUnlisten(args);
    }
    if (cmd === "plugin:event|emit") {
      return handleEmit(args);
    }

    return handleCommand(cmd, args);
  }

  // === Wire up internals ===
  window.__TAURI_INTERNALS__.invoke = invoke;
  window.__TAURI_INTERNALS__.transformCallback = registerCallback;
  window.__TAURI_INTERNALS__.unregisterCallback = unregisterCallback;
  window.__TAURI_INTERNALS__.runCallback = runCallback;
  window.__TAURI_INTERNALS__.callbacks = callbacks;

  // Event plugin internals
  window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener = function (event, id) {
    unregisterCallback(id);
  };

  // convertFileSrc mock (for asset protocol)
  window.__TAURI_INTERNALS__.convertFileSrc = function (filePath, protocol) {
    protocol = protocol || "asset";
    return protocol + "://localhost/" + encodeURIComponent(filePath);
  };
})();
`;
}
