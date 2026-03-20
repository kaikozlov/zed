from __future__ import annotations

import argparse
from dataclasses import dataclass
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from theme_pipeline import (
    adjust_color_for_background,
    background_contrast,
    color_distance_oklab,
    color_equal,
)


@dataclass(frozen=True)
class AccentCase:
    appearance: str
    theme_name: str
    original: Color
    background: Color


def build_cases() -> list[AccentCase]:
    cases: list[AccentCase] = []
    for result in load_results():
        if not result.inputs.explicit_accents:
            continue
        background = Color(result.inputs.background)
        for accent in result.inputs.accents:
            cases.append(
                AccentCase(
                    appearance=result.inputs.appearance,
                    theme_name=result.inputs.theme_name,
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


def print_group(
    cases: list[AccentCase],
    appearance: str,
    thresholds: list[float],
) -> None:
    group = [case for case in cases if case.appearance == appearance]
    print(f"{appearance}: {len(group)} explicit accents")
    print()
    print(
        f"{'thr':>5} {'fail':>5} {'touch':>6} "
        f"{'resc':>5} {'unresc':>6} {'avg_d':>7}"
    )

    for threshold in thresholds:
        failing_cases = [
            case
            for case in group
            if background_contrast(case.original, case.background) < threshold
        ]
        changed_distances: list[float] = []
        rescued = 0
        unrecovered = 0

        for case in failing_cases:
            adjusted = adjust_color_for_background(
                case.original,
                case.background,
                threshold,
            )
            if not color_equal(adjusted, case.original):
                changed_distances.append(
                    color_distance_oklab(
                        case.original.convert("oklab"),
                        adjusted.convert("oklab"),
                    )
                )

            if background_contrast(adjusted, case.background) >= threshold:
                rescued += 1
            else:
                unrecovered += 1

        print(
            f"{threshold:5.1f} "
            f"{len(failing_cases):5d} "
            f"{len(changed_distances):6d} "
            f"{rescued:5d} "
            f"{unrecovered:6d} "
            f"{(mean(changed_distances) if changed_distances else 0.0):7.4f}"
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
