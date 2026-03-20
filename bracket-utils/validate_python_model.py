from __future__ import annotations

from coloraide import Color

from palette_probe import min_adjacent_distance
from theme_pipeline import (
    ADJACENT_OKLAB_LIGHT_INTERVENTION,
    ADJACENT_OKLAB_LIGHT_TARGET,
    BACKGROUND_APCA_LIGHT,
    background_contrast,
    color_distance_oklab,
    color_equal,
    current_rust_auto_palette,
    intervention_threshold,
)


def hsl(hue: float, saturation: float, lightness: float) -> Color:
    return Color("hsl", [hue * 360.0, saturation, lightness])


def assert_true(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def validate() -> None:
    dark_background = hsl(0.0, 0.0, 0.12)
    light_background = hsl(0.0, 0.0, 0.98)

    # Mirrors test_auto_bracket_colorization_mode_reorders_weak_palette.
    weak_palette = [
        hsl(0.0, 1.0, 0.68),
        hsl(0.02, 1.0, 0.68),
        hsl(0.34, 1.0, 0.68),
        hsl(0.36, 1.0, 0.68),
    ]
    weak_result = current_rust_auto_palette(weak_palette, "dark", dark_background)
    weak_original_min_adj = min_adjacent_distance(
        weak_palette, tuple(range(len(weak_palette))), dark_background
    )
    weak_final_min_adj = min_adjacent_distance(
        weak_result.palette, tuple(range(len(weak_result.palette))), dark_background
    )
    assert_true(
        weak_final_min_adj > weak_original_min_adj,
        "weak palette should improve under current auto policy",
    )

    # Mirrors test_preserves_strong_palette.
    strong_palette = [
        hsl(0.0, 1.0, 0.78),
        hsl(0.16, 1.0, 0.78),
        hsl(0.33, 1.0, 0.78),
        hsl(0.66, 1.0, 0.78),
    ]
    strong_result = current_rust_auto_palette(strong_palette, "dark", dark_background)
    assert_true(
        all(color_equal(left, right) for left, right in zip(strong_palette, strong_result.palette)),
        "strong palette should remain unchanged",
    )

    # Mirrors test_adjusts_background_failures_preserving_hue_and_chroma.
    background_failure_palette = [
        hsl(0.58, 1.0, 0.28),
        hsl(0.12, 1.0, 0.28),
        hsl(0.22, 0.9, 0.76),
    ]
    background_failure_result = current_rust_auto_palette(
        background_failure_palette, "light", light_background
    )
    assert_true(
        color_equal(background_failure_result.palette[0], background_failure_palette[0]),
        "first color should stay unchanged",
    )
    assert_true(
        color_equal(background_failure_result.palette[1], background_failure_palette[1]),
        "second color should stay unchanged",
    )
    assert_true(
        not color_equal(background_failure_result.palette[2], background_failure_palette[2]),
        "third color should be adjusted",
    )
    original_oklch = background_failure_palette[2].convert("oklch")
    adjusted_oklch = background_failure_result.palette[2].convert("oklch")
    assert_true(
        abs(original_oklch["c"] - adjusted_oklch["c"]) < 1e-4,
        "background adjustment should preserve chroma",
    )
    hue_delta = abs(original_oklch["h"] - adjusted_oklch["h"])
    assert_true(hue_delta < 1e-3, "background adjustment should preserve hue")
    assert_true(
        background_contrast(background_failure_result.palette[2], light_background)
        >= BACKGROUND_APCA_LIGHT,
        "adjusted color should pass light-theme APCA floor",
    )

    # Mirrors test_preserves_light_near_miss_palette.
    gruvbox_like = [
        Color("#CC241D"),
        Color("#98971A"),
        Color("#D79921"),
        Color("#458588"),
        Color("#B16286"),
        Color("#689D6A"),
        Color("#D65D0E"),
    ]
    gruvbox_background = Color("#FBF1C7")
    gruvbox_result = current_rust_auto_palette(gruvbox_like, "light", gruvbox_background)
    original_min_adj = min_adjacent_distance(
        gruvbox_like, tuple(range(len(gruvbox_like))), gruvbox_background
    )
    assert_true(
        all(color_equal(left, right) for left, right in zip(gruvbox_like, gruvbox_result.palette)),
        "light near-miss palette should remain unchanged",
    )
    assert_true(
        original_min_adj < ADJACENT_OKLAB_LIGHT_TARGET,
        "near-miss palette should be below light target threshold",
    )
    assert_true(
        original_min_adj >= ADJACENT_OKLAB_LIGHT_INTERVENTION,
        "near-miss palette should stay above light intervention threshold",
    )

    print("Python model validations passed")


if __name__ == "__main__":
    validate()
