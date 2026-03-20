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


def evaluate(appearance: str, gate: float, target: float, epsilon: float = 0.0) -> None:
    results = build_cases(appearance)
    chosen = []
    changed = 0
    pass_count = 0
    for authored, candidates in results:
        if authored.min_adjacent_distance >= gate - epsilon:
            chosen.append(authored)
            if authored.min_adjacent_distance >= target - epsilon:
                pass_count += 1
            continue

        structured = threshold_pick(candidates, target - epsilon)
        chosen.append(structured)
        changed += 1
        if structured.min_adjacent_distance >= target - epsilon:
            pass_count += 1

    print(
        f"{appearance} gate={gate:.3f} target={target:.3f} epsilon={epsilon:.3f} "
        f"themes={len(results)} pass={pass_count} changed={changed} "
        f"avg_disp={mean(candidate.displacement_cost for candidate in chosen):.2f} "
        f"avg_min={mean(candidate.min_adjacent_distance for candidate in chosen):.4f}"
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
        candidates = [
            make_candidate("structured", colors, background, order)
            for order in dict.fromkeys(cut_then_move_orders(len(colors)))
        ]
        cases.append((authored, candidates))
    return cases


def main() -> None:
    evaluate("dark", gate=0.080, target=0.080)
    evaluate("dark", gate=0.075, target=0.080)
    evaluate("dark", gate=0.070, target=0.080)
    evaluate("light", gate=0.095, target=0.100)
    evaluate("light", gate=0.090, target=0.100)
    evaluate("light", gate=0.090, target=0.095)
    print("With epsilon 0.001")
    evaluate("dark", gate=0.080, target=0.080, epsilon=0.001)
    evaluate("dark", gate=0.075, target=0.080, epsilon=0.001)
    evaluate("light", gate=0.095, target=0.100, epsilon=0.001)
    evaluate("light", gate=0.090, target=0.100, epsilon=0.001)


if __name__ == "__main__":
    main()
