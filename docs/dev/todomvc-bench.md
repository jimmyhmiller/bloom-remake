# The TodoMVC benchmark

`scripts/bench-todomvc.sh` runs the Blossom TodoMVC (`examples/web/todomvc.bls` in the browser host) as two extra
suites of Speedometer 3 (WebKit/Speedometer at `b0bc16e`), beside Speedometer's own TodoMVC suites, with
Speedometer's runner and steps: add 100 todos (`input`, then Enter as `keydown`, as its React, Vue, Preact, Svelte,
Angular and Lit suites do), click every checkbox, click every delete button. Each step's time is its synchronous work
plus the work until the next frame. The suites:

- `TodoMVC-Blossom`: the host in `?bench` mode — an empty start, the app's own source, no inspector or editor bar, and
  nothing saved (Speedometer's apps do not persist, except React's).
- `TodoMVC-Blossom-Persist`: `?bench=persist`, which saves the durable tables to localStorage after every event, as
  the app does outside a benchmark.

`tests/web/bench/todomvc.mjs` drives it in headless Chromium and prints the table; `--json FILE` keeps every metric.

## Results (2026-10-06, S17 + the bench host mode)

Headless Chromium 153.0.8010.12 on an Apple-silicon laptop, 10 iterations, mean ms per iteration:

| suite | total | Adding100Items | CompletingAllItems | DeletingAllItems |
|---|---:|---:|---:|---:|
| TodoMVC-Svelte | 6.3 | 3.4 | 2.0 | 0.8 |
| TodoMVC-Preact | 7.1 | 3.7 | 2.5 | 1.0 |
| TodoMVC-WebComponents | 9.0 | 5.1 | 2.4 | 1.6 |
| TodoMVC-Lit | 9.6 | 5.3 | 2.8 | 1.4 |
| TodoMVC-Vue | 13.5 | 7.0 | 3.8 | 2.7 |
| TodoMVC-Backbone | 15.5 | 7.5 | 4.8 | 3.2 |
| TodoMVC-Angular | 18.0 | 10.5 | 4.4 | 3.1 |
| TodoMVC-React | 20.4 | 9.1 | 7.1 | 4.3 |
| TodoMVC-JavaScript-ES6-Webpack | 21.4 | 10.9 | 6.7 | 3.7 |
| TodoMVC-React-Redux | 22.6 | 9.6 | 8.6 | 4.5 |
| TodoMVC-JavaScript-ES5 | 32.3 | 23.0 | 6.0 | 3.4 |
| TodoMVC-jQuery | 72.2 | 17.4 | 35.3 | 19.5 |
| **TodoMVC-Blossom** | **1482.0** | **481.7** | **764.3** | **235.9** |
| TodoMVC-Blossom-Persist | 1499.9 | 479.6 | 781.4 | 238.9 |

Blossom is about 20× jQuery, 73× React and 235× Svelte; saving after every event adds about 1%.

### Where the time goes

A CPU profile of the same workload (100 adds, 100 toggles, 100 deletes; 5 ms, 8 ms and 2.5 ms per event), inclusive:

| | share |
|---|---:|
| `App::dispatch` (an event and its rounds until they settle) | 91% |
| `Engine::step` (the rounds' evaluation) | 39% |
| `Page::diff` (the whole page before against the whole page after) | 24% |
| `Page::of` (the page model rebuilt from the output relations, every round) | 23% |
| applying the patches to the DOM | 1% |

Every round re-derives the whole page tree (the `frame` handler emits every element, attribute and text row each
tick), the host rebuilds its page model from all of those rows, and each event diffs the full page before and after;
an event runs at least two rounds (the event's round, and the round its staged writes show in). Each cost is linear
in the number of todos per event, so the run is quadratic in them. The way down is to make the page incremental end
to end: patches from the output relations' changes rather than a rebuild and a diff, and an engine that maintains the
page's rows across ticks instead of re-deriving them.
