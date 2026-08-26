### Per rung: requested, declared, delivered

| request kbps | declared kbps | declared raster | n | distinct sizes | delivered kbps med | s min | s med | s max |
|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 320 | 302 | 320x180 | 40 | 30 | 315 | 0.852 | 1.044 | 1.285 |
| 720 | 425 | 480x270 | 40 | 31 | 405 | 0.777 | 0.952 | 1.155 |
| 4000 | 3538 | 1280x720 | 40 | 40 | 2318 | 0.432 | 0.655 | 0.843 |
| 12000 | 11356 | 1920x1080 | 40 | 40 | 5300 | 0.239 | 0.467 | 0.797 |
| 22000 | 20379 | 1920x1080 | 40 | 40 | 7084 | 0.178 | 0.348 | 0.785 |

### Adjacent pairs: what the ladder assumes vs what it delivers

| step | paired | catalog | declared | delivered geomean | delivered min | delivered max | catalog error |
|---|---:|---:|---:|---:|---:|---:|---:|
| 320->720 | 40 | 2.25 | 1.41 | 1.29 | 1.22 | 1.34 | 0.57x |
| 720->4000 | 40 | 5.56 | 8.32 | 5.64 | 3.77 | 7.65 | 1.02x |
| 4000->12000 | 40 | 3.00 | 3.21 | 2.38 | 1.68 | 3.17 | 0.79x |
| 12000->22000 | 40 | 1.83 | 1.79 | 1.41 | 1.24 | 1.91 | 0.77x |

### Cost: control plane and just-in-time production

| request kbps | decision ms | master ms | media playlist ms | ttfb min | ttfb med | ttfb max | body throughput med Mbit/s |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 320 | 14.4 | 8.4 | 169.7 | 216.6 | 324.4 | 552.9 | 97 |
| 720 | 8.7 | 6.5 | 95.4 | 215.5 | 320.6 | 533.9 | 129 |
| 4000 | 12.4 | 7.6 | 97.1 | 209.0 | 318.5 | 543.3 | 701 |
| 12000 | 10.8 | 6.5 | 91.9 | 207.4 | 317.2 | 747.8 | 1350 |
| 22000 | 11.3 | 8.6 | 95.5 | 206.9 | 315.1 | 751.9 | 1729 |

### Are the two candidate size bounds ever violated, and how loose are they

| bound | scope | n | violations | slack med | slack max |
|---|---|---:|---:|---:|---:|
| B1 rate <= declared | rung 320 | 40 | 30 | 1.044 | 1.285 |
| B1 rate <= declared | rung 720 | 40 | 7 | 0.952 | 1.155 |
| B1 rate <= declared | rung 4000 | 40 | 0 | 0.655 | 0.843 |
| B1 rate <= declared | rung 12000 | 40 | 0 | 0.467 | 0.797 |
| B1 rate <= declared | rung 22000 | 40 | 0 | 0.348 | 0.785 |
| B2 ratio <= declared ratio | 320->720 | 40 | 0 | 0.916 | 0.951 |
| B2 ratio <= declared ratio | 720->4000 | 40 | 0 | 0.681 | 0.919 |
| B2 ratio <= declared ratio | 4000->12000 | 40 | 0 | 0.748 | 0.989 |
| B2 ratio <= declared ratio | 12000->22000 | 40 | 3 | 0.754 | 1.067 |
