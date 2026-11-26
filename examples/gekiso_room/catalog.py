"""Fetch the charts named by one Haneoka team-builder release for offline SS calculation."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from hashlib import sha256
import json
from pathlib import Path
import re
import subprocess
from tempfile import NamedTemporaryFile, TemporaryDirectory
from urllib.parse import urlencode, urlsplit


def timestamp(value):
    stamp = datetime.fromisoformat(value)
    if stamp.tzinfo is None:
        raise argparse.ArgumentTypeError("supply a date and time with a UTC offset")
    return stamp


def public_rows(master, cutoff):
    identity = master["identity"]
    rows, unavailable = [], []
    for song in sorted(master["songs"].values(), key=lambda song: song["musicId"]):
        names = song["musicTitle"]
        title = names[3] or names[0] or names[1]
        for difficulty in sorted(song["difficulty"], key=lambda row: row["difficulty"]):
            row = {"songId": song["musicId"], "difficulty": difficulty["difficulty"], "title": title}
            if type(row["songId"]) is not int or type(row["difficulty"]) is not int:
                raise ValueError("invalid song or difficulty ID in chart release")
            # These release fields store the game availability date in their first slot.
            gates = [d[0] for d in (song["publishedAt"], difficulty["publishedAt"]) if d and d[0]]
            if gates and max(gates) > cutoff.timestamp() * 1000:
                date = datetime.fromtimestamp(max(gates) / 1000, tz=timezone.utc)
                unavailable.append({**row, "reason": "scheduled", "publishedAt": date.isoformat()})
                continue
            asset = difficulty.get("file") or ""
            parsed = urlsplit(asset)
            if (parsed.scheme or parsed.netloc or parsed.query or parsed.fragment
                    or not asset.startswith(f"/assets/{identity['server']}/")):
                unavailable.append({**row, "reason": "missing-or-invalid-chart-path"})
                continue
            url = "https://haneoka.org" + asset + "?" + urlencode({"release": identity["releaseId"]})
            rows.append({**row, "scoreId": difficulty["scoreId"], "url": url})
    return rows, unavailable


def replace_bytes(path, content):
    with NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        staged = Path(stream.name)
        stream.write(content)
    try:
        staged.replace(path)
    finally:
        staged.unlink(missing_ok=True)


def download(row, output, identity, cache):
    path = Path("charts") / identity["releaseId"] / f"{row['songId']}-{row['difficulty']}.bytes"
    target = output / path
    cached = cache.get(row["url"])
    if cached and target.is_file() and sha256(target.read_bytes()).hexdigest() == cached["sha256"]:
        return {**row, "path": path.as_posix(), "sha256": cached["sha256"]}
    with TemporaryDirectory() as temp:
        headers_file = Path(temp) / "headers"
        payload_file = Path(temp) / "chart"
        response = subprocess.run(
            ["curl", "--silent", "--show-error", "--fail", "--retry", "3", "--retry-all-errors",
             "--retry-delay", "1", "--retry-max-time", "40", "--max-time", "20",
             "--dump-header", str(headers_file), "--output", str(payload_file), "--", row["url"]],
            capture_output=True)
        if response.returncode:
            return {**row, "reason": "download", "error": response.stderr.decode(errors="replace").strip()}
        headers = {}
        for line in headers_file.read_text(encoding="latin1").splitlines():
            if line.startswith("HTTP/"):
                headers.clear()
            elif ":" in line:
                key, value = line.split(":", 1)
                headers[key.lower()] = value.strip()
        payload = payload_file.read_bytes()
    if (headers.get("x-haneoka-release-id"), headers.get("x-haneoka-source-id")) != (
            identity["releaseId"], identity["sourceId"]):
        return {**row, "reason": "response-release-mismatch"}
    try:
        score = json.loads(payload)["score"]
        if not isinstance(score["notes"], list) or not isinstance(score["events"], dict):
            raise ValueError("chart notes or events are missing")
    except (ValueError, KeyError, TypeError) as error:
        return {**row, "reason": "invalid-chart", "error": str(error)}
    replace_bytes(target, payload)
    return {**row, "path": path.as_posix(), "sha256": sha256(payload).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("master", type=Path, help="public team-builder release JSON")
    parser.add_argument("output", type=Path, help="directory for the manifest and chart files")
    parser.add_argument("--as-of", type=timestamp, default=datetime.now(timezone.utc),
                        help="exclude songs/difficulties scheduled after this ISO 8601 timestamp")
    args = parser.parse_args()
    raw = args.master.read_bytes()
    master = json.loads(raw)
    identity = master["identity"]
    if not re.fullmatch(r"[A-Za-z0-9_-]+", identity["releaseId"]):
        raise ValueError("invalid release ID")
    rows, missing = public_rows(master, args.as_of)
    (args.output / "charts" / identity["releaseId"]).mkdir(parents=True, exist_ok=True)
    path = args.output / "catalog.json"
    previous = json.loads(path.read_bytes()) if path.exists() else {}
    digest = sha256(raw).hexdigest()
    cache = {row["url"]: row for row in previous.get("charts", [])} if (
        previous.get("identity") == identity and previous.get("masterSha256") == digest) else {}
    with ThreadPoolExecutor(max_workers=4) as pool:
        downloaded = list(pool.map(lambda row: download(row, args.output, identity, cache), rows))
    charts = [row for row in downloaded if "path" in row]
    missing.extend(row for row in downloaded if "reason" in row)
    manifest = {"identity": identity, "masterSha256": digest, "asOf": args.as_of.isoformat(),
                "charts": charts, "missing": missing}
    replace_bytes(path, (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode("utf8"))
    errors = sum(row["reason"] != "scheduled" for row in missing)
    print(f"{len(charts)} charts ready, {len(missing) - errors} scheduled, {errors} unavailable")
    return int(errors > 0)


if __name__ == "__main__":
    raise SystemExit(main())
