//! Cached, single-pass filtering of the projects list by name.
//!
//! `App::visible_projects` used to lowercase every project name on each
//! call, was rebuilt 1–3× per frame, and every caller re-scanned the result
//! with `position()` / `contains()`. This module owns the seam between the
//! project list and its consumers: lowercased names are cached and only
//! rebuilt when the project names change, the visible set is cached per
//! normalized query, and one call returns visible indices plus the selected
//! position so navigation never rescans. The pure helpers (`move_within`,
//! `snap`, `clamp`, `find_project_index`, `name_set`) let `App` stay a thin
//! delegate with unchanged signatures.

use crate::config::ProjectConfig;
use std::cell::RefCell;
use std::collections::HashSet;

/// Snapshot of everything needed to answer visibility queries.
#[derive(Debug, Default)]
struct FilterCache {
    /// Exact project names the cache was built from (invalidation key).
    names: Vec<String>,
    /// Lowercased names, rebuilt only when `names` changes.
    lower: Vec<String>,
    /// Normalized query the visible set was built from.
    query: String,
    /// Cached visible indices for `query`.
    visible: Vec<usize>,
}

/// Name filter with cached lowercase names and cached visible set.
#[derive(Debug, Default)]
pub struct ProjectFilter {
    cache: RefCell<FilterCache>,
}

impl ProjectFilter {
    /// Empty filter (no cached state).
    pub fn new() -> Self {
        Self::default()
    }

    /// Trimmed, lowercased query used as the cache key.
    fn normalize(raw_query: &str) -> String {
        raw_query.trim().to_lowercase()
    }

    /// Refresh the cache when the project names or the query changed.
    /// The rebuild is a single pass over the cached lowercase names.
    fn refresh(&self, projects: &[ProjectConfig], raw_query: &str) {
        let mut cache = self.cache.borrow_mut();
        let names_changed = cache.names.len() != projects.len()
            || cache
                .names
                .iter()
                .zip(projects.iter())
                .any(|(cached, p)| cached != &p.name);
        if names_changed {
            cache.names = projects.iter().map(|p| p.name.clone()).collect();
            cache.lower = cache.names.iter().map(|n| n.to_lowercase()).collect();
        }
        let query = Self::normalize(raw_query);
        if names_changed || cache.query != query {
            let visible: Vec<usize> = if query.is_empty() {
                (0..projects.len()).collect()
            } else {
                cache
                    .lower
                    .iter()
                    .enumerate()
                    .filter(|(_, lowered)| lowered.contains(&query))
                    .map(|(i, _)| i)
                    .collect()
            };
            cache.query = query;
            cache.visible = visible;
        }
    }

    /// Project indices that survive the current name filter.
    pub fn visible(&self, projects: &[ProjectConfig], raw_query: &str) -> Vec<usize> {
        self.refresh(projects, raw_query);
        self.cache.borrow().visible.clone()
    }

    /// Visible indices plus the position of `selected_idx` within them
    /// (`None` when the selection is filtered out), from one cache lookup.
    pub fn visible_and_selected(
        &self,
        projects: &[ProjectConfig],
        raw_query: &str,
        selected_idx: usize,
    ) -> (Vec<usize>, Option<usize>) {
        self.refresh(projects, raw_query);
        let cache = self.cache.borrow();
        let mut pos = None;
        for (p, &i) in cache.visible.iter().enumerate() {
            if i == selected_idx {
                pos = Some(p);
                break;
            }
        }
        (cache.visible.clone(), pos)
    }

    /// Next selection moving `delta` within the filtered list, snapping to
    /// the nearest visible project when the current one is filtered out.
    pub fn move_within(
        &self,
        projects: &[ProjectConfig],
        raw_query: &str,
        current_idx: usize,
        delta: isize,
    ) -> usize {
        let (visible, pos) = self.visible_and_selected(projects, raw_query, current_idx);
        if visible.is_empty() {
            return current_idx;
        }
        match pos {
            Some(p) => visible[(p as isize + delta).clamp(0, visible.len() as isize - 1) as usize],
            None => {
                if delta < 0 {
                    visible[visible.len() - 1]
                } else {
                    visible[0]
                }
            }
        }
    }

    /// Replacement selection: the first visible project when the current
    /// one is filtered out, otherwise the current index unchanged.
    pub fn snap(&self, projects: &[ProjectConfig], raw_query: &str, current_idx: usize) -> usize {
        let (visible, pos) = self.visible_and_selected(projects, raw_query, current_idx);
        if !visible.is_empty() && pos.is_none() {
            visible[0]
        } else {
            current_idx
        }
    }

    /// Adjusted scroll keeping the selected row inside the visible window.
    pub fn clamp(
        &self,
        projects: &[ProjectConfig],
        raw_query: &str,
        selected_idx: usize,
        scroll: usize,
        viewport_rows: usize,
    ) -> usize {
        if viewport_rows == 0 {
            return scroll;
        }
        let (_, pos) = self.visible_and_selected(projects, raw_query, selected_idx);
        let Some(p) = pos else {
            return 0;
        };
        if p < scroll {
            p
        } else if p >= scroll + viewport_rows {
            p + 1 - viewport_rows
        } else {
            scroll
        }
    }

    /// Single-pass project lookup: exact name match wins, otherwise the
    /// first case-insensitive match. Replaces the old double `position()`
    /// scan with identical preference order.
    pub fn find_project_index(projects: &[ProjectConfig], name: &str) -> Option<usize> {
        let mut ci_match = None;
        for (i, p) in projects.iter().enumerate() {
            if p.name == name {
                return Some(i);
            }
            if ci_match.is_none() && p.name.eq_ignore_ascii_case(name) {
                ci_match = Some(i);
            }
        }
        ci_match
    }

    /// Borrowed name set for call sites that probe repeatedly
    /// (duplicate-name loop, restore membership checks).
    pub fn name_set(projects: &[ProjectConfig]) -> HashSet<&str> {
        projects.iter().map(|p| p.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionType, Engine};

    fn project(name: &str) -> ProjectConfig {
        ProjectConfig {
            name: name.to_string(),
            engine: Engine::Postgres,
            connection_type: ConnectionType::Ssh,
            ssh_connection: String::new(),
            db_url: String::new(),
            db_host: String::new(),
            db_port: String::new(),
            db_name: String::new(),
            db_user: String::new(),
            db_path: String::new(),
            cf_account_id: String::new(),
            cf_database_id: String::new(),
            last_opened: 0,
        }
    }

    fn projects() -> Vec<ProjectConfig> {
        vec![
            project("Alpha"),
            project("beta"),
            project("ALPINE"),
            project("gamma"),
        ]
    }

    #[test]
    fn empty_query_shows_everything() {
        let filter = ProjectFilter::new();
        let all = projects();
        assert_eq!(filter.visible(&all, ""), vec![0, 1, 2, 3]);
        assert_eq!(filter.visible(&all, "   "), vec![0, 1, 2, 3]);
    }

    #[test]
    fn matching_is_case_insensitive_substring() {
        let filter = ProjectFilter::new();
        let all = projects();
        assert_eq!(filter.visible(&all, "alp"), vec![0, 2]);
        assert_eq!(filter.visible(&all, "  BETA "), vec![1]);
        assert!(filter.visible(&all, "zzz").is_empty());
    }

    #[test]
    fn cache_rebuilds_only_when_names_or_query_change() {
        let filter = ProjectFilter::new();
        let all = projects();
        assert_eq!(filter.visible(&all, "alp"), vec![0, 2]);
        // Same inputs hit the cache.
        assert_eq!(filter.visible(&all, "alp"), vec![0, 2]);
        // New query recomputes from the cached lowercase names.
        assert_eq!(filter.visible(&all, "beta"), vec![1]);
        // Renamed projects invalidate the lowercase cache.
        let mut renamed = all.clone();
        renamed[0].name = "delta".to_string();
        assert_eq!(filter.visible(&renamed, "alp"), vec![2]);
        assert_eq!(filter.visible(&renamed, ""), vec![0, 1, 2, 3]);
    }

    #[test]
    fn visible_and_selected_reports_position() {
        let filter = ProjectFilter::new();
        let all = projects();
        let (visible, pos) = filter.visible_and_selected(&all, "alp", 2);
        assert_eq!(visible, vec![0, 2]);
        assert_eq!(pos, Some(1));
        let (_, hidden) = filter.visible_and_selected(&all, "alp", 1);
        assert_eq!(hidden, None);
    }

    #[test]
    fn move_within_clamps_and_snaps() {
        let filter = ProjectFilter::new();
        let all = projects();
        // Within the filtered list.
        assert_eq!(filter.move_within(&all, "alp", 0, 1), 2);
        assert_eq!(filter.move_within(&all, "alp", 2, 1), 2);
        assert_eq!(filter.move_within(&all, "alp", 2, -1), 0);
        // Filtered-out selection snaps by delta direction.
        assert_eq!(filter.move_within(&all, "alp", 1, 1), 0);
        assert_eq!(filter.move_within(&all, "alp", 1, -1), 2);
        // Empty visible set keeps the selection.
        assert_eq!(filter.move_within(&all, "zzz", 1, 1), 1);
    }

    #[test]
    fn snap_jumps_to_first_visible_only_when_hidden() {
        let filter = ProjectFilter::new();
        let all = projects();
        assert_eq!(filter.snap(&all, "alp", 1), 0);
        assert_eq!(filter.snap(&all, "alp", 2), 2);
        assert_eq!(filter.snap(&all, "", 3), 3);
        assert_eq!(filter.snap(&all, "zzz", 1), 1);
    }

    #[test]
    fn clamp_keeps_selection_in_window() {
        let filter = ProjectFilter::new();
        let all = projects();
        // Already visible: unchanged.
        assert_eq!(filter.clamp(&all, "", 1, 0, 3), 0);
        // Below the window: scroll down.
        assert_eq!(filter.clamp(&all, "", 3, 0, 2), 2);
        // Above the window: scroll up.
        assert_eq!(filter.clamp(&all, "", 0, 2, 2), 0);
        // Zero viewport: untouched.
        assert_eq!(filter.clamp(&all, "", 3, 5, 0), 5);
        // Filtered-out selection resets.
        assert_eq!(filter.clamp(&all, "alp", 1, 4, 2), 0);
    }

    #[test]
    fn find_project_index_prefers_exact_then_ci() {
        let all = projects();
        assert_eq!(ProjectFilter::find_project_index(&all, "beta"), Some(1));
        assert_eq!(ProjectFilter::find_project_index(&all, "BETA"), Some(1));
        // Exact match wins even when a ci match comes first.
        let mut dup = all.clone();
        dup.insert(
            0,
            ProjectConfig {
                name: "ALPHA".to_string(),
                ..project("ALPHA")
            },
        );
        assert_eq!(ProjectFilter::find_project_index(&dup, "Alpha"), Some(1));
        assert_eq!(ProjectFilter::find_project_index(&dup, "missing"), None);
        assert_eq!(ProjectFilter::find_project_index(&[], "x"), None);
    }

    #[test]
    fn name_set_covers_all_projects() {
        let all = projects();
        let set = ProjectFilter::name_set(&all);
        assert_eq!(set.len(), 4);
        assert!(set.contains("Alpha"));
        assert!(!set.contains("alpha"));
    }
}
