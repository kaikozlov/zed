from __future__ import annotations

from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import authored_order
from strategy_sweep import cut_then_move_orders, make_candidate
from theme_pipeline import adjust_color_for_background, minimum_background_contrast


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


def build_cases(appearance: str):
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
        structured_candidates = [
            make_candidate("cut-then-move-threshold", colors, background, order)
            for order in dict.fromkeys(cut_then_move_orders(len(colors)))
        ]
        cases.append((authored, structured_candidates))
    return cases


def sweep(appearance: str, thresholds: list[float], epsilon: float = 0.0) -> None:
    cases = build_cases(appearance)
    print()
    print(f"{appearance} explicit themes: {len(cases)}")
    print(
        f"{'threshold':>9} {'authored':>8} {'structured':>11} {'changed':>8} {'avg_disp':>9} {'avg_min':>8}"
    )
    for threshold in thresholds:
        adjusted_threshold = threshold - epsilon
        chosen = []
        authored_pass = 0
        structured_pass = 0
        for authored, candidates in cases:
            if authored.min_adjacent_distance >= adjusted_threshold:
                chosen.append(authored)
                authored_pass += 1
                structured_pass += 1
                continue
            structured = threshold_pick(candidates, adjusted_threshold)
            chosen.append(structured)
            if structured.min_adjacent_distance >= adjusted_threshold:
                structured_pass += 1

        print(
            f"{threshold:9.3f} {authored_pass:8d} {structured_pass:11d} "
            f"{sum(candidate.name != 'authored' for candidate in chosen):8d} "
            f"{mean(candidate.displacement_cost for candidate in chosen):9.2f} "
            f"{mean(candidate.min_adjacent_distance for candidate in chosen):8.4f}"
        )


def main() -> None:
    sweep("dark", [0.06, 0.07, 0.08, 0.09, 0.10, 0.11, 0.12])
    sweep("light", [0.08, 0.09, 0.10, 0.11, 0.12, 0.13, 0.14])
    print()
    print("With epsilon 0.001")
    sweep("dark", [0.06, 0.07, 0.08, 0.09, 0.10, 0.11, 0.12], epsilon=0.001)
    sweep("light", [0.08, 0.09, 0.10, 0.11, 0.12, 0.13, 0.14], epsilon=0.001)


if __name__ == "__main__":
    main()
