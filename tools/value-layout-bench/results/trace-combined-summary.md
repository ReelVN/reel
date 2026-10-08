| case | execute ns/instruction E / Box / Slots | Box/E | Slots/E | construct µs E / Box / Slots | release µs E / Box / Slots | peak requested B E / Box / Slots |
|---|---:|---:|---:|---:|---:|---:|
| story_branch | 2.130 / 3.162 / 2.978 | 1.484 | 1.398 | 0.060 / 0.090 / 0.090 | 0.030 / 0.090 / 0.070 | 752 / 440 / 440 |
| ui_properties | 2.375 / 4.313 / 4.165 | 1.816 | 1.754 | 0.065 / 0.106 / 0.110 | 0.030 / 0.070 / 0.080 | 800 / 464 / 464 |
| float_animation | 2.012 / 4.289 / 4.135 | 2.132 | 2.055 | 0.060 / 0.096 / 0.100 | 0.040 / 0.081 / 0.066 | 768 / 448 / 448 |
| wide_trace_000pct | 2.507 / 4.309 / 4.090 | 1.719 | 1.632 | 1.397 / 5.268 / 5.078 | 0.040 / 0.371 / 0.331 | 17136 / 8632 / 8632 |
| wide_trace_001pct | 2.496 / 4.100 / 3.936 | 1.642 | 1.577 | 1.432 / 5.448 / 5.273 | 0.040 / 0.596 / 0.451 | 17136 / 9112 / 9016 |
| wide_trace_010pct | 2.524 / 4.199 / 4.010 | 1.664 | 1.589 | 1.518 / 6.695 / 6.189 | 0.050 / 0.982 / 0.491 | 17136 / 12552 / 11704 |
| wide_trace_050pct | 2.494 / 4.824 / 4.412 | 1.934 | 1.769 | 1.547 / 11.462 / 9.364 | 0.050 / 3.695 / 1.262 | 17136 / 37344 / 33208 |
| wide_trace_100pct | 2.483 / 5.162 / 4.619 | 2.079 | 1.860 | 1.417 / 13.750 / 7.165 | 0.050 / 11.572 / 1.422 | 17136 / 66016 / 57784 |

Time min–max relative to median (construct / execute / release; all samples retained):
- story_branch explicit16: 0.667–9.017 / 0.993–1.125 / 1.000–4.000 (22 samples)
- story_branch nanbox8_box_rc: 0.778–2.889 / 0.978–1.103 / 0.667–2.556 (22 samples)
- story_branch nanbox8_slots_rc: 0.889–5.667 / 0.985–2.454 / 0.714–4.857 (22 samples)
- ui_properties explicit16: 0.769–5.862 / 0.976–1.026 / 0.667–4.000 (22 samples)
- ui_properties nanbox8_box_rc: 0.758–1.801 / 0.991–1.358 / 0.857–12.157 (22 samples)
- ui_properties nanbox8_slots_rc: 0.818–4.000 / 0.991–1.045 / 0.750–7.513 (22 samples)
- float_animation explicit16: 0.667–22.533 / 0.990–1.026 / 0.750–1.775 (22 samples)
- float_animation nanbox8_box_rc: 0.733–2.000 / 0.984–1.036 / 0.745–3.354 (22 samples)
- float_animation nanbox8_slots_rc: 0.800–2.010 / 0.992–1.045 / 0.916–3.053 (22 samples)
- wide_trace_000pct explicit16: 0.910–1.183 / 0.989–1.025 / 1.000–6.750 (22 samples)
- wide_trace_000pct nanbox8_box_rc: 0.989–2.840 / 0.948–1.065 / 0.943–2.590 (22 samples)
- wide_trace_000pct nanbox8_slots_rc: 0.984–1.454 / 0.952–1.044 / 0.967–1.695 (22 samples)
- wide_trace_001pct explicit16: 0.881–1.279 / 0.991–1.022 / 1.000–5.000 (22 samples)
- wide_trace_001pct nanbox8_box_rc: 0.976–1.120 / 0.974–1.031 / 0.805–2.050 (22 samples)
- wide_trace_001pct nanbox8_slots_rc: 0.963–1.094 / 0.993–1.046 / 0.909–2.242 (22 samples)
- wide_trace_010pct explicit16: 0.904–1.525 / 0.978–1.178 / 0.600–2.400 (22 samples)
- wide_trace_010pct nanbox8_box_rc: 0.948–1.076 / 0.984–1.019 / 0.929–2.020 (22 samples)
- wide_trace_010pct nanbox8_slots_rc: 0.864–3.122 / 0.994–1.010 / 0.835–2.570 (22 samples)
- wide_trace_050pct explicit16: 0.887–3.094 / 0.990–1.012 / 0.600–2.220 (22 samples)
- wide_trace_050pct nanbox8_box_rc: 0.925–4.089 / 0.993–1.022 / 0.886–1.301 (22 samples)
- wide_trace_050pct nanbox8_slots_rc: 0.640–1.076 / 0.996–1.012 / 0.761–3.571 (22 samples)
- wide_trace_100pct explicit16: 0.919–1.173 / 0.988–1.048 / 0.600–4.000 (22 samples)
- wide_trace_100pct nanbox8_box_rc: 0.973–1.798 / 0.987–1.012 / 0.984–1.242 (22 samples)
- wide_trace_100pct nanbox8_slots_rc: 0.978–2.520 / 0.906–1.088 / 0.965–1.134 (22 samples)
