//! "Free up space": the user picks a target, `wizard::plan` proposes what to free, ticked
//! items go through the usual Trash and system confirmations.

use super::a11y::item_label;
use super::cleanup::{group_by_rule, group_title, system_name};
use super::{AppView, card, heading, loc};
use crate::actions::Origin;
use crate::finders::cleaners::{self, Cleaner, Estimate, SystemStatus};
use crate::rules::Finding;
use crate::finders::dupes::Removal;
use crate::state::{Core, ItemStatus, Report, SystemItem};
use crate::wizard::{Candidate, Source, Target, plan, reconcile};
use crate::format;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::*;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;

const GB: u64 = 1_000_000_000;
const PRESETS: [u64; 4] = [5, 10, 20, 50];

/// Sensible custom targets, in GB.
const MIN_GB: f64 = 0.1;
const MAX_GB: f64 = 1000.;

/// "20", "2,5" or "0.5" (GB) to bytes; anything outside `MIN_GB..=MAX_GB` is ignored.
pub(super) fn parse_gb(s: &str) -> Option<u64> {
    let v: f64 = s.trim().replace(',', ".").parse().ok()?;
    (MIN_GB..=MAX_GB).contains(&v).then_some((v * GB as f64) as u64)
}

/// What may be typed into the GB field: up to 4 digits, then optionally "." or "," and 2 digits.
pub(super) fn gb_typing_ok(s: &str) -> bool {
    let (int, frac) = match s.split_once(['.', ',']) {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |x: &str, max: usize| x.len() <= max && x.bytes().all(|b| b.is_ascii_digit());
    digits(int, 4) && frac.is_none_or(|f| digits(f, 2))
}

/// Size of a system cleanup the wizard may offer. Left out: unknown sizes, Git items (they may
/// hold work) and emptying the Trash, which would also delete what the wizard just moved there.
fn wizard_size(i: &SystemItem) -> Option<u64> {
    match i.status {
        SystemStatus::Ready { estimate: Estimate::Bytes(n), .. } if i.repo.is_none() && i.cleaner != Cleaner::EmptyTrash => Some(n),
        _ => None,
    }
}

fn system_sizes(core: &Core) -> Vec<(String, u64)> {
    core.system.iter().filter_map(|(id, i)| wizard_size(i).map(|n| (id.clone(), n))).collect()
}

/// What a candidate's path or id stands for, built once per render for `candidate_groups`.
struct Lookup<'a> {
    by_path: &'a HashMap<&'a std::path::Path, &'a Finding>,
    names: HashMap<String, String>,
    removal_of: &'a HashMap<&'a PathBuf, &'a Removal>,
    app_of: &'a HashMap<&'a PathBuf, &'a crate::finders::apps::App>,
    old_of: &'a HashMap<&'a PathBuf, &'a crate::finders::oldfiles::OldFile>,
    rule_counts: HashMap<&'a str, usize>,
}

/// The "Free up space" screen.
pub(crate) struct WizardState {
    pub(crate) target: crate::wizard::Target,
    pub(crate) ticks: std::collections::HashSet<crate::wizard::Source>,
    pub(crate) seen: Option<crate::wizard::Target>,
    /// The user ticked or unticked something: keep their choice when the plan is rebuilt.
    pub(crate) manual: bool,
    /// Bytes the wizard last moved to the Trash; shows the "empty the Trash" hint.
    pub(crate) trashed: Option<u64>,
    pub(crate) open: std::collections::HashSet<String>,
    pub(crate) gb: Entity<gpui_kit::component::input::InputState>,
}

impl AppView {
    pub(super) fn free_up(&mut self, core: &Core, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, bg, muted, warning) = (theme.border, theme.background, theme.muted_foreground, theme.warning);
        let l = loc();
        let head = heading(Some(Icon::new(Lucide::Gauge).size_6().into_any_element()), t!("free_title").to_string(), t!("free_subtitle").to_string(), muted);
        if core.tree.is_none() {
            return div().v_flex().gap_4().p_6().child(head).child(card(border, bg).child(t!("free_no_scan").to_string()).child(div().child(Button::new("free-scan").primary().icon(Lucide::FolderOpen).label(t!("home_pick").to_string()).on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))))).into_any_element();
        }
        let system = system_sizes(core);
        let removals = self.dupe_removals(core);
        let dupes: Vec<(PathBuf, u64)> = removals.iter().map(|r| (r.path.clone(), r.size)).collect();
        let removal_of: HashMap<&PathBuf, &Removal> = removals.iter().map(|r| (&r.path, r)).collect();
        let unused: Vec<&crate::finders::apps::App> = match &core.apps {
            crate::state::Apps::Done(list) => list.iter().filter(|a| a.removable()).collect(),
            _ => Vec::new(),
        };
        let apps: Vec<(PathBuf, u64)> = unused.iter().map(|a| (a.path.clone(), a.size())).collect();
        let app_of: HashMap<&PathBuf, &crate::finders::apps::App> = unused.iter().map(|a| (&a.path, *a)).collect();
        let old_list: Vec<&crate::finders::oldfiles::OldFile> = match &core.old_files {
            crate::state::OldFiles::Done(list) => list.iter().collect(),
            _ => Vec::new(),
        };
        let old: Vec<(PathBuf, u64)> = old_list.iter().map(|o| (o.path.clone(), o.size)).collect();
        let old_of: HashMap<&PathBuf, &crate::finders::oldfiles::OldFile> = old_list.iter().map(|o| (&o.path, *o)).collect();
        let p = plan(&core.findings, &system, &dupes, &apps, &old, self.wizard.target);
        if self.wizard.seen != Some(self.wizard.target) {
            self.wizard.seen = Some(self.wizard.target);
            self.wizard.manual = false;
        }
        self.wizard.ticks = reconcile(&self.wizard.ticks, &p, !self.wizard.manual);
        let by_path: HashMap<&std::path::Path, &Finding> = core.findings.iter().map(|f| (f.path.as_path(), f)).collect();
        let size_of = |s: &Source| -> u64 {
            match s {
                Source::Finding(path) => by_path.get(path.as_path()).map_or(0, |f| f.size),
                Source::System(id) => system.iter().find(|(i, _)| i == id).map_or(0, |(_, n)| *n),
                Source::Duplicate(path) => removal_of.get(path).map_or(0, |r| r.size),
                Source::App(path) => app_of.get(path).map_or(0, |a| a.size()),
                Source::Old(path) => old_of.get(path).map_or(0, |o| o.size),
            }
        };
        let have: u64 = self.wizard.ticks.iter().map(size_of).sum();
        let checking = core.system.values().any(|i| matches!(i.status, SystemStatus::Checking));

        let chip = |id: usize, label: String, target: Target, active: bool, cx: &mut Context<Self>| {
            Button::new(("free-chip", id)).label(label).map(|b| if active { b.primary() } else { b.outline() }).on_click(cx.listener(move |this, _, window, cx| {
                this.wizard.target = target;
                // A chip replaces the custom amount, so its text (and any hint about it) goes.
                this.wizard.gb.update(cx, |s, cx| s.set_value("", window, cx));
                cx.notify();
            }))
        };
        let mut chips = div().h_flex().flex_wrap().gap_2();
        for (i, gb) in PRESETS.iter().enumerate() {
            let target = Target::Bytes(gb * GB);
            chips = chips.child(chip(i, format!("{gb} GB"), target, self.wizard.target == target, cx));
        }
        chips = chips
            .child(div().w(px(110.)).child(Input::new(&self.wizard.gb).suffix(t!("free_custom").to_string())))
            .child(chip(9, t!("free_all_safe").to_string(), Target::AllSafe, self.wizard.target == Target::AllSafe, cx));

        // Typed but outside the sensible range (e.g. "0" or "0.05"): the target stays, say why.
        let typed = self.wizard.gb.read(cx).value();
        let gb_invalid = !typed.trim().is_empty() && parse_gb(&typed).is_none();
        let danger = cx.theme().danger;
        let progress = match self.wizard.target {
            Target::Bytes(goal) => div()
                .v_flex()
                .gap_1()
                .child(Progress::new("free-progress").value((have as f32 / goal.max(1) as f32 * 100.).min(100.)))
                .child(div().text_sm().child(t!("free_progress", have = format::size(have, &l), goal = format::size(goal, &l)).to_string())),
            Target::AllSafe => div().text_sm().child(format::size(have, &l)),
        };

        let mut list = div().v_flex().gap_1();
        let main: Vec<&Candidate> = p.ticked.iter().collect();
        // Findings per rule over all lists, so a rule split between the picks and the extras says which part is which.
        let mut rule_counts: HashMap<&str, usize> = HashMap::new();
        for c in p.ticked.iter().chain(&p.more_safe).chain(&p.review) {
            if let Source::Finding(path) = &c.source
                && let Some(f) = by_path.get(path.as_path())
            {
                *rule_counts.entry(f.rule_id.as_str()).or_default() += 1;
            }
        }
        let lookup = Lookup { by_path: &by_path, names: system_names(core), removal_of: &removal_of, app_of: &app_of, old_of: &old_of, rule_counts };
        list = list.children(self.candidate_groups("main", &main, &lookup, cx));
        if !p.more_safe.is_empty() {
            let more: Vec<&Candidate> = p.more_safe.iter().collect();
            list = list.child(div().pt_3().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(muted).child(t!("free_more_safe").to_string()));
            list = list.children(self.candidate_groups("more", &more, &lookup, cx));
        }
        if let (Target::Bytes(goal), false) = (self.wizard.target, p.review.is_empty()) {
            let review: Vec<&Candidate> = p.review.iter().collect();
            list = list.child(div().pt_3().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(warning).child(t!("free_short", size = format::size(goal.saturating_sub(p.safe_total), &l)).to_string()));
            list = list.children(self.candidate_groups("review", &review, &lookup, cx));
        }
        if let Target::Bytes(goal) = self.wizard.target
            && p.review.is_empty()
            && p.safe_total < goal
            && !checking
        {
            list = list.child(div().pt_3().text_sm().text_color(muted).child(t!("free_short_none", size = format::size(goal - p.safe_total, &l)).to_string()));
        }
        let empty = p.ticked.is_empty() && p.more_safe.is_empty() && p.review.is_empty();

        div()
            .id("free-up")
            .v_flex()
            .size_full()
            .overflow_y_scrollbar()
            .gap_4()
            .p_6()
            .child(head)
            .children(self.trash_hint(core, cx))
            .child(card(border, bg).child(div().font_weight(FontWeight::SEMIBOLD).child(t!("free_need").to_string())).child(chips).when(gb_invalid, |d| d.child(div().text_sm().text_color(danger).child(t!("free_gb_range").to_string()))).child(progress))
            .when(checking, |d| d.child(div().text_sm().text_color(muted).child(t!("free_checking").to_string())))
            .child(card(border, bg).child(if empty { div().child(t!("free_nothing").to_string()) } else { list }))
            .child(
                div().h_flex().justify_end().child(
                    Button::new("free-go").primary().large().icon(Lucide::Trash).label(t!("free_button", size = format::size(have, &l)).to_string()).disabled(have == 0 || core.pending.is_some()).on_click(cx.listener(|this, _, _, cx| this.free_ticked(cx))),
                ),
            )
            .into_any_element()
    }

    /// Candidates grouped like the Cleanup page: one row per rule (tri-state checkbox, expandable),
    /// one row per system tool.
    fn candidate_groups(&self, section: &'static str, items: &[&Candidate], lookup: &Lookup, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Lookup { by_path, names, removal_of, app_of, old_of, rule_counts } = lookup;
        let mut rows = Vec::new();
        let copies: Vec<&Removal> = items.iter().filter_map(|c| match &c.source {
            Source::Duplicate(p) => removal_of.get(p).copied(),
            _ => None,
        }).collect();
        if !copies.is_empty() {
            let key = format!("{section}-dupes");
            let open = self.wizard.open.contains(&key);
            let sources: Vec<Source> = copies.iter().map(|r| Source::Duplicate(r.path.clone())).collect();
            let size = copies.iter().map(|r| r.size).sum();
            rows.push(self.pick_row(&key, sources, format!("{} ({})", t!("free_dupes_group"), copies.len()), String::new(), size, Some((key.clone(), open)), cx));
            if open {
                // ponytail: first 50 copies only; the group checkbox still covers all of them
                let children: Vec<AnyElement> = copies.iter().take(50).map(|r| self.pick_row(&format!("{key}-{}", r.path.display()), vec![Source::Duplicate(r.path.clone())], r.path.display().to_string(), t!("dupes_copy_of", path = r.kept.display().to_string()).to_string(), r.size, None, cx)).collect();
                rows.push(div().pl_8().v_flex().children(children).into_any_element());
            }
        }
        for c in items.iter().filter(|c| matches!(c.source, Source::System(_))) {
            let Source::System(id) = &c.source else { continue };
            rows.push(self.pick_row(&format!("{section}-{id}"), vec![c.source.clone()], names.get(id).cloned().unwrap_or_else(|| id.clone()), t!("free_system_group").to_string(), c.size, None, cx));
        }
        let now = crate::state::now_secs();
        for c in items.iter().filter(|c| matches!(c.source, Source::App(_))) {
            let Source::App(p) = &c.source else { continue };
            let Some(a) = app_of.get(p) else { continue };
            rows.push(self.pick_row(&format!("{section}-app-{}", p.display()), vec![c.source.clone()], a.name.clone(), super::apps_card::last_used_text(a, now), a.size(), None, cx));
        }
        for g in crate::finders::oldfiles::Group::ALL {
            let files: Vec<&crate::finders::oldfiles::OldFile> = items.iter().filter_map(|c| match &c.source {
                Source::Old(p) => old_of.get(p).copied(),
                _ => None,
            }).filter(|o| o.group == g).collect();
            if files.is_empty() {
                continue;
            }
            let key = format!("{section}-old-{g:?}");
            let open = self.wizard.open.contains(&key);
            let sources: Vec<Source> = files.iter().map(|o| Source::Old(o.path.clone())).collect();
            let size = files.iter().map(|o| o.size).sum();
            rows.push(self.pick_row(&key, sources, format!("{} ({})", super::old_files_card::group_title(g), files.len()), String::new(), size, Some((key.clone(), open)), cx));
            if open {
                // ponytail: first 50 only; the group checkbox still covers all of them
                let children: Vec<AnyElement> = files.iter().take(50).map(|o| self.pick_row(&format!("{key}-{}", o.path.display()), vec![Source::Old(o.path.clone())], o.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), super::old_files_card::used_text(o.last_used, now), o.size, None, cx)).collect();
                rows.push(div().pl_8().v_flex().children(children).into_any_element());
            }
        }
        let findings: Vec<&Finding> = items.iter().filter_map(|c| match &c.source {
            Source::Finding(p) => by_path.get(p.as_path()).copied(),
            _ => None,
        }).collect();
        for group in group_by_rule(&findings) {
            let key = format!("{section}-{}", group[0].rule_id);
            let sources: Vec<Source> = group.iter().map(|f| Source::Finding(f.path.clone())).collect();
            let size = group.iter().map(|f| f.size).sum();
            let title = group_label(&group_title(&group), group.len(), rule_counts.get(group[0].rule_id.as_str()).copied().unwrap_or(group.len()), section);
            let open = self.wizard.open.contains(&key);
            let children = (open && group.len() > 1).then(|| {
                group.iter().map(|f| self.pick_row(&format!("{key}-{}", f.path.display()), vec![Source::Finding(f.path.clone())], f.path.display().to_string(), String::new(), f.size, None, cx).into_any_element()).collect::<Vec<_>>()
            });
            rows.push(self.pick_row(&key, sources, title, t!(&group[0].message).to_string(), size, (group.len() > 1).then_some((key.clone(), open)), cx));
            if let Some(children) = children {
                rows.push(div().pl_8().v_flex().children(children).into_any_element());
            }
        }
        rows
    }

    /// One tickable row; `expand` = (group key, open) adds a chevron.
    #[allow(clippy::too_many_arguments)]
    fn pick_row(&self, id: &str, sources: Vec<Source>, title: String, reason: String, size: u64, expand: Option<(String, bool)>, cx: &mut Context<Self>) -> AnyElement {
        let l = loc();
        let muted = cx.theme().muted_foreground;
        let ticked = sources.iter().filter(|s| self.wizard.ticks.contains(s)).count();
        let checked = ticked == sources.len();
        let eid = ElementId::Name(id.to_string().into());
        div()
            .h_flex()
            .gap_3()
            .py_1()
            .child(Checkbox::new(eid.clone()).accessibility_label(item_label(&title, &format::size(size, &l))).checked(checked).on_click(cx.listener(move |this, on: &bool, _, cx| {
                for s in &sources {
                    super::tick(&mut this.wizard.ticks, s, *on);
                }
                this.wizard.manual = true;
                cx.notify();
            })))
            .when_some(expand, |d, (key, open)| {
                d.child(Button::new(ElementId::Name(format!("{id}-x").into())).accessibility_label(t!("a11y_expand").to_string()).ghost().xsmall().icon(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).on_click(cx.listener(move |this, _, _, cx| {
                    if !this.wizard.open.remove(&key) { this.wizard.open.insert(key.clone()); }
                    cx.notify();
                })))
            })
            .child(div().v_flex().flex_1().min_w_0().child(div().truncate().font_weight(FontWeight::MEDIUM).child(title)).when(!reason.is_empty(), |d| d.child(div().text_xs().text_color(muted).line_clamp(2).child(reason))))
            .child(div().w(px(90.)).flex_none().text_right().font_weight(FontWeight::SEMIBOLD).child(format::size(size, &l)))
            .into_any_element()
    }

    /// Ticked files to the Trash dialog; ticked system tools to their own dialog afterwards.
    fn free_ticked(&mut self, cx: &mut Context<Self>) {
        let mut files = Vec::new();
        let mut picks = Vec::new();
        let mut copies = Vec::new();
        let mut app_picks = Vec::new();
        for s in &self.wizard.ticks {
            match s {
                Source::Finding(p) => files.push(p.clone()),
                Source::System(id) => picks.push((id.clone(), None)),
                Source::Duplicate(p) => copies.push(p.clone()),
                Source::App(p) => app_picks.push(p.clone()),
                Source::Old(p) => files.push(p.clone()),
            }
        }
        let app = self.app.clone();
        let removals = self.dupe_removals(&app.core.lock().unwrap());
        let app_parts: Vec<(PathBuf, u64)> = match &app.core.lock().unwrap().apps {
            crate::state::Apps::Done(list) => list.iter().filter(|a| a.removable() && app_picks.contains(&a.path)).flat_map(|a| std::iter::once((a.path.clone(), a.own)).chain(a.leftovers.iter().cloned())).collect(),
            _ => Vec::new(),
        };
        // A copy goes only if its kept twin stays: not trashed in this same step, still there, unchanged.
        let ticked: Vec<Removal> = removals.iter().filter(|r| copies.contains(&r.path)).cloned().collect();
        let safe_copies = crate::finders::dupes::without_kept_in(ticked, &files).into_iter().filter(Removal::still_safe);
        let sizes: HashMap<PathBuf, u64> = {
            let core = app.core.lock().unwrap();
            core.findings.iter().map(|f| (f.path.clone(), f.size)).chain(removals.iter().map(|r| (r.path.clone(), r.size))).chain(app_parts.iter().cloned()).chain(match &core.old_files {
    crate::state::OldFiles::Done(list) => list.iter().map(|o| (o.path.clone(), o.size)).collect::<Vec<_>>(),
    _ => Vec::new(),
}).collect()
        };
        files.extend(safe_copies.map(|r| r.path));
        files.extend(app_parts.iter().map(|(p, _)| p.clone()));
        let (now, rx) = if files.is_empty() { (Vec::new(), None) } else { app.request_trash(files, None, Origin::User) };
        cx.spawn(async move |this, cx| {
            let mut report = now;
            if let Some(rx) = rx {
                report.extend(rx.await.unwrap_or_default());
            }
            // Trashed files only free space once the Trash is emptied: offer that next.
            let moved = trashed_bytes(&report, &sizes);
            if moved > 0 {
                app.recheck_tool(Cleaner::EmptyTrash);
                let _ = this.update(cx, |v, cx| {
                    v.wizard.trashed = Some(moved);
                    cx.notify();
                });
            }
            if !picks.is_empty() {
                let _ = app.request_system_batch(picks, None, Origin::User); // each system tool asks on its own
            }
        })
        .detach();
        cx.notify();
    }

    /// After the wizard moved files to the Trash: say the space is not free yet, offer to empty it.
    fn trash_hint(&self, core: &Core, cx: &mut Context<Self>) -> Option<AnyElement> {
        let moved = self.wizard.trashed?;
        let trash = core.system.get(cleaners::item_id(Cleaner::EmptyTrash, None).as_str())?;
        let size = match trash.status {
            SystemStatus::Ready { estimate: Estimate::Bytes(n), .. } if n > 0 => Some(n),
            SystemStatus::Checking => None,
            _ => return None,
        };
        let l = loc();
        let theme = cx.theme();
        Some(
            card(theme.warning, theme.background)
                .child(div().h_flex().gap_2().child(Icon::new(Lucide::Info).text_color(theme.warning)).child(div().flex_1().child(t!("free_trashed", size = format::size(moved, &l)).to_string())))
                .child(
                    div().h_flex().justify_end().child(
                        Button::new("free-empty-trash")
                            .danger()
                            .icon(Lucide::Trash)
                            .label(t!("free_empty_trash", size = size.map(|n| format::size(n, &l)).unwrap_or_else(|| "…".into())).to_string())
                            .disabled(size.is_none() || core.pending.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let _ = this.app.request_system_batch(vec![(cleaners::item_id(Cleaner::EmptyTrash, None), None)], None, Origin::User);
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
        )
    }
}

/// Bytes of the reported items that actually went to the Trash.
fn trashed_bytes(report: &Report, sizes: &HashMap<PathBuf, u64>) -> u64 {
    report.iter().filter(|(_, s)| *s == ItemStatus::Trashed).filter_map(|(p, _)| sizes.get(p)).sum()
}

fn system_names(core: &Core) -> HashMap<String, String> {
    core.system.iter().map(|(id, i)| (id.clone(), system_name(i.cleaner, i.repo.as_deref()))).collect()
}

/// "node_modules (9)", or, when a rule is split between the picks and "More safe items",
/// "node_modules · 9 biggest of 198" and "node_modules · 189 more".
fn group_label(name: &str, here: usize, total: usize, section: &str) -> String {
    match (here < total, section) {
        (false, _) if here > 1 => format!("{name} ({here})"),
        (false, _) => name.to_string(),
        (true, "more") => t!("free_split_more", name = name, count = here.to_string()).to_string(),
        (true, _) => t!("free_split_biggest", name = name, count = here.to_string(), total = total.to_string()).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{gb_typing_ok, group_label, parse_gb, trashed_bytes, wizard_size};
    use crate::state::ItemStatus;
    use std::path::PathBuf;
    use crate::finders::cleaners::{Cleaner, Estimate, SystemStatus};
    use crate::state::SystemItem;

    fn item(cleaner: Cleaner, repo: Option<&str>, estimate: Estimate) -> SystemItem {
        SystemItem { cleaner, repo: repo.map(Into::into), status: SystemStatus::Ready { estimate, detail: vec![] } }
    }

    #[test]
    fn trashed_bytes_counts_only_trashed_items() {
        let sizes = [(PathBuf::from("/a"), 5), (PathBuf::from("/b"), 7), (PathBuf::from("/c"), 11)].into_iter().collect();
        let report = vec![(PathBuf::from("/a"), ItemStatus::Trashed), (PathBuf::from("/b"), ItemStatus::Declined), (PathBuf::from("/c"), ItemStatus::Trashed), (PathBuf::from("/x"), ItemStatus::Trashed)];
        assert_eq!(trashed_bytes(&report, &sizes), 16);
    }

    #[test]
    fn wizard_skips_empty_trash_git_and_unknown_sizes() {
        // Emptying the Trash would also delete what the wizard just moved there.
        assert_eq!(wizard_size(&item(Cleaner::EmptyTrash, None, Estimate::Bytes(9))), None);
        assert_eq!(wizard_size(&item(Cleaner::GitWorktrees, Some("/r"), Estimate::Bytes(9))), None);
        assert_eq!(wizard_size(&item(Cleaner::Homebrew, None, Estimate::Count(3))), None);
        assert_eq!(wizard_size(&item(Cleaner::Homebrew, None, Estimate::Bytes(9))), Some(9));
    }

    #[test]
    fn parse_gb_accepts_positive_numbers_only() {
        const GB: u64 = 1_000_000_000;
        assert_eq!(parse_gb("20"), Some(20 * GB));
        assert_eq!(parse_gb(" 2,5 "), Some(5 * GB / 2));
        assert_eq!(parse_gb("0.5"), Some(GB / 2));
        assert_eq!(parse_gb("0.1"), Some(GB / 10));
        assert_eq!(parse_gb("1000"), Some(1000 * GB));
        for bad in ["", "0", "-3", "abc", "1e400", "0.0000000001", "0.09", "1000.5", "9999", "."] {
            assert_eq!(parse_gb(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_amounts_can_be_typed() {
        for ok in ["", "2", "25", "2.", "2,5", "0.25", "1000"] {
            assert!(gb_typing_ok(ok), "{ok}");
        }
        for bad in ["a", "-1", "1e4", "2.5.1", "2,,5", "0.255", "12345", " 2", "2 "] {
            assert!(!gb_typing_ok(bad), "{bad}");
        }
    }

    #[::core::prelude::v1::test]
    fn a_rule_split_between_picks_and_extras_says_which_part() {
        assert_eq!(group_label("node_modules", 9, 9, "main"), "node_modules (9)");
        assert_eq!(group_label("Xcode", 1, 1, "main"), "Xcode");
        assert_eq!(group_label("node_modules", 9, 198, "main"), "node_modules · 9 biggest of 198");
        assert_eq!(group_label("node_modules", 189, 198, "more"), "node_modules · 189 more");
    }
}
