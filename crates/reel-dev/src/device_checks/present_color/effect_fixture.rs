//! Build device comparisons through the production typed effect generator.
use reel_render::effect::{
    FragmentCoverage, Language, OutputTail, compile_fragment_variant,
    gxp_compiler::{Compiler, Stage},
};
use reel_ui::stage::effect::{
    EffectBinary, EffectInput, EffectNode, EffectNodeId, EffectOp, EffectProgram, EffectType,
    EffectValue,
};
use std::{path::Path, sync::Arc};

#[derive(Default)]
struct Graph(Vec<EffectNode>);
impl Graph {
    fn node(&mut self, ty: EffectType, op: EffectOp) -> EffectNodeId {
        let id = EffectNodeId(self.0.len() as u32);
        self.0.push(EffectNode { ty, op });
        id
    }
    fn float(&mut self, value: f32) -> EffectNodeId {
        self.node(
            EffectType::Float,
            EffectOp::Constant(EffectValue::Float(value.to_bits())),
        )
    }
    fn binary(
        &mut self,
        op: EffectBinary,
        left: EffectNodeId,
        right: EffectNodeId,
    ) -> EffectNodeId {
        self.node(EffectType::Float, EffectOp::Binary { op, left, right })
    }
}

fn mean(axis: u8, taps: usize) -> Arc<EffectProgram> {
    let mut g = Graph::default();
    let uv = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv));
    let x = g.node(EffectType::Float, EffectOp::Extract { value: uv, lane: 0 });
    let y = g.node(EffectType::Float, EffectOp::Extract { value: uv, lane: 1 });
    let size = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Size));
    let extent = g.node(
        EffectType::Float,
        EffectOp::Extract {
            value: size,
            lane: axis,
        },
    );
    let zero = g.float(0.0);
    let mut channels = [zero; 4];
    for tap in 0..taps {
        let offset = g.float(tap as f32 - (taps / 2) as f32);
        let offset = g.binary(EffectBinary::Divide, offset, extent);
        let mut at = [x, y];
        at[axis as usize] = g.binary(EffectBinary::Add, at[axis as usize], offset);
        let at = g.node(EffectType::Vec2, EffectOp::Vec2(at));
        let sample = g.node(EffectType::Vec4, EffectOp::SourceSample { uv: at });
        for (lane, channel) in channels.iter_mut().enumerate() {
            let value = g.node(
                EffectType::Float,
                EffectOp::Extract {
                    value: sample,
                    lane: lane as u8,
                },
            );
            *channel = g.binary(EffectBinary::Add, *channel, value);
        }
    }
    let divisor = g.float(taps as f32);
    for channel in &mut channels {
        *channel = g.binary(EffectBinary::Divide, *channel, divisor);
    }
    let output = g.node(EffectType::Vec4, EffectOp::Vec4(channels));
    EffectProgram::new(Vec::new(), g.0, output).expect("valid mean fixture")
}

pub fn prepare(compiler: &Compiler, output: &Path) {
    const REFERENCE: &str = include_str!("effect_output_reference.cg");
    println!("cargo:rerun-if-changed=src/device_checks/present_color/effect_output_reference.cg");
    for (name, axis, taps) in [("h3", 0, 3), ("v3", 1, 3), ("h17", 0, 17)] {
        let program = mean(axis, taps);
        for (suffix, coverage) in [
            ("local", FragmentCoverage::Local),
            ("full", FragmentCoverage::FullTarget),
        ] {
            let candidate = compile_fragment_variant(
                &program,
                Language::Cg,
                OutputTail::Ordinary,
                coverage,
                1 << 20,
            )
            .expect("compile production effect")
            .source;
            assert!(candidate.contains("float quantize = float(mode == 1.0);"));
            let start = candidate
                .find("float4 reel_output(")
                .expect("output helper");
            let end = start + candidate[start..].find("\n}\n").expect("output helper end") + 3;
            // Only the output helper changes; sampling, clipping and the author
            // expression are generated once and shared byte-for-byte.
            let mut baseline = candidate.clone();
            baseline.replace_range(start..end, REFERENCE);
            for (variant, source) in [("baseline", baseline), ("candidate", candidate)] {
                let stem = format!("effect-output-{name}-{suffix}-{variant}");
                std::fs::write(output.join(format!("{stem}.cg")), &source).unwrap();
                std::fs::write(
                    output.join(format!("{stem}.gxp")),
                    compiler
                        .compile(Stage::Fragment, &source)
                        .expect("compile effect oracle GXP"),
                )
                .unwrap();
            }
        }
    }
}
