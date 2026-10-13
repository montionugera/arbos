//! A new worktree's setup action: the engine starts it when the checkout is
//! created, and the shell shows its terminal in the chat's drawer.
use super::*;

use zeron_proto::ProjectActionRun;

impl Shell {
    pub(super) fn attach_worktree_setup(
        &mut self,
        chat_id: String,
        setup_action: Option<ProjectActionRun>,
        setup_error: Option<String>,
        target_device_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(error) = setup_error {
            self.show_notice(crate::toast::ToastKind::Error, format!("Setup action failed: {error}"), cx);
        }
        let Some(run) = setup_action else {
            cx.notify();
            return;
        };

        let panel = self.terminal_panel(cx);
        let title = format!("{} (setup)", run.action_name);
        let tab = panel.update(cx, |panel, cx| {
            panel.reserve_tab_for_chat(chat_id.clone(), title, cx)
        });
        let attached = panel.update(cx, |panel, cx| {
            panel.attach_reserved_session(&chat_id, tab, run.terminal, target_device_id, cx)
        });
        if !attached {
            self.show_notice(crate::toast::ToastKind::Error, "Setup action started, but its terminal could not be attached", cx);
        }

        let selected = self.active_chat == chat_id;
        self.panels
            .update(&chat_id, |panels| panels.terminal_open = true);
        if selected {
            self.terminal_tween = None;
            self.terminal_tween_task = None;
            panel.update(cx, |panel, cx| panel.set_open(true, cx));
            panel.update(cx, |panel, cx| panel.select_tab_by_key(tab, cx));
        }
        cx.notify();
    }
}
