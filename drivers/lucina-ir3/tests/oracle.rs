//! Compiled shaders run on the interpreter (`common`), against the same formula in Rust.

mod common;
use common::{Machine, f, run_raw};

fn close(got: &[u32], want: &[f32]) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        let g = f32::from_bits(*g);
        assert!((g - w).abs() <= 1e-5 * w.abs().max(1.0), "got {} want {} (all {:?})", g, w, want);
    }
}

#[test]
fn arithmetic_and_math() {
    let (a, b) = ([1.5f32, -2.0, 0.25], [4.0f32, 0.5, 8.0]);
    let (_, out) = run_raw(
        "@vertex fn vs(@location(0) a: vec3<f32>, @location(1) b: vec3<f32>) -> @location(0) vec4<f32> {
            let v = (a + b * 2.0 - a / b) * -1.0;
            return vec4<f32>(v.zy, dot(a, b), length(b));
        }",
        "vs",
        &[(0, f(&a)), (1, f(&b))],
        &[],
    );
    let v = |i: usize| (a[i] + b[i] * 2.0 - a[i] / b[i]) * -1.0;
    let d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let l = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
    close(&out[&0], &[v(2), v(1), d, l]);
}

#[test]
fn functions() {
    let x = [0.3f32, 2.7];
    let (_, out) = run_raw(
        "@vertex fn vs(@location(0) x: vec2<f32>) -> @location(0) vec4<f32> {
            let n = normalize(x);
            return vec4<f32>(n.x, floor(x.y) + fract(x.y) * 10.0, clamp(x.y, 0.0, 1.0) + mix(x.x, x.y, 0.25), pow(x.y, 2.0) + sin(x.x) + saturate(x.y));
        }",
        "vs",
        &[(0, f(&x))],
        &[],
    );
    let len = (x[0] * x[0] + x[1] * x[1]).sqrt();
    close(
        &out[&0],
        &[x[0] / len, x[1].floor() + x[1].fract() * 10.0, 1.0 + x[0] + (x[1] - x[0]) * 0.25, x[1].powf(2.0) + x[0].sin() + 1.0],
    );
}

#[test]
fn a_matrix_from_uniforms_and_structs_in_and_out() {
    // Columns of a 4x4, then a vec4 tint: std140.
    let mut u: Vec<f32> = (0..16).map(|i| i as f32 + 1.0).collect();
    u.extend([0.5, 0.25, 2.0, 1.0]);
    let p = [1.0f32, 2.0, 3.0];
    let (_, out) = run_raw(
        "struct U { m: mat4x4<f32>, tint: vec4<f32> }
         @group(0) @binding(0) var<uniform> u: U;
         struct In { @location(0) p: vec3<f32>, @location(1) w: f32 }
         struct Out { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> }
         @vertex fn vs(i: In) -> Out {
             var o: vec4<f32> = vec4<f32>(i.p, i.w);
             let r = u.m * o;
             return Out(r, u.tint * r.x);
         }",
        "vs",
        &[(0, f(&p)), (1, f(&[1.0]))],
        &f(&u),
    );
    let v = [p[0], p[1], p[2], 1.0];
    let r: Vec<f32> = (0..4).map(|row| (0..4).map(|col| u[col * 4 + row] * v[col]).sum()).collect();
    close(&out[&1000], &r);
    close(&out[&0], &[u[16] * r[0], u[17] * r[0], u[18] * r[0], u[19] * r[0]]);
}

#[test]
fn comparisons_selects_integers_conversions() {
    let (_, out) = run_raw(
        "@vertex fn vs(@location(0) a: f32, @location(1) n: i32, @location(2) m: u32) -> @location(0) vec4<f32> {
            let s = select(1.5, 2.5, a > 0.25);
            let i = (n + 5) ^ 3;
            let k = (m << 2u) | (m >> 1u);
            return vec4<f32>(s, f32(i), f32(k), select(7.0, 9.0, n < 0));
        }",
        "vs",
        &[(0, f(&[0.5])), (1, vec![(-9i32) as u32]), (2, vec![6])],
        &[],
    );
    let i = (-9i32 + 5) ^ 3;
    let k = (6u32 << 2) | (6 >> 1);
    close(&out[&0], &[2.5, i as f32, k as f32, 9.0]);
}

#[test]
fn the_interpreter_refuses_code_with_a_slot_removed() {
    let (c, _) = run_raw(
        "@vertex fn vs(@location(0) a: f32) -> @location(0) f32 { return (a * 2.0 + 1.0) * a; }",
        "vs",
        &[(0, f(&[3.0]))],
        &[],
    );
    let d = lucina_ir3::isa::Decoder::new(600);
    let nop = c.code.iter().position(|w| d.disasm(*w).unwrap().ends_with("nop")).expect("a nop");
    let mut broken = c.code.clone();
    broken.remove(nop);
    let mut m = Machine::new(vec![]);
    assert!(m.run(&broken).unwrap_err().contains("cycles after"), "must refuse");
}

#[test]
fn a_shader_that_needs_many_registers_reuses_them() {
    let mut body = String::from("var t = a;\n");
    for i in 0..300 {
        body += &format!("t = t * 1.01 + {}.0;\n", i % 7);
    }
    let src = format!("@vertex fn vs(@location(0) a: f32) -> @location(0) f32 {{ {body} return t; }}");
    let (_, out) = run_raw(&src, "vs", &[(0, f(&[2.0]))], &[]);
    let mut t = 2.0f32;
    for i in 0..300 {
        t = t * 1.01 + (i % 7) as f32;
    }
    close(&out[&0], &[t]);
}
