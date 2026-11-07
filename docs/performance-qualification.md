# Private Windows performance qualification

Historical private evidence: artifact identities below predate public 0.13.0.
Integrated release validation is recorded separately; these observations do not
certify the new artifact or qualify untested models/options.

These observations apply to the stated fixtures and settings. They are not
general speedup guarantees or RASMapper timing comparisons.

## Environment and method

Measurements ran through verified SSH inside the Windows benchmark VM (CLB09),
whose Windows name remains CLB-04: Xeon W-1370P, eight guest logical CPUs and
24 GiB guest RAM. Exact input copies and fresh outputs used local NTFS scratch.
SCP, extraction, hashing and native-reference export time are outside engine
timings. Original model files were unchanged; no share credentials or login
caches were transferred. No infrastructure, security-service or toolchain
changes were needed.

The desktop had background activity, chiefly Search indexing. Admission
required three samples below 20% CPU and at least 6 GiB available RAM; some
runs exceeded that background estimate during execution. The raw records
retain those measurements. This was not an idle-machine or cold-cache test.
Windows process CPU has approximately 15.625 ms resolution. Short single-map
measurements are diagnostic only.

The main workload is Davis p02, all 217 saved times, plain sloping with rendered
velocity clipping, gravity 32.174, three derived Max products, and integer source
window [129,128,394,345] on the original 681 by 569 source grid. Requests retain
all global hydraulic geometry. The engine memory budget is 1 GiB. Every accepted
run checks complete masks and valid Float32 bits against independent HEC-RAS
7.0.1 reference buffers.

## Adopted operation selection

The known operation is selected before the pixel loop; formulas and floating
point operation order are unchanged. Hydraulic reconstruction, source-tile
rounding and donation scratch history are unchanged. Unknown-name behavior of
the public helper is preserved.

At two threads and one in-flight profile, three warm counterbalanced pairs
observed median request wall time 3.8660 s for the control and 3.7130 s for the
candidate, about 4.0% lower. Every paired candidate used less peak working set.
Process CPU differences were inconsistent, so no CPU speedup is claimed.

Control executable SHA-256:
`cd535a8333ef716f2c1d8ed9dfe15b50132af76a8fa8e6b068ecc78fe17e2a5d`.
Measured operation-selection candidate SHA-256:
`bc6b100b57fa314964a609b9eb2eb3326a9098a13c09b20762f35d9442ef1609`.
Both report version 0.11.0; this is private work, not a published release.

A separate face-only hydraulic reconstruction trial was discarded because its
timing and CPU results did not establish a clear benefit. Combined trial
measurements are retained as experimental evidence, not the adopted result.

## Independent profile concurrency

Using the control executable at a fixed four threads, three warm repeats per
setting observed these median request wall times:

| In-flight profiles | Median wall time | Effective limit |
| --- | ---: | ---: |
| 1 | 3.9410 s | 1 |
| 2 | 3.0942 s | 2 |
| 4 | 2.8438 s | 4 |

Four in-flight profiles used the same global four-thread pool and about 28%
less wall time than one on this fixture. All outputs remain exact. This
configuration result and the operation-selection result are separate
experiments; their percentages must not be added or multiplied. More profiles
require additional admitted memory, so CLI defaults and global budget controls
remain unchanged.

## Qualification scope

Seven staged cases pass complete native masks and Float32 bits on CLB09:
Depth/WSE, Max velocity, six single-time products, full-217 derived Max, a
source-grid crop, Davis temporal maps, and Bald Eagle p07 temporal maps with
storage over 865 times. These prepared engine tests do not close the original
Davis public-wrapper CRS reader gap or historical velocity fixture rechecks.
Raw timing, CPU, memory, admission and artifact-hash receipts are retained
outside the repository with the private profiling packet.
