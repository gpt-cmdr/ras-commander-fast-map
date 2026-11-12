# Other map qualification

Historical private evidence: artifact identities below predate public 0.13.0.
Integrated release validation is recorded separately; these observations do not
certify the new artifact or qualify untested models/options.

These are bounded private qualification results, not a blanket native-parity
claim. All raster acceptance checks compare complete valid/nodata masks and
every valid binary32 bit against independent native exports. Model data,
reference caches and generated rasters remain outside this repository.

Installed Windows wheel gates also cover delivery to the pool share. Atomic
JSON replacement retries only transient Windows access/sharing/lock errors,
with at most 310 ms of scheduled delay; it retains the previous receipt and
propagates a persistent failure. A held-reader regression and the complete
217-time NAS reduction exercise this behavior. No timing speedup is claimed.

## Whole-simulation products

Davis HEC-RAS 7.0.1, all 217 saved output times, a 0.5 ft threshold and the
681 by 569 terrain grid match native arrival time, duration and percent
inundated exactly. The opt-in `whole_simulation_native` integration test
replays the prepared request and checks all three complete buffers. Set
`FAST_MAP_WHOLE_SIMULATION_FIXTURE` to an external fixture manifest and run
the ignored test explicitly.

Native postprocessing caches are effective reference inputs. A cache from a
different saved-time sequence can silently produce incorrect reference maps;
use a fresh native cache for the exact plan HDF being qualified. This result
does not qualify storage duration, irregular output intervals or arbitrary
thresholds.

Bald Eagle dam-break p07 also matches native arrival time and percent
inundated exactly over all 865 saved times, 87,039 physical 2D cells and one
storage pool, at a 0.5 ft threshold on the 1192 by 819 prepared 100 ft terrain
grid. Both products have 53,055 valid pixels. This gate exercises the public
Commander wrapper, separate immutable HDF copies, complete raster masks/bits
and delivered EPSG:2271 georeferencing. Storage duration remains refused.

## Grouped depth maxima

See [the grouped-depth contract](depth-groups.md) for the qualified private mixed fixture C p67
physical-2D component reduction over three saved profiles. A maximum of saved
depth maps has different semantics from rendering a native Max-WSE profile.

## Velocity and derived maps

Davis HEC-RAS 7.0.1 Max velocity on the retained nine-time fixture matches
all 22,041 valid pixels and the complete mask exactly after source-tile
coordinate and square-cell normalized scanline stepping corrections. Depth
and WSE remain exact on this comparison. This does not yet qualify every
individual saved-time velocity map or other fixture. A separate native Davis saved-time
gate (index 4, 10JAN2000 12:40:00) matches Depth, WSE, velocity, depth times
velocity, depth times velocity squared and Froude exactly across all 10,036
valid pixels and the complete masks.

Derived Max depth times velocity, depth times velocity squared and Froude
number also match native exactly over all 217 Davis saved times: 21,965 valid
pixels for each product, complete masks and every valid Float32 bit. A
separate nine-time reduction gate passes all three products as well.
Velocity Max consumes the
stored maximum face velocity and maximum WSE; derived Max products reduce
the product calculated at each saved time. These operations are distinct.
Do not infer derived-product parity from WSE/depth parity alone.

## Boundary vectors and terrain metadata

The deterministic boundary method does not claim native vector parity.
Native repeat exports showed geometry variation without complete capture of
all effective native state; this does not establish intrinsic nondeterminism.
Fresh isolated reference exports and a candidate comparison remain required.

The pinned terrio 0.4.2 reader reconstructs a user-defined geographic CRS
with datum EPSG:6152 as generic NAD83 for the original Davis TIFF. The public
Commander CRS guard correctly rejects that mismatch. Raw prepared numerical
tests do not qualify this public-wrapper path. A later private terrio reader
preserves the datum through geographic-on-datum lookup. A narrow private
0.4.2 maintenance backport is preferred over a broad reader upgrade, because
the newer reader also changes its Rayon-pool behavior. Either dependency
change requires separate terrain I/O and distribution qualification.
(2026-11-11: the engine now uses terrio 0.8.0; its own thread pool is bounded
to the request budget by the CLI. Spring River p02 and Whisky Chitto 1D
products were requalified bit for bit; the Davis public-wrapper CRS path has
not been rerun.) Do not relabel source
terrain or bypass boundary provenance checks to obtain a passing gate.
