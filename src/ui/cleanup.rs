use super::{AppView, card, heading, loc};
use super::a11y::{Pressable as _, item_label};
use crate::actions::Origin;
use crate::finders::cleaners::{AUTOMATION_DENIED, Cleaner, Estimate, GIT_REPO_LIMIT, NOT_INSTALLED, SystemStatus};
use crate::format;
use crate::platform;
use crate::rules::{self, Finding, Safety};
use crate::finders::dupes::{Group, Removal, pick_keep};
use crate::state::{Core, Dupes, Screen};
use gpui_kit::component::progress::Progress;
use std::sync::atomic::Ordering::Relaxed;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use gpui_kit::component::scroll::ScrollableElement as _;

/// Display name, e.g. "Homebrew cleanup" or "Merged worktrees in my-repo".
pub(super) fn system_name(cleaner: Cleaner, repo: Option<&Path>) -> String {
    let repo = repo.and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    t!(cleaner.name_key(), repo = repo).to_string()
}

pub(super) fn estimate_text(e: Estimate) -> String {
    match e {
        Estimate::Bytes(b) => format::size(b, &loc()),
        Estimate::Count(n) => t!("sys_count", count = n.to_string()).to_string(),
        Estimate::Unknown => String::new(),
    }
}

const LINUX_CMD: &str = "sudo apt clean && sudo journalctl --vacuum-size=200M";

/// What is ticked and expanded on the Cleanup screen and its cards.
pub(crate) struct CleanupState {
    /// Findings ticked on the Cleanup screen.
    pub(crate) selected: std::collections::BTreeSet<std::path::PathBuf>,
    /// Expanded groups on the Cleanup screen ("safety:rule" for findings, "git:<kind>" and "git:<item id>").
    pub(crate) open: std::collections::BTreeSet<String>,
    /// Git branches / worktrees ticked on the Cleanup screen: (system item id, branch name or path).
    pub(crate) git_selected: std::collections::BTreeSet<(String, String)>,
    /// Copies the user chose to keep (at most one per duplicate group).
    pub(crate) dupe_keep: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) dupes_shown: usize,
    pub(crate) apps_ticked: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) apps_open: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) models_ticked: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) old_ticked: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) old_open: std::collections::HashSet<crate::finders::oldfiles::Group>,
    pub(crate) old_shown: usize,
}

impl AppView {
    fn system_card(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border, bg) = (t.muted_foreground, t.border, t.background);
        // Tool rows first, then only the Git rows that have something to show.
        let shown = |i: &crate::state::SystemItem| match &i.status {
            _ if i.repo.is_none() => true,
            SystemStatus::Checking | SystemStatus::Running | SystemStatus::Failed { .. } => true,
            SystemStatus::Ready { detail, .. } => !detail.is_empty(),
            SystemStatus::Unavailable { .. } => false,
        };
        // Git has its own card (git_card).
        let tools = core.system.iter().filter(|(_, i)| i.repo.is_none());
        let rows: Vec<AnyElement> = tools.filter(|(_, i)| shown(i)).enumerate().map(|(i, (id, item))| self.system_row(i, id, item, cx)).collect();
        let storage_action = if cfg!(target_os = "linux") {
            div()
                .v_flex()
                .items_end()
                .gap_1()
                .child(div().text_sm().text_color(muted).child(t!("sys_linux_hint").to_string()))
                .child(div().font_family("monospace").text_sm().child(LINUX_CMD))
                .child(Button::new("sys-copy").ghost().small().icon(Lucide::Copy).label(t!("sys_copy").to_string()).on_click(|_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(LINUX_CMD.to_string()));
                }))
                .into_any_element()
        } else {
            Button::new("sys-storage").outline().small().icon(Lucide::ExternalLink).label(t!("sys_open").to_string()).on_click(|_, _, _| platform::open_storage_tool()).into_any_element()
        };
        card(border, bg)
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("group_system").to_string()))
                    .child(div().flex_1())
                    .child(Button::new("sys-refresh").accessibility_label(t!("nav_refresh").to_string()).ghost().small().icon(Lucide::RefreshCw).tooltip(t!("nav_refresh").to_string()).on_click(cx.listener(|this, _, _, cx| {
                        this.app.check_system();
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .v_flex()
                    .children(rows)
                    .child(
                        div().id("sys-storage-row").h_flex().gap_3().py_3().child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("sys_storage_name").to_string()))
                                .child(div().text_sm().text_color(muted).child(t!("sys_storage_advice").to_string())),
                        )
                        .child(storage_action),
                    ),
            )
            .into_any_element()
    }

    /// What the Git check did, so "nothing found" never looks like "not working".
    fn git_status_text(core: &Core) -> String {
        let items: Vec<&crate::state::SystemItem> = core.system.values().filter(|i| i.repo.is_some()).collect();
        let repos = items.iter().filter_map(|i| i.repo.as_ref()).collect::<BTreeSet<_>>().len();
        let folder = core.root.as_deref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let checking = items.iter().any(|i| matches!(i.status, SystemStatus::Checking));
        let found = items.iter().filter(|i| matches!(&i.status, SystemStatus::Ready { detail, .. } if !detail.is_empty())).filter_map(|i| i.repo.as_ref()).collect::<BTreeSet<_>>().len();
        let unavailable = items.iter().find_map(|i| match &i.status {
            SystemStatus::Unavailable { reason } if items.iter().all(|j| matches!(j.status, SystemStatus::Unavailable { .. })) => Some(reason.clone()),
            _ => None,
        });
        let mut status = if core.tree.is_none() {
            t!("sys_git_need_scan").to_string()
        } else if repos == 0 {
            t!("sys_git_no_repos", folder = folder).to_string()
        } else if checking {
            t!("sys_git_checking", count = repos).to_string()
        } else if let Some(reason) = unavailable {
            let reason = if reason == NOT_INSTALLED { t!("sys_not_installed").to_string() } else { reason };
            t!("sys_unavailable", reason = reason).to_string()
        } else if found == 0 {
            t!("sys_git_none", count = repos, folder = folder).to_string()
        } else {
            t!("sys_git_found", count = repos, n = found).to_string()
        };
        if repos >= GIT_REPO_LIMIT {
            status = format!("{status} {}", t!("sys_git_capped", count = GIT_REPO_LIMIT));
        }
        status
    }

    /// Git cleanup: merged branches and merged worktrees, each an expandable group with one
    /// sub-group per repository and a checkbox per branch / worktree.
    fn git_card(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border, bg) = (t.muted_foreground, t.border, t.background);
        let mut body: Vec<AnyElement> = Vec::new();
        for (k, (cleaner, key)) in [(Cleaner::GitBranches, "git_group_branches"), (Cleaner::GitWorktrees, "git_group_worktrees")].into_iter().enumerate() {
            // (item id, repo, entries) of repos with something to clean, most first.
            let mut repos: Vec<(String, PathBuf, Vec<String>, bool)> = core
                .system
                .iter()
                .filter(|(_, i)| i.cleaner == cleaner)
                .filter_map(|(id, i)| match &i.status {
                    SystemStatus::Ready { detail, .. } if !detail.is_empty() => Some((id.clone(), i.repo.clone()?, detail.clone(), false)),
                    SystemStatus::Running => Some((id.clone(), i.repo.clone()?, Vec::new(), true)),
                    _ => None,
                })
                .collect();
            if repos.is_empty() {
                continue;
            }
            repos.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then(a.1.cmp(&b.1)));
            let entries: Vec<(String, String)> = repos.iter().flat_map(|(id, _, d, _)| d.iter().map(move |e| (id.clone(), e.clone()))).collect();
            let open_key = format!("git:{}", cleaner.id());
            let open = self.cleanup.open.contains(&open_key);
            let all = !entries.is_empty() && entries.iter().all(|e| self.cleanup.git_selected.contains(e));
            let title = t!(key, count = entries.len(), repos = repos.len()).to_string();
            body.push(self.group_header(("git-kind", k), &open_key, open, Some(all), Lucide::GitBranch, title, None, None, entries.clone(), cx));
            if !open {
                continue;
            }
            for (r, (id, repo, detail, running)) in repos.into_iter().enumerate() {
                let repo_key = format!("git:{id}");
                let repo_open = self.cleanup.open.contains(&repo_key);
                let mine: Vec<(String, String)> = detail.iter().map(|e| (id.clone(), e.clone())).collect();
                let repo_all = !mine.is_empty() && mine.iter().all(|e| self.cleanup.git_selected.contains(e));
                let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let sub = if running { t!("sys_running").to_string() } else { t!("sys_count", count = detail.len().to_string()).to_string() };
                body.push(
                    div()
                        .pl_6()
                        .child(self.group_header(("git-repo", k * 1000 + r), &repo_key, repo_open, (!running).then_some(repo_all), Lucide::Folder, name, Some(repo.display().to_string()), Some(sub), mine, cx))
                        .into_any_element(),
                );
                if repo_open {
                    for (e, entry) in detail.into_iter().enumerate() {
                        let pick = (id.clone(), entry.clone());
                        let ticked = self.cleanup.git_selected.contains(&pick);
                        let label = if cleaner == Cleaner::GitWorktrees { Path::new(&entry).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(entry.clone()) } else { entry.clone() };
                        body.push(
                            div()
                                .id(("git-entry", (k * 1000 + r) * 1000 + e))
                                .h_flex()
                                .gap_3()
                                .pl_16()
                                .py_1()
                                .child(Checkbox::new(("git-pick", (k * 1000 + r) * 1000 + e)).accessibility_label(label.clone()).checked(ticked).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    if *checked {
                                        this.cleanup.git_selected.insert(pick.clone());
                                    } else {
                                        this.cleanup.git_selected.remove(&pick);
                                    }
                                    cx.notify();
                                })))
                                .child(div().flex_1().min_w_0().truncate().child(label))
                                .when(cleaner == Cleaner::GitWorktrees, |d| d.child(div().text_xs().text_color(muted).truncate().max_w(px(380.)).child(entry.clone())))
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        card(border, bg)
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .child(Icon::new(Lucide::GitBranch).text_color(muted))
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("sys_git_name").to_string()))
                    .child(div().flex_1())
                    .child(Button::new("git-refresh").accessibility_label(t!("nav_refresh").to_string()).ghost().small().icon(Lucide::RefreshCw).tooltip(t!("nav_refresh").to_string()).on_click(cx.listener(|this, _, _, cx| {
                        this.app.check_system();
                        cx.notify();
                    }))),
            )
            .child(div().text_sm().text_color(muted).child(Self::git_status_text(core)))
            .when(!body.is_empty(), |d| d.child(div().v_flex().gap_1().children(body)))
            .child(div().text_xs().text_color(muted).child(t!("sys_git_branches_advice").to_string()))
            .into_any_element()
    }

    /// An expandable group row: chevron, optional select-all checkbox (`checked` = all ticked),
    /// icon, title, optional second line and right-hand text. `picks` are the Git entries it covers.
    #[allow(clippy::too_many_arguments)]
    fn group_header(&self, id: (&'static str, usize), key: &str, open: bool, checked: Option<bool>, icon: Lucide, title: String, sub: Option<String>, right: Option<String>, picks: Vec<(String, String)>, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let toggle = key.to_string();
        div()
            .id(id)
            .h_flex()
            .gap_3()
            .py_2()
            .cursor_pointer()
            .a11y_press(&self.focus_for(format!("group-{key}"), cx), title.clone(), cx.theme().ring)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.cleanup.open.remove(&toggle) {
                    this.cleanup.open.insert(toggle.clone());
                }
                cx.notify();
            }))
            .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).size_4().text_color(muted))
            .children(checked.map(|all| {
                Checkbox::new((id.0, id.1 + 500_000)).accessibility_label(title.clone()).checked(all).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    cx.stop_propagation();
                    for p in &picks {
                        if *checked {
                            this.cleanup.git_selected.insert(p.clone());
                        } else {
                            this.cleanup.git_selected.remove(p);
                        }
                    }
                    cx.notify();
                }))
            }))
            .child(Icon::new(icon).size_5().text_color(muted))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
                    .children(sub.map(|s| div().text_xs().text_color(muted).truncate().child(s))),
            )
            .children(right.map(|r| div().text_sm().text_color(muted).child(r)))
            .into_any_element()
    }

    fn system_row(&self, ix: usize, id: &str, item: &crate::state::SystemItem, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border, danger) = (t.muted_foreground, t.border, t.danger);
        let name = system_name(item.cleaner, item.repo.as_deref());
        let advice = t!(item.cleaner.advice_key()).to_string();
        let (status, runnable) = match &item.status {
            SystemStatus::Checking => (t!("sys_checking").to_string(), false),
            SystemStatus::Running => (t!("sys_running").to_string(), false),
            SystemStatus::Ready { estimate, .. } if estimate.is_nothing() => (t!("sys_nothing").to_string(), false),
            SystemStatus::Ready { estimate, .. } => (estimate_text(*estimate), true),
            SystemStatus::Unavailable { reason } => {
                let reason = if reason == NOT_INSTALLED { t!("sys_not_installed").to_string() } else { reason.clone() };
                (t!("sys_unavailable", reason = reason).to_string(), false)
            }
            // A failed item must be re-checked (refresh) before it can run again.
            SystemStatus::Failed { error } => {
                let error = if error == AUTOMATION_DENIED { t!("sys_automation_denied").to_string() } else { error.clone() };
                (t!("sys_failed", error = error).to_string(), false)
            }
        };
        let failed = matches!(item.status, SystemStatus::Failed { .. });
        let busy = matches!(item.status, SystemStatus::Checking | SystemStatus::Running);
        let id = id.to_string();
        div()
            .id(("sys", ix))
            .h_flex()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(name.clone()))
                    .child(div().text_sm().text_color(muted).child(advice))
                    .child(div().text_sm().when(failed, |d| d.text_color(danger)).child(status)),
            )
            .child(
                Button::new(("sys-run", ix))
                    .accessibility_label(format!("{} – {name}", t!("sys_run")))
                    .outline()
                    .small()
                    .loading(busy)
                    .disabled(!runnable)
                    .label(t!("sys_run").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.app.request_system_cleanup(&id, None, Origin::User);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

impl AppView {
    pub(super) fn cleanup(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border, bg) = (t.muted_foreground, t.border, t.background);
        let l = loc();
        // Only ticks on findings that still exist (and aren't "Keep") count.
        let selected: BTreeSet<PathBuf> = core
            .findings
            .iter()
            .filter(|f| f.safety != Safety::Keep && self.cleanup.selected.contains(&f.path))
            .map(|f| f.path.clone())
            .collect();
        let selected_size: u64 = core.findings.iter().filter(|f| selected.contains(&f.path)).map(|f| f.size).sum();
        let content: Vec<AnyElement> = if core.tree.is_none() {
            // No scan yet: point to Home; the system tools below work right away.
            vec![
                Empty::new()
                    .header(
                        EmptyHeader::new()
                            .media(EmptyMedia::new().with_variant(EmptyMediaVariant::Icon).child(Icon::new(Lucide::Search)))
                            .title(EmptyTitle::new().child(t!("cleanup_title").to_string()))
                            .description(EmptyDescription::new().child(t!("cleanup_scan_first").to_string())),
                    )
                    .content(EmptyContent::new().child(Button::new("go-home").primary().icon(Lucide::House).label(t!("nav_home").to_string()).on_click(cx.listener(|this, _, _, cx| {
                        this.app.show(Screen::Home);
                        cx.notify();
                    }))))
                    .into_any_element(),
            ]
        } else if core.findings.is_empty() {
            vec![
                Empty::new()
                    .header(
                        EmptyHeader::new()
                            .media(EmptyMedia::new().with_variant(EmptyMediaVariant::Icon).child(Icon::new(Lucide::Sparkles)))
                            .title(EmptyTitle::new().child(t!("cleanup_title").to_string()))
                            .description(EmptyDescription::new().child(t!("cleanup_empty").to_string())),
                    )
                    .content(EmptyContent::new().child(Button::new("go-map").primary().icon(Lucide::Map).label(t!("nav_map").to_string()).on_click(cx.listener(|this, _, _, cx| {
                        this.app.show(Screen::DiskMap);
                        cx.notify();
                    }))))
                    .into_any_element(),
            ]
        } else {
            let mut v = vec![heading(None, t!("cleanup_title").to_string(), t!("cleanup_subtitle").to_string(), muted).into_any_element()];
            for (group, (safety, key)) in [(Safety::Safe, "group_safe"), (Safety::Review, "group_review"), (Safety::Keep, "group_keep")].into_iter().enumerate() {
                let (items, total) = rules::by_safety(&core.findings, safety);
                if items.is_empty() {
                    continue;
                }
                let all: Vec<PathBuf> = items.iter().map(|f| f.path.clone()).collect();
                let tag = match safety {
                    Safety::Safe => Tag::success(),
                    Safety::Review => Tag::warning(),
                    Safety::Keep => Tag::danger(),
                };
                let all_ticked = safety != Safety::Keep && all.iter().all(|p| selected.contains(p));
                // Items of the same kind (e.g. 37 × node_modules) collapse into one expandable group.
                let mut rows: Vec<AnyElement> = Vec::new();
                for (g, members) in group_by_rule(&items).into_iter().enumerate() {
                    let base = group * 100_000 + g * 1000;
                    if members.len() == 1 {
                        rows.push(self.cleanup_row(base, members[0], safety, selected.contains(&members[0].path), false, cx));
                        continue;
                    }
                    let key = format!("{group}:{}", members[0].rule_id);
                    let open = self.cleanup.open.contains(&key);
                    rows.push(self.finding_group(base, &key, open, &members, safety, &selected, cx));
                    if open {
                        // A long group shows its biggest entries first; hundreds of rows at once make scrolling stutter.
                        let all_key = format!("{key}:all");
                        let shown = if self.cleanup.open.contains(&all_key) { members.len() } else { members.len().min(GROUP_PREVIEW) };
                        rows.extend(members[..shown].iter().enumerate().map(|(i, f)| div().pl_8().child(self.cleanup_row(base + i + 1, f, safety, selected.contains(&f.path), true, cx)).into_any_element()));
                        if shown < members.len() {
                            let rest = members.len() - shown;
                            rows.push(
                                div()
                                    .pl_8()
                                    .py_2()
                                    .child(Button::new(("show-more", base)).ghost().small().icon(Lucide::ChevronDown).label(t!("cleanup_show_more", count = rest).to_string()).on_click(cx.listener(move |this, _, _, cx| {
                                        this.cleanup.open.insert(all_key.clone());
                                        cx.notify();
                                    })))
                                    .into_any_element(),
                            );
                        }
                    }
                }
                let group_paths = all.clone();
                v.push(
                    card(border, bg)
                        .child(
                            div()
                                .h_flex()
                                .gap_3()
                                .when(safety != Safety::Keep, |d| {
                                    d.child(Checkbox::new(("pick-all", group)).accessibility_label(t!("cleanup_select_all").to_string()).checked(all_ticked).tooltip(t!("cleanup_select_all").to_string()).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                        for p in &group_paths {
                                            if *checked {
                                                this.cleanup.selected.insert(p.clone());
                                            } else {
                                                this.cleanup.selected.remove(p);
                                            }
                                        }
                                        cx.notify();
                                    })))
                                })
                                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!(key).to_string()))
                                .child(tag.child(format::size(total, &l)))
                                .child(div().flex_1())
                                .when(safety == Safety::Safe, |d| {
                                    d.child(Button::new("trash-all").primary().small().icon(Lucide::Trash).label(t!("cleanup_trash_all").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                                        let _ = this.app.request_trash(all.clone(), None, Origin::User);
                                        cx.notify();
                                    })))
                                }),
                        )
                        .child(div().v_flex().children(rows))
                        .into_any_element(),
                );
            }
            v
        };
        let mut content = content;
        if core.tree.is_some() {
            content.push(self.dupes_card(core, cx));
        }
        if let Some(apps) = self.apps_card(core, cx) {
            content.push(apps);
        }
        if let Some(old) = self.old_files_card(core, cx) {
            content.push(old);
        }
        if let Some(models) = self.models_card(core, cx) {
            content.push(models);
        }
        let system = self.system_card(core, cx);
        let git = self.git_card(core, cx);
        if core.system_first {
            content.insert(0, system);
            content.insert(1, git);
        } else {
            content.push(system);
            content.push(git);
        }
        // Git picks that still match the last check, grouped per repository item.
        let mut git_picks: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (id, entry) in &self.cleanup.git_selected {
            if core.system.get(id).is_some_and(|i| matches!(&i.status, SystemStatus::Ready { detail, .. } if detail.contains(entry))) {
                git_picks.entry(id.clone()).or_default().push(entry.clone());
            }
        }
        let count_of = |c: Cleaner| git_picks.iter().filter(|(id, _)| core.system.get(*id).is_some_and(|i| i.cleaner == c)).map(|(_, e)| e.len()).sum::<usize>();
        let (branches, worktrees) = (count_of(Cleaner::GitBranches), count_of(Cleaner::GitWorktrees));
        let picked: Vec<PathBuf> = selected.iter().cloned().collect();
        div()
            .v_flex()
            .size_full()
            .child(
                div()
                    .id("cleanup")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(div().v_flex().w_full().max_w(px(880.)).mx_auto().p_8().gap_5().children(content)),
            )
            // Action bar for the ticked items; removal still goes through the confirm dialog.
            .when(!picked.is_empty(), |d| {
                d.child(
                    div()
                        .h_flex()
                        .gap_3()
                        .px_8()
                        .py_3()
                        .border_t_1()
                        .border_color(border)
                        .bg(bg)
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t!("cleanup_selected", count = picked.len().to_string(), size = format::size(selected_size, &l)).to_string()))
                        .child(Button::new("clear-picked").outline().label(t!("cleanup_clear_selection").to_string()).on_click(cx.listener(|this, _, _, cx| {
                            this.cleanup.selected.clear();
                            cx.notify();
                        })))
                        .child(Button::new("trash-picked").danger().icon(Lucide::Trash).label(t!("cleanup_trash_selected").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                            let _ = this.app.request_trash(picked.clone(), None, Origin::User);
                            cx.notify();
                        }))),
                )
            })
            // Action bar for ticked Git branches / worktrees: one confirmation for all of them.
            .when(!git_picks.is_empty(), |d| {
                d.child(
                    div()
                        .h_flex()
                        .gap_3()
                        .px_8()
                        .py_3()
                        .border_t_1()
                        .border_color(border)
                        .bg(bg)
                        .child(Icon::new(Lucide::GitBranch))
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t!("git_selected_bar", branches = branches, worktrees = worktrees).to_string()))
                        .child(Button::new("git-clear").outline().label(t!("cleanup_clear_selection").to_string()).on_click(cx.listener(|this, _, _, cx| {
                            this.cleanup.git_selected.clear();
                            cx.notify();
                        })))
                        .child(Button::new("git-delete").danger().icon(Lucide::Trash).label(t!("git_delete_selected").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                            let picks = git_picks.iter().map(|(id, e)| (id.clone(), Some(e.clone()))).collect();
                            if this.app.request_system_batch(picks, None, Origin::User).is_ok() {
                                this.cleanup.git_selected.clear();
                            }
                            cx.notify();
                        }))),
                )
            })
            .into_any_element()
    }

    /// Header of a group of findings of one kind: chevron, select-all, name, count, total, Trash.
    #[allow(clippy::too_many_arguments)]
    fn finding_group(&self, id: usize, key: &str, open: bool, members: &[&Finding], safety: Safety, selected: &BTreeSet<PathBuf>, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border) = (t.muted_foreground, t.border);
        let paths: Vec<PathBuf> = members.iter().map(|f| f.path.clone()).collect();
        let total: u64 = members.iter().map(|f| f.size).sum();
        let all = paths.iter().all(|p| selected.contains(p));
        let (toggle, picks, trash) = (key.to_string(), paths.clone(), paths);
        div()
            .id(("finding-group", id))
            .h_flex()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(border)
            .cursor_pointer()
            .a11y_press(&self.focus_for(format!("finding-{key}"), cx), item_label(&group_title(members), &format::size(total, &loc())), cx.theme().ring)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.cleanup.open.remove(&toggle) {
                    this.cleanup.open.insert(toggle.clone());
                }
                cx.notify();
            }))
            .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).size_4().text_color(muted))
            .when(safety != Safety::Keep, |d| {
                d.child(Checkbox::new(("pick-group", id)).accessibility_label(item_label(&group_title(members), &format::size(total, &loc()))).checked(all).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    cx.stop_propagation();
                    for p in &picks {
                        if *checked {
                            this.cleanup.selected.insert(p.clone());
                        } else {
                            this.cleanup.selected.remove(p);
                        }
                    }
                    cx.notify();
                })))
            })
            .child(Icon::new(Lucide::Layers).size_5().text_color(muted))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().h_flex().gap_2().child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(group_title(members))).child(Tag::secondary().small().child(t!("cleanup_group_count", count = members.len()).to_string())))
                    .child(div().text_sm().child(t!(&members[0].message).to_string())),
            )
            .child(div().w(px(90.)).text_right().font_weight(FontWeight::SEMIBOLD).child(format::size(total, &loc())))
            .when(safety != Safety::Keep, |d| {
                d.child(Button::new(("trash-group", id)).accessibility_label(t!("cleanup_trash_group").to_string()).ghost().small().icon(Lucide::Trash).tooltip(t!("cleanup_trash_group").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let _ = this.app.request_trash(trash.clone(), None, Origin::User);
                    cx.notify();
                })))
            })
            .into_any_element()
    }

    /// `in_group`: the group header already says what it is, so the row names where it is instead.
    fn cleanup_row(&self, id: usize, f: &Finding, safety: Safety, ticked: bool, in_group: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border) = (t.muted_foreground, t.border);
        let name = if in_group {
            // "shop-api/frontend" for …/shop-api/frontend/node_modules
            let comps: Vec<String> = f.path.parent().into_iter().flat_map(|p| p.components().rev().take(2)).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            comps.into_iter().rev().collect::<Vec<_>>().join("/")
        } else {
            f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.path.display().to_string())
        };
        let parent = f.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        // ponytail: file vs folder guessed from the extension; good enough for an icon
        let icon = if f.path.extension().is_some() { Lucide::File } else { Lucide::Folder };
        let (show, trash, pick) = (f.path.clone(), f.path.clone(), f.path.clone());
        div()
            .id(("finding", id))
            .h_flex()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(border)
            .when(safety != Safety::Keep, |d| {
                d.child(Checkbox::new(("pick", id)).accessibility_label(name.clone()).checked(ticked).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if *checked {
                        this.cleanup.selected.insert(pick.clone());
                    } else {
                        this.cleanup.selected.remove(&pick);
                    }
                    cx.notify();
                })))
            })
            .child(Icon::new(icon).size_5().text_color(muted))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                    .child(div().text_xs().text_color(muted).truncate().child(parent))
                    .when(!in_group, |d| d.child(div().text_sm().child(t!(&f.message).to_string()))),
            )
            .child(div().w(px(90.)).text_right().font_weight(FontWeight::SEMIBOLD).child(format::size(f.size, &loc())))
            .child(Button::new(("show", id)).accessibility_label(t!("cleanup_show").to_string()).ghost().small().icon(Lucide::Map).tooltip(t!("cleanup_show").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.app.navigate(&show);
                cx.notify();
            })))
            .when(safety != Safety::Keep, |d| {
                d.child(Button::new(("trash", id)).accessibility_label(t!("details_trash").to_string()).ghost().small().icon(Lucide::Trash).tooltip(t!("details_trash").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.app.request_trash(vec![trash.clone()], None, Origin::User);
                    cx.notify();
                })))
            })
            .into_any_element()
    }
}

/// Entries an expanded group shows before "Show N more".
const GROUP_PREVIEW: usize = 50;

/// Findings grouped by the rule that found them, biggest group first (items keep their order).
pub(super) fn group_by_rule<'a>(items: &[&'a Finding]) -> Vec<Vec<&'a Finding>> {
    let mut groups: Vec<Vec<&Finding>> = Vec::new();
    for f in items {
        match groups.iter_mut().find(|g| g[0].rule_id == f.rule_id) {
            Some(g) => g.push(f),
            None => groups.push(vec![f]),
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|f| f.size).sum::<u64>()));
    groups
}

/// "node_modules" when all items share a name, else the first sentence of the advice.
pub(super) fn group_title(members: &[&Finding]) -> String {
    let name = |f: &Finding| f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let first = name(members[0]);
    if members.iter().all(|f| name(f) == first) {
        return first;
    }
    let message = t!(&members[0].message).to_string();
    message.split_terminator(['.', '。']).next().unwrap_or(&message).trim().to_string()
}

impl AppView {
    /// The kept copy of a group: the user's pick if it is still in the group, else the default.
    fn kept<'a>(&self, g: &'a Group, core: &Core) -> &'a Path {
        &g.copies[pick_keep(g, &self.cleanup.dupe_keep, &self.app.deps.home, &finding_dirs(core))].path
    }

    /// Every copy that would be removed, with the copy that stays.
    pub(super) fn dupe_removals(&self, core: &Core) -> Vec<Removal> {
        let Dupes::Done(groups) = &core.dupes else { return Vec::new() };
        crate::finders::dupes::removals(groups, &self.cleanup.dupe_keep, &self.app.deps.home, &finding_dirs(core))
    }

    fn dupes_card(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border, bg, success) = (t.muted_foreground, t.border, t.background, t.success);
        let l = loc();
        let title = div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("dupes_title").to_string());
        let find = |label: String, cx: &mut Context<Self>| Button::new("dupes-find").outline().icon(Lucide::Search).label(label).on_click(cx.listener(|this, _, _, cx| {
            this.cleanup.dupe_keep.clear();
            this.cleanup.dupes_shown = 50;
            let _ = this.app.start_dupes();
            cx.notify();
        }));
        let body = match &core.dupes {
            Dupes::Idle => div().v_flex().gap_3().child(div().text_sm().text_color(muted).child(t!("dupes_intro").to_string())).child(div().h_flex().child(find(t!("dupes_find").to_string(), cx))),
            Dupes::Running(p) => {
                let (done, total) = (p.files_done.load(Relaxed), p.files_total.load(Relaxed).max(1));
                div().v_flex().gap_2()
                    .child(Progress::new("dupes-progress").value(done as f32 / total as f32 * 100.))
                    .child(div().h_flex().gap_3().child(div().flex_1().text_sm().text_color(muted).child(t!("dupes_progress", done = format::count(done, &l), total = format::count(total, &l), read = format::size(p.bytes_read.load(Relaxed), &l)).to_string()))
                        .child(Button::new("dupes-cancel").ghost().small().label(t!("confirm_cancel").to_string()).on_click(cx.listener(|this, _, _, cx| {
                            this.app.cancel_dupes();
                            cx.notify();
                        }))))
            }
            Dupes::Done(groups) if groups.is_empty() => div().v_flex().gap_3().child(div().text_sm().child(t!("dupes_none").to_string())).child(div().h_flex().child(find(t!("dupes_again").to_string(), cx))),
            Dupes::Done(groups) => {
                let removals = self.dupe_removals(core);
                let freed: u64 = removals.iter().map(|r| r.size).sum();
                let mut list = div().v_flex().gap_3();
                for (gi, g) in groups.iter().take(self.cleanup.dupes_shown).enumerate() {
                    let kept = self.kept(g, core).to_path_buf();
                    let name = g.copies[0].path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let others: Vec<PathBuf> = g.copies.iter().map(|c| c.path.clone()).collect();
                    list = list.child(
                        div().v_flex().gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("dupes_group", name = name, count = g.copies.len().to_string(), size = format::size(g.wasted(), &l)).to_string()))
                            .children(g.copies.iter().enumerate().map(|(ci, c)| {
                                let is_kept = c.path == kept;
                                let (others, path) = (others.clone(), c.path.clone());
                                div().h_flex().gap_3().pl_4()
                                    .child(div().flex_1().min_w_0().truncate().text_sm().text_color(if is_kept { success } else { muted }).child(c.path.parent().map(|p| p.display().to_string()).unwrap_or_default()))
                                    .child(if is_kept {
                                        Tag::success().child(t!("dupes_keep").to_string()).into_any_element()
                                    } else {
                                        Button::new(SharedString::from(format!("dupe-keep-{gi}-{ci}"))).ghost().xsmall().label(t!("dupes_keep_this").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                                            // One pick per group: drop the group's earlier pick.
                                            for o in &others {
                                                this.cleanup.dupe_keep.remove(o);
                                            }
                                            this.cleanup.dupe_keep.insert(path.clone());
                                            cx.notify();
                                        })).into_any_element()
                                    })
                            })),
                    );
                }
                let more = groups.len().saturating_sub(self.cleanup.dupes_shown);
                let any = !removals.is_empty();
                div().v_flex().gap_3()
                    .child(div().h_flex().gap_3().child(div().flex_1().text_sm().child(t!("dupes_summary", groups = groups.len().to_string(), size = format::size(freed, &l)).to_string())).child(find(t!("dupes_again").to_string(), cx)))
                    .child(list)
                    .when(more > 0, |d| d.child(div().child(Button::new("dupes-more").ghost().small().label(t!("cleanup_show_more", count = more.min(50).to_string()).to_string()).on_click(cx.listener(|this, _, _, cx| {
                        this.cleanup.dupes_shown += 50;
                        cx.notify();
                    })))))
                    .child(div().h_flex().justify_end().child(Button::new("dupes-trash").danger().icon(Lucide::Trash).label(t!("dupes_trash", size = format::size(freed, &l)).to_string()).disabled(!any || core.pending.is_some()).on_click(cx.listener(move |this, _, _, cx| {
                        let removals = this.dupe_removals(&this.app.core.lock().unwrap());
                        // Re-checked on the click: the kept copy must still exist, the copy be unchanged.
                        let paths: Vec<PathBuf> = removals.into_iter().filter(Removal::still_safe).map(|r| r.path).collect();
                        if !paths.is_empty() {
                            let _ = this.app.request_trash(paths, None, Origin::User);
                        }
                        cx.notify();
                    }))))
            }
        };
        card(border, bg).child(title).child(body).into_any_element()
    }
}

/// Folders and files the cleanup suggests removing: a copy there is never the one kept.
pub(super) fn finding_dirs(core: &Core) -> Vec<PathBuf> {
    core.findings.iter().filter(|f| f.safety != Safety::Keep).map(|f| f.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(path: &str, size: u64, rule: &str) -> Finding {
        Finding { path: PathBuf::from(path), size, rule_id: rule.into(), safety: Safety::Safe, message: "msg_x".into() }
    }

    #[::core::prelude::v1::test]
    fn findings_group_by_rule_biggest_group_first() {
        let all = [finding("/a/node_modules", 50, "node_modules"), finding("/c/cache", 40, "caches"), finding("/b/node_modules", 30, "node_modules"), finding("/d/x.dmg", 5, "installer")];
        let refs: Vec<&Finding> = all.iter().collect();
        let groups = group_by_rule(&refs);
        let ids: Vec<(&str, usize)> = groups.iter().map(|g| (g[0].rule_id.as_str(), g.len())).collect();
        assert_eq!(ids, [("node_modules", 2), ("caches", 1), ("installer", 1)]);
        assert_eq!(group_title(&groups[0]), "node_modules");
    }
}
