//! The window manager's [`Scene`] as a list of compositor [`Op`]s, so the shell draws a space switch or the overview with
//! surfaces placed and tinted rather than pixels written per frame. Pure and headless: no I/O, no clock, no GPU, no allocation
//! beyond `out`. `lucina-wm-rules` decides *where* each space's surface goes (`controller::Scene`); this turns that into the ops
//! `lucina-app::compose` shows, the same shape as `dash::slide_ops`.
//!
//! A layer's `space` is an index into the shell's `Spaces` row, so the caller maps it to a surface (`surface_of`) and that
//! surface's size (`surface_size`); either missing means the layer is skipped. Each shown layer draws the whole surface into its
//! rectangle clipped to the screen, dimmed by its opacity as a white tint whose alpha is the opacity (255 = no tint), blended when
//! translucent and filtered when the sizes differ. Ops are bounded by `MAX_LAYERS` plus one backdrop fill, in the scene's draw order.

use lucina_app::compose::{Op, SurfaceId};
use lucina_app::gfx::Rect;
use lucina_app::theme;
use lucina_wm_rules::controller::{MAX_LAYERS, Scene};

/// The frame's ops for `scene`, appended to `out` (cleared first): the page's ground behind everything, then one layer per space
/// that has a surface, in draw order. `screen` is the display's `(width, height)` in pixels; `surface_of` maps a layer's space
/// index to the surface that holds it, `surface_size` a surface's pixels. Never more than [`MAX_LAYERS`] + 1 ops.
#[allow(dead_code)] // the shell's compositor wiring (a later Juno task) is this builder's first caller; only the tests use it now
pub fn scene_ops(
    scene: &Scene,
    surface_of: &dyn Fn(usize) -> Option<SurfaceId>,
    surface_size: &dyn Fn(SurfaceId) -> Option<(u32, u32)>,
    screen: (u32, u32),
    out: &mut Vec<Op>,
) {
    out.clear();
    let page = Rect::new(0, 0, screen.0 as i32, screen.1 as i32);
    // The page's ground behind everything, so a gap between spaces is the black the kit pages on, never stale pixels.
    out.push(Op::Fill {
        to: page,
        colour: theme::GROUND.0,
    });
    for layer in scene.as_slice().iter().take(MAX_LAYERS) {
        let Some(surface) = surface_of(layer.space) else {
            continue;
        };
        let Some((sw, sh)) = surface_size(surface) else {
            continue;
        };
        // `to` is the layer's rectangle clipped to the screen; empty after clipping means it was empty or fully outside, so skip.
        let to = Rect::new(layer.to.x, layer.to.y, layer.to.w, layer.to.h).intersect(&page);
        let from = Rect::new(0, 0, sw as i32, sh as i32);
        if to.is_empty() || from.is_empty() {
            continue;
        }
        // White RGB, the layer's opacity as the alpha; at 255 this is `NO_TINT` and blending is off, so it reads as a plain copy.
        let tint = ((layer.opacity as u32) << 24) | 0x00ff_ffff;
        out.push(Op::Layer {
            surface,
            from,
            to,
            tint,
            blend: layer.opacity < 255,
            filter: from.w != to.w || from.h != to.h,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::compose::{NO_TINT, View, compose_cpu};
    use lucina_wm_rules::controller::{Layer, Mode};
    use lucina_wm_rules::tile::Rect as TileRect;

    const SCREEN: (u32, u32) = (100, 100);

    /// A full-opacity layer of `space` at `to`.
    fn opaque(space: usize, x: i32, y: i32, w: i32, h: i32) -> Layer {
        Layer {
            space,
            to: TileRect { x, y, w, h },
            opacity: 255,
        }
    }

    /// A scene of `layers`, in order, spaces mode.
    fn scene_of(layers: &[Layer]) -> Scene {
        let mut s = Scene::new();
        for (i, l) in layers.iter().enumerate() {
            s.layers[i] = *l;
        }
        s.len = layers.len();
        s.mode = Mode::Spaces;
        s
    }

    /// Every space has a 100x100 surface whose id is its index.
    fn surface_at(space: usize) -> Option<SurfaceId> {
        Some(space)
    }
    fn size_100(_id: SurfaceId) -> Option<(u32, u32)> {
        Some((100, 100))
    }

    /// The layer ops (dropping the leading backdrop fill) for `scene` on a 100x100 screen of 100x100 surfaces.
    fn layers_of(scene: &Scene) -> Vec<Op> {
        let mut out = Vec::new();
        scene_ops(scene, &surface_at, &size_100, SCREEN, &mut out);
        out.into_iter().skip(1).collect()
    }

    /// The one layer that `scene` shows (its single space layer), for the per-layer cases.
    fn the_layer(scene: &Scene) -> Op {
        let mut l = layers_of(scene);
        assert_eq!(l.len(), 1, "expected exactly one layer op");
        l.remove(0)
    }

    #[test]
    fn empty_scene_is_only_the_backdrop_fill() {
        let mut out = Vec::new();
        scene_ops(&Scene::new(), &surface_at, &size_100, SCREEN, &mut out);
        assert_eq!(out.len(), 1, "the ground alone");
        assert!(
            matches!(
                out[0],
                Op::Fill {
                    to: Rect {
                        x: 0,
                        y: 0,
                        w: 100,
                        h: 100
                    },
                    ..
                }
            ),
            "the fill covers the screen"
        );
        assert_eq!(
            fill_colour(&out[0]),
            theme::GROUND.0,
            "the page's ground, from the theme"
        );
    }

    #[test]
    fn a_full_opacity_layer_is_a_plain_copy() {
        let scene = scene_of(&[opaque(2, 0, 0, 100, 100)]);
        match the_layer(&scene) {
            Op::Layer {
                surface,
                from,
                to,
                tint,
                blend,
                filter,
            } => {
                assert_eq!(surface, 2, "the surface is the layer's space");
                assert_eq!(from, Rect::new(0, 0, 100, 100), "the whole surface");
                assert_eq!(to, Rect::new(0, 0, 100, 100), "fills the screen");
                assert_eq!(tint, NO_TINT, "255 opacity is no tint");
                assert!(!blend, "opaque does not blend");
                assert!(!filter, "same size is not scaled");
            }
            other => panic!("expected a layer, got {other:?}"),
        }
    }

    #[test]
    fn half_opacity_sets_the_tint_alpha_and_blends() {
        let mut l = opaque(0, 0, 0, 100, 100);
        l.opacity = 128;
        match the_layer(&scene_of(&[l])) {
            Op::Layer {
                tint,
                blend,
                filter,
                ..
            } => {
                assert_eq!(tint, 0x80ff_ffff, "alpha is the opacity, RGB white");
                assert!(blend, "translucent blends");
                assert!(!filter, "a full-size translucent layer is not scaled");
            }
            other => panic!("expected a layer, got {other:?}"),
        }
    }

    #[test]
    fn a_scaled_layer_sets_the_filter() {
        // A 100x100 surface drawn into a 50x50 rectangle: the sizes differ, so it filters.
        let scene = scene_of(&[opaque(0, 0, 0, 50, 50)]);
        match the_layer(&scene) {
            Op::Layer {
                from, to, filter, ..
            } => {
                assert_eq!(from, Rect::new(0, 0, 100, 100));
                assert_eq!(to, Rect::new(0, 0, 50, 50));
                assert!(filter, "from and to differ in size");
            }
            other => panic!("expected a layer, got {other:?}"),
        }
    }

    #[test]
    fn a_layer_off_screen_is_skipped() {
        // Wholly to the right of a 100-wide screen, and wholly above it.
        let off_right = opaque(1, 100, 0, 100, 100);
        let off_top = opaque(2, 0, -100, 100, 100);
        assert!(
            layers_of(&scene_of(&[off_right])).is_empty(),
            "clipped to nothing"
        );
        assert!(
            layers_of(&scene_of(&[off_top])).is_empty(),
            "clipped to nothing"
        );
        // Empty rectangles are skipped too.
        assert!(layers_of(&scene_of(&[opaque(3, 10, 10, 0, 50)])).is_empty());
    }

    #[test]
    fn the_layer_order_is_preserved() {
        let scene = scene_of(&[
            opaque(0, 0, 0, 100, 100),
            opaque(3, 0, 0, 40, 40),
            opaque(1, 60, 60, 40, 40),
        ]);
        let ids: Vec<usize> = layers_of(&scene)
            .iter()
            .map(|op| match op {
                Op::Layer { surface, .. } => *surface,
                other => panic!("expected a layer, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, [0, 3, 1], "draw order is the scene's order");
    }

    #[test]
    fn a_layer_without_a_surface_is_skipped() {
        // Space 1 has no surface; spaces 0 and 2 do.
        let scene = scene_of(&[
            opaque(0, 0, 0, 40, 40),
            opaque(1, 40, 0, 40, 40),
            opaque(2, 60, 0, 40, 40),
        ]);
        let surface_but_1 =
            |space: usize| -> Option<SurfaceId> { if space == 1 { None } else { Some(space) } };
        let mut out = Vec::new();
        scene_ops(&scene, &surface_but_1, &size_100, SCREEN, &mut out);
        let ids: Vec<usize> = out
            .iter()
            .skip(1)
            .map(|op| match op {
                Op::Layer { surface, .. } => *surface,
                other => panic!("expected a layer, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, [0, 2], "the surface-less middle layer is gone");
        // The size lookup failing drops a layer too.
        let no_size = |_id: SurfaceId| -> Option<(u32, u32)> { None };
        let mut out = Vec::new();
        scene_ops(&scene, &surface_at, &no_size, SCREEN, &mut out);
        assert_eq!(out.len(), 1, "no surface size means no layer");
    }

    #[test]
    fn never_more_than_max_layers_plus_one_ops() {
        // A full scene: every layer used, each with a surface on-screen. That is the most ops the function can emit.
        let mut s = Scene::new();
        for i in 0..MAX_LAYERS {
            s.layers[i] = opaque(i, 0, 0, 100, 100);
        }
        s.len = MAX_LAYERS;
        let mut out = Vec::new();
        scene_ops(&s, &surface_at, &size_100, SCREEN, &mut out);
        assert_eq!(out.len(), MAX_LAYERS + 1, "every layer plus the ground");
        assert!(out.len() <= MAX_LAYERS + 1, "the bound holds");
    }

    #[test]
    fn scene_ops_clears_what_was_in_out() {
        let mut out = vec![
            Op::Fill {
                to: Rect::new(0, 0, 1, 1),
                colour: 0xdead,
            };
            5
        ];
        scene_ops(&Scene::new(), &surface_at, &size_100, SCREEN, &mut out);
        assert_eq!(
            out.len(),
            1,
            "the stale ops are gone, only the ground remains"
        );
    }

    /// The `colour` of an [`Op::Fill`]; panics on anything else.
    fn fill_colour(op: &Op) -> u32 {
        match *op {
            Op::Fill { colour, .. } => colour,
            other => panic!("expected a fill, got {other:?}"),
        }
    }

    /// A CPU golden: two small 1x2 surfaces, opaque, laid side by side as the two columns of a 2x2 screen, drawn by the
    /// reference compositor. Each column keeps its own surface's pixels, in order, over the ground.
    #[test]
    fn compose_cpu_draws_two_surfaces_side_by_side() {
        let (sw, sh) = (2usize, 2usize);
        // Surface 0: the left column (a 2x1, width 1); surface 1: the right column.
        let left = vec![0xffaa_0000u32, 0xffbb_0000];
        let right = vec![0xff00_cc00u32, 0xff00_dd00];
        let views = [
            Some(View { px: &left, w: 1 }),
            Some(View { px: &right, w: 1 }),
        ];
        let scene = scene_of(&[opaque(0, 0, 0, 1, 2), opaque(1, 1, 0, 1, 2)]);
        let col_size = |_id: SurfaceId| -> Option<(u32, u32)> { Some((1, 2)) };
        let mut ops = Vec::new();
        scene_ops(&scene, &surface_at, &col_size, (2, 2), &mut ops);
        assert_eq!(ops.len(), 3, "ground + two columns");
        let mut px = vec![0u32; sw * sh];
        compose_cpu(&mut px, sw, &views, &ops);
        // Row-major 2x2: top-left and bottom-left from `left`, top-right and bottom-right from `right`.
        assert_eq!(px[0], 0xffaa_0000, "left column, top");
        assert_eq!(px[1], 0xff00_cc00, "right column, top");
        assert_eq!(px[2], 0xffbb_0000, "left column, bottom");
        assert_eq!(px[3], 0xff00_dd00, "right column, bottom");
    }
}
