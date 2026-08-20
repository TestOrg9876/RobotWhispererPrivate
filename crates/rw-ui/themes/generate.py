"""Generate the Robot Whisperer ThemeSet.

Each theme is described by a small seed — an elevation ramp, an accent, and the
semantic trio — and the full gpui-component key set is derived from it. Keeping
the derivation in one place is what makes the seven themes feel like one system
instead of seven hand-tuned guesses.
"""
import collections, json

Seed = collections.namedtuple("Seed", (
    "name mode "
    "window panel card subtle "            # window < panel < card, plus the subtle
                                           # contrast surface used for hover states,
                                           # card headers and quiet tiles: lighter
                                           # than the card on dark themes, darker on
                                           # light ones, so it always reads
    "border border_strong "
    "fg fg_muted fg_faint "
    "accent accent_hover accent_active on_accent "
    "success warning danger "
    "topic service action"
))

SEEDS = [
    # Designed pair -------------------------------------------------------------
    Seed("Robot Whisperer Dark", "dark",
         "#08090c", "#101318", "#181d24", "#222831",
         "#2b323d", "#3c4553",
         "#e8ecf2", "#8d97a8", "#5c6675",
         "#3b82f6", "#60a5fa", "#2563eb", "#f8fafc",
         "#34d399", "#fbbf24", "#f87171",
         "#60a5fa", "#34d399", "#a78bfa"),
    Seed("Robot Whisperer Light", "light",
         "#eef0f4", "#f7f8fa", "#ffffff", "#eceff4",
         "#dde1e8", "#c3cad4",
         "#111827", "#5b6472", "#8a929e",
         "#2563eb", "#3b82f6", "#1d4ed8", "#ffffff",
         "#059669", "#d97706", "#dc2626",
         "#2563eb", "#059669", "#7c3aed"),
    # Ported favourites --------------------------------------------------------
    Seed("One Dark", "dark",
         "#16181d", "#1c1f25", "#282c34", "#31363f",
         "#3b4351", "#4d5665",
         "#abb2bf", "#828997", "#5c6370",
         "#61afef", "#7cc4f5", "#4a9ae0", "#1a1d23",
         "#98c379", "#e5c07b", "#e06c75",
         "#61afef", "#98c379", "#c678dd"),
    Seed("Dracula", "dark",
         "#15161c", "#1c1d26", "#282a36", "#33364a",
         "#3e4255", "#565a72",
         "#f8f8f2", "#a9adc1", "#6272a4",
         "#ff79c6", "#ff92d0", "#e45faf", "#191a21",
         "#50fa7b", "#f1fa8c", "#ff5555",
         "#8be9fd", "#50fa7b", "#bd93f9"),
    Seed("Nord", "dark",
         "#21252e", "#2a2f3a", "#333b4a", "#3e4759",
         "#48526a", "#5a6480",
         "#eceff4", "#b8c0ce", "#7b8494",
         "#88c0d0", "#9ed0dd", "#6fadbf", "#242933",
         "#a3be8c", "#ebcb8b", "#bf616a",
         "#81a1c1", "#a3be8c", "#b48ead"),
    Seed("Solarized Light", "light",
         "#e8e1cc", "#f4eedb", "#fdf6e3", "#f1ebd8",
         "#d6cfb8", "#bdb69f",
         "#073642", "#657b83", "#93a1a1",
         "#268bd2", "#3a9ee0", "#1f7ab8", "#fdf6e3",
         "#859900", "#b58900", "#dc322f",
         "#268bd2", "#859900", "#6c71c4"),
    Seed("Rosé Pine Dawn", "light",
         "#f2ebe3", "#fbf6f0", "#ffffff", "#f7f0ea",
         "#e3d6cd", "#cfc0b6",
         "#575279", "#797593", "#9893a5",
         "#d7827e", "#e0958f", "#b4636f", "#fffaf3",
         "#56949f", "#ea9d34", "#b4637a",
         "#286983", "#56949f", "#907aa9"),
]

def alpha(hex_colour, aa):
    return hex_colour + aa

def toward(hex_colour, target, amount):
    """Mixes `hex_colour` `amount` of the way toward `target`."""
    a = [int(hex_colour[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(target[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * amount):02x}" for x, y in zip(a, b))

def theme(seed):
    s = seed
    colours = collections.OrderedDict([
        # Base surfaces --------------------------------------------------------
        ("background", s.window),
        ("foreground", s.fg),
        ("border", s.border),
        ("input.border", s.border_strong),
        ("ring", s.accent),
        ("caret", s.accent),
        ("selection.background", alpha(s.accent, "3d")),
        ("overlay", alpha("#000000", "99") if s.mode == "dark" else alpha("#0b1220", "40")),
        ("window.border", s.border),

        # `accent` is shadcn's hover surface, not the brand colour.
        ("accent.background", s.subtle),
        ("accent.foreground", s.fg),
        ("muted.background", s.subtle),
        ("muted.foreground", s.fg_muted),

        # Brand ---------------------------------------------------------------
        ("primary.background", s.accent),
        ("primary.hover.background", s.accent_hover),
        ("primary.active.background", s.accent_active),
        ("primary.foreground", s.on_accent),
        ("secondary.background", s.card),
        ("secondary.hover.background", s.subtle),
        ("secondary.active.background", s.border_strong),
        ("secondary.foreground", s.fg),

        # Semantics -----------------------------------------------------------
        ("success.background", s.success),
        ("success.foreground", s.on_accent),
        ("warning.background", s.warning),
        ("warning.foreground", s.window if s.mode == "dark" else "#ffffff"),
        ("danger.background", s.danger),
        ("danger.foreground", s.on_accent),
        ("info.background", s.accent),
        ("info.foreground", s.on_accent),

        # Buttons -------------------------------------------------------------
        ("button.background", s.card),
        ("button.hover.background", s.subtle),
        ("button.active.background", s.border_strong),
        ("button.foreground", s.fg),
        ("button.primary.background", s.accent),
        ("button.primary.hover.background", s.accent_hover),
        ("button.primary.active.background", s.accent_active),
        ("button.primary.foreground", s.on_accent),
        ("button.secondary.background", s.card),
        ("button.secondary.hover.background", s.subtle),
        ("button.secondary.active.background", s.border_strong),
        ("button.secondary.foreground", s.fg),
        ("button.danger.background", s.danger),
        ("button.danger.hover.background", s.danger),
        ("button.danger.active.background", s.danger),
        ("button.danger.foreground", s.on_accent),
        ("button.success.background", s.success),
        ("button.success.foreground", s.on_accent),
        ("button.warning.background", s.warning),
        ("button.warning.foreground", s.window if s.mode == "dark" else "#ffffff"),
        ("button.info.background", s.accent),
        ("button.info.foreground", s.on_accent),

        # Sidebar -------------------------------------------------------------
        ("sidebar.background", s.panel),
        ("sidebar.foreground", s.fg),
        ("sidebar.border", s.border),
        ("sidebar.accent.background", s.subtle),
        ("sidebar.accent.foreground", s.fg),
        ("sidebar.primary.background", s.accent),
        ("sidebar.primary.foreground", s.on_accent),

        # Chrome --------------------------------------------------------------
        ("title_bar.background", s.panel),
        ("title_bar.border", s.border),
        ("status_bar.background", s.panel),
        ("status_bar.border", s.border),
        ("tab_bar.background", s.panel),
        ("tab_bar.segmented.background", s.window),
        ("tab.background", s.panel),
        ("tab.foreground", s.fg_muted),
        # The dock fixes its tabs to one variant — a browser tab, square and
        # notched into the rule under the strip. The app draws a chip inside it
        # instead (`tokens::tab_chip`), so the tab the dock draws has to get out
        # of the way: no fill of its own, and the chip supplies the colour.
        ("tab.active.background", alpha(s.window, "00")),
        ("tab.active.foreground", s.fg),

        # Containers ----------------------------------------------------------
        # Popovers float above every other surface. The library draws them with a
        # hairline ring and no border, so on a dark palette — where the whole ramp
        # is compressed into the bottom of the range — one more step keeps them
        # off the card behind them. On a light palette the card colour already is
        # the top of the ramp, and the shadow does the separating.
        ("popover.background",
         toward(s.subtle, s.fg, 0.06) if s.mode == "dark" else s.card),
        ("popover.foreground", s.fg),
        ("accordion.background", s.card),
        ("group_box.background", s.card),
        ("group_box.foreground", s.fg),
        ("tiles.background", s.window),
        ("skeleton.background", s.subtle),

        # Lists and tables ----------------------------------------------------
        ("list.background", s.card),
        ("list.even.background", s.card),
        ("list.hover.background", s.subtle),
        ("list.active.background", alpha(s.accent, "26")),
        ("list.active.border", s.accent),
        ("list.head.background", s.panel),
        ("table.background", s.card),
        ("table.even.background", s.card),
        ("table.hover.background", s.subtle),
        ("table.active.background", alpha(s.accent, "26")),
        ("table.active.border", s.accent),
        ("table.head.background", s.panel),
        ("table.head.foreground", s.fg_muted),
        ("table.foot.background", s.panel),
        ("table.foot.foreground", s.fg_muted),
        ("table.row.border", s.border),

        # Drag and drop -------------------------------------------------------
        ("drag.border", s.accent),
        ("drop_target.background", alpha(s.accent, "33")),

        # Links ---------------------------------------------------------------
        ("link", s.accent),
        ("link.hover", s.accent_hover),
        ("link.active", s.accent_active),

        # Scrollbar — invisible track, thumb only on the hairline ramp --------
        ("scrollbar.background", alpha(s.window, "00")),
        ("scrollbar.thumb.background", alpha(s.border_strong, "cc")),
        ("scrollbar.thumb.hover.background", s.fg_faint),

        # Request-kind tags, reused as chart series ---------------------------
        ("base.blue", s.topic),
        ("base.green", s.service),
        ("base.magenta", s.action),
        ("chart.1", s.topic),
        ("chart.2", s.service),
        ("chart.3", s.action),
        ("chart.4", s.warning),
        ("chart.5", s.danger),
        ("chart_bullish", s.success),
        ("chart_bearish", s.danger),
    ])

    return collections.OrderedDict([
        ("is_default", s.name == "Robot Whisperer Dark"),
        ("name", s.name),
        ("mode", s.mode),
        # Comfortable density: 14px base, softly rounded, elevation via shadow.
        ("font.size", 14),
        ("radius", 10),
        ("radius.lg", 14),
        ("shadow", True),
        ("colors", colours),
    ])

doc = collections.OrderedDict([
    ("$schema", "https://github.com/longbridge/gpui-component/raw/refs/heads/main/.theme-schema.json"),
    ("name", "Robot Whisperer"),
    ("url", "https://github.com/Mika412/RobotWhisperer"),
    ("themes", [theme(s) for s in SEEDS]),
])

out = "crates/rw-ui/themes/robot-whisperer.json"
with open(out, "w") as fh:
    json.dump(doc, fh, indent=2, ensure_ascii=False)
    fh.write("\n")
print(f"{out}: {len(SEEDS)} themes, {len(doc['themes'][0]['colors'])} colours each")
