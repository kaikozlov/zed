from __future__ import annotations

import argparse
from dataclasses import dataclass
from statistics import mean, median

from coloraide import Color

from analyze_extension_themes import load_results
from theme_pipeline import (
    BACKGROUND_APCA_DARK,
    BACKGROUND_APCA_LIGHT,
    adjust_color_for_background,
    background_contrast,
    color_distance_oklab,
    color_equal,
)


@dataclass(frozen=True)
class AccentCase:
    appearance: str
    explicit_accents: bool
    theme_name: str
    file: str
    original: Color
    background: Color


def build_cases() -> list[AccentCase]:
    cases: list[AccentCase] = []
    for result in load_results():
        background = Color(result.inputs.background)
        for accent in result.inputs.accents:
            cases.append(
                AccentCase(
                    appearance=result.inputs.appearance,
                    explicit_accents=result.inputs.explicit_accents,
                    theme_name=result.inputs.theme_name,
                    file=result.inputs.file,
                    original=Color(accent),
                    background=background,
                )
            )
    return cases


def frange(start: float, stop: float, step: float) -> list[float]:
    values: list[float] = []
    current = start
    while current <= stop + 1e-9:
        values.append(round(current, 3))
        current += step
    return values


def summarize_group(
    cases: list[AccentCase],
    threshold: float,
) -> dict[str, float | int]:
    failing_cases = [
        case
        for case in cases
        if background_contrast(case.original, case.background) < threshold
    ]

    touched_theme_keys: set[tuple[str, str]] = set()
    unrecovered_theme_keys: set[tuple[str, str]] = set()
    opaque_failing = 0
    translucent_failing = 0
    opaque_unrecovered = 0
    translucent_unrecovered = 0
    deltas: list[float] = []
    lightness_deltas: list[float] = []
    before_values: list[float] = []
    after_values: list[float] = []
    margin_values: list[float] = []
    rescued = 0
    unrecovered = 0
    touched = 0

    for case in failing_cases:
        before = background_contrast(case.original, case.background)
        adjusted = adjust_color_for_background(
            case.original,
            case.background,
            threshold,
        )
        after = background_contrast(adjusted, case.background)
        theme_key = (case.file, case.theme_name)
        alpha = case.original.alpha()

        if alpha >= 0.999:
            opaque_failing += 1
        else:
            translucent_failing += 1

        before_values.append(before)
        after_values.append(after)
        margin_values.append(after - threshold)

        if not color_equal(adjusted, case.original):
            touched += 1
            touched_theme_keys.add(theme_key)
            deltas.append(
                color_distance_oklab(
                    case.original.convert("oklab"),
                    adjusted.convert("oklab"),
                )
            )
            lightness_deltas.append(
                abs(
                    adjusted.convert("oklch")["l"]
                    - case.original.convert("oklch")["l"]
                )
            )

        if after >= threshold:
            rescued += 1
        else:
            unrecovered += 1
            unrecovered_theme_keys.add(theme_key)
            if alpha >= 0.999:
                opaque_unrecovered += 1
            else:
                translucent_unrecovered += 1

    return {
        "accent_count": len(cases),
        "failing": len(failing_cases),
        "touched": touched,
        "rescued": rescued,
        "unrecovered": unrecovered,
        "theme_touched": len(touched_theme_keys),
        "theme_unrecovered": len(unrecovered_theme_keys),
        "opaque_failing": opaque_failing,
        "translucent_failing": translucent_failing,
        "opaque_unrecovered": opaque_unrecovered,
        "translucent_unrecovered": translucent_unrecovered,
        "avg_before": mean(before_values) if before_values else 0.0,
        "avg_after": mean(after_values) if after_values else 0.0,
        "avg_margin": mean(margin_values) if margin_values else 0.0,
        "avg_delta": mean(deltas) if deltas else 0.0,
        "median_delta": median(deltas) if deltas else 0.0,
        "max_delta": max(deltas) if deltas else 0.0,
        "avg_lightness_delta": mean(lightness_deltas) if lightness_deltas else 0.0,
        "median_lightness_delta": median(lightness_deltas) if lightness_deltas else 0.0,
    }


def print_one_line(label: str, stats: dict[str, float | int]) -> None:
    print(
        f"{label:12} "
        f"fail={stats['failing']:4d} "
        f"touch={stats['touched']:4d} "
        f"resc={stats['rescued']:4d} "
        f"unresc={stats['unrecovered']:4d} "
        f"themes_touch={stats['theme_touched']:3d} "
        f"themes_unresc={stats['theme_unrecovered']:3d} "
        f"avg_d={stats['avg_delta']:.4f} "
        f"med_d={stats['median_delta']:.4f} "
        f"avg_l={stats['avg_lightness_delta']:.4f}"
    )


def print_current_threshold_report(cases: list[AccentCase]) -> None:
    print("Current threshold report")
    print("------------------------")
    for appearance, threshold in (
        ("dark", BACKGROUND_APCA_DARK),
        ("light", BACKGROUND_APCA_LIGHT),
    ):
        group = [case for case in cases if case.appearance == appearance]
        explicit = [case for case in group if case.explicit_accents]
        fallback = [case for case in group if not case.explicit_accents]
        opaque = [case for case in group if case.original.alpha() >= 0.999]
        translucent = [case for case in group if case.original.alpha() < 0.999]

        print()
        print(f"{appearance} threshold={threshold:.1f}")
        print_one_line("all", summarize_group(group, threshold))
        print_one_line("explicit", summarize_group(explicit, threshold))
        print_one_line("fallback", summarize_group(fallback, threshold))
        print_one_line("opaque", summarize_group(opaque, threshold))
        print_one_line("alpha<1", summarize_group(translucent, threshold))


def print_sweep(
    cases: list[AccentCase],
    appearance: str,
    thresholds: list[float],
) -> None:
    group = [case for case in cases if case.appearance == appearance]
    explicit = [case for case in group if case.explicit_accents]
    fallback = [case for case in group if not case.explicit_accents]

    print()
    print(f"{appearance} sweep")
    print("------------")
    print(
        f"{'thr':>5} {'group':12} {'fail':>5} {'touch':>6} {'resc':>5} "
        f"{'unresc':>6} {'t_themes':>8} {'u_themes':>8} {'avg_d':>7}"
    )
    for threshold in thresholds:
        for label, subset in (("all", group), ("explicit", explicit), ("fallback", fallback)):
            stats = summarize_group(subset, threshold)
            print(
                f"{threshold:5.1f} {label:12} {stats['failing']:5d} {stats['touched']:6d} "
                f"{stats['rescued']:5d} {stats['unrecovered']:6d} {stats['theme_touched']:8d} "
                f"{stats['theme_unrecovered']:8d} {stats['avg_delta']:7.4f}"
            )
        print()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dark-start", type=float, default=20.0)
    parser.add_argument("--dark-stop", type=float, default=40.0)
    parser.add_argument("--light-start", type=float, default=25.0)
    parser.add_argument("--light-stop", type=float, default=45.0)
    parser.add_argument("--step", type=float, default=2.5)
    args = parser.parse_args()

    cases = build_cases()
    print_current_threshold_report(cases)
    print_sweep(cases, "dark", frange(args.dark_start, args.dark_stop, args.step))
    print_sweep(cases, "light", frange(args.light_start, args.light_stop, args.step))


if __name__ == "__main__":
    main()
