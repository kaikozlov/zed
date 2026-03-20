from __future__ import annotations

import argparse
from dataclasses import dataclass
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from strategy_sweep import build_strategy_candidates
from theme_pipeline import adjust_color_for_background, minimum_background_contrast


@dataclass(frozen=True)
class ThemeCase:
    theme_name: str
    appearance: str
    authored_min_adj: float
    candidates: dict[str, float]
    displacements: dict[str, int]


def build_cases() -> list[ThemeCase]:
    cases: list[ThemeCase] = []
    for result in load_results():
        if not result.inputs.explicit_accents:
            continue

        background = Color(result.inputs.background)
        adjusted_colors = [
            adjust_color_for_background(
                Color(value),
                background,
                minimum_background_contrast(result.inputs.appearance),
            )
            for value in result.inputs.accents
        ]
        candidates = build_strategy_candidates(adjusted_colors, background)
        cases.append(
            ThemeCase(
                theme_name=result.inputs.theme_name,
                appearance=result.inputs.appearance,
                authored_min_adj=candidates["authored"].min_adjacent_distance,
                candidates={
                    name: candidate.min_adjacent_distance
                    for name, candidate in candidates.items()
                    if name != "optimal-anchored"
                },
                displacements={
                    name: candidate.displacement_cost
                    for name, candidate in candidates.items()
                    if name != "optimal-anchored"
                },
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


def print_group(
    cases: list[ThemeCase],
    appearance: str,
    thresholds: list[float],
) -> None:
    group = [case for case in cases if case.appearance == appearance]
    print(f"{appearance}: {len(group)} explicit multi-accent themes")
    print()
    print(
        f"{'thr':>5} {'auth':>5} {'need':>5} "
        f"{'move':>5} {'rot':>5} {'swap':>5} {'cut':>5} {'greedy':>6} "
        f"{'move_d':>7} {'rot_d':>7} {'cut_d':>7}"
    )

    for threshold in thresholds:
        authored_pass = sum(case.authored_min_adj >= threshold for case in group)
        needs_help = [case for case in group if case.authored_min_adj < threshold]

        def rescue_count(name: str) -> int:
            return sum(case.candidates[name] >= threshold for case in needs_help)

        def average_disp(name: str) -> float:
            values = [
                case.displacements[name]
                for case in needs_help
                if case.candidates[name] >= threshold
            ]
            return mean(values) if values else 0.0

        print(
            f"{threshold:5.3f} {authored_pass:5d} {len(needs_help):5d} "
            f"{rescue_count('best-move-one'):5d} "
            f"{rescue_count('best-rotation-window-4'):5d} "
            f"{rescue_count('best-single-swap'):5d} "
            f"{rescue_count('best-cut-interleave'):5d} "
            f"{rescue_count('greedy-anchored'):6d} "
            f"{average_disp('best-move-one'):7.2f} "
            f"{average_disp('best-rotation-window-4'):7.2f} "
            f"{average_disp('best-cut-interleave'):7.2f}"
        )
    print()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dark-start", type=float, default=0.06)
    parser.add_argument("--dark-stop", type=float, default=0.14)
    parser.add_argument("--light-start", type=float, default=0.08)
    parser.add_argument("--light-stop", type=float, default=0.18)
    parser.add_argument("--step", type=float, default=0.01)
    args = parser.parse_args()

    cases = build_cases()
    print_group(
        cases,
        "dark",
        frange(args.dark_start, args.dark_stop, args.step),
    )
    print_group(
        cases,
        "light",
        frange(args.light_start, args.light_stop, args.step),
    )


if __name__ == "__main__":
    main()
