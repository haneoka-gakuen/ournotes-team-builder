"""Build a two- to five-player room on the released Panorama Expert fixture."""
import argparse
import json
from copy import deepcopy
from pathlib import Path


def player(fixture, owner, label, *, locked=None, weak=False, order=0, seed=0):
    seat = deepcopy(fixture["players"][owner])
    seat["allowedDifficulties"] = [fixture["song"]["difficulty"]]
    seat["orderIndex"], seat["seed"] = order, seed
    if weak:
        seat["player"] = {"characterTotalRank": 1, "vipRank": 0}
        for member in seat["members"]:
            member.update(maximize=False, level=1, awake=0, rank=1,
                          liveSkillLevel=1, gekisoSkillLevel=1)
    if locked is not None:
        seat["lockedTeam"] = deepcopy(fixture["teams"][owner][locked])
    # Give each owned instance its own account's key, including the two identical locked teams.
    keys = {row["key"]: f"{label}-{row['key']}" for row in seat["members"] + seat["snaps"]}
    for row in seat["members"] + seat["snaps"]:
        row["key"] = keys[row["key"]]
    limits = seat["constraints"]
    limits["leader"] = keys[limits["leader"]]
    limits["bindings"] = [[keys[m], keys.get(s)] for m, s in limits["bindings"]]
    limits["requiredSnaps"] = [keys[s] for s in limits["requiredSnaps"]]
    if locked is not None:
        team = seat["lockedTeam"]
        team["leader"] = keys[team["leader"]]
        team["members"] = [keys[m] for m in team["members"]]
        team["snaps"] = [keys.get(s) for s in team["snaps"]]
    return seat


def make_room(fixture, seats):
    if seats == 2:
        players = [player(fixture, 0, "a"), player(fixture, 1, "b")]
    else:
        players = [
            player(fixture, 0, "a", locked=1),
            player(fixture, 1, "b", locked=0),
            player(fixture, 1, "c", weak=True),
        ]
        if seats >= 4:
            players.append(player(fixture, 0, "d", order=1, seed=7))
        if seats == 5:
            players.append(player(fixture, 1, "e", order=37, seed=29))
    return {**{key: fixture[key] for key in ("data", "chart", "song", "accuracy")}, "players": players}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--seats", type=int, choices=range(2, 6), default=5)
    parser.add_argument("--fixture", type=Path, default=Path(__file__).with_name("fixture.json"))
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fixture = json.loads(args.fixture.read_bytes())
    args.output.write_text(json.dumps(make_room(fixture, args.seats), ensure_ascii=False), encoding="utf-8")
