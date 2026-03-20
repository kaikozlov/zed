from __future__ import annotations

import argparse
import math
from collections import Counter, defaultdict
from dataclasses import dataclass
from itertools import permutations
from statistics import mean

from coloraide import Color

from analyze_extension_themes import load_results
from palette_probe import (
    authored_order,
    anchored_stride_orders,
    displacement_cost,
    greedy_anchored_order,
    inversion_count,
    min_adjacent_distance,
    best_single_swap_order,
)
from theme_pipeline import (
    adjust_color_for_background,
    minimum_background_contrast,
    target_threshold,
)


MAX_ORACLE_ACCENTS = 9


@dataclass(frozen=True)
class StrategyCandidate:
    name: str
    order: tuple[int, ...]
    min_adjacent_distance: float
    inversion_count: int
    displacement_cost: int


def make_candidate(
    name: str,
    colors: list[Color],
    background: Color,
    order: tuple[int, ...],
) -> StrategyCandidate:
    return StrategyCandidate(
        name=name,
        order=order,
        min_adjacent_distance=min_adjacent_distance(colors, order, background),
        inversion_count=inversion_count(order),
        displacement_cost=displacement_cost(order),
    )


def candidate_sort_key(candidate: StrategyCandidate) -> tuple[float, int, int]:
    return (
        candidate.min_adjacent_distance,
        -candidate.inversion_count,
        -candidate.displacement_cost,
    )


def best_candidate(
    name: str,
    colors: list[Color],
    background: Color,
    orders: list[tuple[int, ...]],
) -> StrategyCandidate:
    unique_orders = list(dict.fromkeys(orders))
    candidates = [
        make_candidate(name, colors, background, order)
        for order in unique_orders
    ]
    return max(candidates, key=candidate_sort_key)


def move_one_orders(length: int) -> list[tuple[int, ...]]:
    baseline = list(range(length))
    orders = [tuple(baseline)]
    for source in range(1, length):
        for destination in range(1, length):
            if source == destination:
                continue
            order = baseline.copy()
            value = order.pop(source)
            order.insert(destination, value)
            orders.append(tuple(order))
    return orders


def best_move_one_candidate(
    colors: list[Color],
    background: Color,
) -> StrategyCandidate:
    return best_candidate(
        "best-move-one",
        colors,
        background,
        move_one_orders(len(colors)),
    )


def rotation_window_orders(length: int, max_window: int = 4) -> list[tuple[int, ...]]:
    baseline = list(range(length))
    orders = [tuple(baseline)]
    for start in range(1, length):
        for window_length in range(2, min(max_window, length - start) + 1):
            stop = start + window_length
            segment = baseline[start:stop]
            left_rotated = baseline.copy()
            left_rotated[start:stop] = segment[1:] + segment[:1]
            orders.append(tuple(left_rotated))
            right_rotated = baseline.copy()
            right_rotated[start:stop] = segment[-1:] + segment[:-1]
            orders.append(tuple(right_rotated))
    return orders


def best_rotation_window_candidate(
    colors: list[Color],
    background: Color,
    max_window: int = 4,
) -> StrategyCandidate:
    return best_candidate(
        f"best-rotation-window-{max_window}",
        colors,
        background,
        rotation_window_orders(len(colors), max_window=max_window),
    )


def even_odd_order(length: int) -> tuple[int, ...]:
    return (0, *range(2, length, 2), *range(1, length, 2))


def outside_in_order(length: int) -> tuple[int, ...]:
    order = [0]
    left = 1
    right = length - 1
    while left <= right:
        order.append(right)
        if left != right:
            order.append(left)
        left += 1
        right -= 1
    return tuple(order)


def cut_interleave_orders(length: int) -> list[tuple[int, ...]]:
    if length < 4:
        return [authored_order(length)]

    orders: list[tuple[int, ...]] = [authored_order(length)]
    for cut in range(2, length):
        left = list(range(1, cut))
        right = list(range(cut, length))

        right_first = [0]
        for index in range(max(len(left), len(right))):
            if index < len(right):
                right_first.append(right[index])
            if index < len(left):
                right_first.append(left[index])
        orders.append(tuple(right_first))

        left_first = [0]
        for index in range(max(len(left), len(right))):
            if index < len(left):
                left_first.append(left[index])
            if index < len(right):
                left_first.append(right[index])
        orders.append(tuple(left_first))

    return orders


def best_cut_interleave_candidate(
    colors: list[Color],
    background: Color,
) -> StrategyCandidate:
    return best_candidate(
        "best-cut-interleave",
        colors,
        background,
        cut_interleave_orders(len(colors)),
    )


def cut_then_move_orders(length: int) -> list[tuple[int, ...]]:
    orders: list[tuple[int, ...]] = []
    for cut_order in cut_interleave_orders(length):
        baseline = list(cut_order)
        orders.append(tuple(baseline))
        for source in range(1, length):
            for destination in range(1, length):
                if source == destination:
                    continue
                order = baseline.copy()
                value = order.pop(source)
                order.insert(destination, value)
                orders.append(tuple(order))
    return orders


def best_cut_then_move_candidate(
    colors: list[Color],
    background: Color,
) -> StrategyCandidate:
    return best_candidate(
        "best-cut-then-move",
        colors,
        background,
        cut_then_move_orders(len(colors)),
    )


def best_stride_candidate(
    colors: list[Color],
    background: Color,
) -> StrategyCandidate:
    orders = anchored_stride_orders(len(colors)) or [authored_order(len(colors))]
    return best_candidate("best-stride", colors, background, orders)


def optimal_anchored_candidate(
    colors: list[Color],
    background: Color,
) -> StrategyCandidate | None:
    if len(colors) > MAX_ORACLE_ACCENTS:
        return None
    orders = [
        (0, *tail_order)
        for tail_order in permutations(range(1, len(colors)))
    ]
    return best_candidate("optimal-anchored", colors, background, orders)


def cheapest_passing_oracle(
    colors: list[Color],
    background: Color,
    threshold: float,
) -> StrategyCandidate | None:
    if len(colors) > MAX_ORACLE_ACCENTS:
        return None

    passers: list[StrategyCandidate] = []
    for tail_order in permutations(range(1, len(colors))):
        candidate = make_candidate(
            "oracle-cheapest-pass",
            colors,
            background,
            (0, *tail_order),
        )
        if candidate.min_adjacent_distance >= threshold:
            passers.append(candidate)

    if not passers:
        return None

    return min(
        passers,
        key=lambda candidate: (
            candidate.displacement_cost,
            candidate.inversion_count,
            -candidate.min_adjacent_distance,
        ),
    )


def build_strategy_candidates(
    colors: list[Color],
    background: Color,
) -> dict[str, StrategyCandidate]:
    candidates = {
        "authored": make_candidate(
            "authored",
            colors,
            background,
            authored_order(len(colors)),
        ),
        "best-single-swap": make_candidate(
            "best-single-swap",
            colors,
            background,
            best_single_swap_order(colors, background),
        ),
        "best-move-one": best_move_one_candidate(colors, background),
        "best-rotation-window-4": best_rotation_window_candidate(
            colors, background, max_window=4
        ),
        "best-stride": best_stride_candidate(colors, background),
        "greedy-anchored": make_candidate(
            "greedy-anchored",
            colors,
            background,
            greedy_anchored_order(colors, background),
        ),
        "even-odd": make_candidate(
            "even-odd",
            colors,
            background,
            even_odd_order(len(colors)),
        ),
        "outside-in": make_candidate(
            "outside-in",
            colors,
            background,
            outside_in_order(len(colors)),
        ),
        "best-cut-interleave": best_cut_interleave_candidate(colors, background),
        "best-cut-then-move": best_cut_then_move_candidate(colors, background),
    }
    optimal = optimal_anchored_candidate(colors, background)
    if optimal is not None:
        candidates[optimal.name] = optimal
    return candidates


def filtered_results(group: str):
    results = load_results()
    if group == "explicit":
        return [result for result in results if result.inputs.explicit_accents]
    if group == "fallback":
        return [result for result in results if not result.inputs.explicit_accents]
    if group == "light":
        return [result for result in results if result.inputs.appearance == "light"]
    if group == "dark":
        return [result for result in results if result.inputs.appearance == "dark"]
    return results


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--group",
        choices=["all", "explicit", "fallback", "light", "dark"],
        default="explicit",
    )
    parser.add_argument("--limit", type=int, default=10)
    args = parser.parse_args()

    results = filtered_results(args.group)
    summaries: dict[str, list[tuple[StrategyCandidate, str]]] = defaultdict(list)
    outright_winners: Counter[str] = Counter()
    cheapest_passer_winners: Counter[str] = Counter()
    oracle_matches: Counter[str] = Counter()
    oracle_coverage: Counter[str] = Counter()
    strict_examples: dict[str, list[str]] = defaultdict(list)

    for result in results:
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
        threshold = target_threshold(result.inputs.appearance)

        for name, candidate in candidates.items():
            summaries[name].append((candidate, result.inputs.appearance))

        non_oracle_candidates = {
            name: candidate
            for name, candidate in candidates.items()
            if name != "optimal-anchored"
        }
        best_score = max(
            candidate.min_adjacent_distance
            for candidate in non_oracle_candidates.values()
        )
        best_names = [
            name
            for name, candidate in non_oracle_candidates.items()
            if math.isclose(candidate.min_adjacent_distance, best_score)
        ]
        outright_winners.update(best_names)

        passing_candidates = [
            candidate
            for candidate in non_oracle_candidates.values()
            if candidate.min_adjacent_distance >= threshold
        ]
        if passing_candidates:
            cheapest = min(
                passing_candidates,
                key=lambda candidate: (
                    candidate.displacement_cost,
                    candidate.inversion_count,
                    -candidate.min_adjacent_distance,
                ),
            )
            cheapest_passer_winners[cheapest.name] += 1

        oracle = cheapest_passing_oracle(adjusted_colors, background, threshold)
        if oracle is not None:
            for name, candidate in non_oracle_candidates.items():
                if (
                    candidate.min_adjacent_distance >= threshold
                    and candidate.displacement_cost == oracle.displacement_cost
                    and candidate.inversion_count == oracle.inversion_count
                ):
                    oracle_matches[name] += 1
                if candidate.min_adjacent_distance >= threshold:
                    oracle_coverage[name] += 1
            best_non_oracle = max(
                non_oracle_candidates.values(),
                key=lambda candidate: candidate.min_adjacent_distance,
            )
            if (
                best_non_oracle.min_adjacent_distance
                < candidates["optimal-anchored"].min_adjacent_distance
            ):
                strict_examples[best_non_oracle.name].append(
                    (
                        f"{result.inputs.theme_name}: best={best_non_oracle.name} "
                        f"{best_non_oracle.min_adjacent_distance:.4f}, "
                        f"optimal={candidates['optimal-anchored'].min_adjacent_distance:.4f}"
                    )
                )

    print(f"group={args.group} theme_count={len(results)}")
    print()
    print(
        f"{'strategy':24} {'pass':>7} {'avg_min_adj':>12} "
        f"{'avg_disp':>9} {'wins':>7} {'cheap':>7} {'oracle':>7}"
    )
    for name in sorted(summaries):
        if name == "optimal-anchored":
            continue
        entries = summaries[name]
        candidates = [candidate for candidate, _ in entries]
        pass_count = sum(
            candidate.min_adjacent_distance >= target_threshold(appearance)
            for candidate, appearance in entries
        )
        oracle_match_count = oracle_matches[name]
        print(
            f"{name:24} {pass_count:7d} "
            f"{mean(candidate.min_adjacent_distance for candidate in candidates):12.4f} "
            f"{mean(candidate.displacement_cost for candidate in candidates):9.2f} "
            f"{outright_winners[name]:7d} "
            f"{cheapest_passer_winners[name]:7d} "
            f"{oracle_match_count:7d}"
        )

    if "optimal-anchored" in summaries:
        optimal_entries = summaries["optimal-anchored"]
        optimal_candidates = [candidate for candidate, _ in optimal_entries]
        pass_count = sum(
            candidate.min_adjacent_distance >= target_threshold(appearance)
            for candidate, appearance in optimal_entries
        )
        print(
            f"{'optimal-anchored':24} {pass_count:7d} "
            f"{mean(candidate.min_adjacent_distance for candidate in optimal_candidates):12.4f} "
            f"{mean(candidate.displacement_cost for candidate in optimal_candidates):9.2f}"
        )

    print()
    print("Strict gaps to optimal-anchored")
    print("------------------------------")
    for name, examples in sorted(
        strict_examples.items(),
        key=lambda item: len(item[1]),
        reverse=True,
    ):
        print(f"{name}: {len(examples)}")
        for example in examples[: args.limit]:
            print(f"  {example}")
        print()


if __name__ == "__main__":
    main()
