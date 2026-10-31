# Effective terrain with terrain modifications (parked, 2026-10-31)

**Status: parked.** The code is kept on branch `feat/effective-terrain-modifications`
(draft PR #21, CI green), with the commander side on gpt-cmdr/ras-commander branch
`feat/fim-effective-terrain-modifications` (draft PR #790). Neither is merged or
released. Pick the work up from here.

## Decision

Rust terrain operations are used **only on unmodified terrains**. Any terrain with
modifications goes to RAS Mapper (the HEC-RAS terrain export, and RAS Mapper's
own boundary). Callers enforce this before the engine runs, whatever an engine
build advertises: ras2cng reads the terrain HDF's `/Modifications` group and the
`.rasmap` modification layers first (`ras2cng.terrain.terrain_modifications`).

Why: the native effective terrain was only judged viable if it also reproduces
terrain modifications. The modification rules below are largely solved, but
**0 of 11 runnable real modified terrains came out pixel-identical to RAS Mapper
as a whole**: 6 carry modification forms not yet derived, and the other 5 differ
in the *base* mosaic (the rule from #19), with modifications switched off too.

## What is on the branch

- `ras2fim-batch effective-terrain`, contract `rasmapper-effective-terrain/2`:
  the base mosaic from #19 plus modification layers, applied in descending
  Priority. Advertises the supported forms in `ras2fim-batch method` and refuses
  everything unqualified with a reason.
- Supported forms: ground lines (levees and channels, SetIfHigher/SetIfLower),
  constant polygons (SetValue/Hi/Lo), unrotated ellipses.
- Optional per-source `crs_identity` (sources whose WKT text differs but whose CRS
  is equal were refused without it).
- Tests: 4 new lib tests in `terrain_mods`, 2 new in `tests/effective_terrain.rs`.

## Files

| File | Contents |
|---|---|
| [base_mosaic_rule.md](base_mosaic_rule.md) | The #19 base rule, its qualification, and the gaps the modified corpora exposed |
| [modification_rules.md](modification_rules.md) | Rules derived per modification type, qualification, open discrepancies |
| [corpora.md](corpora.md) | The real and synthetic terrains used, by form (anonymized) |
| [ras_commander_writer.md](ras_commander_writer.md) | `RasTerrainModWriter` bugs found and fixed in ras-commander #790 |
| [performance.md](performance.md) | Engine vs HEC-RAS export, time and memory |
| [reproduction.md](reproduction.md) | How references are made and how to re-qualify |
| [roadmap.md](roadmap.md) | What remains, in order of payoff, and the bar for un-parking |

`LOCAL_corpus_key.md` (if present) maps the anonymized corpus letters to local
project paths. It is excluded from git on purpose (client data); never commit it.

## Method rule

Every rule here was derived from RAS Mapper **outputs only**, by fitting
hypotheses pixel by pixel against HEC-RAS 7.0.1 exports. No vendor code was read,
decompiled or disassembled. Keep it that way.
