| case | execute ns/instruction E / Box / Slots | Box/E | Slots/E | construct µs E / Box / Slots | release µs E / Box / Slots | peak requested B E / Box / Slots |
|---|---:|---:|---:|---:|---:|---:|
| story_branch | 2.128 / 3.260 / 3.041 | 1.532 | 1.429 | 0.100 / 0.100 / 0.121 | 0.040 / 0.100 / 0.100 | 752 / 440 / 440 |
| ui_properties | 2.328 / 4.336 / 4.185 | 1.862 | 1.797 | 0.090 / 0.130 / 0.120 | 0.040 / 0.110 / 0.120 | 800 / 464 / 464 |
| float_animation | 1.999 / 4.365 / 4.237 | 2.183 | 2.119 | 0.080 / 0.111 / 0.110 | 0.040 / 0.120 / 0.090 | 768 / 448 / 448 |
| wide_trace_000pct | 2.494 / 4.100 / 3.920 | 1.644 | 1.572 | 1.442 / 5.268 / 5.088 | 0.040 / 0.371 / 0.351 | 17136 / 8632 / 8632 |
| wide_trace_001pct | 2.504 / 4.136 / 3.949 | 1.652 | 1.577 | 1.502 / 5.539 / 5.308 | 0.060 / 0.601 / 0.560 | 17136 / 9112 / 9016 |
| wide_trace_010pct | 2.547 / 4.241 / 4.019 | 1.665 | 1.578 | 1.613 / 6.740 / 6.299 | 0.060 / 1.101 / 0.571 | 17136 / 12552 / 11704 |
| wide_trace_050pct | 2.500 / 4.848 / 4.415 | 1.939 | 1.766 | 1.572 / 11.597 / 9.505 | 0.060 / 3.715 / 1.292 | 17136 / 37344 / 33208 |
| wide_trace_100pct | 2.506 / 5.173 / 4.196 | 2.064 | 1.674 | 1.442 / 13.811 / 7.261 | 0.050 / 11.628 / 1.462 | 17136 / 66016 / 57784 |

Time min–max relative to median (construct / execute / release; all samples retained):
- story_branch explicit16: 0.600–5.410 / 0.995–1.126 / 0.750–3.000 (11 samples)
- story_branch nanbox8_box_rc: 0.800–2.600 / 0.979–1.070 / 0.600–2.300 (11 samples)
- story_branch nanbox8_slots_rc: 0.744–4.215 / 0.964–2.404 / 0.700–3.400 (11 samples)
- ui_properties explicit16: 0.556–4.233 / 0.995–1.008 / 0.750–3.000 (11 samples)
- ui_properties nanbox8_box_rc: 0.615–1.462 / 0.994–1.351 / 0.545–7.736 (11 samples)
- ui_properties nanbox8_slots_rc: 0.842–3.667 / 0.994–1.040 / 0.583–3.092 (11 samples)
- float_animation explicit16: 0.625–16.900 / 0.996–1.009 / 0.750–1.775 (11 samples)
- float_animation nanbox8_box_rc: 0.721–1.721 / 0.986–1.018 / 0.500–2.250 (11 samples)
- float_animation nanbox8_slots_rc: 0.909–1.827 / 0.979–1.019 / 0.667–2.222 (11 samples)
- wide_trace_000pct explicit16: 0.945–1.146 / 0.994–1.007 / 1.000–2.000 (11 samples)
- wide_trace_000pct nanbox8_box_rc: 0.991–1.053 / 0.997–1.017 / 0.943–2.590 (11 samples)
- wide_trace_000pct nanbox8_slots_rc: 0.994–1.451 / 0.994–1.016 / 0.940–1.598 (11 samples)
- wide_trace_001pct explicit16: 0.840–1.101 / 0.994–1.019 / 0.667–3.333 (11 samples)
- wide_trace_001pct nanbox8_box_rc: 0.969–1.025 / 0.990–1.015 / 0.899–1.532 (11 samples)
- wide_trace_001pct nanbox8_slots_rc: 0.976–1.032 / 0.992–1.009 / 0.804–1.091 (11 samples)
- wide_trace_010pct explicit16: 0.863–1.435 / 0.991–1.022 / 0.833–2.000 (11 samples)
- wide_trace_010pct nanbox8_box_rc: 0.967–1.068 / 0.997–1.009 / 0.865–1.801 (11 samples)
- wide_trace_010pct nanbox8_slots_rc: 0.898–3.067 / 0.994–1.008 / 0.842–2.210 (11 samples)
- wide_trace_050pct explicit16: 0.930–3.045 / 0.988–1.010 / 0.667–1.850 (11 samples)
- wide_trace_050pct nanbox8_box_rc: 0.915–4.042 / 0.997–1.017 / 0.933–1.278 (11 samples)
- wide_trace_050pct nanbox8_slots_rc: 0.648–1.060 / 0.995–1.011 / 0.768–2.295 (11 samples)
- wide_trace_100pct explicit16: 0.903–1.153 / 0.991–1.038 / 0.800–4.000 (11 samples)
- wide_trace_100pct nanbox8_box_rc: 0.984–1.790 / 0.995–1.010 / 0.990–1.236 (11 samples)
- wide_trace_100pct nanbox8_slots_rc: 0.974–2.487 / 0.997–1.011 / 0.973–1.103 (11 samples)
