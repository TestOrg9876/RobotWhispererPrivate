//! Settings: the things that are chosen once and then left alone.
//!
//! A rail of sections and a pane, rather than one scroll of everything. There
//! are eight settings and there will be more, and a single column of them makes
//! you read all of it to find the one you came for.
//!
//! Every number here was a constant nobody could reach, and every default is
//! still that constant — see `prefs::Settings`. Changes apply as they are made
//! rather than on a Save button: a setting you have to confirm is a setting you
//! have to guess about, and the theme has always worked this way.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::ListItem;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};

use crate::prefs::{Prefs, Settings};
use crate::theme::{self, Preference};
use crate::tokens;

/// Emitted when a setting changes, so the shell can persist it.
#[derive(Debug, Clone)]
pub enum SettingsEvent {
    ThemeChosen(Preference),
    Changed(Settings),
}

/// Which group of settings is showing.
///
/// Named after what you would be looking for, not after the crate the value
/// happens to live in: nobody opens Settings thinking "I want to change
/// `rw-pipeline`".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Appearance,
    Requests,
    Data,
    Plots,
    Transforms,
    Console,
}

impl Section {
    const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Requests,
        Self::Data,
        Self::Plots,
        Self::Transforms,
        Self::Console,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Requests => "Requests",
            Self::Data => "Data",
            Self::Plots => "Plots",
            Self::Transforms => "Transforms",
            Self::Console => "Console",
        }
    }
}

/// One editable number, and how to read it back out of its box.
struct Field {
    state: Entity<InputState>,
    /// Writes the parsed value into the settings. `None` when the text is not a
    /// number, which leaves the setting alone rather than taking a zero.
    apply: fn(&mut Settings, usize),
    /// The smallest value that still means something. A plot window of zero is
    /// not a preference, it is a broken plot.
    least: usize,
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    preference: Preference,
    settings: Settings,
    section: Section,
    fields: Vec<Field>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn view(prefs: &Prefs, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let preference = prefs.theme();
        let settings = *prefs.settings();
        cx.new(|cx| {
            let mut fields = Vec::new();
            let mut subscriptions = Vec::new();

            for (value, apply, least) in Self::numbers(&settings) {
                let state = cx.new(|cx| {
                    InputState::new(window, cx).default_value(SharedString::from(value.to_string()))
                });
                subscriptions.push(cx.subscribe(&state, |this: &mut Self, _, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.read_numbers(cx);
                    }
                }));
                fields.push(Field {
                    state,
                    apply,
                    least,
                });
            }

            Self {
                focus_handle: cx.focus_handle(),
                preference,
                settings,
                section: Section::Appearance,
                fields,
                _subscriptions: subscriptions,
            }
        })
    }

    /// Every numeric setting, in the order the fields are built and read.
    ///
    /// One list so the boxes and the write-back cannot drift apart — the bug
    /// this shape exists to prevent is a field that edits the wrong setting.
    #[allow(clippy::type_complexity)]
    fn numbers(settings: &Settings) -> Vec<(usize, fn(&mut Settings, usize), usize)> {
        vec![
            (
                settings.history_depth,
                (|s, v| s.history_depth = v) as fn(&mut Settings, usize),
                1,
            ),
            (settings.point_budget, |s, v| s.point_budget = v, 1_000),
            (settings.plot_window, |s, v| s.plot_window = v, 2),
            (settings.plot_fields, |s, v| s.plot_fields = v, 1),
            (
                settings.rate_window_secs as usize,
                |s, v| s.rate_window_secs = v as u64,
                1,
            ),
            (
                settings.tf_window_secs as usize,
                |s, v| s.tf_window_secs = v as u64,
                1,
            ),
            (settings.console_lines, |s, v| s.console_lines = v, 100),
        ]
    }

    /// Collects every box back into the settings and announces the result.
    ///
    /// A box that is empty or not a number leaves its setting alone. Typing is
    /// transient — a field is briefly empty every time someone selects all and
    /// retypes — and taking a zero from it would be acting on a keystroke
    /// rather than on an intention.
    fn read_numbers(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.settings;
        for field in &self.fields {
            let text = field.state.read(cx).value().to_string();
            if let Ok(value) = text.trim().parse::<usize>()
                && value >= field.least
            {
                (field.apply)(&mut settings, value);
            }
        }
        self.announce(settings, cx);
    }

    fn announce(&mut self, settings: Settings, cx: &mut Context<Self>) {
        if settings == self.settings {
            return;
        }
        self.settings = settings;
        cx.emit(SettingsEvent::Changed(settings));
        cx.notify();
    }

    fn choose_theme(&mut self, preference: Preference, cx: &mut Context<Self>) {
        self.preference = preference.clone();
        cx.emit(SettingsEvent::ThemeChosen(preference));
        cx.notify();
    }

    fn rail(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w(px(140.))
            .flex_shrink_0()
            .gap_0p5()
            .children(Section::ALL.map(|section| {
                let chosen = section == self.section;
                h_flex()
                    .id(SharedString::from(format!("section:{}", section.label())))
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .rounded(cx.theme().radius)
                    .text_sm()
                    .when(chosen, |row| {
                        row.bg(cx.theme().list_active)
                            .text_color(cx.theme().foreground)
                    })
                    .when(!chosen, |row| {
                        row.text_color(cx.theme().muted_foreground)
                            .hover(|row| row.bg(cx.theme().list_hover))
                    })
                    .child(section.label())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.section = section;
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    /// A labelled row: what it is, why you would change it, and the control.
    ///
    /// The explanation is the point. A number called "Point budget" means
    /// nothing without "above this a cloud is subsampled", and a settings pane
    /// whose rows need a manual is a settings pane nobody touches.
    fn row(
        &self,
        title: &'static str,
        detail: &'static str,
        control: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .w_full()
            .gap_4()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    ),
            )
            .child(div().w(px(120.)).flex_shrink_0().child(control))
            .into_any_element()
    }

    fn number(&self, index: usize) -> AnyElement {
        match self.fields.get(index) {
            Some(field) => Input::new(&field.state).small().into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = match self.section {
            Section::Appearance => return self.themes(cx),
            Section::Requests => vec![self.row(
                "History depth",
                "How many past runs of one request are kept.",
                self.number(0),
                cx,
            )],
            Section::Data => vec![self.row(
                "Point budget",
                "Above this a cloud is subsampled, so its shape survives and the frame rate does too.",
                self.number(1),
                cx,
            )],
            Section::Plots => vec![
                self.row(
                    "Samples kept",
                    "How much of each plotted field's past is drawn.",
                    self.number(2),
                    cx,
                ),
                self.row(
                    "Fields at once",
                    "Past this a message stops adding series to the plot.",
                    self.number(3),
                    cx,
                ),
                self.row(
                    "Rate window",
                    "Seconds a topic's rate and bandwidth are averaged over. Shorter notices a topic stopping sooner.",
                    self.number(4),
                    cx,
                ),
            ],
            Section::Transforms => vec![
                self.row(
                    "Follow /tf",
                    "Subscribe to /tf and /tf_static automatically, so layers can be placed without asking.",
                    self.switch(cx),
                    cx,
                ),
                self.row(
                    "History kept",
                    "Seconds of transforms held. A lookup older than this is refused rather than guessed.",
                    self.number(5),
                    cx,
                ),
            ],
            Section::Console => vec![self.row(
                "Lines kept",
                "Older lines are dropped from the front, so a week-long session costs what a minute does.",
                self.number(6),
                cx,
            )],
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .children(rows)
            .into_any_element()
    }

    fn switch(&self, cx: &mut Context<Self>) -> AnyElement {
        Switch::new("follow-transforms")
            .checked(self.settings.follow_transforms)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                let settings = Settings {
                    follow_transforms: *checked,
                    ..this.settings
                };
                this.announce(settings, cx);
            }))
            .into_any_element()
    }

    /// One theme, shown as itself: the list is a preview rather than a column
    /// of names.
    fn themes(&self, cx: &mut Context<Self>) -> AnyElement {
        // `ListItem`, which the sidebar already draws every request with. It
        // carries the selected fill, the hover, the accent border and the slot
        // the check sits in — all four of which this row used to spell out for
        // itself, and one of which it was missing.
        //
        // `selected` paints it and `confirmed` is what reveals the check, so a
        // chosen theme is both.
        let swatch = |preference: Preference, label: String, cx: &mut Context<Self>| {
            let chosen = self.preference == preference;
            ListItem::new(SharedString::from(format!("theme:{label}")))
                .h(px(tokens::CONTROL_HEIGHT))
                .px_3()
                .selected(chosen)
                .confirmed(chosen)
                .check_icon(IconName::Check)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(label),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.choose_theme(preference.clone(), cx)
                }))
                .into_any_element()
        };

        let system = swatch(Preference::System, "Match system".to_string(), cx);
        let named: Vec<AnyElement> = theme::names()
            .into_iter()
            .map(|name| swatch(Preference::Named(name.clone()), name, cx))
            .collect();

        v_flex()
            .id("theme-list")
            .flex_1()
            .min_w_0()
            .max_h(px(320.))
            .overflow_y_scroll()
            .gap_0p5()
            .child(system)
            .children(named)
            .into_any_element()
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rail = self.rail(cx);
        let body = self.body(cx);

        v_flex()
            .id("settings")
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(340.))
                    .gap_4()
                    .items_start()
                    .child(rail)
                    .child(body),
            )
            .child(tokens::hairline(cx))
            .child(
                // Where a version belongs: looked up when it is wanted, rather
                // than occupying a strip of the window forever.
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Robot Whisperer"),
                    )
                    .child(
                        tokens::mono(cx)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                    ),
            )
    }
}
