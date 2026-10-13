//! The project rail: every button wears an icon, and a double-click edits
//! the project's name in place, without booting an engine.
use super::project_glyph::ProjectIcon;
use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext};

struct RailHost(Entity<Shell>);

impl Render for RailHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0.update(cx, |shell, cx| {
            shell.focus_rename_project(window, cx);
            div()
                .relative()
                .w(px(480.0))
                .h(px(800.0))
                .capture_key_down(cx.listener(Shell::on_key_down_capture))
                .child(shell.render_chat_sidebar(&Theme::default(), cx))
                .children(shell.render_icon_menu(cx))
        })
    }
}

fn space(id: &str, path: &std::path::Path) -> zeron_proto::Space {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "deviceId": "local",
        "path": path.to_string_lossy(),
        "createdAt": Utc::now(),
    }))
    .unwrap()
}

fn setup(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("alpha")).unwrap();
    std::fs::create_dir_all(dir.path().join("beta")).unwrap();
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
    let root = dir.path().to_path_buf();
    let (host, cx) = cx.add_window_view(|_, cx| {
        RailHost(cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: root.clone(),
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
                state.spaces = vec![
                    space("space-alpha", &root.join("alpha")),
                    space("space-beta", &root.join("beta")),
                ];
            });
            shell
        }))
    });
    let shell = host.read_with(cx, |host, _| host.0.clone());
    cx.update(|window, cx| {
        window.activate_window();
        window.draw(cx).clear();
    });
    (shell, cx, dir)
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
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

/// A rename that reached `mutate` reports the missing engine on the sidebar.
fn mutated(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    shell.read_with(cx, |shell, _| shell.sidebar_notice.is_some())
}

#[gpui::test]
fn every_project_wears_a_stable_default_icon(cx: &mut TestAppContext) {
    let (shell, cx, _dir) = setup(cx);
    let (first, again, other) = shell.read_with(cx, |shell, cx| {
        let rows = shell.project_rows(cx);
        assert_eq!(rows.len(), 2);
        (
            rows[0].icon(),
            shell.project_rows(cx)[0].icon(),
            rows[1].icon(),
        )
    });
    // No choice was made and the folders hold no artwork, so each project
    // wears a built-in glyph in a palette color — the same one on every
    // read, derived from its id.
    assert_eq!(first, again);
    let alpha = ProjectIcon::default_for("space-alpha").glyph_and_color("space-alpha");
    let beta = ProjectIcon::default_for("space-beta").glyph_and_color("space-beta");
    assert_eq!(first.glyph_and_color("space-alpha"), alpha);
    assert_eq!(other.glyph_and_color("space-beta"), beta);
    // Both rail buttons paint that tile.
    for (id, (glyph, color)) in [("space-alpha", alpha), ("space-beta", beta)] {
        let button = cx
            .debug_bounds(format!("project-rail-{id}").leak())
            .unwrap();
        let tile = cx
            .debug_bounds(format!("project-glyph-{glyph}-{color}").leak())
            .expect("the default glyph tile paints");
        assert!(button.contains(&tile.center()), "{id}");
    }
}

#[gpui::test]
fn double_click_edits_a_project_name_in_place(cx: &mut TestAppContext) {
    let (shell, cx, _dir) = setup(cx);
    let row = cx.debug_bounds("project-rail-space-alpha").unwrap();
    assert!(cx.debug_bounds("project-rail-name-space-alpha").is_some());
    double_click(cx, row.center());

    // The name swaps for a focused field holding the whole name.
    assert!(cx.debug_bounds("project-rail-name-space-alpha").is_none());
    let field = cx.debug_bounds("project-rail-editor-space-alpha").unwrap();
    assert!(row.contains(&field.center()));
    let input = shell.read_with(cx, |shell, _| {
        let rename = shell.project.rename.as_ref().unwrap();
        assert_eq!(rename.space_id, "space-alpha");
        rename.input.clone()
    });
    cx.update(|window, cx| {
        assert!(input.focus_handle(cx).is_focused(window));
        assert_eq!(input.read(cx).text(), "alpha");
    });

    // Escape drops the edit without renaming.
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(shell.read_with(cx, |shell, _| shell.project.rename.is_none()));
    assert!(cx.debug_bounds("project-rail-name-space-alpha").is_some());
    assert!(!mutated(&shell, cx));

    // Enter on an unchanged name closes without a mutation.
    double_click(cx, row.center());
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert!(shell.read_with(cx, |shell, _| shell.project.rename.is_none()));
    assert!(!mutated(&shell, cx));

    // Typing replaces the selected name; Enter commits it.
    double_click(cx, row.center());
    cx.simulate_input("Renamed");
    let input = shell.read_with(cx, |shell, _| {
        shell.project.rename.as_ref().unwrap().input.clone()
    });
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_string()),
        "Renamed"
    );
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert!(shell.read_with(cx, |shell, _| shell.project.rename.is_none()));
    assert!(mutated(&shell, cx));
}

#[gpui::test]
fn the_context_menu_picks_a_glyph_or_a_color(cx: &mut TestAppContext) {
    let (shell, cx, _dir) = setup(cx);
    let row = cx.debug_bounds("project-rail-space-alpha").unwrap();
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Right,
        position: row.center(),
        click_count: 1,
        ..Default::default()
    });
    redraw(cx);
    assert!(
        shell.read_with(cx, |shell, _| shell.project.icon_for.as_deref()
            == Some("space-alpha"))
    );
    assert!(cx.debug_bounds("project-icon-menu").is_some());
    // Every built-in glyph and color is a pick.
    for (glyph, _) in super::project_glyph::GLYPHS {
        let selector: &'static str = format!("icon-menu-glyph-{glyph}").leak();
        assert!(cx.debug_bounds(selector).is_some(), "{glyph}");
    }
    for (color, ..) in super::project_glyph::COLORS {
        let selector: &'static str = format!("icon-menu-color-{color}").leak();
        assert!(cx.debug_bounds(selector).is_some(), "{color}");
    }
    assert!(cx.debug_bounds("icon-menu-rename").is_some());

    // Picking a glyph stores it with the project (the engine call fails
    // here, which the sidebar reports) and keeps the menu open for a color.
    let glyph = cx.debug_bounds("icon-menu-glyph-terminal").unwrap();
    cx.simulate_click(glyph.center(), gpui::Modifiers::default());
    redraw(cx);
    assert!(mutated(&shell, cx));
    assert!(shell.read_with(cx, |shell, _| shell.project.icon_for.is_some()));

    // A click outside the menu closes it.
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: gpui::point(px(470.0), px(790.0)),
        click_count: 1,
        ..Default::default()
    });
    redraw(cx);
    assert!(shell.read_with(cx, |shell, _| shell.project.icon_for.is_none()));
    assert!(cx.debug_bounds("project-icon-menu").is_none());

    // Rename from the menu opens the same field as a double-click.
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Right,
        position: row.center(),
        click_count: 1,
        ..Default::default()
    });
    redraw(cx);
    let rename = cx.debug_bounds("icon-menu-rename").unwrap();
    cx.simulate_click(rename.center(), gpui::Modifiers::default());
    redraw(cx);
    assert!(shell.read_with(cx, |shell, _| shell.project.icon_for.is_none()));
    assert!(cx.debug_bounds("project-rail-editor-space-alpha").is_some());
}
