# Original-client timing paths

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

## Artifact identity

These addresses refer to original `libil2cpp.so` SHA-256 `119a19de9dfae037e75b4443769247ac38df4f1e60e61031363aa1227be48d4e`. The native execution checks restore the original client's readonly constants as well as its executable instructions.
