from __future__ import annotations

from dataclasses import dataclass
import math

from coloraide import Color

from palette_probe import Candidate, build_candidates, min_adjacent_distance


BACKGROUND_APCA_LIGHT = 35.0
BACKGROUND_APCA_DARK = 30.0
ADJACENT_OKLAB_DARK = 0.08
ADJACENT_OKLAB_LIGHT_INTERVENTION = 0.095
ADJACENT_OKLAB_LIGHT_TARGET = 0.10
LIGHTNESS_CLAMP_MIN = 0.18
LIGHTNESS_CLAMP_MAX = 0.92

DEFAULT_ACCENTS = (
    "#0090ffff",
    "#f76b15ff",
    "#d6409fff",
    "#bdee63ff",
    "#8e4ec6ff",
    "#ffc53dff",
    "#29a383ff",
    "#e54d2eff",
    "#00a2c7ff",
    "#978365ff",
    "#46a758ff",
    "#3e63ddff",
    "#5b5bd6ff",
)


def is_valid_color_string(value: str) -> bool:
    try:
        Color(value)
    except ValueError:
        return False
    return True


@dataclass(frozen=True)
class ThemeInputs:
    file: str
    theme_name: str
    appearance: str
    accents: tuple[str, ...]
    explicit_accents: bool
    background: str


@dataclass(frozen=True)
class PipelineResult:
    inputs: ThemeInputs
    authored: Candidate
    background_adjusted: Candidate
    swap: Candidate
    stride: Candidate
    greedy: Candidate


@dataclass(frozen=True)
class PaletteSelection:
    palette: list[Color]
    changed: bool


def resolve_theme_inputs(file_path: str, theme: dict[str, object]) -> ThemeInputs:
    appearance = theme["appearance"]
    style = theme["style"]
    raw_accents = style.get("accents") or []
    filtered_accents = tuple(
        accent
        for accent in raw_accents
        if isinstance(accent, str) and accent and is_valid_color_string(accent)
    )
    explicit_accents = bool(filtered_accents)
    accents = filtered_accents or DEFAULT_ACCENTS
    background = style["editor.background"]
    return ThemeInputs(
        file=file_path,
        theme_name=theme["name"],
        appearance=appearance,
        accents=accents,
        explicit_accents=explicit_accents,
        background=background,
    )


def intervention_threshold(appearance: str) -> float:
    return (
        ADJACENT_OKLAB_LIGHT_INTERVENTION
        if appearance == "light"
        else ADJACENT_OKLAB_DARK
    )


def target_threshold(appearance: str) -> float:
    return ADJACENT_OKLAB_LIGHT_TARGET if appearance == "light" else ADJACENT_OKLAB_DARK


def minimum_background_contrast(appearance: str) -> float:
    return BACKGROUND_APCA_LIGHT if appearance == "light" else BACKGROUND_APCA_DARK


def analyze_theme(theme_inputs: ThemeInputs) -> PipelineResult:
    background = Color(theme_inputs.background)
    authored_colors = [Color(value) for value in theme_inputs.accents]
    authored_candidates = build_candidates(authored_colors, background)
    authored = next(candidate for candidate in authored_candidates if candidate.name == "authored")

    adjusted_colors = [
        adjust_color_for_background(
            color,
            background,
            minimum_background_contrast(theme_inputs.appearance),
        )
        for color in authored_colors
    ]
    adjusted_candidates = build_candidates(adjusted_colors, background)
    background_adjusted = next(
        candidate for candidate in adjusted_candidates if candidate.name == "authored"
    )
    swap = next(
        candidate for candidate in adjusted_candidates if candidate.name == "best-single-swap"
    )
    stride_candidates = [
        candidate
        for candidate in adjusted_candidates
        if candidate.name.startswith("stride-")
    ]
    stride = (
        max(
            stride_candidates,
            key=lambda candidate: (
                candidate.min_adjacent_distance,
                -candidate.inversion_count,
                -candidate.displacement_cost,
            ),
        )
        if stride_candidates
        else background_adjusted
    )
    greedy = next(
        candidate for candidate in adjusted_candidates if candidate.name == "greedy-anchored"
    )
    return PipelineResult(
        inputs=theme_inputs,
        authored=authored,
        background_adjusted=background_adjusted,
        swap=swap,
        stride=stride,
        greedy=greedy,
    )


def current_rust_auto_palette(
    accents: list[Color], appearance: str, background: Color
) -> PaletteSelection:
    original_palette = [color.clone().convert("srgb", fit=True) for color in accents]
    adjusted_palette = [
        adjust_color_for_background(
            color,
            background,
            minimum_background_contrast(appearance),
        )
        for color in original_palette
    ]
    adjusted_changed = not palettes_equal(original_palette, adjusted_palette)
    adjusted_min_adj = min_adjacent_distance(
        adjusted_palette, tuple(range(len(adjusted_palette))), background
    )
    threshold = intervention_threshold(appearance)

    if len(adjusted_palette) < 3 or adjusted_min_adj >= threshold:
        return PaletteSelection(adjusted_palette if adjusted_changed else original_palette, adjusted_changed)

    candidates = build_candidates(adjusted_palette, background)
    greedy = next(candidate for candidate in candidates if candidate.name == "greedy-anchored")
    if greedy.min_adjacent_distance >= threshold:
        palette = [adjusted_palette[index] for index in greedy.order]
        return PaletteSelection(palette, True)

    return PaletteSelection(adjusted_palette if adjusted_changed else original_palette, adjusted_changed)


def palettes_equal(left: list[Color], right: list[Color], epsilon: float = 1e-6) -> bool:
    if len(left) != len(right):
        return False
    return all(color_equal(left_color, right_color, epsilon) for left_color, right_color in zip(left, right))


def color_equal(left: Color, right: Color, epsilon: float = 1e-6) -> bool:
    left_srgb = left.convert("srgb", fit=True)
    right_srgb = right.convert("srgb", fit=True)
    return (
        all(abs(left_channel - right_channel) <= epsilon for left_channel, right_channel in zip(left_srgb.coords(), right_srgb.coords()))
        and abs(left_srgb.alpha() - right_srgb.alpha()) <= epsilon
    )


def composite_over_background(foreground: Color, background: Color) -> Color:
    foreground = foreground.convert("srgb", fit=True)
    background = background.convert("srgb", fit=True)
    alpha = foreground.alpha()
    if alpha >= 1:
        return foreground
    red, green, blue = [min(max(channel, 0.0), 1.0) for channel in foreground.coords()]
    bg_red, bg_green, bg_blue = [
        min(max(channel, 0.0), 1.0) for channel in background.coords()
    ]
    return Color(
        "srgb",
        [
            bg_red * (1 - alpha) + red * alpha,
            bg_green * (1 - alpha) + green * alpha,
            bg_blue * (1 - alpha) + blue * alpha,
        ],
    )


def background_contrast(foreground: Color, background: Color) -> float:
    composited = composite_over_background(foreground, background)
    return abs(apca_contrast(composited, background))


def adjust_color_for_background(
    color: Color,
    background: Color,
    minimum_contrast: float,
) -> Color:
    if background_contrast(color, background) >= minimum_contrast:
        return color

    original = color.convert("oklab")
    darker_candidate = adjusted_lightness_candidate(
        color, background, minimum_contrast, LIGHTNESS_CLAMP_MIN
    )
    lighter_candidate = adjusted_lightness_candidate(
        color, background, minimum_contrast, LIGHTNESS_CLAMP_MAX
    )

    if darker_candidate and lighter_candidate:
        darker_distance = color_distance_oklab(original, darker_candidate.convert("oklab"))
        lighter_distance = color_distance_oklab(original, lighter_candidate.convert("oklab"))
        return darker_candidate if darker_distance <= lighter_distance else lighter_candidate
    if darker_candidate:
        return darker_candidate
    if lighter_candidate:
        return lighter_candidate
    return color


def adjusted_lightness_candidate(
    color: Color,
    background: Color,
    minimum_contrast: float,
    target_lightness: float,
) -> Color | None:
    original = color.convert("oklch")
    lightness_delta = target_lightness - original["l"]
    if abs(lightness_delta) <= 1e-9:
        return None

    for step in range(1, 129):
        amount = step / 128.0
        candidate = original.clone()
        candidate["l"] = min(max(original["l"] + lightness_delta * amount, 0.0), 1.0)
        candidate["alpha"] = color.alpha()
        if background_contrast(candidate, background) >= minimum_contrast:
            return candidate.convert("srgb", fit=True)
    return None


def color_distance_oklab(left: Color, right: Color) -> float:
    left_coords = left.coords()
    right_coords = right.coords()
    return math.sqrt(
        sum(
            (left_component - right_component) ** 2
            for left_component, right_component in zip(left_coords, right_coords)
        )
    )


def apca_contrast(text_color: Color, background_color: Color) -> float:
    constants = {
        "main_trc": 2.4,
        "s_rco": 0.2126729,
        "s_gco": 0.7151522,
        "s_bco": 0.0721750,
        "norm_bg": 0.56,
        "norm_txt": 0.57,
        "rev_txt": 0.62,
        "rev_bg": 0.65,
        "blk_thrs": 0.022,
        "blk_clmp": 1.414,
        "scale_bow": 1.14,
        "scale_wob": 1.14,
        "lo_bow_offset": 0.027,
        "lo_wob_offset": 0.027,
        "delta_y_min": 0.0005,
        "lo_clip": 0.1,
    }

    text_y = srgb_to_y(text_color, constants)
    bg_y = srgb_to_y(background_color, constants)

    text_y_clamped = (
        text_y
        if text_y > constants["blk_thrs"]
        else text_y + (constants["blk_thrs"] - text_y) ** constants["blk_clmp"]
    )
    bg_y_clamped = (
        bg_y
        if bg_y > constants["blk_thrs"]
        else bg_y + (constants["blk_thrs"] - bg_y) ** constants["blk_clmp"]
    )

    if abs(bg_y_clamped - text_y_clamped) < constants["delta_y_min"]:
        return 0.0

    if bg_y_clamped > text_y_clamped:
        sapc = (
            bg_y_clamped ** constants["norm_bg"]
            - text_y_clamped ** constants["norm_txt"]
        ) * constants["scale_bow"]
        output = 0.0 if sapc < constants["lo_clip"] else sapc - constants["lo_bow_offset"]
    else:
        sapc = (
            bg_y_clamped ** constants["rev_bg"]
            - text_y_clamped ** constants["rev_txt"]
        ) * constants["scale_wob"]
        output = 0.0 if sapc > -constants["lo_clip"] else sapc + constants["lo_wob_offset"]

    return output * 100.0


def srgb_to_y(color: Color, constants: dict[str, float]) -> float:
    red, green, blue = [
        min(max(channel, 0.0), 1.0) for channel in color.convert("srgb", fit=True).coords()
    ]
    red_linear = red ** constants["main_trc"]
    green_linear = green ** constants["main_trc"]
    blue_linear = blue ** constants["main_trc"]
    return (
        constants["s_rco"] * red_linear
        + constants["s_gco"] * green_linear
        + constants["s_bco"] * blue_linear
    )
