"""Rebuild the two-seat fixture from the unmodified public release and chart downloads."""
import argparse
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

MASTER_SHA = "bf05ae76e7e1885b9587da365952016504281514f7a6052151904fa04b847b5e"
CHART_SHA = "82be7d9986dbfd873fba6401075ad65ed0df957ebb603d8f8ea17d1a2c5a59c5"
SONG = "100013"
CARD_IDS = [1, 3, 26, 28, 51, 52, 53, 54, 55]
SNAP_IDS = [51]
ALTERNATIVES = ([3, 28, 53], [1, 26, 51])
ANCHORS = ([1, 52, 54, 55], [28, 52, 54, 55])


def read_verified(path, expected):
    raw = Path(path).read_bytes()
    if hashlib.sha256(raw).hexdigest() != expected:
        raise ValueError(f"unexpected source hash: {path}")
    return json.loads(raw)


def select(rows, fields):
    return {key: rows[key] for key in fields if key in rows}


def release_subset(data):
    selected = select(data, ["identity"])
    song = data["songs"][SONG]
    selected_song = select(song, ["musicId", "musicTitle", "musicType", "bandIds",
                                  "bestMusicTagIds", "liveScoreRankGroup", "gekisou"])
    difficulty = next(r for r in song["difficulty"] if r["difficulty"] == 3)
    selected_song["difficulty"] = [select(difficulty, ["difficulty", "scoreId", "playLevel",
                                                        "displayLevel", "noteCount", "file"])]
    selected["songs"] = {SONG: selected_song}
    for table, ids, fields in [
        ("members", CARD_IDS, ["id", "name", "characterId", "bandId", "rarity", "attribute",
                               "statMax", "levelGroup", "trainingGroup", "awakeningGroup",
                               "leaderSkillId", "liveSkillId", "gekisoSkillId", "levelLimits", "bestMusicTagIds"]),
        ("snapshots", SNAP_IDS, ["id", "name", "characterIds", "rarity", "attribute", "statMax",
                                 "levelGroup", "awakeningGroup", "supportSkillIds", "gekisoSupportSkillIds"]),
    ]:
        selected[table] = {str(i): select(data[table][str(i)], fields) for i in ids}
    selected["characters"] = {i: select(r, ["characterId", "id", "bandId", "isNonPlayable"])
                              for i, r in data["characters"].items()}
    selected["progression"] = select(data["progression"], ["memberCardLevels", "memberCardAwake",
        "memberCardRanks", "memberCardLevelLimits", "supportCardLevels", "supportCardRanks",
        "characterRanks", "characterTotalRanks"])
    tables = data["runtimeRules"]["tables"]
    selected["runtimeRules"] = {"tables": {k: {"rows": tables[k]["rows"]}
                                           for k in ["parameters", "vipRankBonuses"]}}
    selected["liveTools"] = select(data["liveTools"], ["liveSettings", "noteParameters", "judgementParameters",
        "judgementTiming", "judgementAreaOffsets", "comboScoreBonuses", "gekisouRankingScoreBonuses"])
    selected["liveTools"]["scoreRanks"] = [r for r in data["liveTools"]["scoreRanks"]
                                           if r["group"] == song["liveScoreRankGroup"]]
    selected["skillReference"] = select(data["skillReference"],
        ["targets", "conditions", "conditionSets", "cumulativeConditions", "effectSettings"])
    selected["skills"] = {}
    for name, column in [("leader", "leaderSkillId"), ("live", "liveSkillId"), ("gekiso", "gekisoSkillId"),
                         ("support", "supportSkillIds"), ("gekisoSupport", "gekisoSupportSkillIds")]:
        if column.endswith("Ids"):
            ids = {i for snap in selected["snapshots"].values() for i in snap[column] if i}
        else:
            ids = {card[column] for card in selected["members"].values()}
        selected["skills"][name] = {str(i): select(data["skills"][name][str(i)], [
            "id", "categories", "effects", "gekisouMissionType"]) for i in sorted(ids) if i}
    return selected


def player(seat):
    return {
        "members": [{"key": f"m{i}", "cardId": i, **(
            {"level": 49, "awake": 1, "rank": 1, "liveSkillLevel": 1, "gekisoSkillLevel": 1}
            if i == 53 else {"maximize": True})} for i in sorted(ANCHORS[seat] + ALTERNATIVES[seat])],
        "snaps": [{"key": "s51", "cardId": 51, "maximize": True}],
        "player": {"characterTotalRank": 25, "vipRank": 1, "characterRanks": {},
                   "bandItems": {}, "characterMemory": {}, "musicMemory": {}},
        "unknownPolicy": "min",
        "constraints": {"leader": "m52", "requiredSnaps": ["s51"],
                        "bindings": [["m52", "s51"]]},
    }


def team(data, seat, alternative):
    others = [i for i in [alternative, *ANCHORS[seat]] if i != 52]
    others.sort(key=lambda i: data["members"][str(i)]["characterId"])
    return {"members": ["m52", *(f"m{i}" for i in others)], "snaps": ["s51", None, None, None, None],
            "leader": "m52"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("master")
    parser.add_argument("chart")
    parser.add_argument("output")
    parser.add_argument("--verify-with", type=Path, metavar="GEKISO_ROOM_EXE",
                        help="compare every result field against the complete release master")
    args = parser.parse_args()
    data = read_verified(args.master, MASTER_SHA)
    chart = read_verified(args.chart, CHART_SHA)
    fixture = {
        "song": {"songId": int(SONG), "difficulty": 3},
        "players": [player(0), player(1)],
        "teams": [[team(data, seat, i) for i in ALTERNATIVES[seat]] for seat in range(2)],
        "accuracy": {"just": 0.35, "great": 0, "missEvery": 0},
        "orderIndices": [0, 0], "seeds": [0, 0],
        "data": release_subset(data), "chart": chart,
    }
    Path(args.output).write_bytes((json.dumps(fixture, ensure_ascii=False, separators=(",", ":")) + "\n").encode())
    if args.verify_with:
        cli = args.verify_with.resolve()

        def verified(path):
            run = subprocess.run([cli, path, "--verify"], check=True, capture_output=True)
            return json.loads(run.stdout)

        with tempfile.TemporaryDirectory(dir=Path(args.output).resolve().parent) as folder:
            full = Path(folder) / "full-master-room.json"
            full.write_text(json.dumps({**fixture, "data": data}, ensure_ascii=False), encoding="utf8")
            if verified(full) != verified(args.output):
                raise ValueError("room output from the fixture differs from the complete release")
        print("Verified nine room results and all individual and section fields against the full release.")


if __name__ == "__main__":
    main()
