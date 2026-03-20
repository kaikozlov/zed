from __future__ import annotations

import json
import urllib.request
from pathlib import Path


EXTENSIONS_URL = "https://zed.dev/api/extensions?max_schema_version=1&provides=themes"
OUTPUT_PATH = Path("bracket-utils/theme-sample/extensions_index.json")


def fetch_json(url: str) -> dict[str, object]:
    request = urllib.request.Request(
        url,
        headers={
            "User-Agent": "Mozilla/5.0 (compatible; bracket-utils/0.1; +https://zed.dev)"
        }
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read().decode("utf-8"))


def extract_extensions_payload(api_payload: dict[str, object]) -> dict[str, object]:
    extensions = api_payload["data"]
    return {
        "source": EXTENSIONS_URL,
        "total": len(extensions),
        "count": len(extensions),
        "extensions": extensions,
    }


def main() -> None:
    api_payload = fetch_json(EXTENSIONS_URL)
    payload = extract_extensions_payload(api_payload)
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_PATH.write_text(json.dumps(payload, indent=2))
    print(f"Wrote {payload['count']} theme extensions to {OUTPUT_PATH}")


if __name__ == "__main__":
    main()
