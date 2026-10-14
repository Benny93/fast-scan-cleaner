//! Folder tree next to the Disk Map (like TreeSize): children sorted by size with a bar for their
//! share of the parent, synced both ways with the map's zoom and selection. Only expanded folders
//! are turned into rows, and the list is virtual (uniform_list), so a full-disk scan stays fast.

use super::a11y::Pressable as _;
use super::{AppView, loc};
use crate::format;
use crate::scan::{Kind, Node};
use crate::state::Core;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Children listed per folder; the rest become one "+ N more" row that opens the folder on the map.
const CHILD_LIMIT: usize = 100;
const ROW_H: f32 = 24.;
const INDENT: f32 = 14.;
/// Rough width of a character of the row text (text_sm), to size rows to their names.
const CHAR_W: f32 = 7.4;
/// Everything in a row except indent and name: chevron, icon, size, bar, gaps, padding.
const ROW_FIXED: f32 = 14. + 16. + 64. + 36. + 4. * 4. + 16.;

#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    /// Relative to the scan root ("" is the root).
    pub rel: PathBuf,
    pub name: String,
    pub size: u64,
    /// Share of the parent folder, 0..=1.
    pub share: f32,
    pub depth: usize,
    pub dir: bool,
    pub has_children: bool,
    pub open: bool,
    /// The folder shown on the map, or the selected item.
    pub current: bool,
    /// "+ N more" row: (hidden count, their size); `rel` is the parent folder.
    pub more: Option<(usize, u64)>,
}

/// Rows for the expanded part of the tree, depth first, biggest children first.
pub fn flatten(tree: &Node, root_name: &str, open: &BTreeSet<PathBuf>, current: &[&Path]) -> Vec<TreeRow> {
    #[allow(clippy::too_many_arguments)]
    fn walk(node: &Node, rel: PathBuf, name: String, share: f32, depth: usize, open: &BTreeSet<PathBuf>, current: &[&Path], out: &mut Vec<TreeRow>) {
        let dir = node.kind == Kind::Dir;
        let is_open = dir && (depth == 0 || open.contains(&rel));
        out.push(TreeRow {
            current: current.contains(&rel.as_path()),
            rel: rel.clone(),
            name,
            size: node.size,
            share,
            depth,
            dir,
            has_children: !node.children.is_empty(),
            open: is_open,
            more: None,
        });
        if !is_open {
            return;
        }
        let parent = node.size.max(1) as f32;
        for c in node.children.iter().take(CHILD_LIMIT) {
            walk(c, rel.join(&*c.name), c.name.to_string(), c.size as f32 / parent, depth + 1, open, current, out);
        }
        if node.children.len() > CHILD_LIMIT {
            let rest = &node.children[CHILD_LIMIT..];
            let size: u64 = rest.iter().map(|c| c.size).sum();
            out.push(TreeRow { rel, name: String::new(), size, share: size as f32 / parent, depth: depth + 1, dir: false, has_children: false, open: false, current: false, more: Some((rest.len(), size)) });
        }
    }
    let mut out = Vec::new();
    walk(tree, PathBuf::new(), root_name.to_string(), 1.0, 0, open, current, &mut out);
    out
}

fn is_sep(c: char) -> bool {
    matches!(c, ' ' | '_' | '-' | '.')
}

/// Lowercase without separators, so "node_modules", "Node Modules" and "nodemodules" are alike.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !is_sep(*c)).flat_map(char::to_lowercase).collect()
}

/// Fuzzy match of `query` (already `normalize`d) against a file name, case- and
/// separator-insensitive: the query's letters must appear in order. Higher is better:
/// whole name > prefix > substring > scattered letters; shorter names win ties.
pub fn fuzzy_score(name: &str, query: &str) -> Option<i32> {
    // Cheap subsequence check first, without allocating: almost all names fail here.
    let mut q = query.chars().peekable();
    if name.is_ascii() && query.is_ascii() {
        for b in name.bytes() {
            if q.peek().is_some_and(|qc| b.to_ascii_lowercase() == *qc as u8) {
                q.next();
            }
        }
    } else {
        for c in name.chars() {
            if q.peek().is_some_and(|qc| c.to_lowercase().eq(std::iter::once(*qc))) {
                q.next();
            }
        }
    }
    if q.peek().is_some() {
        return None;
    }
    let n = normalize(name);
    let len_penalty = (n.chars().count() as i32 - query.chars().count() as i32).clamp(0, 200);
    if n == query {
        return Some(3000);
    }
    if n.starts_with(query) {
        return Some(2000 - len_penalty);
    }
    if n.contains(query) {
        return Some(1500 - len_penalty);
    }
    // Scattered: fewer gaps between the matched letters is better.
    let (mut gaps, mut last, mut qi) = (0i32, None, query.chars().peekable());
    for (i, c) in n.chars().enumerate() {
        if qi.peek() == Some(&c) {
            if let Some(l) = last
                && i > l + 1
            {
                gaps += 1;
            }
            last = Some(i);
            qi.next();
        }
    }
    Some(1000 - gaps * 20 - len_penalty)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub rel: PathBuf,
    pub name: String,
    pub size: u64,
    pub dir: bool,
    score: i32,
}

/// Search hits across the whole scan, best match first, then biggest. `stop` is polled so a
/// newer query can cancel this one; returns `None` when cancelled.
pub fn search(tree: &Node, query: &str, limit: usize, stop: &dyn Fn() -> bool) -> Option<Vec<Hit>> {
    struct Walk<'a> {
        query: String,
        limit: usize,
        stop: &'a dyn Fn() -> bool,
        seen: u32,
        hits: Vec<Hit>,
        path: PathBuf,
    }
    impl Walk<'_> {
        fn trim(&mut self) {
            self.hits.sort_by(|a, b| b.score.cmp(&a.score).then(b.size.cmp(&a.size)));
            self.hits.truncate(self.limit);
        }
        /// The path is built in place (push/pop) and cloned only for hits: allocating a path
        /// for each of millions of entries made a search take most of a second.
        fn visit(&mut self, node: &Node) -> bool {
            self.seen += 1;
            if self.seen.is_multiple_of(65_536) && (self.stop)() {
                return false;
            }
            self.path.push(&*node.name);
            if let Some(score) = fuzzy_score(&node.name, &self.query) {
                self.hits.push(Hit { name: node.name.to_string(), size: node.size, dir: node.kind == Kind::Dir, score, rel: self.path.clone() });
                // Keep memory bounded on vague queries ("a").
                if self.hits.len() > self.limit * 8 {
                    self.trim();
                }
            }
            let ok = node.children.iter().all(|c| self.visit(c));
            self.path.pop();
            ok
        }
    }
    let query = normalize(query);
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut w = Walk { query, limit, stop, seen: 0, hits: Vec::new(), path: PathBuf::new() };
    if !tree.children.iter().all(|c| w.visit(c)) {
        return None;
    }
    w.trim();
    Some(w.hits)
}

/// Results shown at most.
const HIT_LIMIT: usize = 300;

/// Folder tree and fuzzy search next to the Disk Map.
pub(crate) struct TreeState {
    /// Folder tree next to the map: shown?, expanded folders (relative), rows, scroll, last synced zoom.
    pub(crate) visible: bool,
    pub(crate) open: std::collections::BTreeSet<std::path::PathBuf>,
    pub(crate) rows: Rc<Vec<TreeRow>>,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) last_zoom: Option<std::path::PathBuf>,
    /// Row widths of the tree and of the search results (all rows equal, for horizontal scrolling).
    pub(crate) row_w: f32,
    /// Width of the folder tree pane, changed by dragging its right edge.
    pub(crate) width: f32,
    /// `Some` while dragging that edge: the pointer's offset from it when the drag started.
    pub(crate) drag: Option<f32>,
    pub(crate) hit_w: f32,
    pub(crate) hits_scroll: UniformListScrollHandle,
    /// Fuzzy search over the whole scan: the field, the results (`None` = no query, show the tree)
    /// and a counter that cancels an older search when the query changes.
    pub(crate) query: Entity<gpui_kit::component::input::InputState>,
    pub(crate) hits: Option<Rc<Vec<Hit>>>,
    pub(crate) search_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl AppView {
    /// Searches the scan off the UI thread; typing again cancels the running search.
    pub(super) fn start_tree_search(&mut self, query: String, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering::SeqCst;
        let generation = self.tree.search_gen.fetch_add(1, SeqCst) + 1;
        if query.trim().is_empty() {
            self.tree.hits = None;
            cx.notify();
            return;
        }
        let (app, gen_counter) = (self.app.clone(), self.tree.search_gen.clone());
        cx.spawn(async move |this, cx| {
            // Short pause so a burst of keystrokes starts one search, not one per key.
            cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
            if gen_counter.load(SeqCst) != generation {
                return;
            }
            let stale = gen_counter.clone();
            let hits = cx
                .background_executor()
                .spawn(async move {
                    // Take a reference and let go of the lock: the walk must not block the UI.
                    let tree = app.core.lock().unwrap().tree.clone();
                    tree.and_then(|t| search(&t, &query, HIT_LIMIT, &|| stale.load(SeqCst) != generation))
                })
                .await;
            if let Some(hits) = hits {
                let _ = this.update(cx, |v, cx| {
                    if v.tree.search_gen.load(SeqCst) == generation {
                        v.tree.hits = Some(Rc::new(hits));
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Recomputes the rows (cheap: only expanded folders) and, when the map zoomed elsewhere,
    /// opens the tree down to that folder and scrolls it into view.
    pub(super) fn sync_tree(&mut self, core: &Core) {
        let Some(tree) = core.tree.as_ref() else {
            self.tree.rows = Rc::default();
            return;
        };
        let zoom_changed = self.tree.last_zoom.as_ref() != Some(&core.zoom);
        if zoom_changed {
            for a in core.zoom.ancestors() {
                self.tree.open.insert(a.to_path_buf());
            }
            self.tree.last_zoom = Some(core.zoom.clone());
        }
        let root_name = core.root.as_deref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        let current: Vec<&Path> = std::iter::once(core.zoom.as_path()).chain(core.selected.as_deref()).collect();
        let rows = flatten(tree, &root_name, &self.tree.open, &current);
        if zoom_changed && let Some(ix) = rows.iter().position(|r| r.rel == core.zoom && r.more.is_none()) {
            self.tree.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        self.tree.rows = Rc::new(rows);
    }

    pub(super) fn tree_pane(&mut self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let border = t.border;
        if !self.tree.visible {
            return div()
                .h_full()
                .border_r_1()
                .border_color(border)
                .p_1()
                .child(Button::new("tree-open").accessibility_label(t!("tree_show").to_string()).ghost().small().icon(Lucide::PanelLeftOpen).tooltip(t!("tree_show").to_string()).on_click(cx.listener(|this, _, _, cx| {
                    this.tree.visible = true;
                    cx.notify();
                })))
                .into_any_element();
        }
        self.sync_tree(core);
        let count = self.tree.rows.len();
        // All rows share one width, wide enough for the deepest / longest name, so the list
        // scrolls sideways instead of cutting names to "Aug…".
        let label_w = |depth: usize, chars: usize| 4. + depth as f32 * INDENT + chars as f32 * CHAR_W + ROW_FIXED;
        let tree_w = self.tree.rows.iter().map(|r| label_w(r.depth, r.name.chars().count().max(8))).fold(0., f32::max).min(1600.);
        let hits_w = self.tree.hits.as_ref().map_or(0., |h| h.iter().map(|h| label_w(0, h.name.chars().count().max(h.rel.as_os_str().len() * 6 / 10))).fold(0., f32::max).min(1600.));
        self.tree.row_w = tree_w;
        self.tree.hit_w = hits_w;
        let list = match self.tree.hits.clone() {
            Some(hits) if hits.is_empty() => div().flex_1().px_3().py_2().text_sm().text_color(t.muted_foreground).child(t!("tree_no_hits").to_string()).into_any_element(),
            Some(hits) => div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list("tree-hits", hits.len(), cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range.filter_map(|ix| hits.get(ix).map(|h| this.hit_row(ix, h, cx))).collect::<Vec<_>>()
                    }))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(Some(0))
                    .track_scroll(&self.tree.hits_scroll)
                    .size_full()
                    .px_1(),
                )
                .vertical_scrollbar(&self.tree.hits_scroll)
                .horizontal_scrollbar(&self.tree.hits_scroll)
                .into_any_element(),
            None => div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list("tree", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let rows = this.tree.rows.clone();
                        range.filter_map(|ix| rows.get(ix).map(|r| this.tree_row(ix, r, cx))).collect::<Vec<_>>()
                    }))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(Some(0))
                    .track_scroll(&self.tree.scroll)
                    .size_full()
                    .px_1(),
                )
                .vertical_scrollbar(&self.tree.scroll)
                .horizontal_scrollbar(&self.tree.scroll)
                .into_any_element(),
        };
        div()
            .v_flex()
            .relative()
            .w(px(self.tree.width))
            .overflow_hidden()
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .h_flex()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t!("tree_title").to_string()))
                    .child(Button::new("tree-close").accessibility_label(t!("tree_hide").to_string()).ghost().small().icon(Lucide::PanelLeftClose).tooltip(t!("tree_hide").to_string()).on_click(cx.listener(|this, _, _, cx| {
                        this.tree.visible = false;
                        cx.notify();
                    }))),
            )
            .child(div().px_2().pb_2().child(Input::new(&self.tree.query).small().cleanable(true).prefix(Icon::new(Lucide::Search).size_4())))
            .child(list)
            // Drag handle on the right edge; the Disk Map tracks the drag (see disk_map).
            .child(
                div()
                    .id("tree-resize")
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(6.))
                    .cursor_col_resize()
                    .hover(|s| s.bg(t.primary.opacity(0.4)))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.tree.drag = Some(f32::from(e.position.x) - this.tree.width);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// A search result: name, where it is, size. Clicking opens it on the map.
    fn hit_row(&self, ix: usize, h: &Hit, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, primary) = (t.muted_foreground, t.primary);
        let root = self.app.core.lock().unwrap().root.clone().unwrap_or_default();
        let abs = root.join(&h.rel);
        let parent = h.rel.parent().map(|p| p.display().to_string()).filter(|p| !p.is_empty()).unwrap_or_else(|| "/".into());
        let marked = self.map.marked.contains(&abs);
        let mark = abs.clone();
        div()
            .id(("tree-hit", ix))
            .min_w_full()
            .w(px(self.tree.hit_w))
            .h(px(ROW_H * 1.6))
            .h_flex()
            .gap_2()
            .px_2()
            .rounded_sm()
            .cursor_pointer()
            .a11y_press(&self.focus_for(format!("tree-hit-{ix}"), cx), h.name.clone(), primary)
            .when(marked, |d| d.bg(primary.opacity(0.32)))
            .hover(|s| s.bg(primary.opacity(0.08)))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if e.modifiers().secondary() {
                    this.toggle_mark(&abs, window, cx);
                    return;
                }
                let _ = this.app.activate(&abs);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.toggle_mark(&mark, window, cx);
                }),
            )
            .child(div().w(px(64.)).flex_shrink_0().text_right().text_xs().text_color(muted).child(format::size(h.size, &loc())))
            .child(Icon::new(if marked { Lucide::SquareCheck } else if h.dir { Lucide::Folder } else { Lucide::File }).size_4().flex_shrink_0().text_color(if marked { primary } else { muted }))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().whitespace_nowrap().child(h.name.clone()))
                    .child(div().text_xs().text_color(muted).whitespace_nowrap().child(parent)),
            )
            .into_any_element()
    }

    /// Marks / unmarks an item for "Move selected to Trash"; the map uses the same set, so a mark
    /// made here shows there and the other way round.
    pub(super) fn toggle_mark(&mut self, abs: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if !self.map.marked.remove(abs) {
            self.map.marked.insert(abs.to_path_buf());
        }
        // Delete / Backspace / Esc act on the marks while the Disk Map has focus.
        window.focus(&self.map.focus, cx);
        cx.notify();
    }

    fn tree_row(&self, ix: usize, r: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, primary) = (t.muted_foreground, t.primary);
        let l = loc();
        let root = self.app.core.lock().unwrap().root.clone().unwrap_or_default();
        let abs = root.join(&r.rel);
        let (label, icon) = match r.more {
            Some((n, _)) => (t!("tree_more", count = n).to_string(), Lucide::Ellipsis),
            None => (r.name.clone(), if r.dir { Lucide::Folder } else { Lucide::File }),
        };
        let toggle = (r.dir && r.has_children && r.depth > 0).then(|| r.rel.clone());
        let more = r.more.is_some();
        // The scan root itself can't be trashed; "+ N more" is not an item.
        let markable = r.depth > 0 && !more;
        let marked = markable && self.map.marked.contains(&abs);
        let mark = abs.clone();
        div()
            .id(("tree-row", ix))
            .min_w_full()
            .w(px(self.tree.row_w))
            .h(px(ROW_H))
            .h_flex()
            .gap_1()
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .a11y_press(&self.focus_for(format!("tree-row-{ix}"), cx), label.clone(), primary)
            .text_sm()
            .when(r.current, |d| d.bg(primary.opacity(0.18)))
            .when(marked, |d| d.bg(primary.opacity(0.32)))
            .hover(|s| s.bg(primary.opacity(0.08)))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                // Cmd-click (Ctrl on Windows/Linux) marks several, like right-click.
                if markable && e.modifiers().secondary() {
                    this.toggle_mark(&abs, window, cx);
                    return;
                }
                // Directories open on the map (and in the tree); files are selected.
                let _ = if more { this.app.navigate(&abs) } else { this.app.activate(&abs) };
                cx.notify();
            }))
            .when(markable, |d| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_mark(&mark, window, cx);
                    }),
                )
            })
            // Size and share first, like TreeSize: they stay in view while long names scroll sideways.
            .child(div().w(px(64.)).flex_shrink_0().text_right().text_xs().text_color(muted).child(format::size(r.size, &l)))
            .child(div().w(px(36.)).h(px(6.)).flex_shrink_0().rounded_sm().bg(muted.opacity(0.2)).child(div().h_full().rounded_sm().w(px(36. * r.share.clamp(0.0, 1.0))).bg(primary.opacity(0.8))))
            .child(div().w(px(4. + r.depth as f32 * INDENT)).flex_shrink_0())
            .child(
                div().w(px(14.)).flex_shrink_0().children(toggle.map(|rel| {
                    div()
                        .id(("tree-toggle", ix))
                        .child(Icon::new(if r.open { Lucide::ChevronDown } else { Lucide::ChevronRight }).size_3().text_color(muted))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !this.tree.open.remove(&rel) {
                                this.tree.open.insert(rel.clone());
                            }
                            cx.notify();
                        }))
                })),
            )
            .child(Icon::new(if marked { Lucide::SquareCheck } else { icon }).size_4().flex_shrink_0().text_color(if marked { primary } else { muted }))
            .child(div().flex_1().whitespace_nowrap().when(more, |d| d.text_color(muted)).child(label))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(name: &str, size: u64) -> Node {
        Node { name: name.into(), size, kind: Kind::File, children: Box::default() }
    }
    fn dir(name: &str, children: Vec<Node>) -> Node {
        Node { name: name.into(), size: children.iter().map(|c| c.size).sum(), kind: Kind::Dir, children: children.into() }
    }

    #[::core::prelude::v1::test]
    fn only_open_folders_are_expanded_and_shares_add_up() {
        let tree = dir("/r", vec![dir("a", vec![leaf("x", 30), leaf("y", 10)]), leaf("b", 60)]);
        let rows = flatten(&tree, "r", &BTreeSet::new(), &[Path::new("")]);
        assert_eq!(rows.iter().map(|r| (r.name.as_str(), r.depth)).collect::<Vec<_>>(), [("r", 0), ("a", 1), ("b", 1)]);
        assert!(rows[0].current && rows[0].open && !rows[1].open);
        assert_eq!((rows[1].share, rows[2].share), (0.4, 0.6));
        let rows = flatten(&tree, "r", &BTreeSet::from([PathBuf::from("a")]), &[]);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["r", "a", "x", "y", "b"]);
        assert_eq!(rows[2].rel, PathBuf::from("a/x"));
    }

    #[::core::prelude::v1::test]
    fn fuzzy_ranks_exact_then_prefix_then_substring_then_scattered() {
        assert_eq!(fuzzy_score("node_modules", "xyz"), None);
        let s = |n: &str| fuzzy_score(n, "app").unwrap();
        assert!(s("app") > s("app.js"));
        assert!(s("app.js") > s("my-app"));
        assert!(s("my-app") > s("apxp")); // substring beats scattered letters
        assert!(fuzzy_score("Größe.txt", "größe").is_some());
        // Separators and case don't matter, and the shorter name wins.
        let q = normalize("nodemod");
        assert!(fuzzy_score("node_modules", &q).unwrap() > fuzzy_score("nodeModulesPaths.js", &q).unwrap());
        assert_eq!(fuzzy_score("Node Modules", &normalize("node_modules")), Some(3000));
    }

    #[::core::prelude::v1::test]
    fn search_finds_nested_items_best_first_and_can_be_cancelled() {
        let tree = dir("/r", vec![dir("web", vec![dir("node_modules", vec![leaf("x", 5)]), leaf("app.js", 3)]), dir("mobile", vec![dir("node_modules", vec![leaf("y", 9)])])]);
        let hits = search(&tree, "Node_Modules", 10, &|| false).unwrap();
        // Compare as paths: the display separator is `\` on Windows and `/` elsewhere.
        assert_eq!(hits.iter().map(|h| h.rel.clone()).collect::<Vec<_>>(), [PathBuf::from("mobile/node_modules"), PathBuf::from("web/node_modules")]); // equal score: bigger first
        assert_eq!(search(&tree, "apjs", 10, &|| false).unwrap()[0].rel, PathBuf::from("web/app.js"));
        let big = dir("/r", (0..70_000).map(|i| leaf(&format!("f{i}"), 1)).collect());
        assert_eq!(search(&big, "f", 10, &|| true), None);
    }

    #[::core::prelude::v1::test]
    fn huge_folders_end_in_a_more_row() {
        let tree = dir("/r", (0..CHILD_LIMIT + 5).map(|i| leaf(&format!("f{i}"), 1)).collect());
        let rows = flatten(&tree, "r", &BTreeSet::new(), &[]);
        assert_eq!(rows.len(), 1 + CHILD_LIMIT + 1);
        assert_eq!(rows.last().unwrap().more, Some((5, 5)));
    }
}
