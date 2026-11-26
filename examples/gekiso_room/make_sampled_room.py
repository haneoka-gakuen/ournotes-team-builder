"""Attach the selected difficulties from a saved release to a cooperative room request."""
import argparse
from hashlib import sha256
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("master", type=Path)
    parser.add_argument("catalog", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--request", type=Path, default=Path(__file__).with_name("mixed_room.json"))
    args = parser.parse_args()
    master = args.master.read_bytes()
    catalog = json.loads(args.catalog.read_bytes())
    if sha256(master).hexdigest() != catalog["masterSha256"]:
        parser.error("catalog and master must come from the same release")
    data = json.loads(master)
    room = json.loads(args.request.read_bytes())
    song = room["song"]
    by_key = {(row["songId"], row["difficulty"]): row for row in catalog["charts"]}
    charts = []
    for difficulty in sorted({p.get("difficulty", song["difficulty"]) for p in room["players"]}):
        row = by_key.get((song["songId"], difficulty))
        if row is None:
            parser.error(f"catalog has no chart for song {song['songId']}, difficulty {difficulty}")
        payload = (args.catalog.parent / row["path"]).read_bytes()
        if sha256(payload).hexdigest() != row["sha256"]:
            parser.error(f"chart differs from the catalog at {row['path']}")
        charts.append({"song": {**song, "difficulty": difficulty}, "chart": json.loads(payload)})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({**room, "data": data, "charts": charts}, ensure_ascii=False),
                           encoding="utf-8")


if __name__ == "__main__":
    main()
