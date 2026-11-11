# Effective terrain with terrain modifications

**Status (2026-11-11): qualified and advertised.** Parked on 2026-10-31; resumed on
2026-11-10 on branch `feat/effective-terrain-modifications-main` (rebased on
`main`). The base mosaic, the per-tile export TIN and every RAS-Mapper-authored
modification form in the corpora are now reproduced (`base_mosaic_rule.md`,
`modification_rules.md`, `roadmap.md`), and `ras2fim-batch method` advertises
`rasmapper-effective-terrain/2`. The history below records why it was parked.
Callers still decide separately whether to use the engine for modified
terrains (ras2cng's policy check, Commander's `read_layers`).

## Decision at parking (2026-10-31, superseded)

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

## What is on the branch (2026-11-11)

- `ras2fim-batch effective-terrain`, advertised as `rasmapper-effective-terrain/2`
  (request schemas 1 and 2): RAS Mapper's render-block base mosaic, its per-tile
  export TIN, and modification layers applied in descending Priority, with values
  rounded to 1/32 once at the end. `ras2fim-batch method` lists the supported and
  refused forms under `effective_terrain.modifications`.
- Supported forms: ground lines (levees and channels, SetValue/Add/SetIfHigher/
  SetIfLower, NaN templates, negative slopes, Max Reach <= Top Width, named and
  joined `System Name` systems, control points); polygons (constant, stored
  boundary profiles, control points, interior points; SetValue/Add/Hi/Lo);
  ellipses and circles, rotated or not. Optional `hecras_version` (`7.0.1`
  default, `6.6`).
- Refused with a reason: boundary elevations sampled from the terrain, shape-Z
  polygons, multi-part polygons that need a TIN, named ground lines with control
  points or a NaN template (and under 6.6), a NaN Top Width with a finite Max
  Reach, other modification types.
- Optional per-source `crs_identity` (sources whose WKT text differs but whose CRS
  is equal).
- Tests: `terrain_mods` and `export_tin` unit tests and `tests/effective_terrain.rs`
  integration tests (run `cargo test --locked --all-targets`); real-data
  qualification is opt-in and described in `reproduction.md`.

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
