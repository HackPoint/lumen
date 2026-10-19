# Lumen, version over version (linux)

Platform linux-x86_64. Every version is the file Lumen published for it, scored with today's engine on the same corpus and the same inputs. `benchmarks/versions/METHOD.md` is the method; `benchmarks/versions/run.sh` reproduces this file.

## How to read the table

- Each figure shows its change against the previous version (`vs previous`) and against no Lumen (`vs no Lumen`). Dollars change by a difference, everything else by a percentage.
- `n/a — <reason>` is a cell that cannot exist. It is never 0 and never enters a change.
- A cell in bold is a measurement that failed. An ingest that did not finish is `incomplete: N of M turns`, never a throughput.
- `(script)` marks a script hook (bash and python3, up to 1.5.1). Script hooks are never compared with binary hooks.
- `worse` is defined per metric in METHOD.md ("What is worse"): tokens and dollars by the median; latency, ingest, cold start and memory only when the runs separate. A change inside the noise is `equal`, not worse.
- `compress_logs` is not measured by the token runner and has no row.

MODELLED: dollars are scored against a reference session, not a real one: `claude-sonnet-5-5` at the prices of `crates/lumen-core/src/rates.rs` as of 2026-10-13, the context and output sizes lumen-core measured (362965 prompt tokens, 1085 output tokens), the session closed more than an hour before the clock. Net dollars are the engine's floor.
R from the no-Lumen E2E arm: not available yet. Only the R=0 floor is shown.

Run notes:

- a fixture: the figures are made up to exercise every case

## Versions

| version | asset | notes |
|---|---|---|
| no Lumen | no Lumen: a file is read whole, with the Read cap |  |
| 1.0.0 | n/a — 1.0.0 shipped no Linux app asset |  |
| 1.1.5 | Lumen_1.1.5_amd64.deb (sha256 abababababababababababababababababababababababababababababababab) | hooks are script hook (bash + python3), interpreted from the tag; cold start: warm file cache |
| 1.5.1 | Lumen_1.5.1_amd64.deb (sha256 abababababababababababababababababababababababababababababababab) | hooks are script hook (bash + python3), interpreted from the tag; cold start: warm file cache |
| 1.6.0 | Lumen_1.6.0_amd64.deb (sha256 abababababababababababababababababababababababababababababababab) | cold start: warm file cache |
| 1.7.0 | Lumen_1.7.0_amd64.deb (sha256 abababababababababababababababababababababababababababababababab) | cold start: warm file cache |
| 1.8.0 | Lumen_1.8.0_amd64.deb (sha256 abababababababababababababababababababababababababababababababab) | cold start: warm file cache |

## Tokens returned per call

| metric | no Lumen | 1.0.0 | 1.1.5 | 1.5.1 | 1.6.0 | 1.7.0 | 1.8.0 |
|---|---|---|---|---|---|---|---|
| smart_read outline, <300 lines (tokens, median) | 1500 | n/a — 1.0.0 shipped no Linux app asset | 450<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -70.0% | 375<br>vs previous: -16.7% better<br>vs no Lumen: -75.0% | 300<br>vs previous: -20.0% better<br>vs no Lumen: -80.0% | 360<br>vs previous: +20.0% **worse**<br>vs no Lumen: -76.0% | 300<br>vs previous: -16.7% better<br>vs no Lumen: -80.0% |
| smart_read outline, 300-999 lines (tokens, median) | 6000 | n/a — 1.0.0 shipped no Linux app asset | 1800<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -70.0% | 1500<br>vs previous: -16.7% better<br>vs no Lumen: -75.0% | 1200<br>vs previous: -20.0% better<br>vs no Lumen: -80.0% | 1440<br>vs previous: +20.0% **worse**<br>vs no Lumen: -76.0% | 1200<br>vs previous: -16.7% better<br>vs no Lumen: -80.0% |
| smart_read outline, 1000-2999 lines (tokens, median) | 20000 | n/a — 1.0.0 shipped no Linux app asset | 6000<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -70.0% | 5000<br>vs previous: -16.7% better<br>vs no Lumen: -75.0% | 4000<br>vs previous: -20.0% better<br>vs no Lumen: -80.0% | 4800<br>vs previous: +20.0% **worse**<br>vs no Lumen: -76.0% | 4000<br>vs previous: -16.7% better<br>vs no Lumen: -80.0% |
| smart_read outline, 3000+ lines (tokens, median) | 25000 | n/a — 1.0.0 shipped no Linux app asset | 13500<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -46.0% | 11250<br>vs previous: -16.7% better<br>vs no Lumen: -55.0% | 9000<br>vs previous: -20.0% better<br>vs no Lumen: -64.0% | 10800<br>vs previous: +20.0% **worse**<br>vs no Lumen: -56.8% | 9000<br>vs previous: -16.7% better<br>vs no Lumen: -64.0% |
| smart_read outline, all sizes (tokens, median) | 10000 | n/a — 1.0.0 shipped no Linux app asset | 3000<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -70.0% | 2500<br>vs previous: -16.7% better<br>vs no Lumen: -75.0% | 2000<br>vs previous: -20.0% better<br>vs no Lumen: -80.0% | 2400<br>vs previous: +20.0% **worse**<br>vs no Lumen: -76.0% | 2000<br>vs previous: -16.7% better<br>vs no Lumen: -80.0% |
| recall_file, <300 lines (tokens, median) | 1500 | n/a — 1.0.0 shipped no Linux app asset | 400<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -73.3% | 400<br>vs previous: +0.0% (equal)<br>vs no Lumen: -73.3% | 400<br>vs previous: +0.0% (equal)<br>vs no Lumen: -73.3% | 400<br>vs previous: +0.0% (equal)<br>vs no Lumen: -73.3% | 400<br>vs previous: +0.0% (equal)<br>vs no Lumen: -73.3% |
| recall_file, 300-999 lines (tokens, median) | 6000 | n/a — 1.0.0 shipped no Linux app asset | 402<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -93.3% | 402<br>vs previous: +0.0% (equal)<br>vs no Lumen: -93.3% | 402<br>vs previous: +0.0% (equal)<br>vs no Lumen: -93.3% | 402<br>vs previous: +0.0% (equal)<br>vs no Lumen: -93.3% | 402<br>vs previous: +0.0% (equal)<br>vs no Lumen: -93.3% |
| recall_file, 1000-2999 lines (tokens, median) | 20000 | n/a — 1.0.0 shipped no Linux app asset | 404<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -98.0% | 404<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.0% | 404<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.0% | 404<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.0% | 404<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.0% |
| recall_file, 3000+ lines (tokens, median) | 25000 | n/a — 1.0.0 shipped no Linux app asset | 406<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -98.4% | 406<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.4% | 406<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.4% | 406<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.4% | 406<br>vs previous: +0.0% (equal)<br>vs no Lumen: -98.4% |
| recall_file, all sizes (tokens, median) | 10000 | n/a — 1.0.0 shipped no Linux app asset | 403<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -96.0% | 403<br>vs previous: +0.0% (equal)<br>vs no Lumen: -96.0% | 403<br>vs previous: +0.0% (equal)<br>vs no Lumen: -96.0% | 403<br>vs previous: +0.0% (equal)<br>vs no Lumen: -96.0% | 403<br>vs previous: +0.0% (equal)<br>vs no Lumen: -96.0% |
| repeat-read stub, <300 lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 40<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 40<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| repeat-read stub, 300-999 lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 40<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 40<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| repeat-read stub, 1000-2999 lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 40<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 40<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| repeat-read stub, 3000+ lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 40<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 40<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| repeat-read stub, all sizes (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 40<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 40<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| edit delta, <300 lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 150<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 150<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| edit delta, 300-999 lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 152<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 152<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| edit delta, 1000-2999 lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 154<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 154<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| edit delta, 3000+ lines (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 156<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 156<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |
| edit delta, all sizes (tokens, median) | n/a — no Lumen: a file is read whole each time, there is no repeat-read gate | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | 153<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 153<br>vs previous: +0.0% (equal)<br>vs no Lumen: no delta (no figure to compare with) |

## Net dollars per call (MODELLED)

| metric | no Lumen | 1.0.0 | 1.1.5 | 1.5.1 | 1.6.0 | 1.7.0 | 1.8.0 |
|---|---|---|---|---|---|---|---|
| smart_read outline, 1 call, <300 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0464<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0464 | -$0.0461<br>vs previous: +$0.0003 better<br>vs no Lumen: -$0.0461 | -$0.0457<br>vs previous: +$0.0003 better<br>vs no Lumen: -$0.0457 | -$0.0460<br>vs previous: -$0.0003 **worse**<br>vs no Lumen: -$0.0460 | -$0.0457<br>vs previous: +$0.0003 better<br>vs no Lumen: -$0.0457 |
| smart_read outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0363<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0363 | -$0.0350<br>vs previous: +$0.0013 better<br>vs no Lumen: -$0.0350 | -$0.0336<br>vs previous: +$0.0013 better<br>vs no Lumen: -$0.0336 | -$0.0347<br>vs previous: -$0.0011 **worse**<br>vs no Lumen: -$0.0347 | -$0.0336<br>vs previous: +$0.0011 better<br>vs no Lumen: -$0.0336 |
| smart_read outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0276<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0276 | -$0.0209<br>vs previous: +$0.0067 better<br>vs no Lumen: -$0.0209 | -$0.0142<br>vs previous: +$0.0067 better<br>vs no Lumen: -$0.0142 | -$0.0196<br>vs previous: -$0.0054 **worse**<br>vs no Lumen: -$0.0196 | -$0.0142<br>vs previous: +$0.0054 better<br>vs no Lumen: -$0.0142 |
| smart_read outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0680<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0680 | -$0.0545<br>vs previous: +$0.0135 better<br>vs no Lumen: -$0.0545 | -$0.0411<br>vs previous: +$0.0135 better<br>vs no Lumen: -$0.0411 | -$0.0519<br>vs previous: -$0.0108 **worse**<br>vs no Lumen: -$0.0519 | -$0.0411<br>vs previous: +$0.0108 better<br>vs no Lumen: -$0.0411 |
| smart_read outline, 1 call, all sizes (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0430<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0430 | -$0.0377<br>vs previous: +$0.0053 better<br>vs no Lumen: -$0.0377 | -$0.0336<br>vs previous: +$0.0041 better<br>vs no Lumen: -$0.0336 | -$0.0357<br>vs previous: -$0.0021 **worse**<br>vs no Lumen: -$0.0357 | -$0.0336<br>vs previous: +$0.0021 better<br>vs no Lumen: -$0.0336 |
| recall_file after an outline, 1 call, <300 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0462<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0462 | -$0.0462<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0462 | -$0.0462<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0462 | -$0.0462<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0462 | -$0.0462<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0462 |
| recall_file after an outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0300<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0300 | -$0.0300<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0300 | -$0.0300<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0300 | -$0.0300<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0300 | -$0.0300<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0300 |
| recall_file after an outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | $0.0109<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 |
| recall_file after an outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | $0.0109<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 | $0.0109<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: +$0.0109 |
| recall_file after an outline, 1 call, all sizes (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0157<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0157 | -$0.0157<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0157 | -$0.0157<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0157 | -$0.0157<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0157 | -$0.0157<br>vs previous: +$0.0000 (equal)<br>vs no Lumen: -$0.0157 |
| outline then repeat-read stub, 2 calls, <300 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0450<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0450 | -$0.0449<br>vs previous: +$0.0001 better<br>vs no Lumen: -$0.0449 |
| outline then repeat-read stub, 2 calls, 300-999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0301<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0301 | -$0.0298<br>vs previous: +$0.0003 better<br>vs no Lumen: -$0.0298 |
| outline then repeat-read stub, 2 calls, 1000-2999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | $0.0043<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: +$0.0043 | $0.0057<br>vs previous: +$0.0014 better<br>vs no Lumen: +$0.0057 |
| outline then repeat-read stub, 2 calls, 3000+ lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0040<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0040 | -$0.0012<br>vs previous: +$0.0028 better<br>vs no Lumen: -$0.0012 |
| outline then repeat-read stub, 2 calls, all sizes (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0168<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0168 | -$0.0164<br>vs previous: +$0.0005 better<br>vs no Lumen: -$0.0164 |
| outline then edit delta, 2 calls, <300 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0455<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0455 | -$0.0454<br>vs previous: +$0.0001 better<br>vs no Lumen: -$0.0454 |
| outline then edit delta, 2 calls, 300-999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0306<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0306 | -$0.0303<br>vs previous: +$0.0003 better<br>vs no Lumen: -$0.0303 |
| outline then edit delta, 2 calls, 1000-2999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | $0.0038<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: +$0.0038 | $0.0051<br>vs previous: +$0.0014 better<br>vs no Lumen: +$0.0051 |
| outline then edit delta, 2 calls, 3000+ lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0045<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0045 | -$0.0017<br>vs previous: +$0.0028 better<br>vs no Lumen: -$0.0017 |
| outline then edit delta, 2 calls, all sizes (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | n/a — this version has no repeat reads (they arrived in 1.7.0) | -$0.0173<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0173 | -$0.0169<br>vs previous: +$0.0005 better<br>vs no Lumen: -$0.0169 |
| blocked Read then outline, 1 call, <300 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | n/a — a Read under 300 lines is not blocked | n/a — a Read under 300 lines is not blocked | n/a — a Read under 300 lines is not blocked | n/a — a Read under 300 lines is not blocked | n/a — a Read under 300 lines is not blocked |
| blocked Read then outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0363<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0363 | -$0.0350<br>vs previous: +$0.0013 better<br>vs no Lumen: -$0.0350 | -$0.0336<br>vs previous: +$0.0013 better<br>vs no Lumen: -$0.0336 | -$0.0347<br>vs previous: -$0.0011 **worse**<br>vs no Lumen: -$0.0347 | -$0.0336<br>vs previous: +$0.0011 better<br>vs no Lumen: -$0.0336 |
| blocked Read then outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0276<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0276 | -$0.0209<br>vs previous: +$0.0067 better<br>vs no Lumen: -$0.0209 | -$0.0142<br>vs previous: +$0.0067 better<br>vs no Lumen: -$0.0142 | -$0.0196<br>vs previous: -$0.0054 **worse**<br>vs no Lumen: -$0.0196 | -$0.0142<br>vs previous: +$0.0054 better<br>vs no Lumen: -$0.0142 |
| blocked Read then outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0680<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0680 | -$0.0545<br>vs previous: +$0.0135 better<br>vs no Lumen: -$0.0545 | -$0.0411<br>vs previous: +$0.0135 better<br>vs no Lumen: -$0.0411 | -$0.0519<br>vs previous: -$0.0108 **worse**<br>vs no Lumen: -$0.0519 | -$0.0411<br>vs previous: +$0.0108 better<br>vs no Lumen: -$0.0411 |
| blocked Read then outline, 1 call, all sizes (USD net, median, R=0 (floor)) | $0.0000 (baseline) | n/a — 1.0.0 shipped no Linux app asset | -$0.0363<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: -$0.0363 | -$0.0350<br>vs previous: +$0.0013 better<br>vs no Lumen: -$0.0350 | -$0.0276<br>vs previous: +$0.0073 better<br>vs no Lumen: -$0.0276 | -$0.0347<br>vs previous: -$0.0071 **worse**<br>vs no Lumen: -$0.0347 | -$0.0276<br>vs previous: +$0.0071 better<br>vs no Lumen: -$0.0276 |

## Hook and tool latency

| metric | no Lumen | 1.0.0 | 1.1.5 | 1.5.1 | 1.6.0 | 1.7.0 | 1.8.0 |
|---|---|---|---|---|---|---|---|
| hook.floor p50 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 3.000 (script)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 3.000 (script)<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 5.000<br>vs previous: no delta (script hooks and binary hooks are not compared)<br>vs no Lumen: no delta (no figure to compare with) | 5.000<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 5.000<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| hook.floor p90 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 3.600 (script)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 3.600 (script)<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 6.000<br>vs previous: no delta (script hooks and binary hooks are not compared)<br>vs no Lumen: no delta (no figure to compare with) | 6.000<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 6.000<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) |
| hook.floor p99 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 4.500 (script)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 4.500 (script)<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 7.500<br>vs previous: no delta (script hooks and binary hooks are not compared)<br>vs no Lumen: no delta (no figure to compare with) | 7.500<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 7.500<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) |
| hook.intercept_block.spawn p50 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 45.000 (script)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 42.000 (script)<br>vs previous: -6.7% better<br>vs no Lumen: no delta (no figure to compare with) | 6.000<br>vs previous: no delta (script hooks and binary hooks are not compared)<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: +50.0% **worse**<br>vs no Lumen: no delta (no figure to compare with) | 6.000<br>vs previous: -33.3% better<br>vs no Lumen: no delta (no figure to compare with) |
| hook.intercept_block.spawn p90 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 54.000 (script)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 50.400 (script)<br>vs previous: -6.7%<br>vs no Lumen: no delta (no figure to compare with) | 7.200<br>vs previous: no delta (script hooks and binary hooks are not compared)<br>vs no Lumen: no delta (no figure to compare with) | 10.800<br>vs previous: +50.0%<br>vs no Lumen: no delta (no figure to compare with) | 7.200<br>vs previous: -33.3%<br>vs no Lumen: no delta (no figure to compare with) |
| hook.intercept_block.spawn p99 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 67.500 (script)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 63.000 (script)<br>vs previous: -6.7%<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: no delta (script hooks and binary hooks are not compared)<br>vs no Lumen: no delta (no figure to compare with) | 13.500<br>vs previous: +50.0%<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: -33.3%<br>vs no Lumen: no delta (no figure to compare with) |
| server.ping p50 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 0.400<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 0.380<br>vs previous: -5.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 0.350<br>vs previous: -7.9% better<br>vs no Lumen: no delta (no figure to compare with) | 0.365<br>vs previous: +4.3% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 0.350<br>vs previous: -4.1% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| server.ping p90 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 0.480<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 0.456<br>vs previous: -5.0%<br>vs no Lumen: no delta (no figure to compare with) | 0.420<br>vs previous: -7.9%<br>vs no Lumen: no delta (no figure to compare with) | 0.438<br>vs previous: +4.3%<br>vs no Lumen: no delta (no figure to compare with) | 0.420<br>vs previous: -4.1%<br>vs no Lumen: no delta (no figure to compare with) |
| server.ping p99 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 0.600<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 0.570<br>vs previous: -5.0%<br>vs no Lumen: no delta (no figure to compare with) | 0.525<br>vs previous: -7.9%<br>vs no Lumen: no delta (no figure to compare with) | 0.547<br>vs previous: +4.3%<br>vs no Lumen: no delta (no figure to compare with) | 0.525<br>vs previous: -4.1%<br>vs no Lumen: no delta (no figure to compare with) |
| hook.meter_bash.spawn p50 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | n/a — the Bash meter arrived in 1.6.0 | n/a — the Bash meter arrived in 1.6.0 | 6.500<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 9.500<br>vs previous: +46.2% **worse**<br>vs no Lumen: no delta (no figure to compare with) | 6.500<br>vs previous: -31.6% better<br>vs no Lumen: no delta (no figure to compare with) |
| hook.meter_bash.spawn p90 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | n/a — the Bash meter arrived in 1.6.0 | n/a — the Bash meter arrived in 1.6.0 | 7.800<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 11.400<br>vs previous: +46.2%<br>vs no Lumen: no delta (no figure to compare with) | 7.800<br>vs previous: -31.6%<br>vs no Lumen: no delta (no figure to compare with) |
| hook.meter_bash.spawn p99 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | n/a — the Bash meter arrived in 1.6.0 | n/a — the Bash meter arrived in 1.6.0 | 9.750<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 14.250<br>vs previous: +46.2%<br>vs no Lumen: no delta (no figure to compare with) | 9.750<br>vs previous: -31.6%<br>vs no Lumen: no delta (no figure to compare with) |
| server.smart_read.300-999 p50 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 9.000<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 9.000<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| server.smart_read.300-999 p90 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 10.800<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 10.800<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 10.800<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 10.800<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 10.800<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) |
| server.smart_read.300-999 p99 (ms) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — not measured for this version | 13.500<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 13.500<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 13.500<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 13.500<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) | 13.500<br>vs previous: +0.0%<br>vs no Lumen: no delta (no figure to compare with) |

## Daemon ingest

| metric | no Lumen | 1.0.0 | 1.1.5 | 1.5.1 | 1.6.0 | 1.7.0 | 1.8.0 |
|---|---|---|---|---|---|---|---|
| ingest throughput (MB/s, median of runs) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 20.00<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | **incomplete: 12 of 40 turns** | 24.00<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 26.00<br>vs previous: +8.3% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 28.00<br>vs previous: +7.7% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| ingest throughput (turns per s, median of runs) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 800<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | **incomplete: 12 of 40 turns** | 960<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 1040<br>vs previous: +8.3% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 1120<br>vs previous: +7.7% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |

## Cold start

| metric | no Lumen | 1.0.0 | 1.1.5 | 1.5.1 | 1.6.0 | 1.7.0 | 1.8.0 |
|---|---|---|---|---|---|---|---|
| cold start: daemon spawn to port and schema (ms, median of runs) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 60.0<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 58.0<br>vs previous: -3.3% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 50.0<br>vs previous: -13.8% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 51.0<br>vs previous: +2.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 49.0<br>vs previous: -3.9% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| cold start: lumen-mcp spawn to initialize (ms, median of runs) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 15.0<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 14.5<br>vs previous: -3.3% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 12.5<br>vs previous: -13.8% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 12.8<br>vs previous: +2.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 12.2<br>vs previous: -3.9% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| cold start: first hook spawn (ms, median of runs) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | n/a — this version's hooks are scripts, timed by the script-hook series | n/a — this version's hooks are scripts, timed by the script-hook series | 5.0<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 5.0<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 5.0<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |

## Idle CPU and memory

| metric | no Lumen | 1.0.0 | 1.1.5 | 1.5.1 | 1.6.0 | 1.7.0 | 1.8.0 |
|---|---|---|---|---|---|---|---|
| idle CPU, daemon, empty (CPU s/min) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 0.300<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 0.280<br>vs previous: -6.7% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.200<br>vs previous: -28.6% better<br>vs no Lumen: no delta (no figure to compare with) | 0.210<br>vs previous: +5.0% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.200<br>vs previous: -4.8% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) |
| idle RSS, daemon, empty (MiB, median) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 40.0 (peak 42.0)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 41.0 (peak 43.0)<br>vs previous: +2.5% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 38.0 (peak 40.0)<br>vs previous: -7.3% better<br>vs no Lumen: no delta (no figure to compare with) | 39.0 (peak 41.0)<br>vs previous: +2.6% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 38.0 (peak 40.0)<br>vs previous: -2.6% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| idle CPU, daemon, after ingest (CPU s/min) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 0.600<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 0.560<br>vs previous: -6.7% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.400<br>vs previous: -28.6% better<br>vs no Lumen: no delta (no figure to compare with) | 0.420<br>vs previous: +5.0% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.400<br>vs previous: -4.8% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) |
| idle RSS, daemon, after ingest (MiB, median) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 80.0 (peak 82.0)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 82.0 (peak 84.0)<br>vs previous: +2.5% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 76.0 (peak 78.0)<br>vs previous: -7.3% better<br>vs no Lumen: no delta (no figure to compare with) | 78.0 (peak 80.0)<br>vs previous: +2.6% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 76.0 (peak 78.0)<br>vs previous: -2.6% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |
| idle CPU, lumen-mcp (CPU s/min) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 0.030<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 0.028<br>vs previous: -6.7% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.020<br>vs previous: -28.6% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.021<br>vs previous: +5.0% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) | 0.020<br>vs previous: -4.8% (equal: within the margin)<br>vs no Lumen: no delta (no figure to compare with) |
| idle RSS, lumen-mcp (MiB, median) | n/a — no Lumen: there is no hook, no server and no daemon to time | n/a — 1.0.0 shipped no Linux app asset | 10.0 (peak 12.0)<br>vs previous: no delta (no figure to compare with)<br>vs no Lumen: no delta (no figure to compare with) | 10.0 (peak 12.0)<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 9.0 (peak 11.0)<br>vs previous: -10.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 9.0 (peak 11.0)<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) | 9.0 (peak 11.0)<br>vs previous: +0.0% (equal: ranges overlap)<br>vs no Lumen: no delta (no figure to compare with) |

## Regressions

16 regression(s). Each is published as worse and opens an issue in HackPoint/lumen. The commands below are printed, not run.

| metric | older | newer | evidence |
|---|---|---|---|
| smart_read outline, <300 lines (tokens, median) | 1.6.0: 300 | 1.7.0: 360 | the median went from 300 to 360 |
| smart_read outline, 300-999 lines (tokens, median) | 1.6.0: 1200 | 1.7.0: 1440 | the median went from 1200 to 1440 |
| smart_read outline, 1000-2999 lines (tokens, median) | 1.6.0: 4000 | 1.7.0: 4800 | the median went from 4000 to 4800 |
| smart_read outline, 3000+ lines (tokens, median) | 1.6.0: 9000 | 1.7.0: 10800 | the median went from 9000 to 10800 |
| smart_read outline, all sizes (tokens, median) | 1.6.0: 2000 | 1.7.0: 2400 | the median went from 2000 to 2400 |
| smart_read outline, 1 call, <300 lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0457 | 1.7.0: -$0.0460 | the median went from -$0.0457 to -$0.0460 |
| smart_read outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0336 | 1.7.0: -$0.0347 | the median went from -$0.0336 to -$0.0347 |
| smart_read outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0142 | 1.7.0: -$0.0196 | the median went from -$0.0142 to -$0.0196 |
| smart_read outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0411 | 1.7.0: -$0.0519 | the median went from -$0.0411 to -$0.0519 |
| smart_read outline, 1 call, all sizes (USD net, median, R=0 (floor)) | 1.6.0: -$0.0336 | 1.7.0: -$0.0357 | the median went from -$0.0336 to -$0.0357 |
| blocked Read then outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0336 | 1.7.0: -$0.0347 | the median went from -$0.0336 to -$0.0347 |
| blocked Read then outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0142 | 1.7.0: -$0.0196 | the median went from -$0.0142 to -$0.0196 |
| blocked Read then outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) | 1.6.0: -$0.0411 | 1.7.0: -$0.0519 | the median went from -$0.0411 to -$0.0519 |
| blocked Read then outline, 1 call, all sizes (USD net, median, R=0 (floor)) | 1.6.0: -$0.0276 | 1.7.0: -$0.0347 | the median went from -$0.0276 to -$0.0347 |
| hook.intercept_block.spawn p50 (ms) | 1.6.0: 6.000 | 1.7.0: 9.000 | the runs separate: the newer range 8.800 to 9.200 lies entirely above the older range 5.900 to 6.100 |
| hook.meter_bash.spawn p50 (ms) | 1.6.0: 6.500 | 1.7.0: 9.500 | the runs separate: the newer range 9.500 to 9.500 lies entirely above the older range 6.500 to 6.500 |

## Issues to file

```sh
gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, <300 lines (tokens, median) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, <300 lines (tokens, median) (Tokens returned per call)
- older: 1.6.0, 300
- newer: 1.7.0, 360
- evidence: the median went from 300 to 360

Likely cause, to confirm: the outline, recall or repeat-read output changed between the tags: `crates/lumen-mcp` and `crates/lumen-core`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 300-999 lines (tokens, median) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 300-999 lines (tokens, median) (Tokens returned per call)
- older: 1.6.0, 1200
- newer: 1.7.0, 1440
- evidence: the median went from 1200 to 1440

Likely cause, to confirm: the outline, recall or repeat-read output changed between the tags: `crates/lumen-mcp` and `crates/lumen-core`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 1000-2999 lines (tokens, median) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 1000-2999 lines (tokens, median) (Tokens returned per call)
- older: 1.6.0, 4000
- newer: 1.7.0, 4800
- evidence: the median went from 4000 to 4800

Likely cause, to confirm: the outline, recall or repeat-read output changed between the tags: `crates/lumen-mcp` and `crates/lumen-core`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 3000+ lines (tokens, median) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 3000+ lines (tokens, median) (Tokens returned per call)
- older: 1.6.0, 9000
- newer: 1.7.0, 10800
- evidence: the median went from 9000 to 10800

Likely cause, to confirm: the outline, recall or repeat-read output changed between the tags: `crates/lumen-mcp` and `crates/lumen-core`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, all sizes (tokens, median) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, all sizes (tokens, median) (Tokens returned per call)
- older: 1.6.0, 2000
- newer: 1.7.0, 2400
- evidence: the median went from 2000 to 2400

Likely cause, to confirm: the outline, recall or repeat-read output changed between the tags: `crates/lumen-mcp` and `crates/lumen-core`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 1 call, <300 lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 1 call, <300 lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0457
- newer: 1.7.0, -$0.0460
- evidence: the median went from -$0.0457 to -$0.0460

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0336
- newer: 1.7.0, -$0.0347
- evidence: the median went from -$0.0336 to -$0.0347

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0142
- newer: 1.7.0, -$0.0196
- evidence: the median went from -$0.0142 to -$0.0196

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0411
- newer: 1.7.0, -$0.0519
- evidence: the median went from -$0.0411 to -$0.0519

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: smart_read outline, 1 call, all sizes (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: smart_read outline, 1 call, all sizes (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0336
- newer: 1.7.0, -$0.0357
- evidence: the median went from -$0.0336 to -$0.0357

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: blocked Read then outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: blocked Read then outline, 1 call, 300-999 lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0336
- newer: 1.7.0, -$0.0347
- evidence: the median went from -$0.0336 to -$0.0347

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: blocked Read then outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: blocked Read then outline, 1 call, 1000-2999 lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0142
- newer: 1.7.0, -$0.0196
- evidence: the median went from -$0.0142 to -$0.0196

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: blocked Read then outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: blocked Read then outline, 1 call, 3000+ lines (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0411
- newer: 1.7.0, -$0.0519
- evidence: the median went from -$0.0411 to -$0.0519

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: blocked Read then outline, 1 call, all sizes (USD net, median, R=0 (floor)) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: blocked Read then outline, 1 call, all sizes (USD net, median, R=0 (floor)) (Net dollars per call (MODELLED))
- older: 1.6.0, -$0.0276
- newer: 1.7.0, -$0.0347
- evidence: the median went from -$0.0276 to -$0.0347

The dollar figures are MODELLED (the primary reference session, scored with today'\''s engine).

Likely cause, to confirm: follows the token figures of the same tool and bucket, or the way the engine prices the extra round: `crates/lumen-core/src/savings.rs`. See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: hook.intercept_block.spawn p50 (ms) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: hook.intercept_block.spawn p50 (ms) (Hook and tool latency)
- older: 1.6.0, 6.000
- newer: 1.7.0, 9.000
- evidence: the runs separate: the newer range 8.800 to 9.200 lies entirely above the older range 5.900 to 6.100

Likely cause, to confirm: the hook or the request path did more work, or started slower: `crates/lumen-mcp/src` (and `.claude/hooks` for script hooks). See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

gh issue create -R HackPoint/lumen --title 'bench: hook.meter_bash.spawn p50 (ms) is worse in 1.7.0 than in 1.6.0 (linux)' --body 'The version-over-version benchmark (`benchmarks/versions/run.sh`, linux) found a newer version worse than the one before it.

- metric: hook.meter_bash.spawn p50 (ms) (Hook and tool latency)
- older: 1.6.0, 6.500
- newer: 1.7.0, 9.500
- evidence: the runs separate: the newer range 9.500 to 9.500 lies entirely above the older range 6.500 to 6.500

Likely cause, to confirm: the hook or the request path did more work, or started slower: `crates/lumen-mcp/src` (and `.claude/hooks` for script hooks). See `git log --oneline v1.6.0..v1.7.0`.

Published as worse in `benchmarks/versions/results/linux.md`; the row is not left out.
'

```
