from __future__ import annotations

import argparse
import math
from dataclasses import dataclass
from math import gcd

from coloraide import Color


@dataclass(frozen=True)
class Candidate:
    name: str
    order: tuple[int, ...]
    min_adjacent_distance: float
    inversion_count: int
    displacement_cost: int


PALETTES: dict[str, tuple[str, ...]] = {
    "rgbcmy": ("#ff0000", "#ffff00", "#ff00ff", "#00ff00", "#00ffff", "#0000ff"),
    "gruvbox-light": (
        "#CC241D",
        "#98971A",
        "#D79921",
        "#458588",
        "#B16286",
        "#689D6A",
        "#D65D0E",
    ),
    "near-duplicate-reds": ("#ff0000", "#ff2200", "#00ff00", "#0099ff"),
    "one-dark-accents": ("#61afef", "#d19a66", "#c678dd", "#56b6c2", "#e06c75"),
}

BACKGROUNDS: dict[str, str] = {
    "dark": "#1e1e1e",
    "light": "#faf6e8",
    "mid": "#7f7f7f",
}


def composite_over_background(color: Color, background: Color) -> Color:
    foreground = color.convert("srgb")
    if foreground.alpha() >= 1:
        return foreground

    alpha = foreground.alpha()
    red, green, blue = foreground.coords()
    bg_red, bg_green, bg_blue = background.convert("srgb").coords()
    return Color(
        "srgb",
        [
            bg_red * (1 - alpha) + red * alpha,
            bg_green * (1 - alpha) + green * alpha,
            bg_blue * (1 - alpha) + blue * alpha,
        ],
    )


def oklab_distance(left: Color, right: Color, background: Color) -> float:
    left_oklab = composite_over_background(left, background).convert("oklab").coords()
    right_oklab = composite_over_background(right, background).convert("oklab").coords()
    return math.sqrt(sum((left_component - right_component) ** 2 for left_component, right_component in zip(left_oklab, right_oklab)))


def min_adjacent_distance(colors: list[Color], order: tuple[int, ...], background: Color) -> float:
    if len(order) < 2:
        return float("inf")

    distances = []
    for index, current in enumerate(order):
        following = order[(index + 1) % len(order)]
        distances.append(oklab_distance(colors[current], colors[following], background))
    return min(distances)


def inversion_count(order: tuple[int, ...]) -> int:
    total = 0
    for left_index, left in enumerate(order):
        for right in order[left_index + 1 :]:
            if left > right:
                total += 1
    return total


def displacement_cost(order: tuple[int, ...]) -> int:
    return sum(abs(position - value) for position, value in enumerate(order))


def authored_order(length: int) -> tuple[int, ...]:
    return tuple(range(length))


def greedy_anchored_order(colors: list[Color], background: Color) -> tuple[int, ...]:
    used = [False] * len(colors)
    order = [0]
    used[0] = True

    while len(order) < len(colors):
        last = order[-1]
        first = order[0]
        remaining = [index for index in range(len(colors)) if not used[index]]
        next_index = max(
            remaining,
            key=lambda index: (
                oklab_distance(colors[last], colors[index], background),
                oklab_distance(colors[first], colors[index], background),
            ),
        )
        used[next_index] = True
        order.append(next_index)

    return tuple(order)


def anchored_stride_orders(length: int) -> list[tuple[int, ...]]:
    orders: list[tuple[int, ...]] = []
    for stride in range(1, length):
        if gcd(stride, length) != 1:
            continue
        order = tuple((step * stride) % length for step in range(length))
        if order[0] == 0:
            orders.append(order)
    return orders


def best_single_swap_order(colors: list[Color], background: Color) -> tuple[int, ...]:
    baseline = authored_order(len(colors))
    best_order = baseline
    best_distance = min_adjacent_distance(colors, baseline, background)
    best_inversions = inversion_count(baseline)
    best_displacement = displacement_cost(baseline)

    for left in range(1, len(colors)):
        for right in range(left + 1, len(colors)):
            candidate = list(baseline)
            candidate[left], candidate[right] = candidate[right], candidate[left]
            candidate = tuple(candidate)
            candidate_distance = min_adjacent_distance(colors, candidate, background)
            candidate_inversions = inversion_count(candidate)
            candidate_displacement = displacement_cost(candidate)
            if (
                candidate_distance > best_distance
                or (
                    math.isclose(candidate_distance, best_distance)
                    and (
                        candidate_inversions < best_inversions
                        or (
                            candidate_inversions == best_inversions
                            and candidate_displacement < best_displacement
                        )
                    )
                )
            ):
                best_order = candidate
                best_distance = candidate_distance
                best_inversions = candidate_inversions
                best_displacement = candidate_displacement

    return best_order


def build_candidates(colors: list[Color], background: Color) -> list[Candidate]:
    orders: dict[str, tuple[int, ...]] = {
        "authored": authored_order(len(colors)),
        "best-single-swap": best_single_swap_order(colors, background),
        "greedy-anchored": greedy_anchored_order(colors, background),
    }
    for stride_order in anchored_stride_orders(len(colors)):
        stride = stride_order[1]
        orders[f"stride-{stride}"] = stride_order

    candidates = []
    for name, order in orders.items():
        candidates.append(
            Candidate(
                name=name,
                order=order,
                min_adjacent_distance=min_adjacent_distance(colors, order, background),
                inversion_count=inversion_count(order),
                displacement_cost=displacement_cost(order),
            )
        )
    return sorted(candidates, key=lambda candidate: candidate.name)


def parse_palette(values: list[str]) -> tuple[str, ...]:
    if len(values) == 1 and values[0] in PALETTES:
        return PALETTES[values[0]]
    return tuple(values)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "palette",
        nargs="+",
        help="Palette name or a list of colors like '#ff0000 #00ff00 #0000ff'",
    )
    parser.add_argument(
        "--background",
        default="dark",
        help="Background name or a hex color",
    )
    args = parser.parse_args()

    palette_values = parse_palette(args.palette)
    background_value = BACKGROUNDS.get(args.background, args.background)
    colors = [Color(value) for value in palette_values]
    background = Color(background_value)

    print("palette:", ", ".join(palette_values))
    print("background:", background_value)
    print()
    print(f"{'candidate':18} {'order':24} {'min_adj':>8} {'inv':>5} {'disp':>5}")
    for candidate in sorted(
        build_candidates(colors, background),
        key=lambda candidate: (
            -candidate.min_adjacent_distance,
            candidate.inversion_count,
            candidate.displacement_cost,
            candidate.name,
        ),
    ):
        print(
            f"{candidate.name:18} {str(candidate.order):24} "
            f"{candidate.min_adjacent_distance:8.4f} "
            f"{candidate.inversion_count:5d} "
            f"{candidate.displacement_cost:5d}"
        )


if __name__ == "__main__":
    main()
