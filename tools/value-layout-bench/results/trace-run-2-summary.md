| case | execute ns/instruction E / Box / Slots | Box/E | Slots/E | construct µs E / Box / Slots | release µs E / Box / Slots | peak requested B E / Box / Slots |
|---|---:|---:|---:|---:|---:|---:|
| story_branch | 2.131 / 3.120 / 2.972 | 1.464 | 1.395 | 0.050 / 0.080 / 0.081 | 0.030 / 0.070 / 0.060 | 752 / 440 / 440 |
| ui_properties | 2.422 / 4.295 / 4.143 | 1.774 | 1.711 | 0.050 / 0.091 / 0.100 | 0.030 / 0.060 / 0.060 | 800 / 464 / 464 |
| float_animation | 2.029 / 4.236 / 4.110 | 2.088 | 2.026 | 0.050 / 0.081 / 0.090 | 0.030 / 0.060 / 0.061 | 768 / 448 / 448 |
| wide_trace_000pct | 2.518 / 4.465 / 4.208 | 1.773 | 1.671 | 1.382 / 5.288 / 5.027 | 0.040 / 0.371 / 0.321 | 17136 / 8632 / 8632 |
| wide_trace_001pct | 2.486 / 4.003 / 3.921 | 1.610 | 1.577 | 1.412 / 5.358 / 5.138 | 0.040 / 0.491 / 0.430 | 17136 / 9112 / 9016 |
| wide_trace_010pct | 2.495 / 4.140 / 4.004 | 1.659 | 1.605 | 1.463 / 6.660 / 6.149 | 0.041 / 0.952 / 0.461 | 17136 / 12552 / 11704 |
| wide_trace_050pct | 2.489 / 4.801 / 4.412 | 1.929 | 1.773 | 1.462 / 11.387 / 9.264 | 0.040 / 3.675 / 1.262 | 17136 / 37344 / 33208 |
| wide_trace_100pct | 2.476 / 5.108 / 5.005 | 2.063 | 2.022 | 1.382 / 13.570 / 7.051 | 0.040 / 11.497 / 1.392 | 17136 / 66016 / 57784 |

Time min–max relative to median (construct / execute / release; all samples retained):
- story_branch explicit16: 0.800–3.200 / 0.993–1.016 / 1.000–1.000 (11 samples)
- story_branch nanbox8_box_rc: 0.875–2.263 / 0.991–1.003 / 0.857–1.714 (11 samples)
- story_branch nanbox8_slots_rc: 0.988–2.346 / 0.992–1.015 / 0.833–2.517 (11 samples)
- ui_properties explicit16: 1.000–7.220 / 0.992–1.007 / 0.667–1.000 (11 samples)
- ui_properties nanbox8_box_rc: 0.879–1.758 / 0.995–1.005 / 1.000–3.517 (11 samples)
- ui_properties nanbox8_slots_rc: 0.900–1.400 / 0.996–1.006 / 1.000–10.017 (11 samples)
- float_animation explicit16: 0.800–1.400 / 0.990–1.017 / 1.000–1.667 (11 samples)
- float_animation nanbox8_box_rc: 0.864–1.247 / 0.996–1.009 / 1.000–1.667 (11 samples)
- float_animation nanbox8_slots_rc: 0.889–1.111 / 0.998–1.003 / 0.984–1.328 (11 samples)
- wide_trace_000pct explicit16: 0.920–1.182 / 0.991–1.020 / 1.000–6.750 (11 samples)
- wide_trace_000pct nanbox8_box_rc: 0.985–2.830 / 0.996–1.027 / 0.970–1.620 (11 samples)
- wide_trace_000pct nanbox8_slots_rc: 0.994–1.018 / 0.998–1.015 / 0.997–1.495 (11 samples)
- wide_trace_001pct explicit16: 0.908–1.297 / 0.995–1.019 / 1.000–3.250 (11 samples)
- wide_trace_001pct nanbox8_box_rc: 0.993–1.138 / 0.997–1.056 / 0.978–2.489 (11 samples)
- wide_trace_001pct nanbox8_slots_rc: 0.988–1.123 / 0.997–1.050 / 0.953–2.351 (11 samples)
- wide_trace_010pct explicit16: 0.938–1.486 / 0.989–1.192 / 0.732–1.707 (11 samples)
- wide_trace_010pct nanbox8_box_rc: 0.953–1.060 / 0.998–1.007 / 0.958–1.199 (11 samples)
- wide_trace_010pct nanbox8_slots_rc: 0.870–1.023 / 0.996–1.011 / 0.889–1.803 (11 samples)
- wide_trace_050pct explicit16: 0.938–3.254 / 0.992–1.007 / 0.750–1.525 (11 samples)
- wide_trace_050pct nanbox8_box_rc: 0.938–1.059 / 0.997–1.002 / 0.891–1.308 (11 samples)
- wide_trace_050pct nanbox8_slots_rc: 0.646–1.036 / 0.997–1.013 / 0.761–3.571 (11 samples)
- wide_trace_100pct explicit16: 0.942–1.094 / 0.991–1.003 / 0.750–1.500 (11 samples)
- wide_trace_100pct nanbox8_box_rc: 0.986–1.035 / 0.997–1.013 / 0.990–1.037 (11 samples)
- wide_trace_100pct nanbox8_slots_rc: 0.994–1.027 / 0.998–1.004 / 0.986–1.022 (11 samples)
