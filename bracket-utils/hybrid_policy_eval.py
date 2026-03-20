from __future__ import annotations

import argparse
from math import sqrt
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from strategy_sweep import build_strategy_candidates
from theme_pipeline import (
    adjust_color_for_background,
    composite_over_background,
    minimum_background_contrast,
    target_threshold,
)


def oklab_distance(left: Color, right: Color) -> float:
    left_coords = left.convert("oklab").coords()
    right_coords = right.convert("oklab").coords()
    return sqrt(sum((left_component - right_component) ** 2 for left_component, right_component in zip(left_coords, right_coords)))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--group",
        choices=["all", "explicit", "fallback"],
        default="explicit",
    )
    args = parser.parse_args()

    results = load_results()
    if args.group == "explicit":
        results = [result for result in results if result.inputs.explicit_accents]
    elif args.group == "fallback":
        results = [result for result in results if not result.inputs.explicit_accents]
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
        threshold = target_threshold(result.inputs.appearance)
        distances = []
        for index in range(len(colors)):
            left = composite_over_background(colors[index], background)
            right = composite_over_background(colors[(index + 1) % len(colors)], background)
            distances.append(oklab_distance(left, right))
        bad_edges = sum(distance < threshold for distance in distances)
        deficit = max(0.0, threshold - min(distances))
        candidates = build_strategy_candidates(colors, background)
        cases.append((threshold, bad_edges, deficit, candidates))

    def cheapest_local_passer(threshold, candidates):
        local_names = [
            "best-rotation-window-4",
            "best-move-one",
            "best-single-swap",
        ]
        passers = [
            candidates[name]
            for name in local_names
            if candidates[name].min_adjacent_distance >= threshold
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
            (candidates[name] for name in local_names),
            key=lambda candidate: (
                candidate.min_adjacent_distance,
                -candidate.inversion_count,
                -candidate.displacement_cost,
            ),
        )

    policies = {
        "move_only": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else candidates["best-move-one"],
        "rot_only": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else candidates["best-rotation-window-4"],
        "swap_only": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else candidates["best-single-swap"],
        "cut_only": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else candidates["best-cut-interleave"],
        "bad<=2_move_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-move-one"]
            if bad_edges <= 2
            else candidates["best-cut-interleave"]
        ),
        "bad<=1_move_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-move-one"]
            if bad_edges <= 1
            else candidates["best-cut-interleave"]
        ),
        "bad<=2_rot_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if bad_edges <= 2
            else candidates["best-cut-interleave"]
        ),
        "bad<=1_rot_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if bad_edges <= 1
            else candidates["best-cut-interleave"]
        ),
        "bad<=2_swap_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-single-swap"]
            if bad_edges <= 2
            else candidates["best-cut-interleave"]
        ),
        "bad1_rot_bad2_move_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if bad_edges == 1
            else (
                candidates["best-move-one"]
                if bad_edges == 2
                else candidates["best-cut-interleave"]
            )
        ),
        "bad1_rot_bad2_swap_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if bad_edges == 1
            else (
                candidates["best-single-swap"]
                if bad_edges == 2
                else candidates["best-cut-interleave"]
            )
        ),
        "def<0.02_rot_else_move": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if deficit < 0.02
            else candidates["best-move-one"]
        ),
        "def<0.02_rot_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if deficit < 0.02
            else candidates["best-cut-interleave"]
        ),
        "local_passer_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            cheapest_local_passer(threshold, candidates)
            if cheapest_local_passer(threshold, candidates).min_adjacent_distance >= threshold
            else candidates["best-cut-interleave"]
        ),
        "bad<=1_rot_bad2_move_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            candidates["best-rotation-window-4"]
            if bad_edges <= 1
            else (
                candidates["best-move-one"]
                if bad_edges == 2
                else candidates["best-cut-interleave"]
            )
        ),
        "bad<=2_cheapest_local_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            cheapest_local_passer(threshold, candidates)
            if bad_edges <= 2
            else candidates["best-cut-interleave"]
        ),
        "bad<=1_cheapest_local_bad2_move_else_cut": lambda threshold, bad_edges, deficit, candidates: candidates["authored"]
        if candidates["authored"].min_adjacent_distance >= threshold
        else (
            cheapest_local_passer(threshold, candidates)
            if bad_edges <= 1
            else (
                candidates["best-move-one"]
                if bad_edges == 2
                else candidates["best-cut-interleave"]
            )
        ),
    }

    print(f"{'policy':28} {'pass':>5} {'changed':>7} {'avg_disp_changed':>16} {'avg_min':>8} {'avg_disp_all':>12}")
    for name, policy in policies.items():
        chosen = [
            policy(threshold, bad_edges, deficit, candidates)
            for threshold, bad_edges, deficit, candidates in cases
        ]
        thresholds = [threshold for threshold, _, _, _ in cases]
        pass_count = sum(
            candidate.min_adjacent_distance >= threshold
            for candidate, threshold in zip(chosen, thresholds)
        )
        changed = [candidate for candidate in chosen if candidate.name != "authored"]
        print(
            f"{name:28} {pass_count:5d} {len(changed):7d} "
            f"{(mean(candidate.displacement_cost for candidate in changed) if changed else 0.0):16.2f} "
            f"{mean(candidate.min_adjacent_distance for candidate in chosen):8.4f} "
            f"{mean(candidate.displacement_cost for candidate in chosen):12.2f}"
        )


if __name__ == "__main__":
    main()
