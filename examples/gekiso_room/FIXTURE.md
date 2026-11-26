# Two-seat room fixture

`fixture.json` uses the released music, chart, cards and skill effects from Haneoka's
international release `r-2e427037b9e4e81159c8`, source
`v86-1e5e2dca8254-1e765982d690-mf2308cc1dfa8-n5c77f3eb`. The two inventories
and growth levels are declared examples. No player records are included.

- [Release master](https://haneoka.org/api/v1/team-builder/intl?release=r-2e427037b9e4e81159c8), SHA-256 `bf05ae76e7e1885b9587da365952016504281514f7a6052151904fa04b847b5e`.
- [端程山 / Panorama, Expert chart](https://haneoka.org/assets/intl/Assets/AddressableResources/Live/MusicScore/0013/0013_03.bytes?release=r-2e427037b9e4e81159c8), SHA-256 `82be7d9986dbfd873fba6401075ad65ed0df957ebb603d8f8ea17d1a2c5a59c5`.

The objective is the sum of both final scores on song 100013, difficulty 3. Each
seat uses ordinary skill order index 0, seed 0 and the engine's deterministic
judgement schedule with JUST input 0.35, GREAT 0 and no misses. All three
missions are COMBO. Both accounts set character total rank 25 and VIP rank 1.
They equip their own fully developed snapshot 51 on their own leader 52.

| Seat | Fixed members including leader 52 | Alternatives for the fifth member |
| --- | --- | --- |
| A | 1, 52, 54, 55 | A1 = 3, A2 = 28, A3 = 53 |
| B | 28, 52, 54, 55 | B1 = 1, B2 = 26, B3 = 51 |

Members use maximum growth except for A's card 53, which has level 49, awakening
1, rank 1, and both skills at level 1. Each seat owns seven members. The three
alternatives represent the same character, so at most one can play. The other
four characters must play to fill all five slots.

```text
cargo run --locked --release --example gekiso_room -- examples/gekiso_room/fixture.json --verify
```

The comparison for “personal best” fixes all three ranks to first for an
individual player before comparing that seat's three teams. The nine room
replays determine actual ranks after each section.

| Room total | B1 | B2 | B3 |
| --- | ---: | ---: | ---: |
| A1 | 18,596,247 | 19,320,059 | 20,974,726 |
| A2 | 20,854,712 | 20,196,261 | **21,850,928** |
| A3 | 19,486,947 | 20,117,818 | 21,556,143 |

A3 and B3 each score highest in their seat under fixed first-place ranks.
Together they score 21,556,143. Card 53 gives A3 COMBO totals of 253, 277 and
259 against B3's 83, 87 and 84. A3 wins each mission, and B3 receives a 190%
section bonus. With A2, both seats have the same COMBO totals, and B3's higher
section scores win the tiebreaks. Its three bonuses rise to 250%. The seat
scores become 9,045,093 and 12,805,835, for a room gain of 294,785. A2 with
B1 has equal COMBO, section score and PERFECT count in every section. Each
player ties for first three times and scores 10,427,356.

To rebuild the fixture and compare it against the complete public data, save
both downloads above, build the example, and run the following command. The
builder verifies both source hashes. It evaluates the fixture against each
master, including all individual scores, all nine room totals and every
section's standings and ranking bonuses, and compares the complete outputs.

```text
python examples/gekiso_room/build_fixture.py master.json 0013_03.bytes fixture-rebuilt.json --verify-with target/release/examples/gekiso_room
```

On Windows, append `.exe` to the executable path.
## Free team selection and rank-triggered skills

Build the native example once with `cargo build --locked --release --example gekiso_room`. The inventory check constructs two six-member boxes. Each box allows two character variants, five possible leaders, and two placements for its required snapshot. A Python enumeration checks the identities of all 20 teams per seat against the native search. The single-seat history verifier checks the maximum across all 400 pairs. Generated inputs, formations and section results go into the output directory.

```sh
python examples/gekiso_room/check_inventory.py target/release/examples/gekiso_room target/room-inventory
```

On Windows, use `target/release/examples/gekiso_room.exe` for the executable path. The inventory check also varies account bonuses and training separately, and repeats a locked team with reordered inventory and renamed instance keys.

To isolate a skill activated by first place, run the controlled example on the same chart. It adds one synthetic skill to an otherwise identical five-member test formation and changes the opponent's power. The output shows the second section's score with and without the skill after a first- or second-place finish in the opening section.

```sh
cargo run --locked --release --example gekiso_room -- examples/gekiso_room/fixture.json --check-rank-skill
```
