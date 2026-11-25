//! Human summaries and categories for Claude Code's built-in tools.

use serde_json::Value;
use std::path::PathBuf;
use styr_core::event::{PlanEntry, PlanStatus, ToolInfo, ToolKind};

const TITLE_MAX: usize = 120;

fn short(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= TITLE_MAX {
        one_line
    } else {
        format!(
            "{}…",
            one_line.chars().take(TITLE_MAX - 1).collect::<String>()
        )
    }
}

fn field<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
}

pub fn tool_info(name: &str, input: Value) -> ToolInfo {
    let path = field(&input, &["file_path", "notebook_path", "path"]);
    let locations: Vec<PathBuf> = path.map(PathBuf::from).into_iter().collect();
    let (kind, title) = match name {
        "Bash" => (
            ToolKind::Execute,
            format!("Run `{}`", short(field(&input, &["command"]).unwrap_or(""))),
        ),
        "BashOutput" | "KillShell" | "KillBash" => (ToolKind::Execute, name.to_owned()),
        "Read" => (ToolKind::Read, format!("Read {}", path.unwrap_or("file"))),
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            (ToolKind::Edit, format!("Edit {}", path.unwrap_or("file")))
        }
        "Write" => (ToolKind::Edit, format!("Write {}", path.unwrap_or("file"))),
        "Grep" => (
            ToolKind::Search,
            format!(
                "Search for `{}`",
                short(field(&input, &["pattern"]).unwrap_or(""))
            ),
        ),
        "Glob" => (
            ToolKind::Search,
            format!(
                "Find files `{}`",
                short(field(&input, &["pattern"]).unwrap_or(""))
            ),
        ),
        "WebFetch" => (
            ToolKind::Fetch,
            format!("Fetch {}", short(field(&input, &["url"]).unwrap_or(""))),
        ),
        "WebSearch" => (
            ToolKind::Fetch,
            format!(
                "Search the web for “{}”",
                short(field(&input, &["query"]).unwrap_or(""))
            ),
        ),
        "Task" | "Agent" => (
            ToolKind::Think,
            format!(
                "Agent: {}",
                short(field(&input, &["description", "prompt"]).unwrap_or("subtask"))
            ),
        ),
        "TodoWrite" => (ToolKind::Think, "Update plan".to_owned()),
        "ExitPlanMode" => (ToolKind::Think, "Propose plan".to_owned()),
        n if n.starts_with("mcp__") => {
            let mut parts = n.splitn(3, "__").skip(1);
            let server = parts.next().unwrap_or("mcp");
            let tool = parts.next().unwrap_or("tool");
            (ToolKind::Mcp, format!("{server}: {tool}"))
        }
        other => (ToolKind::Other, other.to_owned()),
    };
    ToolInfo {
        name: name.to_owned(),
        kind,
        title,
        input,
        locations,
    }
}

/// TodoWrite's `todos` → plan entries.
pub fn todo_plan(input: &Value) -> Option<Vec<PlanEntry>> {
    let todos = input.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|t| {
                let content = t.get("content").and_then(Value::as_str)?.to_owned();
                let status = match t.get("status").and_then(Value::as_str) {
                    Some("in_progress") => PlanStatus::InProgress,
                    Some("completed") => PlanStatus::Completed,
                    _ => PlanStatus::Pending,
                };
                Some(PlanEntry { content, status })
            })
            .collect(),
    )
}
