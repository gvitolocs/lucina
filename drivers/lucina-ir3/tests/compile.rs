//! naga to machine code, checked by disassembly while the emulator is not in.

use lucina_ir3::{frontend, isa::Decoder};

fn build(src: &str, entry: &str) -> frontend::Compiled {
    let m = naga::front::wgsl::parse_str(src).unwrap();
    frontend::compile(&m, entry).unwrap()
}

#[test]
fn a_vertex_shader_compiles() {
    let c = build(
        "struct U { m: mat4x4<f32>, tint: vec4<f32> }
         @group(0) @binding(0) var<uniform> u: U;
         @vertex fn vs(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
             let n = normalize(p);
             return u.m * vec4<f32>(n * 2.0 + u.tint.xyz, 1.0);
         }",
        "vs",
    );
    let d = Decoder::new(600);
    let text: Vec<String> = c.code.iter().map(|w| d.disasm(*w).unwrap()).collect();
    assert!(text.last().unwrap().contains("end"), "{:?}", text);
    assert_eq!(c.inputs[0].regs.len(), 3);
    assert_eq!(c.outputs[0].regs.len(), 4);
    assert!(text.iter().any(|t| t.contains("rsq")));
    for line in &text {
        println!("{line}");
    }
}
