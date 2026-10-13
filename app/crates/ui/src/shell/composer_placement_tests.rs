//! Where the message bar lands in a chat that has no messages yet. A selected
//! chat keeps it at the foot of the column from its first frame: before its
//! history arrives, after an empty replay, on a `+` chat, and on a project
//! switch. The placeholder is painted only by the bar's input, so the input
//! staying inside a pill at the foot means nothing reads "Do anything…" above
//! the message area.
use super::*;
use gpui::{AppContext, Bounds, TestAppContext, VisualTestContext};
use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

const WINDOW_WIDTH: f32 = project_chrome::RAIL_WIDTH + 1200.0;
const WINDOW_HEIGHT: f32 = 900.0;
/// Long enough for the dock route's 470ms glide to settle twice over.
const SETTLE_MS: u64 = 1200;
const FRAME_MS: u64 = 25;

fn space(id: &str, path: &std::path::Path) -> zeron_proto::Space {
    serde_json::from_value(serde_json::json!({
        "id": id, "deviceId": "local", "path": path.display().to_string(),
        "createdAt": "2026-09-20T00:00:00Z",
    }))
    .unwrap()
}

fn chat(id: &str, space_id: &str, minutes_ago: i64) -> zeron_proto::Chat {
    serde_json::from_value(serde_json::json!({
        "id": id, "title": id, "deviceId": "local", "archived": false,
        "spaceId": space_id,
        "createdAt": Utc::now() - chrono::Duration::minutes(minutes_ago),
    }))
    .unwrap()
}

fn user_message(text: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "m1".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "t1".into(),
            text: text.into(),
        }],
        created_at: 0,
        device_id: "local".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        duration_ms: None,
    }
}

fn setup<'a>(
    cx: &'a mut TestAppContext,
    selected: &str,
) -> (tempfile::TempDir, Entity<Shell>, &'a mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(UiSettings::default(), dir.path(), cx);
    });
    for project in ["p1", "p2"] {
        std::fs::create_dir_all(dir.path().join(project)).unwrap();
    }
    let selected = selected.to_owned();
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.workspace_scope = Some(WorkspaceScope::Local);
            state.connection = ConnectionStatus::Ready;
            state.local_device_id = Some("local".into());
            state.spaces = vec![
                space("p1", &dir.path().join("p1")),
                space("p2", &dir.path().join("p2")),
            ];
            state.chats = vec![
                chat("p1-main", "p1", 30),
                chat("p1-new", "p1", 0),
                chat("p2-main", "p2", 20),
            ];
            state.selected_space = Some("p1".into());
            state.selected_chat = Some(selected.clone());
            state
        });
        let mut shell = Shell::new(
            state,
            EngineBootConfig {
                data_dir: dir.path().into(),
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: zeron_proto::HarnessId::Mock,
            },
            cx,
        );
        // This fixture has no engine. Keep state notifications from
        // invoking the shell's disconnect cleanup.
        shell._state_observation = cx.observe(&shell.state, |_, _, _| {});
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        shell.active_chat = selected;
        shell.test_time = Some(std::time::Instant::now());
        shell
    });
    cx.simulate_resize(gpui::size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
    (dir, shell, cx)
}

struct Placement {
    pill: Bounds<Pixels>,
    input: Bounds<Pixels>,
}

impl Placement {
    /// The pill sits in the bottom stack and the placeholder's input sits
    /// inside it, so nothing of the bar paints up the column.
    fn assert_at_foot(&self, when: &str) {
        let pill_bottom = f32::from(self.pill.bottom());
        let pill_top = f32::from(self.pill.top());
        assert!(
            pill_bottom > WINDOW_HEIGHT - 60.0 && pill_top > WINDOW_HEIGHT * 0.75,
            "{when}: the composer left the foot of the column: {:?}",
            self.pill
        );
        assert!(
            self.input.top() >= self.pill.top()
                && self.input.bottom() <= self.pill.bottom()
                && self.input.left() >= self.pill.left()
                && self.input.right() <= self.pill.right(),
            "{when}: the placeholder's input left the composer pill: input {:?}, pill {:?}",
            self.input,
            self.pill
        );
    }
}

fn draw(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Placement {
    cx.update(|window, cx| window.draw(cx).clear());
    cx.update(|_, cx| {
        let composer = shell.read(cx).composer.read(cx);
        Placement {
            pill: composer.surface_bounds().get().expect("pill painted"),
            input: composer
                .input
                .read(cx)
                .painted_bounds()
                .expect("input painted"),
        }
    })
}

fn advance(shell: &Entity<Shell>, cx: &mut VisualTestContext, millis: u64) {
    shell.update(cx, |shell, cx| {
        shell.test_time = shell
            .test_time
            .map(|t| t + std::time::Duration::from_millis(millis));
        cx.notify();
    });
}

/// Draw frames across the route clock; the bar must stay at the foot on each.
fn settle_at_foot(shell: &Entity<Shell>, cx: &mut VisualTestContext, when: &str) {
    for elapsed in (0..=SETTLE_MS).step_by(FRAME_MS as usize) {
        draw(shell, cx).assert_at_foot(&format!("{when}, {elapsed}ms in"));
        advance(shell, cx, FRAME_MS);
    }
}

fn replay(shell: &Entity<Shell>, cx: &mut VisualTestContext, entries: Vec<SessionMessageEntry>) {
    shell.update(cx, |shell, cx| {
        shell
            .state
            .update(cx, |state, _| state.apply_transcript(entries));
        cx.notify();
    });
}

/// Restore at boot: the remembered chat is selected before any history
/// arrives, and the replay then says it is empty.
#[gpui::test]
fn an_empty_chat_keeps_the_composer_at_the_foot_from_the_first_frame(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx, "p1-new");
    draw(&shell, cx).assert_at_foot("first frame, history pending");
    replay(&shell, cx, Vec::new());
    settle_at_foot(&shell, cx, "after the empty replay");
}

/// `+` on the tab strip: a chat that already shows messages hands over to a
/// brand-new one whose history is pending, then arrives empty.
#[gpui::test]
fn a_new_chat_from_plus_keeps_the_composer_at_the_foot(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx, "p1-main");
    replay(&shell, cx, vec![user_message("hello")]);
    settle_at_foot(&shell, cx, "the chat with messages");
    shell.update(cx, |shell, cx| shell.open_project_chat(cx));
    shell.read_with(cx, |shell, cx| {
        let state = shell.state.read(cx);
        assert!(
            state
                .selected_chat
                .as_deref()
                .is_some_and(|id| id != "p1-main")
        );
        assert!(state.transcript.is_empty() && !state.transcript_replayed);
    });
    settle_at_foot(&shell, cx, "the new chat, history pending");
    replay(&shell, cx, Vec::new());
    settle_at_foot(&shell, cx, "the new chat after its empty replay");
}

/// A project switch lands on that project's Main chat, which may be empty.
#[gpui::test]
fn a_project_switch_onto_an_empty_main_chat_keeps_the_composer_at_the_foot(
    cx: &mut TestAppContext,
) {
    let (_dir, shell, cx) = setup(cx, "p1-main");
    replay(&shell, cx, vec![user_message("hello")]);
    settle_at_foot(&shell, cx, "the first project");
    shell.update(cx, |shell, cx| shell.open_project("p2", cx));
    shell.read_with(cx, |shell, cx| {
        assert_eq!(
            shell.state.read(cx).selected_chat.as_deref(),
            Some("p2-main")
        );
    });
    settle_at_foot(&shell, cx, "the second project, history pending");
    replay(&shell, cx, Vec::new());
    settle_at_foot(&shell, cx, "the second project after its empty replay");
}
