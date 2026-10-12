use super::{AppView, loc};
use crate::actions::Origin;
use crate::rules::Safety;
use crate::settings::{MapColors, MapOrder};
use crate::scan::{self, Kind, Node};
use crate::state::{Banner, Core, Screen};
use crate::treemap::{self, Rect};
use crate::{format, platform};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::{black, transparent_black, white};
use rust_i18n::t;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use gpui_kit::component::scroll::ScrollableElement as _;

const LABEL_H: f32 = 16.0;
const MIN_BLOCK: f32 = 3.0;
/// Block ids are `parent_index * NEST + child_index + 1`.
const NEST: usize = 1000;

/// 0 for a top-level block, 1 for one nested inside a folder block (see NEST).
fn block_depth(id: usize) -> u8 {
    u8::from(id % NEST != 0)
}

fn hue_of(s: &str) -> f32 {
    (s.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360) as f32 / 360.0
}

// File type hues, shared by the map and its legend.
const HUE_VIDEO: f32 = 0.78;
const HUE_PHOTO: f32 = 0.48;
const HUE_MUSIC: f32 = 0.92;
const HUE_ARCHIVE: f32 = 0.08;
const HUE_DOC: f32 = 0.60;

/// `advice` is the block's own rule or the nearest folder above it that has one;
/// `removable` is the share (0..=1) of the block that is safe to remove.
/// `parent_hue` tints files of no known type, so "other files" differ a little per folder.
fn color_for(node: &Node, parent_hue: f32, depth: u8, advice: Option<Safety>, removable: f32, mode: MapColors) -> Hsla {
    let l = if depth == 0 { 0.62 } else { 0.72 };
    match mode {
        MapColors::Types => match node.kind {
            Kind::Dir => hsla(hue_of(&node.name), 0.40, l, 1.0),
            Kind::Symlink => hsla(0.0, 0.0, 0.75, 1.0),
            Kind::File => {
                let ext = Path::new(&*node.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                let hue = match ext.as_str() {
                    "mp4" | "mov" | "mkv" | "avi" | "m4v" | "webm" => HUE_VIDEO,
                    "jpg" | "jpeg" | "png" | "heic" | "gif" | "raw" | "cr2" | "nef" | "tiff" | "webp" => HUE_PHOTO,
                    "mp3" | "m4a" | "wav" | "flac" | "aac" => HUE_MUSIC,
                    "zip" | "dmg" | "iso" | "tar" | "gz" | "7z" | "rar" | "pkg" | "exe" | "msi" | "deb" | "rpm" => HUE_ARCHIVE,
                    "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" => HUE_DOC,
                    _ => return other_color(parent_hue, l),
                };
                hsla(hue, 0.60, l, 1.0)
            }
        },
        MapColors::Advice => match advice {
            Some(s) => advice_color(s, depth),
            None => removable_color(removable, depth),
        },
    }
}

/// Light bluish grey, tinted slightly towards the folder's own colour.
fn other_color(parent_hue: f32, l: f32) -> Hsla {
    const BLUE: f32 = 0.6;
    hsla(BLUE + (parent_hue - BLUE) * 0.3, 0.14, l, 1.0)
}

/// GrandPerspective-style cushion: lighter top left, darker bottom right.
fn cushion(c: Hsla) -> Background {
    let shade = |dl: f32| Hsla { l: (c.l + dl).clamp(0.0, 1.0), ..c };
    linear_gradient(135., linear_color_stop(shade(0.07), 0.), linear_color_stop(shade(-0.09), 1.))
}

/// Fits `name` into `width` pt of small label text, cutting in the middle so the
/// end (often a date or the file type) stays visible: "Screen Rec…15.47.mov".
fn fit_label(name: &str, width: f32) -> Option<String> {
    // ponytail: average glyph width of the xs label font; measure text if labels ever get clipped
    let max = ((width - 8.0) / 6.2).floor() as usize;
    let chars: Vec<char> = name.chars().collect();
    if max < 6 {
        return None; // "Sc…ov" helps nobody
    }
    if chars.len() <= max {
        return Some(name.to_string());
    }
    let tail = (max / 2).min(12);
    let head = max - tail - 1;
    Some(format!("{}…{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>()))
}

/// "name · size" on one line: only the name is shortened; without room for both, just the name.
fn name_and_size(name: &str, size: &str, width: f32) -> Option<String> {
    let size_w = (size.chars().count() + 3) as f32 * 6.2;
    match fit_label(name, width - size_w) {
        Some(n) => Some(format!("{n} · {size}")),
        None => fit_label(name, width),
    }
}

/// White (nothing to remove) to full green (all of it).
fn removable_color(share: f32, depth: u8) -> Hsla {
    let share = share.clamp(0.0, 1.0);
    let white = if depth == 0 { 0.90 } else { 0.96 };
    hsla(0.37, 0.65 * share, white - (white - 0.50) * share, 1.0)
}

/// Safe-to-remove bytes per folder: every safe finding counted in all its ancestors.
// ponytail: rebuilt every frame in traffic light mode (findings × depth hash inserts); cache per scan if it shows in profiles
fn removable_bytes(core: &Core) -> HashMap<PathBuf, u64> {
    let mut out = HashMap::new();
    let Some(root) = core.root.as_deref() else { return out };
    for f in core.findings.iter().filter(|f| f.safety == Safety::Safe) {
        for p in f.path.ancestors().take_while(|p| p.starts_with(root)) {
            *out.entry(p.to_path_buf()).or_default() += f.size;
        }
    }
    out
}

/// Deep paths keep the first crumb, "…" (opens the folder just above the visible tail) and the last three,
/// so the breadcrumb never pushes the toolbar buttons aside.
fn shorten_crumbs(crumbs: Vec<(String, PathBuf)>) -> Vec<(String, PathBuf)> {
    const TAIL: usize = 3;
    if crumbs.len() <= TAIL + 2 {
        return crumbs;
    }
    let tail = crumbs.len() - TAIL;
    let mut out = vec![crumbs[0].clone(), ("…".to_string(), crumbs[tail - 1].1.clone())];
    out.extend_from_slice(&crumbs[tail..]);
    out
}

/// Lets text wrap after path separators and underscores (zero-width spaces) instead of
/// between arbitrary letters.
fn breakable(s: &str) -> String {
    s.replace('/', "/\u{200B}").replace('\\', "\\\u{200B}").replace('_', "_\u{200B}")
}

/// Marked paths without the ones inside another marked folder (trashing the folder covers them).
fn top_level(marked: &std::collections::BTreeSet<PathBuf>) -> Vec<PathBuf> {
    marked.iter().filter(|p| !p.ancestors().skip(1).any(|a| marked.contains(a))).cloned().collect()
}

fn advice_color(s: Safety, depth: u8) -> Hsla {
    let l = if depth == 0 { 0.50 } else { 0.60 };
    match s {
        Safety::Safe => hsla(0.37, 0.60, l, 1.0),
        Safety::Review => hsla(0.13, 0.85, l, 1.0),
        Safety::Keep => hsla(0.0, 0.65, l, 1.0),
    }
}

fn safety_color(s: Safety, cx: &App) -> Hsla {
    let t = cx.theme();
    match s {
        Safety::Safe => t.success,
        Safety::Review => t.warning,
        Safety::Keep => t.danger,
    }
}

/// Snaps a tile to whole device pixels and leaves a one-pixel gap on its right and bottom
/// (when it is at least 3 device pixels wide/high), so the grid of tiny tiles looks even
/// instead of blurred and irregular. Slivers keep at least one pixel so no gap opens up.
fn snap(x: f32, y: f32, w: f32, h: f32, scale: f32) -> (f32, f32, f32, f32) {
    let edge = |v: f32| (v * scale).round();
    let (x0, y0, x1, y1) = (edge(x), edge(y), edge(x + w), edge(y + h));
    let len = |span: f32| if span >= 3.0 { span - 1.0 } else { span.max(1.0) };
    (x0 / scale, y0 / scale, len(x1 - x0) / scale, len(y1 - y0) / scale)
}

/// Items smaller than this (pt²) are gathered into one "+ N small items" block.
const SMALL_AREA: f32 = 24. * 24.;
/// Inside an opened group the tiles may be smaller, so many similar small files fit on one map
/// instead of being grouped again and again.
const SMALL_AREA_OPENED: f32 = 12. * 12.;

/// What a folder shows in a treemap of `area` pt²: its children from `skip` on, with the
/// tail of small ones (if there are at least two) gathered into one group.
#[derive(Debug)]
enum Item<'a> {
    Node(&'a Node),
    /// Children from `skip` on of the folder, `count` of them, `size` bytes together.
    Group { skip: usize, count: usize, size: u64 },
}

impl Item<'_> {
    fn size(&self) -> u64 {
        match self {
            Item::Node(n) => n.size,
            Item::Group { size, .. } => *size,
        }
    }
}

fn items(children: &[Node], skip: usize, area: f32, small_area: f32) -> Vec<Item<'_>> {
    let shown = children.get(skip..).unwrap_or_default();
    let total = shown.iter().map(|c| c.size).sum::<u64>().max(1) as f32;
    let small = shown.iter().position(|c| c.size as f32 / total * area < small_area).unwrap_or(shown.len());
    // Only small items (a folder of photos): show them as tiles; one grey block would hide what the folder is.
    if small == 0 {
        return shown.iter().map(Item::Node).collect();
    }
    let mut out: Vec<Item> = shown[..small].iter().map(Item::Node).collect();
    match shown.len() - small {
        0 => {}
        1 => out.push(Item::Node(&shown[small])),
        count => {
            let size = shown[small..].iter().map(|c| c.size).sum();
            // Keep the list sorted by size, as the layout expects.
            let at = out.iter().position(|i| i.size() < size).unwrap_or(out.len());
            out.insert(at, Item::Group { skip: skip + small, count, size });
        }
    }
    out
}

/// Treemap rects for `items` inside `bounds`, in the user's chosen order.
fn layout(items: &[Item], bounds: Rect, order: MapOrder) -> Vec<Rect> {
    let sizes: Vec<u64> = items.iter().map(Item::size).collect();
    let rects = treemap::layout(&sizes, bounds);
    let rects: Vec<Rect> = match order {
        MapOrder::TopLeft => rects,
        MapOrder::BottomLeft => rects.into_iter().map(|r| treemap::mirror_y(r, bounds)).collect(),
    };
    rects.into_iter().map(snap_edges).collect()
}

/// Rounds a block's edges (not its size) to whole points, so neighbours share the exact same
/// edge and every gutter between them is equally wide instead of 1 or 2 pixels at random.
fn snap_edges(r: Rect) -> Rect {
    let (x0, y0, x1, y1) = (r.x.round(), r.y.round(), (r.x + r.w).round(), (r.y + r.h).round());
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

/// Blocks too small (or too many) to be UI elements, painted as plain rectangles so the map has no gaps.
type Fine = RefCell<Vec<(Rect, Hsla)>>;

/// What every block of one map needs.
struct MapCtx<'a> {
    core: &'a Core,
    root: &'a Path,
    removable: &'a HashMap<PathBuf, u64>,
    mode: MapColors,
    order: MapOrder,
    fine: Fine,
    interactive: bool,
}

fn safety_label(s: Safety) -> String {
    match s {
        Safety::Safe => t!("safety_safe"),
        Safety::Review => t!("safety_review"),
        Safety::Keep => t!("safety_keep"),
    }
    .to_string()
}

pub(super) fn safety_tag(s: Safety) -> Tag {
    match s {
        Safety::Safe => Tag::success().child(t!("safety_safe").to_string()),
        Safety::Review => Tag::warning().child(t!("safety_review").to_string()),
        Safety::Keep => Tag::danger().child(t!("safety_keep").to_string()),
    }
}

pub struct FileRow {
    pub path: PathBuf,
    pub name: String,
    pub folder: String,
    pub size: u64,
    pub advice: Option<Safety>,
}

pub struct FileRows {
    pub rows: Vec<FileRow>,
}

// ponytail: headers are translated when the table is built; a language switch shows new headers after restart
impl TableDelegate for FileRows {
    fn columns_count(&self, _: &App) -> usize {
        4
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        match ix {
            0 => Column::new("size", t!("col_size").to_string()).width(px(100.)).text_right(),
            1 => Column::new("name", t!("col_name").to_string()).width(px(260.)),
            2 => Column::new("folder", t!("col_folder").to_string()).width(px(360.)),
            _ => Column::new("advice", t!("col_advice").to_string()).width(px(170.)),
        }
    }

    fn render_td(&mut self, row: usize, col: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let r = &self.rows[row];
        match col {
            0 => div().child(format::size(r.size, &loc())).into_any_element(),
            1 => div().truncate().child(r.name.clone()).into_any_element(),
            2 => div().truncate().child(r.folder.clone()).into_any_element(),
            _ => r.advice.map(|s| safety_tag(s).small().into_any_element()).unwrap_or_else(|| div().into_any_element()),
        }
    }
}

impl AppView {
    pub(super) fn disk_map(&mut self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        if let Some(root) = &core.root {
            self.history.visit(&root.join(&core.zoom));
        }
        // Marked blocks that were trashed or rescanned away are dropped.
        self.map_marked.retain(|p| core.rel(p).and_then(|r| core.tree.as_ref()?.find(&r)).is_some());
        // "+ N small items" belongs to the folder it was opened in (absolute, so a new scan resets it).
        if self.map_skip.as_ref().is_some_and(|(p, _)| Some(p.as_path()) != core.root.as_ref().map(|r| r.join(&core.zoom)).as_deref()) {
            self.map_skip = None;
        }
        let main = if self.show_list { self.file_list(core, cx) } else { self.map_view(core, self.map_bounds.clone(), true, cx) };
        let border = cx.theme().border;
        let tree = self.tree_pane(core, cx);
        div()
            .h_flex()
            .size_full()
            .track_focus(&self.map_focus)
            .key_context("DiskMap")
            // Resizing the folder tree: follow the pointer while its edge is dragged.
            .when(self.tree_drag.is_some(), |d| {
                d.cursor_col_resize()
                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                        if let Some(grab) = this.tree_drag {
                            this.tree_width = (f32::from(e.position.x) - grab).clamp(200., 700.);
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.tree_drag = None;
                        cx.notify();
                    }))
            })
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Back), cx.listener(|this, _, _, cx| this.go_history(false, cx)))
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Forward), cx.listener(|this, _, _, cx| this.go_history(true, cx)))
            .child(tree)
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.toolbar(core, cx))
                    .children(self.guidance(core, cx))
                    .child(div().flex_1().min_h_0().p_2().child(main))
                    .when(!self.show_list, |d| d.child(self.marked_bar(core, cx)).child(self.legend(core.settings.map_colors, core.settings.map_order, cx))),
            )
            .child(if self.details_open {
                self.details(core, cx)
            } else {
                div()
                    .h_full()
                    .border_l_1()
                    .border_color(border)
                    .p_1()
                    .child(Button::new("details-open").ghost().small().icon(Lucide::PanelRightOpen).on_click(cx.listener(|this, _, _, cx| {
                        this.details_open = true;
                        cx.notify();
                    })))
                    .into_any_element()
            })
            .into_any_element()
    }

    fn toolbar(&self, core: &Core, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h_flex()
            .gap_2()
            .px_3()
            .pt_3()
            .child(div().flex_1().min_w_0().overflow_hidden().child(self.breadcrumb(core, cx)))
            .child(
                ButtonGroup::new("view")
                    .outline()
                    .small()
                    .child(Button::new("view-map").icon(Lucide::LayoutGrid).label(t!("view_treemap").to_string()).selected(!self.show_list))
                    .child(Button::new("view-list").icon(Lucide::List).label(t!("view_list").to_string()).selected(self.show_list))
                    .on_click(cx.listener(|this, ix: &Vec<usize>, _, cx| {
                        if let Some(i) = ix.first() {
                            this.show_list = *i == 1;
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("refresh")
                    .small()
                    .outline()
                    .icon(Lucide::RefreshCw)
                    .label(t!("nav_refresh").to_string())
                    .loading(core.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let _ = this.app.refresh(None);
                        cx.notify();
                    })),
            )
    }

    /// "N selected (size) · Clear · Move to Trash" for right-clicked blocks, or how to select them.
    /// Always the same height, so the map never re-lays out under the mouse while selecting.
    fn marked_bar(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let row = || div().h_flex().h(px(44.)).gap_3().mx_2().mb_2().px_3().rounded_lg().border_1();
        if self.map_marked.is_empty() {
            return row()
                .border_color(t.border)
                .text_sm()
                .text_color(t.muted_foreground)
                .child(Icon::new(Lucide::MousePointerClick))
                .child(div().flex_1().min_w_0().truncate().child(t!("map_select_hint").to_string()))
                .into_any_element();
        }
        let paths = top_level(&self.map_marked);
        let size: u64 = paths.iter().filter_map(|p| core.tree.as_ref()?.find(&core.rel(p)?)).map(|n| n.size).sum();
        row()
            .border_color(t.primary)
            .bg(t.primary.opacity(0.12))
            .child(Icon::new(Lucide::SquareCheck).text_color(t.primary))
            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(t!("cleanup_selected", count = paths.len().to_string(), size = format::size(size, &loc())).to_string()))
            .child(Button::new("clear-marked").outline().small().label(t!("cleanup_clear_selection").to_string()).on_click(cx.listener(|this, _, _, cx| {
                this.map_marked.clear();
                cx.notify();
            })))
            .child(Button::new("trash-marked").danger().small().icon(Lucide::Trash).label(t!("cleanup_trash_selected").to_string()).on_click(cx.listener(|this, _, _, cx| this.trash_marked(cx))))
            .into_any_element()
    }

    /// Asks to move the marked blocks to the Trash (the usual confirm dialog); they leave the map once done.
    pub(super) fn trash_marked(&mut self, cx: &mut Context<Self>) {
        if !self.map_marked.is_empty() {
            let _ = self.app.request_trash(top_level(&self.map_marked), None, Origin::User);
            cx.notify();
        }
    }

    /// Colour scheme switch and what the colours mean.
    fn legend(&self, mode: MapColors, order: MapOrder, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let swatch = |color: Hsla, label: String| {
            div().h_flex().gap_1().min_w_0().child(div().size(px(12.)).flex_shrink_0().bg(color)).child(div().min_w_0().truncate().child(label))
        };
        let dot = |color: Hsla, label: String| {
            div().h_flex().gap_1().min_w_0().child(div().size(px(8.)).flex_shrink_0().rounded_full().border_1().border_color(white()).bg(color)).child(div().min_w_0().truncate().child(label))
        };
        let file = |hue: f32, key: &str| swatch(hsla(hue, 0.60, 0.62, 1.0), t!(key).to_string());
        let items: Vec<AnyElement> = match mode {
            MapColors::Types => vec![
                div()
                    .h_flex()
                    .gap_1()
                    .min_w_0()
                    .children([0.52, 0.62, 0.72].map(|h| div().size(px(12.)).flex_shrink_0().bg(hsla(h, 0.40, 0.62, 1.0))))
                    .child(div().min_w_0().truncate().child(t!("legend_folders").to_string()))
                    .into_any_element(),
                file(HUE_VIDEO, "legend_videos").into_any_element(),
                file(HUE_PHOTO, "legend_photos").into_any_element(),
                file(HUE_MUSIC, "legend_music").into_any_element(),
                file(HUE_ARCHIVE, "legend_archives").into_any_element(),
                file(HUE_DOC, "legend_documents").into_any_element(),
                swatch(other_color(0.6, 0.62), t!("legend_other").to_string()).into_any_element(),
                dot(safety_color(Safety::Safe, cx), t!("safety_safe").to_string()).into_any_element(),
                dot(safety_color(Safety::Review, cx), t!("safety_review").to_string()).into_any_element(),
                dot(safety_color(Safety::Keep, cx), t!("safety_keep").to_string()).into_any_element(),
            ],
            MapColors::Advice => vec![
                swatch(advice_color(Safety::Safe, 0), t!("safety_safe").to_string()).into_any_element(),
                swatch(removable_color(0.4, 0), t!("legend_partly").to_string()).into_any_element(),
                swatch(advice_color(Safety::Review, 0), t!("safety_review").to_string()).into_any_element(),
                swatch(advice_color(Safety::Keep, 0), t!("safety_keep").to_string()).into_any_element(),
                swatch(removable_color(0.0, 0), t!("legend_no_advice").to_string()).into_any_element(),
            ],
        };
        let modes = [MapColors::Types, MapColors::Advice];
        let orders = [MapOrder::TopLeft, MapOrder::BottomLeft];
        // Switches on one row, the colour key below: on a narrow map nothing runs out of the column.
        div()
            .v_flex()
            .gap_2()
            .px_3()
            .pb_2()
            .min_w_0()
            .text_xs()
            .child(
                div().h_flex().flex_wrap().gap_2().child(
                ButtonGroup::new("map-colors")
                    .outline()
                    .xsmall()
                    .child(Button::new("colors-types").label(t!("map_colors_types").to_string()).selected(mode == MapColors::Types))
                    .child(Button::new("colors-advice").label(t!("map_colors_advice").to_string()).selected(mode == MapColors::Advice))
                    .on_click(cx.listener(move |this, ix: &Vec<usize>, _, cx| {
                        if let Some(&m) = ix.first().and_then(|i| modes.get(*i)) {
                            this.app.update_settings(|s| s.map_colors = m);
                            cx.notify();
                        }
                    })),
            )
                .child(
                ButtonGroup::new("map-order")
                    .outline()
                    .xsmall()
                    .flex_shrink_0()
                    .child(Button::new("order-top").label(t!("map_order_top").to_string()).selected(order == MapOrder::TopLeft))
                    .child(Button::new("order-bottom").label(t!("map_order_bottom").to_string()).selected(order == MapOrder::BottomLeft))
                    .on_click(cx.listener(move |this, ix: &Vec<usize>, _, cx| {
                        if let Some(&o) = ix.first().and_then(|i| orders.get(*i)) {
                            this.app.update_settings(|s| s.map_order = o);
                            cx.notify();
                        }
                    })),
            ))
            .child(div().w_full().min_w_0().h_flex().flex_wrap().gap_x_3().gap_y_1().text_color(muted).children(items))
            .into_any_element()
    }

    fn breadcrumb(&self, core: &Core, cx: &mut Context<Self>) -> impl IntoElement {
        let root = core.root.clone().unwrap_or_default();
        // The scan root by its folder name; the full path is in the details panel.
        let root_label = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        let mut crumbs = vec![(root_label, root.clone())];
        let mut acc = root;
        for c in core.zoom.components() {
            acc = acc.join(c);
            crumbs.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
        }
        // Looking at a folder's small items: one more step, after the folder itself.
        let small = self.map_skip.as_ref().and_then(|(_, skip)| {
            let n = core.tree.as_ref()?.find(&core.zoom)?.children.len().saturating_sub(*skip);
            Some(t!("map_small_items", count = format::count(n as u64, &loc())).to_string())
        });
        if let Some(label) = small {
            crumbs.push((label, acc));
        }
        Breadcrumb::new().children(shorten_crumbs(crumbs).into_iter().map(|(label, path)| {
            BreadcrumbItem::new(label).on_click(cx.listener(move |this, _, _, cx| {
                this.map_skip = None;
                let _ = this.app.navigate(&path);
                cx.notify();
            }))
        }))
    }

    fn guidance(&self, core: &Core, cx: &mut Context<Self>) -> Option<AnyElement> {
        match core.banner() {
            // Clicking the "you could free X" notice opens Cleanup.
            Banner::Found(b) => Some(
                div()
                    .id("guidance")
                    .px_3()
                    .pt_2()
                    .cursor_pointer()
                    .child(Alert::success("guidance", format!("{} →", t!("banner_found", size = format::size(b, &loc())))).icon(Lucide::Sparkles))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.app.show(Screen::Cleanup);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            Banner::Nothing => Some(div().px_3().pt_2().child(Alert::info("guidance", t!("banner_nothing").to_string())).into_any_element()),
            _ => None,
        }
    }

    /// Treemap of the zoomed folder (interactive) or of the scan root (Home preview).
    pub(super) fn map_view(&self, core: &Core, cell: Rc<Cell<Bounds<Pixels>>>, interactive: bool, cx: &mut Context<Self>) -> AnyElement {
        let (Some(tree), Some(root)) = (core.tree.as_ref(), core.root.as_ref()) else {
            return div().into_any_element();
        };
        self.tree_map(core, tree, root, cell, core.settings.map_colors, interactive, cx)
    }

    /// Treemap of `tree` (whose path is `root`); also used for the live map while scanning.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn tree_map(&self, core: &Core, tree: &Node, root: &Path, cell: Rc<Cell<Bounds<Pixels>>>, mode: MapColors, interactive: bool, cx: &mut Context<Self>) -> AnyElement {
        let zoom_rel = if interactive { core.zoom.clone() } else { PathBuf::new() };
        let Some(zoom) = tree.find(&zoom_rel) else {
            return div().into_any_element();
        };
        // Showing a folder's "+ N small items" as their own map.
        let zoom_abs = root.join(&zoom_rel);
        let skip = self.map_skip.as_ref().filter(|(p, _)| interactive && *p == zoom_abs).map_or(0, |(_, s)| *s);
        let removable = if mode == MapColors::Advice { removable_bytes(core) } else { HashMap::new() };
        let inherited = zoom_abs.ancestors().take_while(|p| p.starts_with(root)).find_map(|p| self.app.deps.rules.matching(p)).map(|r| r.safety);
        let b = cell.get();
        let area = Rect { x: 0.0, y: 0.0, w: f32::from(b.size.width), h: f32::from(b.size.height) };
        // Measures the available area; a size change triggers one more frame with the new layout.
        let measure = canvas(
            move |bounds, window, _| {
                if bounds.size != cell.get().size {
                    cell.set(bounds);
                    window.request_animation_frame(); // refresh() is ignored while drawing
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let ctx = MapCtx { core, root, removable: &removable, mode, order: core.settings.map_order, fine: Fine::default(), interactive };
        let shown = items(&zoom.children, skip, area.w * area.h, if skip > 0 { SMALL_AREA_OPENED } else { SMALL_AREA });
        let rects = layout(&shown, area, ctx.order);
        let mut blocks = Vec::new();
        for (i, (item, r)) in shown.iter().zip(rects).enumerate() {
            let id = (i + 1) * NEST;
            match item {
                Item::Node(child) if r.w >= MIN_BLOCK && r.h >= MIN_BLOCK && i < NEST - 1 => {
                    blocks.push(self.block(&ctx, child, hue_of(&zoom.name), zoom_rel.join(&*child.name), r, id, 0, inherited, cx))
                }
                Item::Node(child) => ctx.fine.borrow_mut().push((r, color_for(child, hue_of(&zoom.name), 0, inherited, 0.0, mode))),
                Item::Group { skip, count, size } => blocks.push(self.group_block(&ctx, zoom, zoom_rel.clone(), *skip, *count, *size, r, id, cx)),
            }
        }
        let fine = ctx.fine.into_inner();
        // Drawn above the blocks (they sit inside their parent's face) but without a hitbox,
        // so hover and clicks go to the folder around them.
        let small = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let scale = window.scale_factor();
                let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                for (r, color) in &fine {
                    let (x, y, w, h) = snap(ox + r.x, oy + r.y, r.w, r.h, scale);
                    window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(w), px(h))), *color));
                }
            },
        )
        .absolute()
        .size_full();
        let (moved, left) = (self.hover_tip.clone(), self.hover_tip.clone());
        div()
            .id("map-area")
            .relative()
            .size_full()
            .when(interactive, |d| {
                // Only the label redraws as the mouse moves (it is its own view), not the map.
                d.on_mouse_move(move |e: &MouseMoveEvent, _, cx| moved.update(cx, |h, cx| h.move_to(e.position, cx)))
                    .on_hover(move |on, _, cx| {
                        if !*on {
                            left.update(cx, |h, cx| h.clear(cx));
                        }
                    })
            })
            .child(measure)
            .children(blocks)
            .child(small)
            .into_any_element()
    }

    /// One "+ N small items · size" block; clicking it shows those items as their own map.
    #[allow(clippy::too_many_arguments)]
    fn group_block(&self, ctx: &MapCtx, folder: &Node, folder_rel: PathBuf, skip: usize, count: usize, size: u64, r: Rect, id: usize, cx: &mut Context<Self>) -> AnyElement {
        // A calm mid-tone slate in both themes, so it reads as "more in here", not as a hole.
        let dark = cx.theme().background.l < 0.5;
        let (bg, fg) = if dark { (hsla(0.62, 0.14, 0.42, 1.0), white().opacity(0.9)) } else { (hsla(0.62, 0.14, 0.78, 1.0), black().opacity(0.8)) };
        let l = loc();
        let title = t!("map_small_items", count = format::count(count as u64, &l)).to_string();
        let size_text = format::size(size, &l);
        let largest: Vec<String> = folder.children[skip..].iter().take(3).map(|c| format!("• {} · {}", c.name, format::size(c.size, &l))).collect();
        let tip = format!("{title}\n{size_text}\n\n{}\n\n{}", largest.join("\n"), t!("map_small_items_tip"));
        let interactive = ctx.interactive;
        let folder_abs = ctx.root.join(&folder_rel);
        div()
            .id(("block", id))
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .p(px(1.0))
            .when(interactive, |d| {
                let hover = self.hover_tip.clone();
                d.cursor_pointer()
                    .on_hover(move |on, _, cx| hover.update(cx, |h, cx| if *on { h.enter(id, block_depth(id), tip.clone(), cx) } else { h.leave(id, cx) }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.hover_tip.update(cx, |h, cx| h.clear(cx));
                    // Opening a folder's group from a parent map: open the folder first.
                    let _ = this.app.navigate(&folder_abs);
                    this.map_skip = Some((folder_abs.clone(), skip));
                    cx.notify();
                }))
            })
            .child(
                div()
                    .id(("block-face", id))
                    .size_full()
                    .bg(cushion(bg))
                    .border_1()
                    .border_color(transparent_black())
                    .when(interactive, |d| d.hover(|s| s.border_color(white().opacity(0.8))))
                    .overflow_hidden()
                    .text_xs()
                    .text_color(fg)
                    .when(r.h > LABEL_H, |d| {
                        // Never cut the title in the middle: the full one, else just "+ N".
                        let short = format!("+ {}", format::count(count as u64, &l));
                        let fits = |s: &str| fit_label(s, r.w).filter(|f| f == s);
                        let line = fits(&title).or_else(|| fits(&short));
                        let two_lines = line.is_some() && r.h >= 2.0 * LABEL_H + 2.0;
                        d.children(line.map(|l| div().px_1().whitespace_nowrap().child(l)))
                            .when(two_lines, |d| d.children(fits(&size_text).map(|s| div().px_1().opacity(0.8).child(s))))
                    }),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn block(&self, ctx: &MapCtx, node: &Node, parent_hue: f32, rel: PathBuf, r: Rect, id: usize, depth: u8, inherited: Option<Safety>, cx: &mut Context<Self>) -> AnyElement {
        let (core, root, removable, mode, interactive) = (ctx.core, ctx.root, ctx.removable, ctx.mode, ctx.interactive);
        let abs = root.join(&rel);
        let selected = interactive && core.selected.as_ref() == Some(&rel);
        let marked = interactive && self.map_marked.contains(&abs);
        let rule = self.app.deps.rules.matching(&abs);
        let advice = rule.map(|r| r.safety).or(inherited);
        let share = if advice == Some(Safety::Safe) {
            1.0
        } else {
            removable.get(&abs).map_or(0.0, |b| *b as f32 / node.size.max(1) as f32)
        };
        // In the advice modes the block colour already says it.
        let badge = rule.filter(|_| mode == MapColors::Types).map(|rule| safety_color(rule.safety, cx));
        let size_text = format::size(node.size, &loc());
        // The reason is looked up only when the tooltip opens (own rule, then the nearest folder above with one).
        let tip = {
            let (app, abs, root) = (self.app.clone(), abs.clone(), root.to_path_buf());
            let head = format!("{}\n{}", node.name, size_text);
            let partly = removable.get(&abs).copied().filter(|b| *b > 0);
            move || {
                let rules = &app.deps.rules;
                let reason = abs.ancestors().take_while(|p| p.starts_with(&root)).find_map(|p| rules.matching(p).map(|r| (p, r)));
                match reason {
                    Some((p, r)) if p == abs => format!("{head}\n\n{}\n{}", safety_label(r.safety), t!(&r.message)),
                    Some((p, r)) => {
                        let folder = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        format!("{head}\n\n{} · {}\n{}", safety_label(r.safety), t!("tip_inside", folder = folder), t!(&r.message))
                    }
                    None => match partly {
                        Some(b) => format!("{head}\n\n{}", t!("tip_partly", size = format::size(b, &loc()))),
                        None => head.clone(),
                    },
                }
            }
        };

        let color = color_for(node, parent_hue, depth, advice, share, mode);
        let mut nested = Vec::new();
        if depth == 0 && node.kind == Kind::Dir && r.w > 60.0 && r.h > 50.0 {
            let inner = Rect { x: 1.0, y: LABEL_H, w: r.w - 4.0, h: r.h - LABEL_H - 3.0 };
            let shown = items(&node.children, 0, inner.w * inner.h, SMALL_AREA);
            let rects = layout(&shown, inner, ctx.order);
            for (j, (item, cr)) in shown.iter().zip(rects).enumerate() {
                let cid = id + j + 1;
                match item {
                    Item::Node(child) if cr.w >= MIN_BLOCK && cr.h >= MIN_BLOCK && j < NEST - 2 => {
                        nested.push(self.block(ctx, child, hue_of(&node.name), rel.join(&*child.name), cr, cid, 1, advice, cx))
                    }
                    Item::Node(child) => {
                        // Nested rects are relative to this block's face: 1 px padding + 1 px border.
                        let at = Rect { x: r.x + 2.0 + cr.x, y: r.y + 2.0 + cr.y, ..cr };
                        ctx.fine.borrow_mut().push((at, color_for(child, hue_of(&node.name), 1, advice, 0.0, mode)));
                    }
                    Item::Group { skip, count, size } => nested.push(self.group_block(ctx, node, rel.clone(), *skip, *count, *size, cr, cid, cx)),
                }
            }
        }
        // A folder with its contents drawn shows its colour only in the label strip; the
        // contents sit on even dark gutters. Leaves (and small folders) are one cushion.
        let has_nested = !nested.is_empty();
        let label_color = black().opacity(0.85);
        let label: Option<AnyElement> = if has_nested {
            name_and_size(&node.name, &size_text, r.w)
                .map(|l| div().h(px(LABEL_H - 1.0)).px_1().bg(cushion(color)).text_xs().text_color(label_color).whitespace_nowrap().child(l).into_any_element())
        } else if r.h >= 2.0 * LABEL_H + 2.0 {
            // Name and size on two lines when there is room.
            fit_label(&node.name, r.w).map(|name| {
                div()
                    .px_1()
                    .text_xs()
                    .text_color(label_color)
                    .whitespace_nowrap()
                    .child(name)
                    .child(div().text_color(black().opacity(0.6)).child(size_text.clone()))
                    .into_any_element()
            })
        } else if r.h > LABEL_H {
            name_and_size(&node.name, &size_text, r.w).map(|l| div().px_1().text_xs().text_color(label_color).whitespace_nowrap().child(l).into_any_element())
        } else {
            None
        };

        let on_click = interactive.then(|| {
            let abs = abs.clone();
            cx.listener(move |this, e: &ClickEvent, window, cx| {
                cx.stop_propagation();
                // The map is about to change under the mouse; drop the old label.
                this.hover_tip.update(cx, |h, cx| h.clear(cx));
                // Cmd-click (Ctrl elsewhere) marks, like right-click and like in the folder tree.
                if e.modifiers().secondary() {
                    this.toggle_mark(&abs, window, cx);
                    return;
                }
                let _ = this.app.activate(&abs);
                cx.notify();
            })
        });

        div()
            .id(("block", id))
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .p(px(1.0))
            .when(interactive, |d| {
                let hover = self.hover_tip.clone();
                d.cursor_pointer().on_hover(move |on, _, cx| hover.update(cx, |h, cx| if *on { h.enter(id, depth, tip(), cx) } else { h.leave(id, cx) }))
            })
            .when_some(on_click, |d, f| d.on_click(f))
            // Right-click marks / unmarks a block; the innermost block under the mouse wins.
            .when(interactive, |d| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_mark(&abs, window, cx);
                    }),
                )
            })
            .child(
                div()
                    // GPUI tracks hover only on elements with an id; without it the highlight
                    // waited for the next unrelated redraw (e.g. the tooltip appearing).
                    .id(("block-face", id))
                    .relative()
                    .size_full()
                    .map(|d| if has_nested { d.bg(cx.theme().background) } else { d.bg(cushion(color)) })
                    .border_1()
                    .when(marked, |d| d.border_2())
                    .border_color(if marked { cx.theme().primary } else if selected { white() } else if has_nested { color } else { transparent_black() })
                    .when(interactive, |d| d.hover(|s| s.border_color(white().opacity(0.8))))
                    .overflow_hidden()
                    .children(label)
                    .children(badge.map(|c| div().absolute().top(px(3.0)).right(px(3.0)).size(px(8.0)).rounded_full().border_1().border_color(white()).bg(c)))
                    .when(marked, |d| d.child(div().absolute().top(px(2.0)).right(px(2.0)).rounded_sm().bg(cx.theme().primary).child(Icon::new(Lucide::Check).size_4().text_color(white()))))
                    .children(nested),
            )
            .into_any_element()
    }

    fn file_list(&mut self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let (Some(tree), Some(root)) = (core.tree.as_ref(), core.root.as_ref()) else {
            return div().into_any_element();
        };
        let Some(zoom) = tree.find(&core.zoom) else {
            return div().into_any_element();
        };
        let key = (root.join(&core.zoom), zoom.size);
        if self.table_key.as_ref() != Some(&key) {
            let rows: Vec<FileRow> = scan::largest_files(zoom, &key.0, 200)
                .into_iter()
                .map(|(path, size)| FileRow {
                    name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    folder: path.parent().and_then(|p| p.strip_prefix(root).ok()).map(|p| p.display().to_string()).unwrap_or_default(),
                    advice: self.app.deps.rules.matching(&path).map(|r| r.safety),
                    size,
                    path,
                })
                .collect();
            self.file_table.update(cx, |t, cx| {
                t.delegate_mut().rows = rows;
                t.refresh(cx);
                t.clear_selection(cx);
            });
            self.table_key = Some(key);
        }
        DataTable::new(&self.file_table).stripe(true).into_any_element()
    }

    fn details(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let (Some(tree), Some(root)) = (core.tree.as_ref(), core.root.as_ref()) else {
            return div().into_any_element();
        };
        let rel = core.selected.clone().unwrap_or_else(|| core.zoom.clone());
        let Some(node) = tree.find(&rel) else {
            return div().into_any_element();
        };
        let abs = root.join(&rel);
        let rule = self.app.deps.rules.matching(&abs).cloned();
        let (reveal_path, trash_path) = (abs.clone(), abs.clone());
        let title = if rel.as_os_str().is_empty() { root.display().to_string() } else { node.name.to_string() };
        let folder = abs.parent().map(|p| p.display().to_string()).unwrap_or_default();
        div()
            .id("details")
            .v_flex()
            .w(relative(0.24))
            .min_w(px(220.))
            .max_w(px(340.))
            .h_full()
            .flex_shrink_0()
            .overflow_y_scrollbar()
            .border_l_1()
            .border_color(border)
            .p_4()
            .gap_3()
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(Icon::new(if node.kind == Kind::Dir { Lucide::Folder } else { Lucide::File }).text_color(muted))
                    // Long names wrap onto a second line instead of ending in "…" after a few letters.
                    .child(div().flex_1().min_w_0().text_lg().font_weight(FontWeight::BOLD).line_clamp(2).child(breakable(&title)))
                    .child(Button::new("details-close").ghost().small().icon(Lucide::PanelRightClose).on_click(cx.listener(|this, _, _, cx| {
                        this.details_open = false;
                        cx.notify();
                    }))),
            )
            .child(div().text_2xl().font_weight(FontWeight::BOLD).child(format::size(node.size, &loc())))
            // Label above the path: side by side, a narrow panel broke the path after every few letters.
            .child(div().v_flex().gap_1().child(div().text_xs().text_color(muted).child(t!("col_folder").to_string())).child(div().text_sm().child(breakable(&folder))))
            .child(match &rule {
                Some(r) => div().v_flex().gap_2().child(safety_tag(r.safety)).child(div().text_sm().child(t!(&r.message).to_string())).into_any_element(),
                None => div().text_sm().text_color(muted).child(t!("rule_none").to_string()).into_any_element(),
            })
            .child(Button::new("reveal").outline().w_full().icon(Lucide::FolderOpen).label(t!("details_show").to_string()).on_click(move |_, _, _| platform::reveal(&reveal_path)))
            .when(!rel.as_os_str().is_empty(), |d| {
                d.child(Button::new("trash").danger().w_full().icon(Lucide::Trash).label(t!("details_trash").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.app.request_trash(vec![trash_path.clone()], None, Origin::User);
                    cx.notify();
                })))
            })
            .when(core.selected.is_none(), |d| d.child(div().text_sm().text_color(muted).child(t!("details_hint").to_string())))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn long_breadcrumbs_keep_first_and_last_three() {
        let c = |names: &[&str]| names.iter().map(|n| (n.to_string(), PathBuf::from(format!("/{n}")))).collect::<Vec<_>>();
        assert_eq!(shorten_crumbs(c(&["dev", "a", "b", "c"])), c(&["dev", "a", "b", "c"]));
        let short = shorten_crumbs(c(&["dev", "a", "b", "c", "d", "e", "f"]));
        assert_eq!(short.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["dev", "…", "d", "e", "f"]);
        assert_eq!(short[1].1, PathBuf::from("/c")); // "…" goes one level above the tail
    }

    #[::core::prelude::v1::test]
    fn tiny_tiles_snap_to_whole_pixels_with_a_one_pixel_gap() {
        // 2x screen: 10.37 pt -> 20.74 px rounds to 21 px; width 4.1 pt -> right edge 28.94 px -> 29 px.
        let (x, _, w, _) = snap(10.37, 0.0, 4.1, 4.1, 2.0);
        assert_eq!((x * 2.0, w * 2.0), (21.0, 7.0)); // 8 px span minus the 1 px gap
        // Two neighbours share an edge, so every gap is exactly one pixel.
        let (a, _, aw, _) = snap(0.0, 0.0, 3.3, 3.3, 2.0);
        let (b, _, _, _) = snap(3.3, 0.0, 3.3, 3.3, 2.0);
        assert_eq!((b - (a + aw)) * 2.0, 1.0);
        // Sub-pixel slivers still get one whole pixel, so no black gap opens up.
        assert_eq!(snap(1.0, 1.0, 0.2, 5.0, 2.0).2 * 2.0, 1.0);
    }

    fn leaf(name: &str, size: u64) -> Node {
        Node { name: name.into(), size, kind: Kind::File, children: Box::default() }
    }

    #[::core::prelude::v1::test]
    fn small_tail_is_grouped_and_kept_in_size_order() {
        // 10 000 pt² area: 6000 and 3000 are big; the eight 100s are each < 24² pt² → one group of 800.
        let mut kids = vec![leaf("a", 6000), leaf("b", 3000)];
        kids.extend((0..8).map(|i| leaf(&format!("s{i}"), 100)));
        let shown = items(&kids, 0, 10_000., SMALL_AREA);
        assert_eq!(shown.len(), 3);
        assert!(matches!(shown[2], Item::Group { skip: 2, count: 8, size: 800 }), "{shown:?}");
        // Opening the group shows those eight on their own, now big enough.
        assert_eq!(items(&kids, 2, 10_000., SMALL_AREA_OPENED).len(), 8);
        // All small (a folder of photos): tiles, no group.
        assert!(items(&kids[2..], 0, 10_000., SMALL_AREA).iter().all(|i| matches!(i, Item::Node(_))));
        // A lone small item is not worth a group.
        assert!(items(&kids[..3], 0, 10_000., SMALL_AREA).iter().all(|i| matches!(i, Item::Node(_))));
        // A group bigger than some shown items is placed among them by size.
        let mut many = vec![leaf("a", 9000), leaf("b", 600)];
        many.extend((0..40).map(|i| leaf(&format!("s{i}"), 20)));
        let shown = items(&many, 0, 100_000., SMALL_AREA);
        assert!(shown.windows(2).all(|w| w[0].size() >= w[1].size()));
    }

    #[::core::prelude::v1::test]
    fn neighbouring_blocks_share_whole_point_edges() {
        let kids: Vec<Node> = [500, 300, 120, 80].iter().enumerate().map(|(i, s)| leaf(&format!("f{i}"), *s)).collect();
        let shown = items(&kids, 0, 1e9, SMALL_AREA);
        let rects = layout(&shown, Rect { x: 0.3, y: 0.7, w: 333.3, h: 211.1 }, MapOrder::TopLeft);
        for r in &rects {
            assert_eq!((r.x.fract(), r.y.fract(), r.w.fract(), r.h.fract()), (0.0, 0.0, 0.0, 0.0), "{r:?}");
        }
        // Every right edge that meets another block's left edge meets it exactly.
        for a in &rects {
            let touching = rects.iter().filter(|b| (b.x - (a.x + a.w)).abs() < 1.0).count();
            let exact = rects.iter().filter(|b| b.x == a.x + a.w).count();
            assert_eq!(touching, exact);
        }
    }

    #[::core::prelude::v1::test]
    fn strip_labels_keep_the_size() {
        assert_eq!(name_and_size("store-project-management", "1.3 GB", 150.).as_deref(), Some("store-…gement · 1.3 GB"));
        assert_eq!(name_and_size("frontend", "1.3 GB", 300.).as_deref(), Some("frontend · 1.3 GB"));
        assert_eq!(name_and_size("frontend", "1.3 GB", 70.).as_deref(), Some("frontend")); // no room for the size
    }

    #[::core::prelude::v1::test]
    fn labels_are_cut_in_the_middle() {
        assert_eq!(fit_label("short.mov", 200.).as_deref(), Some("short.mov"));
        let cut = fit_label("Screen Recording 2026-09-24 at 15.47.33.mov", 120.).unwrap();
        assert!(cut.contains('…') && cut.ends_with(".mov"), "{cut}");
        assert!(cut.chars().count() <= 18, "{cut}");
        assert_eq!(fit_label("anything", 20.), None);
    }

    #[::core::prelude::v1::test]
    fn marked_folder_covers_marked_items_inside_it() {
        let marked: std::collections::BTreeSet<PathBuf> = ["/d/a", "/d/a/node_modules", "/d/b/node_modules", "/d/ab"].map(PathBuf::from).into();
        assert_eq!(top_level(&marked), ["/d/a", "/d/ab", "/d/b/node_modules"].map(PathBuf::from));
    }

    #[::core::prelude::v1::test] // gpui exports its own `test` attribute
    fn removable_shades_from_white_to_green_and_differs_from_advice() {
        assert_eq!(removable_color(0.0, 0).s, 0.0);
        assert!(removable_color(0.5, 0).l > removable_color(1.0, 0).l);
        assert_eq!(removable_color(1.0, 0).l, 0.50);
    }
}
