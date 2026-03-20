from __future__ import annotations

import argparse
from dataclasses import dataclass
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import authored_order
from strategy_sweep import cut_then_move_orders, make_candidate
from theme_pipeline import adjust_color_for_background, color_distance_oklab, target_threshold


@dataclass(frozen=True)
class ThemeCase:
    appearance: str
    explicit_accents: bool
    background: Color
    original_colors: tuple[Color, ...]


def threshold_pick(candidates, threshold):
    passers = [
        candidate
        for candidate in candidates
        if candidate.min_adjacent_distance >= threshold
    ]
    if passers:
        return min(
            passers,
            key=lambda candidate: (
                candidate.displacement_cost,
                candidate.inversion_count,
                -candidate.min_adjacent_distance,
            ),
        )
    return max(
        candidates,
        key=lambda candidate: (
            candidate.min_adjacent_distance,
            -candidate.inversion_count,
            -candidate.displacement_cost,
        ),
    )


def frange(start: float, stop: float, step: float) -> list[float]:
    values: list[float] = []
    current = start
    while current <= stop + 1e-9:
        values.append(round(current, 3))
        current += step
    return values


def build_theme_cases() -> list[ThemeCase]:
    cases: list[ThemeCase] = []
    for result in load_results():
        cases.append(
            ThemeCase(
                appearance=result.inputs.appearance,
                explicit_accents=result.inputs.explicit_accents,
                background=Color(result.inputs.background),
                original_colors=tuple(Color(value) for value in result.inputs.accents),
            )
        )
    return cases


def evaluate_group(cases, appearance: str, background_threshold: float) -> dict[str, float | int]:
    group = [case for case in cases if case.appearance == appearance]
    final_threshold = target_threshold(appearance)

    pass_count = 0
    themes_bg_touched = 0
    themes_reordered = 0
    themes_final_changed = 0
    accent_bg_touched = 0
    color_deltas: list[float] = []
    final_scores: list[float] = []

    for case in group:
        background = case.background
        original_colors = [color.clone().convert("srgb", fit=True) for color in case.original_colors]
        adjusted_colors = [
            adjust_color_for_background(color, background, background_threshold)
            for color in original_colors
        ]

        bg_changed = False
        for original, adjusted in zip(original_colors, adjusted_colors):
            if original.convert("srgb", fit=True).to_string(hex=True) != adjusted.convert("srgb", fit=True).to_string(hex=True):
                bg_changed = True
                accent_bg_touched += 1
                color_deltas.append(
                    color_distance_oklab(
                        original.convert("oklab"),
                        adjusted.convert("oklab"),
                    )
                )
        if bg_changed:
            themes_bg_touched += 1

        authored = make_candidate(
            "authored",
            adjusted_colors,
            background,
            authored_order(len(adjusted_colors)),
        )
        if authored.min_adjacent_distance >= final_threshold:
            chosen = authored
        else:
            candidates = [
                make_candidate("structured", adjusted_colors, background, order)
                for order in dict.fromkeys(cut_then_move_orders(len(adjusted_colors)))
            ]
            chosen = threshold_pick(candidates, final_threshold)

        if chosen.name != "authored":
            themes_reordered += 1
        if bg_changed or chosen.name != "authored":
            themes_final_changed += 1
        if chosen.min_adjacent_distance >= final_threshold:
            pass_count += 1
        final_scores.append(chosen.min_adjacent_distance)

    return {
        "theme_count": len(group),
        "pass_count": pass_count,
        "themes_bg_touched": themes_bg_touched,
        "themes_reordered": themes_reordered,
        "themes_final_changed": themes_final_changed,
        "accent_bg_touched": accent_bg_touched,
        "avg_color_delta": mean(color_deltas) if color_deltas else 0.0,
        "avg_final_min_adj": mean(final_scores) if final_scores else 0.0,
    }


def print_sweep(cases, appearance: str, thresholds: list[float]) -> None:
    print()
    print(f"{appearance} background-floor sweep")
    print("-----------------------------")
    print(
        f"{'bg_thr':>6} {'group':10} {'pass':>5} {'bg_themes':>9} {'reorder':>8} "
        f"{'changed':>8} {'bg_acc':>7} {'avg_d':>7} {'avg_min':>8}"
    )
    explicit = [case for case in cases if case.explicit_accents]
    fallback = [case for case in cases if not case.explicit_accents]
    for threshold in thresholds:
        for label, subset in (("all", cases), ("explicit", explicit), ("fallback", fallback)):
            stats = evaluate_group(subset, appearance, threshold)
            print(
                f"{threshold:6.1f} {label:10} {stats['pass_count']:5d} {stats['themes_bg_touched']:9d} "
                f"{stats['themes_reordered']:8d} {stats['themes_final_changed']:8d} {stats['accent_bg_touched']:7d} "
                f"{stats['avg_color_delta']:7.4f} {stats['avg_final_min_adj']:8.4f}"
            )
        print()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dark-start", type=float, default=22.5)
    parser.add_argument("--dark-stop", type=float, default=32.5)
    parser.add_argument("--light-start", type=float, default=30.0)
    parser.add_argument("--light-stop", type=float, default=40.0)
    parser.add_argument("--step", type=float, default=2.5)
    args = parser.parse_args()

    cases = build_theme_cases()
    print_sweep(cases, "dark", frange(args.dark_start, args.dark_stop, args.step))
    print_sweep(cases, "light", frange(args.light_start, args.light_stop, args.step))


if __name__ == "__main__":
    main()
