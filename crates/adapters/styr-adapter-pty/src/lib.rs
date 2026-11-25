//! Styrman adapter stub: generic PTY fallback for agents without hooks or a protocol — planned.
//!
//! Only the static parts are filled in so the daemon can register it and the
//! launcher can exec the agent; see `docs/design.md` §4 for the integration plan.

use styr_core::adapter::*;
use styr_core::ids::AgentKind;

#[derive(Debug, Clone, Default)]
pub struct PtyAdapter;

impl AgentAdapter for PtyAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Pty
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt_delivery: PromptDelivery::Anytime,
            permissions: PermissionMode::None,
            streaming_text: false,
            interrupt: false,
        }
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "sh".into(),
            extra_args: vec![],
            env: vec![("STYR_LAUNCHED".into(), "1".into())],
        }
    }
}
