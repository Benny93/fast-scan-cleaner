#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Squarified treemap (Bruls, Huizing, van Wijk). `sizes` must be sorted descending.
pub fn layout(sizes: &[u64], bounds: Rect) -> Vec<Rect> {
    let mut out = vec![Rect { x: bounds.x, y: bounds.y, w: 0.0, h: 0.0 }; sizes.len()];
    let total: f64 = sizes.iter().map(|&s| s as f64).sum();
    if total <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return out;
    }
    let scale = bounds.w as f64 * bounds.h as f64 / total;
    let areas: Vec<f64> = sizes.iter().map(|&s| s as f64 * scale).collect();
    let (mut x, mut y, mut w, mut h) = (bounds.x as f64, bounds.y as f64, bounds.w as f64, bounds.h as f64);
    let mut start = 0;
    while start < areas.len() && areas[start] > 0.0 {
        let side = w.min(h);
        let mut end = start + 1;
        while end < areas.len() && areas[end] > 0.0 && worst(&areas[start..=end], side) <= worst(&areas[start..end], side) {
            end += 1;
        }
        let row: f64 = areas[start..end].iter().sum();
        if w >= h {
            // Row is a column on the left edge.
            let thick = row / h;
            let mut cy = y;
            for i in start..end {
                let ih = areas[i] / thick;
                out[i] = Rect { x: x as f32, y: cy as f32, w: thick as f32, h: ih as f32 };
                cy += ih;
            }
            x += thick;
            w -= thick;
        } else {
            // Row is a strip along the top edge.
            let thick = row / w;
            let mut cx = x;
            for i in start..end {
                let iw = areas[i] / thick;
                out[i] = Rect { x: cx as f32, y: y as f32, w: iw as f32, h: thick as f32 };
                cx += iw;
            }
            y += thick;
            h -= thick;
        }
        start = end;
    }
    out
}

/// `r` mirrored top-to-bottom inside `bounds` (biggest blocks end up at the bottom, like GrandPerspective).
pub fn mirror_y(r: Rect, bounds: Rect) -> Rect {
    Rect { y: bounds.y + bounds.h - (r.y - bounds.y) - r.h, ..r }
}

/// Worst aspect ratio of a row laid along a side of length `side`.
fn worst(row: &[f64], side: f64) -> f64 {
    let sum: f64 = row.iter().sum();
    let (max, min) = row.iter().fold((0f64, f64::MAX), |(a, b), &r| (a.max(r), b.min(r)));
    let (s2, w2) = (sum * sum, side * side);
    (w2 * max / s2).max(s2 / (w2 * min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_moves_the_first_block_from_top_to_bottom() {
        let b = Rect { x: 10.0, y: 20.0, w: 100.0, h: 50.0 };
        let top = Rect { x: 10.0, y: 20.0, w: 30.0, h: 10.0 };
        assert_eq!(mirror_y(top, b), Rect { x: 10.0, y: 60.0, w: 30.0, h: 10.0 });
        assert_eq!(mirror_y(mirror_y(top, b), b), top);
    }

    const B: Rect = Rect { x: 10.0, y: 20.0, w: 800.0, h: 500.0 };

    fn overlap(a: &Rect, b: &Rect) -> f32 {
        let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        w.max(0.0) * h.max(0.0)
    }

    #[test]
    fn areas_are_proportional_inside_bounds_without_overlap() {
        let sizes = [500, 300, 120, 80, 80, 40, 10, 5, 1];
        let rects = layout(&sizes, B);
        let total: u64 = sizes.iter().sum();
        let area = B.w * B.h;
        for (s, r) in sizes.iter().zip(&rects) {
            let want = area * *s as f32 / total as f32;
            assert!((r.w * r.h - want).abs() <= want * 0.01 + 0.5, "{s}: {r:?}");
            assert!(r.x >= B.x - 0.01 && r.y >= B.y - 0.01);
            assert!(r.x + r.w <= B.x + B.w + 0.01 && r.y + r.h <= B.y + B.h + 0.01);
        }
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                assert!(overlap(&rects[i], &rects[j]) < 0.5, "{i} overlaps {j}");
            }
        }
    }

    #[test]
    fn equal_sizes_are_close_to_square() {
        let rects = layout(&[1; 16], Rect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 });
        for r in rects {
            assert!(r.w.max(r.h) / r.w.min(r.h) <= 2.0, "{r:?}");
        }
    }

    #[test]
    fn single_item_fills_bounds() {
        assert_eq!(layout(&[42], B), vec![B]);
    }

    #[test]
    fn empty_and_zero_inputs() {
        assert!(layout(&[], B).is_empty());
        assert!(layout(&[0, 0], B).iter().all(|r| r.w * r.h == 0.0));
        assert!(layout(&[5], Rect { w: 0.0, ..B }).iter().all(|r| r.w * r.h == 0.0));
        let mixed = layout(&[5, 0], B);
        assert_eq!(mixed[0], B);
        assert_eq!(mixed[1].w * mixed[1].h, 0.0);
    }
}
