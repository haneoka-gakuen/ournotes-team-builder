"""Check two small inventories against the native gekiso_room example and its score oracle."""
import argparse
from copy import deepcopy
from itertools import combinations
import json
from pathlib import Path
import subprocess


def run(binary, folder, name, request, verify=False):
    path = folder / f"{name}.json"
    path.write_text(json.dumps(request, ensure_ascii=False), encoding="utf-8")
    args = [str(binary), str(path)] + (["--verify"] if verify else [])
    output = subprocess.check_output(args)
    (folder / f"{name}-result.json").write_bytes(output)
    return json.loads(output)


def identity(team):
    return team["leader"], tuple(team["members"]), tuple(team["snaps"])


def legal_teams(data, seat):
    characters = {row["key"]: data["members"][str(row["cardId"])]["characterId"]
                  for row in seat["members"]}
    empty = {key for key, snap in seat["constraints"]["bindings"] if snap is None}
    snap, = seat["constraints"]["requiredSnaps"]
    expected = set()
    for roster in combinations(characters, 5):
        if len({characters[key] for key in roster}) < 5 or not empty.issubset(roster):
            continue
        for leader in roster:
            ordered = (leader, *sorted(set(roster) - {leader}, key=characters.__getitem__))
            for receiving_member in set(roster) - empty:
                expected.add((leader, ordered, tuple(snap if member == receiving_member else None
                                                     for member in ordered)))
    return expected


def make_inventory(request):
    request.pop("teams", None)
    for s, seat in enumerate(request["players"]):
        # Two distinct copies of the inventory share a single alternative card design, m26.
        ids = ([1, 26] if s == 0 else [26, 51]) + [52, 53, 54, 55]
        seat["members"] = [{"key": f"m{i}", "cardId": i, "maximize": True} for i in ids]
        seat["snaps"] = [{"key": "s51", "cardId": 51, "maximize": True}]
        member = next(m for m in seat["members"] if m["key"] == "m26")
        member.update(maximize=False, level=[1, 25][s], rank=1, awake=1,
                      liveSkillLevel=1, gekisoSkillLevel=1)
        seat["player"] = {"vipRank": [1, 10][s], "characterTotalRank": [25, 100][s]}
        seat["constraints"] = {"requiredSnaps": ["s51"],
                               "bindings": [[k, None] for k in ["m52", "m53", "m54"]]}
        seat["allowedDifficulties"] = [request["song"]["difficulty"]]
        seat["accuracy"] = request["accuracy"]


def rename_keys(seat):
    rows = seat["members"] + seat["snaps"]
    mapping = {row["key"]: f"copy-{99-i}" for i, row in enumerate(rows)}
    limits = seat["constraints"]
    limits["leader"] = mapping[limits["leader"]]
    limits["bindings"] = [[mapping[k], mapping.get(v)] for k, v in limits["bindings"]]
    for row in rows:
        row["key"] = mapping[row["key"]]
    seat["members"].reverse()
    return mapping


def check(binary, fixture, folder):
    request = json.loads(fixture.read_text(encoding="utf-8"))
    make_inventory(request)
    result = run(binary, folder, "free-room", request, verify=True)
    expected = [legal_teams(request["data"], seat) for seat in request["players"]]
    for s, rows in enumerate(result["verification"]["candidates"]):
        observed = [identity(row["team"]) for row in rows]
        assert len(expected[s]) == len(observed) == 20
        assert set(observed) == expected[s]
        winner = result["teams"][s]
        assert identity(winner["team"]) in expected[s]
        assert next(r["power"] for r in rows if r["team"] == winner["team"]) == winner["power"]
    assert result["pairsEvaluated"] == result["verification"]["pairs"] == 400
    assert result["room"]["total"] == result["verification"]["maximum"] == sum(
        seat["score"] for seat in result["teams"])

    # Fix the same cards on both seats to isolate account bonuses and member growth.
    locked = {"leader": "m52", "bindings": [[k, "s51" if k == "m55" else None]
                                         for k in ["m26", "m52", "m53", "m54", "m55"]]}
    for seat in request["players"]:
        seat["constraints"] = locked
    powers = {}
    for change in ["account", "growth"]:
        single = deepcopy(request)
        if change == "account":
            for seat in single["players"]:
                next(m for m in seat["members"] if m["key"] == "m26")["level"] = 1
        else:
            single["players"][1]["player"] = single["players"][0]["player"]
        out = run(binary, folder, change, single)
        assert out["candidateCounts"] == [1, 1]
        assert out["teams"][0]["team"] == out["teams"][1]["team"]
        a, b = [seat["power"] for seat in out["teams"]]
        assert b > a
        powers[change] = [a, b]
    request["players"][1] = deepcopy(request["players"][0])
    request["seeds"][1] = request["seeds"][0]
    request["orderIndices"][1] = request["orderIndices"][0]
    keys = rename_keys(request["players"][1])
    renamed = run(binary, folder, "renamed", request)
    first, second = renamed["teams"]
    assert [keys[k] for k in first["team"]["members"]] == second["team"]["members"]
    assert [keys.get(k) for k in first["team"]["snaps"]] == second["team"]["snaps"]
    assert first["score"] == second["score"] and first["power"] == second["power"]
    assert all([seat["rank"] for seat in section["seats"]] == [1, 1]
               for section in renamed["room"]["sections"])
    print(json.dumps({"formationsPerPlayer": [len(x) for x in expected],
                      "pairsVerified": result["verification"]["pairs"],
                      "teams": result["teams"], "maximum": result["verification"]["maximum"],
                      "fixedTeamPowers": powers, "renamedTeamScore": first["score"]},
                     ensure_ascii=False, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="built gekiso_room example")
    parser.add_argument("output", type=Path, help="directory for generated inputs and results")
    parser.add_argument("--fixture", type=Path, default=Path(__file__).with_name("fixture.json"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    check(args.binary.resolve(), args.fixture, args.output)
