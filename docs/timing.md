# Cumulative judgement timing

## Request options

Score requests can set `goal.play.cumulativeTiming` to `"best"` or `"worst"` with `criterion: "max"`; evaluation reads `request.play.cumulativeTiming`. Each of the 120 skill orders gets its own timing optimum and the best order sets the team's score. Without the option, scoring is nominal.

`play.frameRate` is 30, 60, 90 or 120 (default 60). The release document supplies `liveTools.judgementAreaOffsets`. Numeric card dominance is off in these modes, and a selected optimum must pass whole-frame input replay.

A `timeline` request with a `timing` context returns `worst` and `optimal` next to the nominal `score` and the input-checked guide's `best`. Guide steps advance one frame rounded up to whole milliseconds (34, 17, 12 or 9 ms) and address notes by `noteIndices` in the judged-note stream. Timeline bounds cover only the selected skill order; full evaluation returns all 120.

## Original-client paths

The ordinary cumulative scorer separates judgement delivery from the score ledger. `play.frameRate` selects 30, 60, 90 or 120 evenly spaced updates per second, with 60 as the default. The first update has music time 0.

The original `FrameRateHelper.ToFrameRateValue` (`0x6653030`) maps the four setting modes to these rates. `LiveManager.OnUpdate` (`0x5ad24c4`) forwards `AppTimeManager.GetGameDeltaTime` into the live entry point on each update; the music clock supplies judgement time.

## Score ledger

The original 1.1.0 Android ARM64 client records note score commands at the note's `MusicScorePosition.TimeMs`. `LiveScoreController.AddNoteScore` (`0x55b1030`) creates that command through `LiveNoteScoreCommand.Create` (`0x55abcf8`).

`LiveScoreCalculator.ExecuteCommand` (`0x55af92c`) merges factor and score commands by their timestamps. Factors precede equal-time scores. It asks `IComboGetter.GetTimingCombo` for the combo at the score command's timestamp before calculating that note. Earlier ledger buckets can be undone and recomputed when commands arrive later.

`ScoreCalculationUtility.GetFrame` (`0x55ac854`) returns `ceil(f32(timeMs) / 40)`. Those buckets index the ledger and its recomputation; command ordering inside each bucket still uses the timestamps.

## Cumulative activation and expiry

`LiveSkillExecuteTrigger.Update` (`0x55e8ac4`) preserves the skill event's timestamp in its trigger result. `SkillEffectUpdater.UpdateStay` (`0x55eb374`) copies it to `ExecuteTimeMs` and updates the cumulative counter for the current frame.

`NoteCumulativeScoreUpApplier.Update` (`0x55d6720`) handles three states:

| State | Factor command time |
| --- | --- |
| Execute frame, 2 | `ExecuteTimeMs` |
| Executing, 3 | Current `SkillFrameResult.TimeMs` |
| End frame, 4 | `FinishTimeMs`; removes the existing factor |

For ordinary expiry, `SkillEffectUpdater.UpdateExecuting` (`0x55eb68c`) detects `durationMs + extendedMs < elapsedMs` and records `FinishTimeMs = ExecuteTimeMs + ceil(durationMs + extendedMs)`. At exact equality, the executing state can still update the counter; the next update removes its factor at the recorded finish timestamp.

Direct execution of the original function with `ExecuteTimeMs = 1001` and a five-second duration leaves state 3 at time 6001. An update at 6016 changes it to state 4 with `FinishTimeMs = 6001`.

## Reachable schedules in the engine

Flick reservations are the input state carried between updates. The engine caches independent flick intervals and checks other judgement frames separately. Spatial candidates include exterior cells beyond widened hit areas. Input words share their observable selection state and complete finite input languages. Frames without flick judgements factor flick reservations from Enter inputs. Every retained outcome is checked by the whole-frame selector.

Timing search shares immutable frame domains and propagates watched conflicts. Late Perfect flicks that require a saved input first check the preceding update. Failed flick components also check two-update windows for smaller certified conflicts. A single-counter AP fallback advances legal frame states together with the original binary32 factor ledger. A relaxed-window optimum requires a full input certificate; a complete frame search retains its actual touches for replay. `guideAvailable: false` identifies an unavailable fixed early-input plan while preserving the numeric `best` field and the separate `worst` and `optimal` results.

## Artifact identity

These addresses refer to original `libil2cpp.so` SHA-256 `119a19de9dfae037e75b4443769247ac38df4f1e60e61031363aa1227be48d4e`. The native execution checks restore the original client's readonly constants as well as its executable instructions.
