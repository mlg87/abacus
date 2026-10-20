use std::collections::{HashMap, HashSet};

use ratatui::style::Color;
use serde::{Deserialize, Deserializer};

/// `bd` omits optional fields instead of emitting null, but older rows have been
/// seen with explicit nulls; both collapse to the type's default.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub dep_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bead {
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_default")]
    pub status: Status,
    #[serde(default, deserialize_with = "null_default")]
    pub priority: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub issue_type: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub labels: Vec<String>,
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub dependencies: Vec<Dependency>,
    #[serde(default, deserialize_with = "null_default")]
    pub notes: String,
    #[serde(default, deserialize_with = "null_default")]
    pub acceptance_criteria: String,
    #[serde(default, deserialize_with = "null_default")]
    pub created_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub close_reason: Option<String>,
}

/// Status as the user cares about it. Stricter than `bd ready`, which only checks a
/// bead's own `blocks` edges and so reports an epic as ready while every bead under
/// it is still waiting on upstream work; dispatching against such an epic is wasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derived {
    Ready,
    Blocked,
    InProgress,
    Deferred,
    Closed,
    Other,
}

impl Derived {
    pub fn glyph(self) -> &'static str {
        match self {
            Derived::Ready => "○",
            Derived::Blocked => "●",
            Derived::InProgress => "◐",
            Derived::Deferred => "❄",
            Derived::Closed => "✓",
            Derived::Other => "?",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Derived::Ready => Color::Green,
            Derived::Blocked => Color::Red,
            Derived::InProgress => Color::Yellow,
            Derived::Deferred => Color::Blue,
            Derived::Closed => Color::DarkGray,
            Derived::Other => Color::Magenta,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Derived::Ready => "ready",
            Derived::Blocked => "blocked",
            Derived::InProgress => "in progress",
            Derived::Deferred => "deferred",
            Derived::Closed => "closed",
            Derived::Other => "other",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Derived::InProgress => 0,
            Derived::Ready => 1,
            Derived::Blocked => 2,
            Derived::Deferred => 3,
            Derived::Other => 4,
            Derived::Closed => 5,
        }
    }
}

/// Why a bead derives as `Blocked`, so the UI and dispatch can name the cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedBy {
    /// The bead's own status is `blocked`.
    Status,
    /// Open `blocks` targets of the bead itself.
    Beads(Vec<usize>),
    /// Nearest ancestor that is itself gated by status or open blockers; its
    /// dependencies hold back everything beneath it.
    Ancestor(usize),
    /// The bead has open children but none of them is ready or in progress.
    NoReadyChild,
}

/// Parses the `gh-<N>` external ref the GitHub mirror stamps on epic beads.
pub fn gh_number(b: &Bead) -> Option<u32> {
    let digits = b.external_ref.as_deref()?.strip_prefix("gh-")?;
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[derive(Debug, Default)]
pub struct Graph {
    pub beads: Vec<Bead>,
    pub index: HashMap<String, usize>,
    pub parent: Vec<Option<usize>>,
    pub children: Vec<Vec<usize>>,
    pub blockers: Vec<Vec<usize>>,
    pub dependents: Vec<Vec<usize>>,
    pub other_links: Vec<Vec<(String, usize)>>,
    pub roots: Vec<usize>,
    derived: Vec<Derived>,
}

impl Graph {
    pub fn build(beads: Vec<Bead>) -> Graph {
        let n = beads.len();
        let index: HashMap<String, usize> = beads
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.clone(), i))
            .collect();
        let mut g = Graph {
            index,
            parent: vec![None; n],
            children: vec![Vec::new(); n],
            blockers: vec![Vec::new(); n],
            dependents: vec![Vec::new(); n],
            other_links: vec![Vec::new(); n],
            roots: Vec::new(),
            beads,
            derived: vec![Derived::Other; n],
        };

        for i in 0..n {
            let b = &g.beads[i];
            let from_field = b.parent.as_ref().and_then(|p| g.index.get(p).copied());
            let from_dep = || {
                b.dependencies
                    .iter()
                    .filter(|d| d.dep_type == "parent-child")
                    .find_map(|d| g.index.get(&d.depends_on_id).copied())
            };
            g.parent[i] = from_field.or_else(from_dep).filter(|&p| p != i);

            for d in &b.dependencies {
                let Some(&j) = g.index.get(&d.depends_on_id) else {
                    continue;
                };
                if j == i {
                    continue;
                }
                match d.dep_type.as_str() {
                    "parent-child" => {}
                    "blocks" => {
                        if !g.blockers[i].contains(&j) {
                            g.blockers[i].push(j);
                            g.dependents[j].push(i);
                        }
                    }
                    other => g.other_links[i].push((other.to_string(), j)),
                }
            }
        }

        g.break_parent_cycles();
        for i in 0..n {
            match g.parent[i] {
                Some(p) => g.children[p].push(i),
                None => g.roots.push(i),
            }
        }
        g.derive_all();
        let mut children = std::mem::take(&mut g.children);
        for c in &mut children {
            g.sort(c);
        }
        g.children = children;
        let mut roots = std::mem::take(&mut g.roots);
        g.sort(&mut roots);
        g.roots = roots;
        let mut blockers = std::mem::take(&mut g.blockers);
        for b in &mut blockers {
            g.sort(b);
        }
        g.blockers = blockers;
        let mut dependents = std::mem::take(&mut g.dependents);
        for d in &mut dependents {
            g.sort(d);
        }
        g.dependents = dependents;
        g
    }

    /// A parent cycle would make its members unreachable from any root and so
    /// invisible in the tree; cut each cycle at the node where it is detected.
    fn break_parent_cycles(&mut self) {
        let n = self.beads.len();
        // 0 = unvisited, 1 = on current walk, 2 = done
        let mut state = vec![0u8; n];
        for start in 0..n {
            let mut path = Vec::new();
            let mut cur = Some(start);
            while let Some(i) = cur {
                match state[i] {
                    2 => break,
                    1 => {
                        self.parent[i] = None;
                        break;
                    }
                    _ => {
                        state[i] = 1;
                        path.push(i);
                        cur = self.parent[i];
                    }
                }
            }
            for i in path {
                state[i] = 2;
            }
        }
    }

    /// Children must be derived before their parent (roll-up), and gating is read
    /// off ancestors' own edges, so a post-order walk of the parent forest suffices.
    fn derive_all(&mut self) {
        let mut stack: Vec<(usize, bool)> = self.roots.iter().map(|&r| (r, false)).collect();
        while let Some((i, children_done)) = stack.pop() {
            if children_done {
                self.derived[i] = self.derive_one(i);
            } else {
                stack.push((i, true));
                stack.extend(self.children[i].iter().map(|&c| (c, false)));
            }
        }
    }

    fn derive_one(&self, i: usize) -> Derived {
        match self.beads[i].status {
            Status::Open => {}
            Status::InProgress => return Derived::InProgress,
            Status::Blocked => return Derived::Blocked,
            Status::Deferred => return Derived::Deferred,
            Status::Closed => return Derived::Closed,
            Status::Other => return Derived::Other,
        }
        if self.has_open_blocker(i) || self.gated_ancestor(i).is_some() {
            return Derived::Blocked;
        }
        let mut open_children = self.children[i]
            .iter()
            .map(|&c| self.derived[c])
            .filter(|&d| d != Derived::Closed)
            .peekable();
        if open_children.peek().is_none() {
            return Derived::Ready;
        }
        let mut any_in_progress = false;
        for d in open_children {
            match d {
                Derived::Ready => return Derived::Ready,
                Derived::InProgress => any_in_progress = true,
                _ => {}
            }
        }
        if any_in_progress {
            Derived::InProgress
        } else {
            Derived::Blocked
        }
    }

    fn has_open_blocker(&self, i: usize) -> bool {
        self.blockers[i]
            .iter()
            .any(|&j| self.beads[j].status != Status::Closed)
    }

    fn gated_ancestor(&self, i: usize) -> Option<usize> {
        self.ancestors(i)
            .into_iter()
            .find(|&a| self.beads[a].status == Status::Blocked || self.has_open_blocker(a))
    }

    pub fn sort(&self, v: &mut [usize]) {
        v.sort_by(|&a, &b| self.sort_key(a).cmp(&self.sort_key(b)));
    }

    pub fn sort_key(&self, i: usize) -> (u8, i32, &str, &str) {
        let b = &self.beads[i];
        (self.derived(i).rank(), b.priority, &b.created_at, &b.id)
    }

    pub fn derived(&self, i: usize) -> Derived {
        self.derived[i]
    }

    /// `None` unless `i` derives as `Blocked`.
    pub fn blocked_by(&self, i: usize) -> Option<BlockedBy> {
        if self.derived[i] != Derived::Blocked {
            return None;
        }
        if self.beads[i].status == Status::Blocked {
            return Some(BlockedBy::Status);
        }
        let open = self.open_blockers(i);
        if !open.is_empty() {
            return Some(BlockedBy::Beads(open));
        }
        Some(match self.gated_ancestor(i) {
            Some(a) => BlockedBy::Ancestor(a),
            None => BlockedBy::NoReadyChild,
        })
    }

    /// Human-readable cause, shared by the detail pane and dispatch refusals.
    pub fn blocked_reason(&self, i: usize) -> Option<String> {
        Some(match self.blocked_by(i)? {
            BlockedBy::Status => "blocked (status)".into(),
            BlockedBy::Beads(js) => {
                let ids: Vec<&str> = js.iter().map(|&j| self.beads[j].id.as_str()).collect();
                format!("blocked by {}", ids.join(", "))
            }
            BlockedBy::Ancestor(a) => format!("blocked via ancestor {}", self.beads[a].id),
            BlockedBy::NoReadyChild => "blocked: no open child is ready".into(),
        })
    }

    pub fn get(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn is_epic(&self, i: usize) -> bool {
        self.beads[i].issue_type == "epic"
    }

    pub fn open_blockers(&self, i: usize) -> Vec<usize> {
        self.blockers[i]
            .iter()
            .copied()
            .filter(|&j| self.beads[j].status != Status::Closed)
            .collect()
    }

    /// Nearest bead (self first, then up the parent chain) carrying a GitHub issue number.
    pub fn gh_ancestor(&self, i: usize) -> Option<(usize, u32)> {
        std::iter::once(i)
            .chain(self.ancestors(i))
            .find_map(|j| gh_number(&self.beads[j]).map(|n| (j, n)))
    }

    /// Parent chain, nearest first.
    pub fn ancestors(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([i]);
        let mut cur = self.parent[i];
        while let Some(p) = cur {
            if !seen.insert(p) {
                break;
            }
            out.push(p);
            cur = self.parent[p];
        }
        out
    }

    /// Every bead below `i`, in pre-order; excludes `i`.
    pub fn descendants(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([i]);
        let mut stack: Vec<usize> = self.children[i].iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            out.push(c);
            stack.extend(self.children[c].iter().rev().copied());
        }
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn bead(id: &str, status: &str) -> Bead {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("title {id}"), "status": status,
            "priority": 2, "issue_type": "task", "created_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    /// `_from` is the dependent bead; kept so call sites read as edges.
    pub fn dep(_from: &str, to: &str, ty: &str) -> Dependency {
        Dependency {
            depends_on_id: to.into(),
            dep_type: ty.into(),
        }
    }

    #[test]
    fn open_bead_is_blocked_until_its_blocker_closes() {
        let mut a = bead("a", "open");
        a.dependencies.push(dep("a", "b", "blocks"));
        let g = Graph::build(vec![a.clone(), bead("b", "open")]);
        assert_eq!(g.derived(g.get("a").unwrap()), Derived::Blocked);

        let g = Graph::build(vec![a, bead("b", "closed")]);
        assert_eq!(g.derived(g.get("a").unwrap()), Derived::Ready);
    }

    fn under(id: &str, status: &str, parent: &str) -> Bead {
        let mut b = bead(id, status);
        b.parent = Some(parent.into());
        b
    }

    fn blocked_on(mut b: Bead, on: &str) -> Bead {
        b.dependencies.push(dep(&b.id.clone(), on, "blocks"));
        b
    }

    /// The dispatch-against-unworkable-epic case: `bd ready` lists the epic because
    /// it has no blockers of its own, yet nothing beneath it can start.
    #[test]
    fn parent_whose_open_children_are_all_blocked_is_blocked() {
        let g = Graph::build(vec![
            bead("e", "open"),
            blocked_on(under("c1", "open", "e"), "up"),
            under("c2", "closed", "e"),
            bead("up", "open"),
        ]);
        let e = g.get("e").unwrap();
        assert_eq!(g.derived(e), Derived::Blocked);
        assert_eq!(g.blocked_by(e), Some(BlockedBy::NoReadyChild));
    }

    #[test]
    fn one_ready_descendant_makes_every_ancestor_ready() {
        let g = Graph::build(vec![
            bead("e", "open"),
            under("sub", "open", "e"),
            under("leaf", "open", "sub"),
            blocked_on(under("stuck", "open", "e"), "up"),
            bead("up", "open"),
        ]);
        assert_eq!(g.derived(g.get("sub").unwrap()), Derived::Ready);
        assert_eq!(g.derived(g.get("e").unwrap()), Derived::Ready);
    }

    #[test]
    fn parent_with_only_in_progress_work_left_reads_in_progress() {
        let g = Graph::build(vec![
            bead("e", "open"),
            under("busy", "in_progress", "e"),
            blocked_on(under("stuck", "open", "e"), "up"),
            bead("up", "open"),
        ]);
        assert_eq!(g.derived(g.get("e").unwrap()), Derived::InProgress);
    }

    #[test]
    fn children_inherit_an_ancestors_open_blockers() {
        let g = Graph::build(vec![
            blocked_on(bead("e", "open"), "up"),
            under("sub", "open", "e"),
            under("leaf", "open", "sub"),
            bead("up", "open"),
        ]);
        let leaf = g.get("leaf").unwrap();
        assert_eq!(g.derived(leaf), Derived::Blocked);
        assert_eq!(
            g.blocked_by(leaf),
            Some(BlockedBy::Ancestor(g.get("e").unwrap()))
        );

        let g = Graph::build(vec![
            blocked_on(bead("e", "open"), "up"),
            under("leaf", "open", "e"),
            bead("up", "closed"),
        ]);
        assert_eq!(g.derived(g.get("leaf").unwrap()), Derived::Ready);
        assert_eq!(g.derived(g.get("e").unwrap()), Derived::Ready);
    }

    #[test]
    fn parent_resolves_from_field_and_from_parent_child_dependency() {
        let mut via_field = bead("c1", "open");
        via_field.parent = Some("p".into());
        let mut via_dep = bead("c2", "open");
        via_dep.dependencies.push(dep("c2", "p", "parent-child"));
        let g = Graph::build(vec![bead("p", "open"), via_field, via_dep]);
        let p = g.get("p").unwrap();
        assert_eq!(g.parent[g.get("c1").unwrap()], Some(p));
        assert_eq!(g.parent[g.get("c2").unwrap()], Some(p));
        assert_eq!(g.children[p].len(), 2);
        assert_eq!(g.roots, vec![p]);
    }

    #[test]
    fn dangling_ids_are_ignored() {
        let mut a = bead("a", "open");
        a.parent = Some("ghost".into());
        a.dependencies.push(dep("a", "nobody", "blocks"));
        let g = Graph::build(vec![a]);
        assert_eq!(g.parent[0], None);
        assert!(g.blockers[0].is_empty());
        assert_eq!(g.derived(0), Derived::Ready);
        assert_eq!(g.roots, vec![0]);
    }

    #[test]
    fn parent_cycle_keeps_every_member_reachable() {
        let mut a = bead("a", "open");
        a.parent = Some("b".into());
        let mut b = bead("b", "open");
        b.parent = Some("a".into());
        let g = Graph::build(vec![a, b]);
        assert_eq!(g.roots.len(), 1);
        assert_eq!(g.descendants(g.roots[0]).len(), 1);
    }

    #[test]
    fn deserializes_absent_and_null_fields() {
        let b: Bead = serde_json::from_str(
            r#"{"id":"x","status":"weird","labels":null,"external_ref":"gh-12"}"#,
        )
        .unwrap();
        assert_eq!(b.status, Status::Other);
        assert!(b.labels.is_empty());
        assert_eq!(gh_number(&b), Some(12));
    }
}
