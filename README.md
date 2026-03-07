# Demo


```
$ plothole inspect --text "the quick brown fox jumps over the lazy dog" --font-name futural

Paths:          130 (65 pen-down, 65 pen-up)
Points:         532
Length (drawn): 1469.0 units
Plan time:      1.242375ms  (vel=500, accel=2000)

                    Original  NearestNeighbor (greedy)   Change
---------------------------------------------------------------
Pen-up length:  1114.4 units               777.1 units    -30.3%
Total length:   2583.4 units              2246.1 units    -13.1%
Plotter time:        27.65 s                   25.11 s     -9.2%
```

```
plothole animate --text "the quick brown fox jumps over the lazy dog" --font-name futural \
    --max-velocity 400 --acceleration 1500 --cornering 0.5 \
    --width 1200 --height 300 --output demo.gif --duration 8s
```

![Demo](demo.gif)
