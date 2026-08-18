//! The application shell: sidebar, content area, status bar.
//!
//! Replaces `shell/AppSidebar.svelte`, `StatusBar.svelte` and `MainView.svelte`.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, Window, div, px, relative,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
    separator::Separator,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    status_bar::StatusBar,
    v_flex,
};
use rw_core::domain::RequestKind;

use crate::tabs::Tabs;
use crate::theme;
use crate::workspace::{RobotWhisperer, Workspace};

/// Root view of the application window.
pub struct Shell {
    workspace: Entity<Workspace>,
    tabs: Tabs,
    filter: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = RobotWhisperer::global(cx).workspace.clone();
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter requests…"));

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];

        workspace
            .update(cx, |workspace, cx| workspace.load(cx))
            .detach();

        Self {
            workspace,
            tabs: Tabs::default(),
            filter,
            _subscriptions: subscriptions,
        }
    }

    fn new_request(&mut self, cx: &mut Context<Self>) {
        let creating = self
            .workspace
            .update(cx, |workspace, cx| workspace.create_request(cx));

        cx.spawn(async move |shell, cx| {
            let Some(request) = creating.await else {
                return;
            };
            shell
                .update(cx, |shell, cx| {
                    shell.tabs.open_request(&request);
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn open_request(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(request) = self.workspace.read(cx).request(id).cloned() else {
            return;
        };
        self.tabs.open_request(&request);
        cx.notify();
    }

    /// Requests matching the filter box, as sidebar menu items.
    fn request_items(&self, cx: &mut Context<Self>) -> Vec<SidebarMenuItem> {
        let query = self.filter.read(cx).value().trim().to_lowercase();

        self.workspace
            .read(cx)
            .requests()
            .iter()
            .filter(|request| query.is_empty() || request.name.to_lowercase().contains(&query))
            .map(|request| {
                let id = request.id;
                SidebarMenuItem::new(request.name.clone())
                    .icon(kind_icon(request.kind))
                    .active(self.tabs.is_active(&format!("request:{id}")))
                    .on_click(cx.listener(move |shell, _: &ClickEvent, _, cx| {
                        shell.open_request(id, cx);
                    }))
            })
            .collect()
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let items = self.request_items(cx);
        let muted = cx.theme().muted_foreground;

        Sidebar::new("shell-sidebar")
            .w(relative(1.))
            .border_0()
            .header(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        SidebarHeader::new().child(
                            v_flex()
                                .text_sm()
                                .child("Robot Whisperer")
                                .child(div().text_xs().text_color(muted).child("Workspace")),
                        ),
                    )
                    .child(Input::new(&self.filter).cleanable(true)),
            )
            .child(SidebarGroup::new("Requests").child(SidebarMenu::new().children(items)))
            .footer(
                Button::new("new-request")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("New request")
                    .on_click(cx.listener(|shell, _: &ClickEvent, _, cx| shell.new_request(cx))),
            )
            .into_any_element()
    }

    fn content(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;

        match self.tabs.active() {
            Some(tab) => v_flex()
                .size_full()
                .p_5()
                .gap_2()
                .child(div().text_2xl().font_semibold().child(tab.title.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("The request editor arrives in the next milestone."),
                )
                .into_any_element(),

            None => {
                let loaded = self.workspace.read(cx).loaded();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Icon::new(IconName::Inbox).size_8().text_color(muted))
                    .child(div().text_lg().font_semibold().child(if loaded {
                        "No request open"
                    } else {
                        "Loading workspace…"
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("Create a request from the sidebar to get started."),
                    )
                    .into_any_element()
            }
        }
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let danger = cx.theme().danger;
        let workspace = self.workspace.read(cx);
        let connected = workspace.connected().count();
        let sessions = workspace.sessions().len();
        let requests = workspace.requests().len();
        let error = workspace.error().map(str::to_owned);

        StatusBar::new()
            .child(Icon::new(IconName::Globe).xsmall())
            .child(format!("{connected}/{sessions} connected"))
            .child(Separator::vertical())
            .child(format!("{requests} requests"))
            .when_some(error, |this, error| {
                this.child(Separator::vertical())
                    .child(div().text_color(danger).child(error))
            })
            .right(theme::current(cx))
            .right(format!("v{}", env!("CARGO_PKG_VERSION")))
            .into_any_element()
    }
}

/// Icon standing in for the old `TypeBadge` colour coding.
fn kind_icon(kind: RequestKind) -> IconName {
    match kind {
        RequestKind::Topic => IconName::Inbox,
        RequestKind::Service => IconName::Bot,
        RequestKind::Action => IconName::Star,
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.sidebar(cx);
        let content = self.content(cx);
        let status_bar = self.status_bar(cx);

        v_flex()
            .size_full()
            .child(
                h_flex().flex_1().min_h_0().child(
                    h_resizable("shell-split")
                        .child(
                            resizable_panel()
                                .size(px(260.))
                                .size_range(px(200.)..px(400.))
                                .child(sidebar),
                        )
                        .child(resizable_panel().child(content)),
                ),
            )
            .child(status_bar)
    }
}
