//! The titlebar's chat tab strip, painted without booting an engine: every
//! tab of the project shows, Main leads with the project's icon, one `+`
//! ends the strip, and a double-click renames a tab in place.
use super::project_chrome::{TAB_MAX_WIDTH, TAB_MIN_WIDTH};
use super::*;
use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};

const STRIP_WIDTH: f32 = 1400.0;

struct TabsHost {
    shell: Entity<Shell>,
    _data_dir: tempfile::TempDir,
}

impl Render for TabsHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell.update(cx, |shell, cx| {
            shell.focus_rename_chat(window, cx);
            div()
                .w(px(STRIP_WIDTH))
                .h(px(Theme::TITLEBAR_HEIGHT))
                .relative()
                .capture_key_down(cx.listener(Shell::on_key_down_capture))
                .child(shell.render_titlebar_cluster(cx))
                .child(
                    div()
                        .size_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .pl(px(120.0))
                        .child(shell.render_project_tabs(cx)),
                )
        })
    }
}

/// `count` chats on one project: Main, then `chat-01`… newest first.
fn setup(cx: &mut TestAppContext, count: usize) -> (Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        apply_keymap(
            cx,
            &KeymapConfig::default(),
            ComposerSendBehavior::default(),
        );
    });
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let mut shell = Shell::new(
                cx.new(|_| AppState::new()),
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.reduced_motion = true;
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.spaces = vec![serde_json::from_value(serde_json::json!({
                    "id": "space", "deviceId": "local", "path": project.to_string_lossy(),
                    "name": "Arbos", "createdAt": Utc::now(),
                }))
                .unwrap()];
                state.selected_space = Some("space".into());
                let mut ids = vec!["space-main".to_owned()];
                ids.extend((1..count).map(|ix| format!("chat-{ix:02}")));
                state.chats = ids
                    .iter()
                    .enumerate()
                    .map(|(ix, id)| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "title": id, "deviceId": "local", "archived": false,
                            "spaceId": "space",
                            "createdAt": Utc::now() - chrono::Duration::minutes(60 - ix as i64),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.selected_chat = Some("space-main".into());
            });
            shell.active_chat = "space-main".into();
            shell
                .composer
                .update(cx, |composer, _| composer.focus_pending = false);
            shell
        });
        TabsHost {
            shell,
            _data_dir: dir,
        }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    cx.update(|window, cx| {
        window.activate_window();
        window.draw(cx).clear();
    });
    (shell, cx)
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

fn bounds(cx: &mut VisualTestContext, id: String) -> Option<gpui::Bounds<Pixels>> {
    cx.debug_bounds(Box::leak(id.into_boxed_str()))
}

fn tab(cx: &mut VisualTestContext, chat_id: &str) -> Option<gpui::Bounds<Pixels>> {
    bounds(cx, format!("project-tab-{chat_id}"))
}

fn double_click(cx: &mut VisualTestContext, position: Point<Pixels>) {
    for click_count in [1, 2] {
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            click_count,
            ..Default::default()
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            click_count,
            ..Default::default()
        });
    }
    redraw(cx);
}

fn selected(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
}

fn renaming(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, _| {
        shell
            .chat_rename
            .as_ref()
            .map(|rename| rename.chat_id.clone())
    })
}

#[gpui::test]
fn every_tab_shows_main_first_and_each_opens_on_click(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx, 19);
    let strip = cx.debug_bounds("project-tab-scroller").unwrap();

    // Main leads; the other eighteen follow newest first, all painted and
    // all inside the strip: nineteen tabs at the floor width fit a 1400px
    // bar.
    let main = tab(cx, "space-main").expect("Main has a tab");
    let mut previous = main;
    for ix in (1..19).rev() {
        let id = format!("chat-{ix:02}");
        let this = tab(cx, &id).unwrap_or_else(|| panic!("{id} has a tab"));
        assert!(this.left() >= previous.right(), "{id} follows its neighbour");
        assert!(
            this.size.width >= px(TAB_MIN_WIDTH) && this.size.width <= px(TAB_MAX_WIDTH),
            "{id} is {:?} wide",
            this.size.width
        );
        assert!(strip.contains(&this.center()), "{id} is on screen");
        previous = this;
    }
    assert!(
        main.size.width < px(TAB_MAX_WIDTH),
        "tabs shrank to make room instead of hiding"
    );
    assert!(cx.debug_bounds("project-tab-overflow").is_none());

    // Every tab is reachable with one click, including the last.
    for ix in [1, 9, 18] {
        let id = format!("chat-{ix:02}");
        let center = tab(cx, &id).unwrap().center();
        cx.simulate_click(center, Modifiers::default());
        redraw(cx);
        assert_eq!(selected(&shell, cx).as_deref(), Some(id.as_str()));
    }
}

#[gpui::test]
fn the_strip_scrolls_only_once_the_tabs_cannot_shrink_further(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx, 40);
    let strip = cx.debug_bounds("project-tab-scroller").unwrap();
    // The oldest chat is the last tab.
    let last = tab(cx, "chat-01").expect("an off-screen tab is still laid out");
    assert_eq!(last.size.width, px(TAB_MIN_WIDTH));
    assert!(last.left() > strip.right(), "forty tabs overflow the bar");

    // Opening the last tab scrolls it into view.
    shell.update(cx, |shell, cx| shell.open_chat("chat-01".into(), cx));
    redraw(cx);
    redraw(cx);
    let last = tab(cx, "chat-01").unwrap();
    assert!(
        strip.contains(&last.center()),
        "the selected tab is scrolled into view: {last:?} in {strip:?}"
    );
}

#[gpui::test]
fn one_plus_and_no_history_arrows(cx: &mut TestAppContext) {
    let (_shell, cx) = setup(cx, 5);
    assert!(cx.debug_bounds("toggle-sidebar").is_some());
    assert!(cx.debug_bounds("nav-back").is_none(), "no back arrow");
    assert!(cx.debug_bounds("nav-forward").is_none(), "no forward arrow");
    assert!(
        cx.debug_bounds("titlebar-new-session").is_none(),
        "no `+` in the left control cluster"
    );
    let plus = cx.debug_bounds("project-tab-new").expect("the strip's `+`");
    let last = tab(cx, "chat-01").unwrap();
    assert!(plus.left() >= last.right(), "the `+` ends the strip");
}

#[gpui::test]
fn main_wears_the_project_icon(cx: &mut TestAppContext) {
    let (_shell, cx) = setup(cx, 3);
    let main = tab(cx, "space-main").unwrap();
    // Main is older than every other chat here, yet still leads.
    for id in ["chat-01", "chat-02"] {
        let other = tab(cx, id).unwrap();
        assert!(main.right() <= other.left(), "Main is pinned first");
    }
    // No artwork on disk yet: the icon is the project's initial, the same
    // fallback the rail draws, inside Main's tab.
    let title = bounds(cx, "project-tab-title-space-main".into()).unwrap();
    assert!(
        title.left() >= main.left() + px(10.0 + 16.0),
        "the title sits after a 16px icon slot: {title:?} in {main:?}"
    );
}

#[gpui::test]
fn double_click_renames_a_tab_in_place(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx, 4);
    let row = tab(cx, "chat-02").unwrap();
    double_click(cx, row.center());

    // The label swaps for a focused field holding the whole name, selected.
    assert_eq!(renaming(&shell, cx).as_deref(), Some("chat-02"));
    assert!(bounds(cx, "project-tab-title-chat-02".into()).is_none());
    let field = bounds(cx, "project-tab-editor-chat-02".into()).expect("the inline field");
    let tab_now = tab(cx, "chat-02").unwrap();
    assert!(tab_now.contains(&field.center()));
    let input = shell.read_with(cx, |shell, _| {
        let rename = shell.chat_rename.as_ref().unwrap();
        assert_eq!(rename.surface, ChatRenameSurface::Tab);
        rename.input.clone()
    });
    cx.update(|window, cx| {
        assert!(input.focus_handle(cx).is_focused(window));
        assert_eq!(input.read(cx).text(), "chat-02");
    });

    // Escape drops the edit: the label is back and nothing was sent.
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert_eq!(renaming(&shell, cx), None);
    assert!(bounds(cx, "project-tab-title-chat-02".into()).is_some());
    assert!(shell.read_with(cx, |shell, _| shell.sidebar_notice.is_none()));

    // Typing replaces the selected name; Enter commits it as the chat's
    // title (a `renameChat` mutation — which, with no engine here, reports
    // the missing connection).
    let center = tab(cx, "chat-02").unwrap().center();
    double_click(cx, center);
    cx.simulate_input("Release notes");
    let input = shell.read_with(cx, |shell, _| {
        shell.chat_rename.as_ref().unwrap().input.clone()
    });
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_string()),
        "Release notes"
    );
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert_eq!(renaming(&shell, cx), None);
    assert!(shell.read_with(cx, |shell, _| shell.sidebar_notice.is_some()));
}

#[gpui::test]
fn rename_command_targets_the_tab(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx, 3);
    shell.update(cx, |shell, cx| shell.open_rename_chat("chat-01".into(), cx));
    redraw(cx);
    shell.read_with(cx, |shell, _| {
        let rename = shell.chat_rename.as_ref().expect("the rename opened");
        assert_eq!(rename.chat_id, "chat-01");
        assert_eq!(rename.surface, ChatRenameSurface::Tab);
    });
    assert!(bounds(cx, "project-tab-editor-chat-01".into()).is_some());
}
