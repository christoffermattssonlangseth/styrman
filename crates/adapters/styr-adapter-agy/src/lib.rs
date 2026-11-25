//! Styrman adapter stub: antigravity CLI (agy): hooks, plugin and ACP — planned (phase 2).
//!
//! Only the static parts are filled in so the daemon can register it and the
//! launcher can exec the agent; see `docs/design.md` §4 for the integration plan.

use styr_core::adapter::*;
use styr_core::ids::AgentKind;

#[derive(Debug, Clone, Default)]
pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Antigravity
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt_delivery: PromptDelivery::WhenIdle,
            permissions: PermissionMode::HookBlocking,
            streaming_text: false,
            interrupt: false,
        }
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "agy".into(),
            extra_args: vec![],
            env: vec![("STYR_LAUNCHED".into(), "1".into())],
        }
    }
}
