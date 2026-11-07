// SPDX-License-Identifier: MIT
use std::path::Path;

use log::{error, info};
use serde::Serialize;

use crate::commands::async_helpers::await_blocking;

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectedTools {
    pub terminals: Vec<Tool>,
    pub editors: Vec<Tool>,
    pub default_terminal: String,
    pub default_editor: String,
}

fn app_exists(name: &str) -> bool {
    Path::new(&format!("/Applications/{}.app", name)).exists()
}

#[tauri::command]
pub async fn detect_tools() -> Result<DetectedTools, String> {
    await_blocking("detect_tools", detect_tools_impl).await
}

fn detect_tools_impl() -> Result<DetectedTools, String> {
    let mut terminals = Vec::new();
    let mut editors = Vec::new();

    // Detect terminals
    if app_exists("Ghostty") {
        terminals.push(Tool {
            id: "ghostty".into(),
            label: "Ghostty".into(),
        });
    }
    if app_exists("iTerm") {
        terminals.push(Tool {
            id: "iterm2".into(),
            label: "iTerm2".into(),
        });
    }
    // Terminal.app always exists on macOS
    terminals.push(Tool {
        id: "terminal".into(),
        label: "Terminal".into(),
    });

    // Detect editors
    if app_exists("Cursor") {
        editors.push(Tool {
            id: "cursor".into(),
            label: "Cursor".into(),
        });
    }

    // Default terminal: prefer $TERM_PROGRAM, then first detected
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    let default_terminal =
        if term_program.contains("ghostty") && terminals.iter().any(|t| t.id == "ghostty") {
            "ghostty".into()
        } else if term_program.contains("iterm") && terminals.iter().any(|t| t.id == "iterm2") {
            "iterm2".into()
        } else {
            terminals
                .first()
                .map(|t| t.id.clone())
                .unwrap_or("terminal".into())
        };

    // Default editor: first detected
    let default_editor = editors.first().map(|e| e.id.clone()).unwrap_or_default();

    let terminal_names: Vec<&str> = terminals.iter().map(|t| t.id.as_str()).collect();
    let editor_names: Vec<&str> = editors.iter().map(|e| e.id.as_str()).collect();
    info!(target: "tools", "detect_tools: terminals={:?}, editors={:?}", terminal_names, editor_names);

    Ok(DetectedTools {
        terminals,
        editors,
        default_terminal,
        default_editor,
    })
}

#[tauri::command]
pub async fn open_terminal(
    terminal: String,
    custom_cmd: String,
    repo_path: String,
    run_command: String,
) -> Result<(), String> {
    info!(target: "tools", "open_terminal: terminal={}, repo_path={}", terminal, repo_path);
    await_blocking("open_terminal", move || {
        open_terminal_impl(terminal, custom_cmd, repo_path, run_command)
    })
    .await
}

fn open_terminal_impl(
    terminal: String,
    custom_cmd: String,
    repo_path: String,
    run_command: String,
) -> Result<(), String> {
    // Build the command to run in the terminal (cd to repo, optionally run a command)
    let shell_cmd = if run_command.is_empty() {
        format!("cd {} && exec $SHELL", repo_path)
    } else {
        format!("cd {} && {}", repo_path, run_command)
    };

    let result = match terminal.as_str() {
        "ghostty" => {
            let ghostty_bin = "/Applications/Ghostty.app/Contents/MacOS/ghostty";
            // Copy command to clipboard if provided, so user can just paste
            if !run_command.is_empty() {
                let _ = std::process::Command::new("pbcopy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        if let Some(stdin) = child.stdin.as_mut() {
                            stdin.write_all(run_command.as_bytes())?;
                        }
                        child.wait()
                    });
            }
            std::process::Command::new(ghostty_bin)
                .arg(format!("--working-directory={}", repo_path))
                .spawn()
        }
        "iterm2" => std::process::Command::new("osascript")
            .args([
                "-e",
                &format!(
                    r#"tell application "iTerm2"
                        create window with default profile
                        tell current session of current window
                            write text "{}"
                        end tell
                    end tell"#,
                    shell_cmd
                ),
            ])
            .spawn(),
        "terminal" => std::process::Command::new("osascript")
            .args([
                "-e",
                &format!(
                    r#"tell application "Terminal"
                        do script "{}"
                        activate
                    end tell"#,
                    shell_cmd
                ),
            ])
            .spawn(),
        "custom" => {
            if custom_cmd.is_empty() {
                return Err("No custom terminal command configured".into());
            }
            std::process::Command::new(&custom_cmd)
                .arg(&repo_path)
                .spawn()
        }
        _ => return Err(format!("Unknown terminal: {}", terminal)),
    };

    result.map(|_| ()).map_err(|e| {
        error!(target: "tools", "open_terminal: spawn failed for {}: {}", terminal, e);
        format!("Failed to open terminal: {}", e)
    })
}

#[tauri::command]
pub async fn open_editor(
    editor: String,
    custom_cmd: String,
    repo_path: String,
) -> Result<(), String> {
    info!(target: "tools", "open_editor: editor={}, repo_path={}", editor, repo_path);
    await_blocking("open_editor", move || {
        open_editor_impl(editor, custom_cmd, repo_path)
    })
    .await
}

fn open_editor_impl(editor: String, custom_cmd: String, repo_path: String) -> Result<(), String> {
    let result = match editor.as_str() {
        "cursor" => std::process::Command::new("open")
            .args(["-a", "Cursor", &repo_path])
            .spawn(),
        "custom" => {
            if custom_cmd.is_empty() {
                return Err("No custom editor command configured".into());
            }
            std::process::Command::new(&custom_cmd)
                .arg(&repo_path)
                .spawn()
        }
        _ => return Err(format!("Unknown editor: {}", editor)),
    };

    result.map(|_| ()).map_err(|e| {
        error!(target: "tools", "open_editor: spawn failed for {}: {}", editor, e);
        format!("Failed to open editor: {}", e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_tools_always_has_terminal() {
        let tools = detect_tools_impl().unwrap();
        assert!(
            !tools.terminals.is_empty(),
            "Should always have at least Terminal.app"
        );
        assert!(tools.terminals.iter().any(|t| t.id == "terminal"));
    }

    #[test]
    fn test_default_terminal_is_set() {
        let tools = detect_tools_impl().unwrap();
        assert!(!tools.default_terminal.is_empty());
    }
}
