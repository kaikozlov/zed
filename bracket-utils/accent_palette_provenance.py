from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from statistics import mean
from typing import Any

import json5
from coloraide import Color

from analyze_extension_themes import REPOS_DIR
from theme_pipeline import is_valid_color_string


@dataclass(frozen=True)
class SourceColor:
    family: str
    key: str
    color: Color


@dataclass(frozen=True)
class AccentMatch:
    theme_name: str
    file_path: str
    appearance: str
    accent_index: int
    accent: str
    exact_rgba: bool
    exact_rgb: bool
    nearest_family: str
    nearest_key: str
    nearest_distance: float


def iter_theme_json_files() -> list[Path]:
    return sorted(REPOS_DIR.glob("*/themes/*.json"))


def load_theme_file(path: Path) -> object:
    text = path.read_text()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return json5.loads(text)


def normalize_rgba_hex(color: Color) -> str:
    return color.convert("srgb", fit=True).to_string(hex=True).lower()


def normalize_rgb_hex(color: Color) -> str:
    srgb = color.convert("srgb", fit=True)
    coords = [max(0.0, min(1.0, channel)) for channel in srgb.coords()]
    return Color("srgb", coords).to_string(hex=True).lower()


def oklab_distance(left: Color, right: Color) -> float:
    left_coords = left.convert("oklab").coords()
    right_coords = right.convert("oklab").coords()
    return sum((left_channel - right_channel) ** 2 for left_channel, right_channel in zip(left_coords, right_coords)) ** 0.5


def family_for_key(path: str) -> str:
    if path.startswith("terminal.ansi."):
        return "terminal_ansi"
    if path.startswith("syntax."):
        return "syntax"
    if path.startswith("players["):
        return "players"
    if path in {"text.accent", "icon.accent", "link_text.hover"}:
        return "ui_accent"
    if path.split(".")[0] in {
        "created",
        "conflict",
        "deleted",
        "error",
        "hidden",
        "hint",
        "ignored",
        "info",
        "modified",
        "predictive",
        "renamed",
        "success",
        "warning",
    }:
        return "status"
    if path.startswith("vim."):
        return "vim"
    return "ui_other"


def walk_style_colors(value: Any, path: str = "") -> list[SourceColor]:
    source_colors: list[SourceColor] = []
    if isinstance(value, dict):
        for key, nested_value in value.items():
            next_path = f"{path}.{key}" if path else str(key)
            if next_path == "accents":
                continue
            source_colors.extend(walk_style_colors(nested_value, next_path))
        return source_colors

    if isinstance(value, list):
        for index, nested_value in enumerate(value):
            next_path = f"{path}[{index}]"
            source_colors.extend(walk_style_colors(nested_value, next_path))
        return source_colors

    if isinstance(value, str) and value and is_valid_color_string(value):
        source_colors.append(
            SourceColor(
                family=family_for_key(path),
                key=path,
                color=Color(value),
            )
        )
    return source_colors


def analyze_theme(theme: dict[str, Any], file_path: Path) -> list[AccentMatch]:
    style = theme.get("style")
    accents = style.get("accents") if isinstance(style, dict) else None
    if not isinstance(style, dict) or not isinstance(accents, list):
        return []

    accent_values = [
        value for value in accents if isinstance(value, str) and value and is_valid_color_string(value)
    ]
    if len(accent_values) < 2:
        return []

    source_colors = walk_style_colors(style)
    rgba_index: defaultdict[str, list[SourceColor]] = defaultdict(list)
    rgb_index: defaultdict[str, list[SourceColor]] = defaultdict(list)
    for source_color in source_colors:
        rgba_index[normalize_rgba_hex(source_color.color)].append(source_color)
        rgb_index[normalize_rgb_hex(source_color.color)].append(source_color)

    matches: list[AccentMatch] = []
    for accent_index, accent_value in enumerate(accent_values):
        accent_color = Color(accent_value)
        accent_rgba = normalize_rgba_hex(accent_color)
        accent_rgb = normalize_rgb_hex(accent_color)
        exact_rgba = accent_rgba in rgba_index
        exact_rgb = accent_rgb in rgb_index

        nearest_source = min(
            source_colors,
            key=lambda source_color: oklab_distance(accent_color, source_color.color),
        )

        matches.append(
            AccentMatch(
                theme_name=str(theme["name"]),
                file_path=str(file_path),
                appearance=str(theme["appearance"]),
                accent_index=accent_index,
                accent=accent_value,
                exact_rgba=exact_rgba,
                exact_rgb=exact_rgb,
                nearest_family=nearest_source.family,
                nearest_key=nearest_source.key,
                nearest_distance=oklab_distance(accent_color, nearest_source.color),
            )
        )

    return matches


def summarize(matches: list[AccentMatch]) -> None:
    accent_count = len(matches)
    theme_keys = {(match.file_path, match.theme_name) for match in matches}
    theme_groups: defaultdict[tuple[str, str], list[AccentMatch]] = defaultdict(list)
    for match in matches:
        theme_groups[(match.file_path, match.theme_name)].append(match)

    exact_rgba_count = sum(match.exact_rgba for match in matches)
    exact_rgb_count = sum(match.exact_rgb for match in matches)
    all_rgba_themes = sum(all(match.exact_rgba for match in group) for group in theme_groups.values())
    all_rgb_themes = sum(all(match.exact_rgb for match in group) for group in theme_groups.values())
    nearest_family_counts = Counter(match.nearest_family for match in matches)

    near_counts = {
        threshold: sum(match.nearest_distance <= threshold for match in matches)
        for threshold in (0.005, 0.01, 0.02, 0.03, 0.05)
    }

    print("Accent provenance summary")
    print("-------------------------")
    print(f"themes: {len(theme_keys)}")
    print(f"accents: {accent_count}")
    print(f"exact rgba accent matches: {exact_rgba_count}/{accent_count} ({exact_rgba_count / accent_count:.1%})")
    print(f"exact rgb accent matches:  {exact_rgb_count}/{accent_count} ({exact_rgb_count / accent_count:.1%})")
    print(f"themes with all accents exact rgba-matched: {all_rgba_themes}/{len(theme_keys)} ({all_rgba_themes / len(theme_keys):.1%})")
    print(f"themes with all accents exact rgb-matched:  {all_rgb_themes}/{len(theme_keys)} ({all_rgb_themes / len(theme_keys):.1%})")
    print(f"avg nearest-source distance: {mean(match.nearest_distance for match in matches):.4f}")
    print()
    print("Nearest-family counts")
    for family, count in nearest_family_counts.most_common():
        print(f"  {family:14} {count:4d} ({count / accent_count:.1%})")
    print()
    print("Nearest-distance coverage")
    for threshold, count in near_counts.items():
        print(f"  <= {threshold:0.3f}: {count:4d}/{accent_count} ({count / accent_count:.1%})")


def print_examples(matches: list[AccentMatch], limit: int) -> None:
    print()
    print("Examples with no exact RGB match")
    print("-------------------------------")
    rows = [match for match in matches if not match.exact_rgb]
    rows.sort(key=lambda match: (-match.nearest_distance, match.theme_name.lower(), match.accent_index))
    for match in rows[:limit]:
        print(
            f"{match.theme_name[:30]:30} "
            f"{match.appearance:5} "
            f"accent[{match.accent_index}]={match.accent:12} "
            f"nearest={match.nearest_family}:{match.nearest_key} "
            f"d={match.nearest_distance:.4f}"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--limit", type=int, default=20)
    args = parser.parse_args()

    matches: list[AccentMatch] = []
    for path in iter_theme_json_files():
        data = load_theme_file(path)
        if not isinstance(data, dict) or "themes" not in data:
            continue
        for theme in data["themes"]:
            if isinstance(theme, dict):
                matches.extend(analyze_theme(theme, path))

    summarize(matches)
    print_examples(matches, args.limit)


if __name__ == "__main__":
    main()
