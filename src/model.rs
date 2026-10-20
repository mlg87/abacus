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

/// Status as the user cares about it: `Open` splits into `Ready`/`Blocked` the same
/// way `bd ready` does (every `blocks` target closed).
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
        let mut children = std::mem::take(&mut g.children);
        for c in &mut children {
            g.sort(c);
        }
        g.children = children;
        let mut roots = std::mem::take(&mut g.roots);
        g.sort(&mut roots);
        g.roots = roots;
        // Clone, not take: sort_key -> derived() reads self.blockers.
        let mut blockers = g.blockers.clone();
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

    pub fn sort(&self, v: &mut [usize]) {
        v.sort_by(|&a, &b| self.sort_key(a).cmp(&self.sort_key(b)));
    }

    pub fn sort_key(&self, i: usize) -> (u8, i32, &str, &str) {
        let b = &self.beads[i];
        (self.derived(i).rank(), b.priority, &b.created_at, &b.id)
    }

    pub fn derived(&self, i: usize) -> Derived {
        match self.beads[i].status {
            Status::Open => {
                if self.blockers[i]
                    .iter()
                    .all(|&j| self.beads[j].status == Status::Closed)
                {
                    Derived::Ready
                } else {
                    Derived::Blocked
                }
            }
            Status::InProgress => Derived::InProgress,
            Status::Blocked => Derived::Blocked,
            Status::Deferred => Derived::Deferred,
            Status::Closed => Derived::Closed,
            Status::Other => Derived::Other,
        }
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
