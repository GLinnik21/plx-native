### Per rung: requested, declared, delivered

| request kbps | declared kbps | declared raster | n | distinct sizes | delivered kbps med | s min | s med | s max |
|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 320 | 302 | 320x180 | 40 | 28 | 310 | 0.853 | 1.027 | 1.080 |
| 720 | 425 | 480x270 | 40 | 26 | 404 | 0.795 | 0.951 | 0.992 |
| 4000 | 3493 | 1280x720 | 40 | 37 | 2722 | 0.745 | 0.779 | 0.801 |
| 12000 | 11356 | 1920x1080 | 40 | 38 | 8317 | 0.710 | 0.732 | 0.757 |
| 22000 | 20895 | 3840x2160 | 40 | 40 | 15173 | 0.700 | 0.726 | 0.757 |

### Adjacent pairs: what the ladder assumes vs what it delivers

| step | paired | catalog | declared | delivered geomean | delivered min | delivered max | catalog error |
|---|---:|---:|---:|---:|---:|---:|---:|
| 320->720 | 40 | 2.25 | 1.41 | 1.31 | 1.26 | 1.43 | 0.58x |
| 720->4000 | 40 | 5.56 | 8.22 | 6.78 | 6.48 | 8.12 | 1.22x |
| 4000->12000 | 40 | 3.00 | 3.25 | 3.06 | 2.99 | 3.19 | 1.02x |
| 12000->22000 | 40 | 1.83 | 1.84 | 1.82 | 1.79 | 1.86 | 0.99x |

### Cost: control plane and just-in-time production

| request kbps | decision ms | master ms | media playlist ms | ttfb min | ttfb med | ttfb max | body throughput med Mbit/s |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 320 | 130.4 | 6.6 | 367.3 | 102.2 | 159.8 | 544.9 | 163 |
| 720 | 26.6 | 13.6 | 115.5 | 102.0 | 111.7 | 542.8 | 223 |
| 4000 | 23.7 | 12.4 | 63.5 | 210.4 | 221.9 | 655.2 | 2006 |
| 12000 | 25.9 | 13.1 | 66.0 | 321.5 | 425.7 | 752.3 | 4543 |
| 22000 | 19.4 | 14.2 | 69.5 | 743.9 | 861.5 | 1305.7 | 7092 |

### Are the two candidate size bounds ever violated, and how loose are they

| bound | scope | n | violations | slack med | slack max |
|---|---|---:|---:|---:|---:|
| B1 rate <= declared | rung 320 | 40 | 32 | 1.027 | 1.080 |
| B1 rate <= declared | rung 720 | 40 | 0 | 0.951 | 0.992 |
| B1 rate <= declared | rung 4000 | 40 | 0 | 0.779 | 0.801 |
| B1 rate <= declared | rung 12000 | 40 | 0 | 0.732 | 0.757 |
| B1 rate <= declared | rung 22000 | 40 | 0 | 0.726 | 0.757 |
| B2 ratio <= declared ratio | 320->720 | 40 | 2 | 0.924 | 1.014 |
| B2 ratio <= declared ratio | 720->4000 | 40 | 0 | 0.818 | 0.988 |
| B2 ratio <= declared ratio | 4000->12000 | 40 | 0 | 0.939 | 0.981 |
| B2 ratio <= declared ratio | 12000->22000 | 40 | 5 | 0.990 | 1.011 |
