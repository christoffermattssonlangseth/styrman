//! Styrman adapter stub: Codex CLI via codex app-server — planned (phase 3).
//!
//! Only the static parts are filled in so the daemon can register it and the
//! launcher can exec the agent; see `docs/design.md` §4 for the integration plan.

use styr_core::adapter::*;
use styr_core::ids::AgentKind;

#[derive(Debug, Clone, Default)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt_delivery: PromptDelivery::Anytime,
            permissions: PermissionMode::Concurrent,
            streaming_text: false,
            interrupt: false,
        }
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "codex".into(),
            extra_args: vec![],
            env: vec![
                ("STYR_LAUNCHED".into(), "1".into()),
                ("STYR_WRAPPED".into(), "1".into()),
            ],
        }
    }
}
