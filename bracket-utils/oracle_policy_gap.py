from __future__ import annotations

import argparse
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import authored_order
from strategy_sweep import (
    cheapest_passing_oracle,
    cut_then_move_orders,
    cut_interleave_orders,
    make_candidate,
    move_one_orders,
)
from theme_pipeline import (
    adjust_color_for_background,
    minimum_background_contrast,
    target_threshold,
)


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


def build_candidates(name, colors, background, orders):
    return [
        make_candidate(name, colors, background, order)
        for order in dict.fromkeys(orders)
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--epsilon", type=float, default=0.0)
    args = parser.parse_args()

    results = [
        result
        for result in load_results()
        if result.inputs.explicit_accents and len(result.inputs.accents) <= 9
    ]

    stats = {
        "move-then-cut": {
            "exact_matches": 0,
            "exact_cost_matches": 0,
            "pass_matches": 0,
            "cost_gaps": [],
            "score_gaps": [],
            "examples": [],
        },
        "cut-then-move": {
            "exact_matches": 0,
            "exact_cost_matches": 0,
            "pass_matches": 0,
            "cost_gaps": [],
            "score_gaps": [],
            "examples": [],
        },
    }
    total = 0

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
        threshold = target_threshold(result.inputs.appearance) - args.epsilon
        authored = make_candidate(
            "authored",
            colors,
            background,
            authored_order(len(colors)),
        )
        move = threshold_pick(
            build_candidates(
                "move-threshold",
                colors,
                background,
                move_one_orders(len(colors)),
            ),
            threshold,
        )
        cut = threshold_pick(
            build_candidates(
                "cut-threshold",
                colors,
                background,
                cut_interleave_orders(len(colors)),
            ),
            threshold,
        )
        cut_then_move = threshold_pick(
            build_candidates(
                "cut-then-move-threshold",
                colors,
                background,
                cut_then_move_orders(len(colors)),
            ),
            threshold,
        )
        policies = {
            "move-then-cut": authored
            if authored.min_adjacent_distance >= threshold
            else (move if move.min_adjacent_distance >= threshold else cut),
            "cut-then-move": cut_then_move,
        }

        oracle = cheapest_passing_oracle(colors, background, threshold)
        if oracle is None:
            continue

        total += 1
        for name, policy in policies.items():
            if policy.order == oracle.order:
                stats[name]["exact_matches"] += 1
            if policy.displacement_cost == oracle.displacement_cost:
                stats[name]["exact_cost_matches"] += 1
            if (
                policy.min_adjacent_distance >= threshold
                and oracle.min_adjacent_distance >= threshold
            ):
                stats[name]["pass_matches"] += 1

            stats[name]["cost_gaps"].append(
                policy.displacement_cost - oracle.displacement_cost
            )
            stats[name]["score_gaps"].append(
                policy.min_adjacent_distance - oracle.min_adjacent_distance
            )
            if policy.displacement_cost != oracle.displacement_cost:
                stats[name]["examples"].append(
                    (
                        result.inputs.theme_name,
                        result.inputs.appearance,
                        len(colors),
                        policy.displacement_cost,
                        oracle.displacement_cost,
                        policy.order,
                        oracle.order,
                    )
                )

    print("oracle cases", total)
    for name, policy_stats in stats.items():
        print()
        print(name)
        print(" exact order matches", policy_stats["exact_matches"])
        print(" exact cost matches", policy_stats["exact_cost_matches"])
        print(" policy pass matches", policy_stats["pass_matches"])
        print(" avg displacement gap", round(mean(policy_stats["cost_gaps"]), 3))
        print(" max displacement gap", max(policy_stats["cost_gaps"]))
        print(" avg min-adj gap", round(mean(policy_stats["score_gaps"]), 4))
        print(" max min-adj gap", round(max(policy_stats["score_gaps"]), 4))
        print(" examples")
        for example in policy_stats["examples"][:10]:
            print("  ", example)


if __name__ == "__main__":
    main()
