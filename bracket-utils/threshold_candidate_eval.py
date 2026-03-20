from __future__ import annotations

import argparse
from collections import Counter
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import authored_order
from strategy_sweep import (
    cut_then_move_orders,
    StrategyCandidate,
    cut_interleave_orders,
    make_candidate,
    move_one_orders,
    rotation_window_orders,
)
from theme_pipeline import (
    adjust_color_for_background,
    minimum_background_contrast,
    target_threshold,
)


def moved_count(order: tuple[int, ...]) -> int:
    return sum(index != value for index, value in enumerate(order))


def single_swap_orders(length: int) -> list[tuple[int, ...]]:
    baseline = list(range(length))
    orders = [tuple(baseline)]
    for left in range(1, length):
        for right in range(left + 1, length):
            order = baseline.copy()
            order[left], order[right] = order[right], order[left]
            orders.append(tuple(order))
    return orders


def threshold_pick(
    name: str,
    colors: list[Color],
    background: Color,
    orders: list[tuple[int, ...]],
    threshold: float,
) -> StrategyCandidate:
    candidates = [
        make_candidate(name, colors, background, order)
        for order in dict.fromkeys(orders)
    ]
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
                moved_count(candidate.order),
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


def choose_local_then_cut(
    authored: StrategyCandidate,
    swap: StrategyCandidate,
    move: StrategyCandidate,
    rotation: StrategyCandidate,
    cut: StrategyCandidate,
    cut_then_move: StrategyCandidate,
    threshold: float,
) -> StrategyCandidate:
    if authored.min_adjacent_distance >= threshold:
        return authored

    passers = [
        candidate
        for candidate in (swap, move, rotation)
        if candidate.min_adjacent_distance >= threshold
    ]
    if passers:
        return min(
            passers,
            key=lambda candidate: (
                candidate.displacement_cost,
                moved_count(candidate.order),
                candidate.inversion_count,
                -candidate.min_adjacent_distance,
            ),
        )
    return cut


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--group",
        choices=["all", "explicit", "fallback"],
        default="explicit",
    )
    parser.add_argument("--epsilon", type=float, default=0.0)
    parser.add_argument("--show-cut-themes", action="store_true")
    parser.add_argument("--show-differences", action="store_true")
    args = parser.parse_args()

    results = load_results()
    if args.group == "explicit":
        results = [result for result in results if result.inputs.explicit_accents]
    elif args.group == "fallback":
        results = [result for result in results if not result.inputs.explicit_accents]

    rows: list[
        tuple[
            str,
            str,
            float,
            StrategyCandidate,
            StrategyCandidate,
            StrategyCandidate,
            StrategyCandidate,
            StrategyCandidate,
            StrategyCandidate,
        ]
    ] = []

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
        length = len(colors)
        authored = make_candidate(
            "authored",
            colors,
            background,
            authored_order(length),
        )
        swap = threshold_pick(
            "swap-threshold",
            colors,
            background,
            single_swap_orders(length),
            threshold,
        )
        move = threshold_pick(
            "move-threshold",
            colors,
            background,
            move_one_orders(length),
            threshold,
        )
        rotation = threshold_pick(
            "rotation-threshold",
            colors,
            background,
            rotation_window_orders(length, max_window=4),
            threshold,
        )
        cut = threshold_pick(
            "cut-threshold",
            colors,
            background,
            cut_interleave_orders(length),
            threshold,
        )
        cut_then_move = threshold_pick(
            "cut-then-move-threshold",
            colors,
            background,
            cut_then_move_orders(length),
            threshold,
        )
        rows.append(
            (
                result.inputs.theme_name,
                result.inputs.appearance,
                threshold,
                authored,
                swap,
                move,
                rotation,
                cut,
                cut_then_move,
            )
        )

    print(
        f"{'family':18} {'pass':>5} {'avg_disp':>9} {'avg_moved':>10} {'avg_min':>8}"
    )
    for name, index in (
        ("authored", 3),
        ("swap-threshold", 4),
        ("move-threshold", 5),
        ("rotation-threshold", 6),
        ("cut-threshold", 7),
        ("cut-then-move-threshold", 8),
    ):
        selected = [row[index] for row in rows]
        thresholds = [row[2] for row in rows]
        print(
            f"{name:18} "
            f"{sum(candidate.min_adjacent_distance >= threshold for candidate, threshold in zip(selected, thresholds)):5d} "
            f"{mean(candidate.displacement_cost for candidate in selected):9.2f} "
            f"{mean(moved_count(candidate.order) for candidate in selected):10.2f} "
            f"{mean(candidate.min_adjacent_distance for candidate in selected):8.4f}"
        )

    policies = {
        "move-then-cut": lambda authored, swap, move, rotation, cut, cut_then_move, threshold: authored
        if authored.min_adjacent_distance >= threshold
        else (move if move.min_adjacent_distance >= threshold else cut),
        "rotation-then-cut": lambda authored, swap, move, rotation, cut, cut_then_move, threshold: authored
        if authored.min_adjacent_distance >= threshold
        else (rotation if rotation.min_adjacent_distance >= threshold else cut),
        "swap-then-cut": lambda authored, swap, move, rotation, cut, cut_then_move, threshold: authored
        if authored.min_adjacent_distance >= threshold
        else (swap if swap.min_adjacent_distance >= threshold else cut),
        "move-then-cut-move": lambda authored, swap, move, rotation, cut, cut_then_move, threshold: authored
        if authored.min_adjacent_distance >= threshold
        else (move if move.min_adjacent_distance >= threshold else cut_then_move),
        "local-then-cut": choose_local_then_cut,
    }

    print()
    print(
        f"{'policy':18} {'pass':>5} {'changed':>8} {'avg_disp':>9} {'avg_moved':>10} {'avg_min':>8}"
    )
    for name, policy in policies.items():
        chosen = [
            policy(authored, swap, move, rotation, cut, cut_then_move, threshold)
            for _, _, threshold, authored, swap, move, rotation, cut, cut_then_move in rows
        ]
        thresholds = [row[2] for row in rows]
        print(
            f"{name:18} "
            f"{sum(candidate.min_adjacent_distance >= threshold for candidate, threshold in zip(chosen, thresholds)):5d} "
            f"{sum(candidate.name != 'authored' for candidate in chosen):8d} "
            f"{mean(candidate.displacement_cost for candidate in chosen):9.2f} "
            f"{mean(moved_count(candidate.order) for candidate in chosen):10.2f} "
            f"{mean(candidate.min_adjacent_distance for candidate in chosen):8.4f}"
        )

    winner_counter: Counter[str] = Counter()
    move_local_differences: list[str] = []
    cut_themes: list[str] = []
    for theme_name, appearance, threshold, authored, swap, move, rotation, cut, cut_then_move in rows:
        passers = [
            candidate
            for candidate in (authored, swap, move, rotation, cut, cut_then_move)
            if candidate.min_adjacent_distance >= threshold
        ]
        if not passers:
            continue
        chosen = min(
            passers,
            key=lambda candidate: (
                candidate.displacement_cost,
                moved_count(candidate.order),
                candidate.inversion_count,
                -candidate.min_adjacent_distance,
            ),
        )
        winner_counter[chosen.name] += 1
        move_choice = policies["move-then-cut"](
            authored, swap, move, rotation, cut, cut_then_move, threshold
        )
        local_choice = policies["local-then-cut"](
            authored, swap, move, rotation, cut, cut_then_move, threshold
        )
        if move_choice.order != local_choice.order:
            move_local_differences.append(
                f"{theme_name} ({appearance}): {move_choice.name}->{local_choice.name}"
            )
        if move_choice.name == "cut-threshold":
            cut_themes.append(
                f"{theme_name} ({appearance}) -> {cut.order} @ {cut.min_adjacent_distance:.4f}"
            )

    print()
    print("Threshold-aware cheapest passers:", dict(winner_counter))
    print("move-then-cut vs local-then-cut differences:", len(move_local_differences))
    if args.show_differences:
        for difference in move_local_differences:
            print("  ", difference)
    if args.show_cut_themes:
        print("Cut fallback themes:")
        for cut_theme in cut_themes:
            print("  ", cut_theme)


if __name__ == "__main__":
    main()
