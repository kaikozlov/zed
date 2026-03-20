from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path
from urllib.parse import urlparse


INDEX_PATH = Path("bracket-utils/theme-sample/extensions_index.json")
REPOS_DIR = Path("bracket-utils/theme-repos")


def repo_slug(repository_url: str) -> str | None:
    parsed = urlparse(repository_url)
    if parsed.netloc not in {"github.com", "www.github.com"}:
        return None
    parts = [part for part in parsed.path.split("/") if part]
    if len(parts) < 2:
        return None
    return f"{parts[0]}/{parts[1].removesuffix('.git')}"


def clone_url(repository_url: str) -> str | None:
    slug = repo_slug(repository_url)
    if slug is None:
        return None
    return f"https://github.com/{slug}"


def load_repos(limit: int | None) -> list[dict[str, object]]:
    data = json.loads(INDEX_PATH.read_text())
    repos = []
    for extension in data["extensions"]:
        slug = repo_slug(extension["repository"])
        if slug is None:
            continue
        repos.append(
            {
                "id": extension["id"],
                "name": extension["name"],
                "repository": extension["repository"],
                "slug": slug,
                "download_count": extension["download_count"],
            }
        )
    repos.sort(key=lambda repo: repo["download_count"], reverse=True)
    if limit is not None:
        repos = repos[:limit]
    return repos


def clone_repo(repo: dict[str, object]) -> None:
    destination = REPOS_DIR / repo["id"]
    if destination.exists():
        print(f"skip {repo['id']}: already exists")
        return
    repository = clone_url(repo["repository"])
    if repository is None:
        print(f"skip {repo['id']}: unsupported repository url {repo['repository']}")
        return
    try:
        subprocess.run(
            [
                "git",
                "clone",
                "--depth",
                "1",
                repository,
                str(destination),
            ],
            check=True,
        )
    except subprocess.CalledProcessError:
        print(f"skip {repo['id']}: clone failed for {repository}")
        return
    print(f"cloned {repo['id']} ({repo['slug']})")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--limit", type=int, default=20)
    args = parser.parse_args()

    REPOS_DIR.mkdir(parents=True, exist_ok=True)
    repos = load_repos(args.limit)
    for repo in repos:
        clone_repo(repo)


if __name__ == "__main__":
    main()
