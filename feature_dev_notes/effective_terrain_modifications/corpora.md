# Corpora

Real terrains are client or agency projects, so they are named here by letter only.
The local key is `LOCAL_corpus_key.md` (git-excluded). All real modifications below
were authored in RAS Mapper unless marked "scripted" or "writer".

## Real modified terrains (14 unique, scanned 2026-10-30)

| Corpus | Layers | Features by form | Runnable | Result |
|---|---|---|---|---|
| A | 3 | ellipses 2 (Set); high ground 3 (Hi); channels 2 (Lo) | yes | mods exact; base TIN differs |
| B1 | 6 | polygons 8+2 (Set); levee 1 (Hi); culvert-burn channel 2 (Lo); high ground 5 (Hi); channels 4 (Lo) | yes | mods 3 px at 1/32; base TIN differs |
| B2 | 6 | polygons 8+1; levees 1+3; channels 2+6 | yes | mods 433 px off; base differs |
| B3 | 4 | polygons 8+5; levee 1; channels 2 | yes | mods exact; base 27,289 mask px |
| B4 | 3 | polygons 8; levee 1; channels 2 | yes | mods exact; base 478 px |
| B5 | 1 | polygon 1 | no (source TIFF missing) | — |
| C | 13 | polygons with boundary profiles and Lo/Set; levees incl. one **Add** and one with a NaN template; channels incl. **System joins** and a Set channel | refused | several individual layers exact |
| D | 232 | polygons 204 (Set); channels 22 (Lo); levees 4 (Hi); circles 4 (Set); **control points** | refused | — |
| E | 6 | polygons 23 (Set/Hi/Lo, **control points**); levees 26 (Hi/**Set**); channel 1 | refused | — |
| F | 1 | channels 703 (Lo; one with no profile), scripted | refused | — |
| G, H | 1 / 1 | channels 153 / 89 (some Max Reach <= Top Width), scripted | refused | — |
| I | 2 | channels 2 (both Priority 0), ras-commander writer | refused (shared priority) | — |
| J | 1 | channel 1 (crash reproduction) | — | — |

Totals: ground lines on every terrain; polygons on 8; ellipses on 2. No other
Type exists in the corpora (no fill surfaces, no separate culvert type: "culvert
burns" are ground-line channels).

## Synthetic terrains

Built with HEC-RAS 7.0.1 from Bald Eagle clips (public HEC sample data):
- T1-T5: multi-source base mosaic cases (see `base_mosaic_rule.md`).
- W1-W4: one modification rule each (W1 700 ft pond polygon, W3 channel, W4
  priority overlap of a Lo channel over a Set pad).
- The writer-authored 22 km channel case sits on T2.
