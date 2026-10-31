# Performance (T14, shared host)

Full terrains; fresh process per run; peak RSS of the process tree. Engine figures
are `RasFim.build_effective_terrain` end to end.

| Corpus terrain | Valid px | HEC-RAS export (with mods) | Engine |
|---|---:|---|---|
| A | 164.3 M | 132 s / 378 MiB | 8.4 s / 872 MiB |
| B1 | 254.8 M | 198 s (repeat 205 s) / 426 MiB | 16.8 s / 2.35 GiB |
| B2 | 254.8 M | 279 s / 412 MiB | 15.5 s / 2.34 GiB |
| B4 | 254.7 M | 267 s / 399 MiB | 17.0 s / 2.33 GiB |
| B3 | 254.7 M | 233 s / 390 MiB | 14.9 s / 2.38 GiB |
| D (refused) | | 1,169 s / 1.27 GB (base: 464 s) | — |
| Bald Eagle Terrain50 (unmodified, T07) | 197.2 M | ~190 s | 7.0 s / 1.4 GB via RasFim |

- 12-17x faster than the export.
- About 5x the export's memory: the whole output grid is held in memory
  (4 bytes/px plus a 1-bit TIN map), admitted against `max_working_bytes` before any
  read. A strip-wise writer would remove most of this; not attempted.
- The modification pass itself is milliseconds (0.002-0.008 s for 5-22 features).
