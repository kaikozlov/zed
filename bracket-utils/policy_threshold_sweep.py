from __future__ import annotations

from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import authored_order
from strategy_sweep import (
    StrategyCandidate,
    cut_interleave_orders,
    make_candidate,
    move_one_orders,
)
from theme_pipeline import adjust_color_for_background, minimum_background_contrast


def build_candidates(
    name: str,
    colors: list[Color],
    background: Color,
    orders: list[tuple[int, ...]],
) -> list[StrategyCandidate]:
    return [
        make_candidate(name, colors, background, order)
        for order in dict.fromkeys(orders)
    ]


def threshold_pick(
    candidates: list[StrategyCandidate],
    threshold: float,
) -> StrategyCandidate:
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


def build_cases(appearance: str) -> list[tuple[StrategyCandidate, list[StrategyCandidate], list[StrategyCandidate]]]:
    results = [
        result
        for result in load_results()
        if result.inputs.explicit_accents and result.inputs.appearance == appearance
    ]
    cases = []
    for result in results:
        background = Color(result.inputs.background)
        colors = [
            adjust_color_for_background(
                Color(value),
                background,
                minimum_background_contrast(result.inputs.appearance),
            )
            for value in result.inputs.accents
        ]
        authored = make_candidate(
            "authored",
            colors,
            background,
            authored_order(len(colors)),
        )
        move_candidates = build_candidates(
            "move-threshold",
            colors,
            background,
            move_one_orders(len(colors)),
        )
        cut_candidates = build_candidates(
            "cut-threshold",
            colors,
            background,
            cut_interleave_orders(len(colors)),
        )
        cases.append((authored, move_candidates, cut_candidates))
    return cases


def sweep(appearance: str, thresholds: list[float], epsilon: float = 0.0) -> None:
    cases = build_cases(appearance)

    print()
    print(f"{appearance} explicit themes: {len(cases)}")
    print(
        f"{'threshold':>9} {'authored':>8} {'move':>6} {'move+cut':>9} {'cut_used':>8} {'avg_disp':>9}"
    )

    for threshold in thresholds:
        adjusted_threshold = threshold - epsilon
        authored_pass = 0
        move_pass = 0
        move_cut_pass = 0
        cut_used = 0
        chosen = []

        for authored, move_candidates, cut_candidates in cases:
            move = threshold_pick(move_candidates, adjusted_threshold)
            cut = threshold_pick(cut_candidates, adjusted_threshold)

            if authored.min_adjacent_distance >= adjusted_threshold:
                authored_pass += 1
                move_pass += 1
                move_cut_pass += 1
                chosen.append(authored)
                continue

            if move.min_adjacent_distance >= adjusted_threshold:
                move_pass += 1
                move_cut_pass += 1
                chosen.append(move)
                continue

            chosen.append(cut)
            cut_used += 1
            if cut.min_adjacent_distance >= adjusted_threshold:
                move_cut_pass += 1

        print(
            f"{threshold:9.3f} {authored_pass:8d} {move_pass:6d} {move_cut_pass:9d} {cut_used:8d} {mean(candidate.displacement_cost for candidate in chosen):9.2f}"
        )


def main() -> None:
    sweep("dark", [0.06, 0.07, 0.08, 0.09, 0.10, 0.11, 0.12])
    sweep("light", [0.08, 0.09, 0.10, 0.11, 0.12, 0.13, 0.14])
    sweep("dark", [0.070, 0.075, 0.080, 0.085, 0.090])
    sweep("light", [0.090, 0.095, 0.100, 0.105, 0.110])
    print()
    print("With epsilon 0.001")
    sweep("dark", [0.06, 0.07, 0.08, 0.09, 0.10, 0.11, 0.12], epsilon=0.001)
    sweep("light", [0.08, 0.09, 0.10, 0.11, 0.12, 0.13, 0.14], epsilon=0.001)
    sweep("dark", [0.070, 0.075, 0.080, 0.085, 0.090], epsilon=0.001)
    sweep("light", [0.090, 0.095, 0.100, 0.105, 0.110], epsilon=0.001)


if __name__ == "__main__":
    main()
