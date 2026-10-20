use std::collections::{HashSet, VecDeque};

use crate::model::{Derived, Graph};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    Open,
    Ready,
    InProgress,
    All,
}

impl StatusFilter {
    pub fn next(self) -> Self {
        match self {
            StatusFilter::Open => StatusFilter::Ready,
            StatusFilter::Ready => StatusFilter::InProgress,
            StatusFilter::InProgress => StatusFilter::All,
            StatusFilter::All => StatusFilter::Open,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StatusFilter::Open => "Open",
            StatusFilter::Ready => "Ready",
            StatusFilter::InProgress => "InProgress",
            StatusFilter::All => "All",
        }
    }

    fn accepts(self, d: Derived) -> bool {
        match self {
            StatusFilter::Open => d != Derived::Closed,
            StatusFilter::Ready => d == Derived::Ready,
            StatusFilter::InProgress => d == Derived::InProgress,
            StatusFilter::All => true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub status: StatusFilter,
    /// Always lowercased.
    pub query: String,
    pub label: Option<String>,
}

impl Filter {
    pub fn matches(&self, g: &Graph, i: usize) -> bool {
        let b = &g.beads[i];
        self.status.accepts(g.derived(i))
            && (self.query.is_empty()
                || b.id.to_lowercase().contains(&self.query)
                || b.title.to_lowercase().contains(&self.query))
            && self.label.as_ref().is_none_or(|l| b.labels.contains(l))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub idx: usize,
    pub depth: u16,
    pub has_visible_children: bool,
    pub expanded: bool,
    /// Shown only because a descendant matches; rendered dimmed.
    pub context_only: bool,
}

pub fn flatten(g: &Graph, filter: &Filter, expanded: &HashSet<String>) -> Vec<Row> {
    let n = g.beads.len();
    let matches: Vec<bool> = (0..n).map(|i| filter.matches(g, i)).collect();
    let mut visible = vec![false; n];
    // Post-order over the (acyclic, see Graph::break_parent_cycles) forest.
    let mut stack: Vec<(usize, bool)> = g.roots.iter().map(|&r| (r, false)).collect();
    while let Some((i, done)) = stack.pop() {
        if done {
            visible[i] = matches[i] || g.children[i].iter().any(|&c| visible[c]);
        } else {
            stack.push((i, true));
            stack.extend(g.children[i].iter().map(|&c| (c, false)));
        }
    }

    let auto_expand = !filter.query.is_empty();
    let mut rows = Vec::new();
    let mut stack: Vec<(usize, u16)> = g
        .roots
        .iter()
        .rev()
        .filter(|&&r| visible[r])
        .map(|&r| (r, 0))
        .collect();
    while let Some((i, depth)) = stack.pop() {
        let has_visible_children = g.children[i].iter().any(|&c| visible[c]);
        let is_expanded =
            has_visible_children && (auto_expand || expanded.contains(&g.beads[i].id));
        rows.push(Row {
            idx: i,
            depth,
            has_visible_children,
            expanded: is_expanded,
            context_only: !matches[i],
        });
        if is_expanded {
            stack.extend(
                g.children[i]
                    .iter()
                    .rev()
                    .filter(|&&c| visible[c])
                    .map(|&c| (c, depth + 1)),
            );
        }
    }
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphScope {
    #[default]
    Local,
    AllOpen,
}

pub fn scope_members(g: &Graph, anchor: usize, scope: GraphScope, filter: &Filter) -> Vec<usize> {
    let raw: Vec<usize> = match scope {
        GraphScope::AllOpen => (0..g.beads.len()).collect(),
        GraphScope::Local if !g.children[anchor].is_empty() => g.descendants(anchor),
        GraphScope::Local => match g.parent[anchor] {
            Some(p) => g.descendants(p),
            None => blocks_component(g, anchor),
        },
    };
    raw.into_iter().filter(|&i| filter.matches(g, i)).collect()
}

fn blocks_component(g: &Graph, start: usize) -> Vec<usize> {
    let mut seen = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    let mut out = Vec::new();
    while let Some(i) = queue.pop_front() {
        out.push(i);
        for &j in g.blockers[i].iter().chain(&g.dependents[i]) {
            if seen.insert(j) {
                queue.push_back(j);
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layers {
    pub columns: Vec<Vec<usize>>,
    pub cycle: Vec<usize>,
}

/// Longest-path layering over in-scope `blocks` edges: a bead sits one column
/// right of its deepest in-scope blocker, so columns read as execution waves.
pub fn layers(g: &Graph, members: &[usize]) -> Layers {
    let set: HashSet<usize> = members.iter().copied().collect();
    let mut indegree: std::collections::HashMap<usize, usize> = members
        .iter()
        .map(|&i| (i, g.blockers[i].iter().filter(|b| set.contains(b)).count()))
        .collect();
    let mut layer: std::collections::HashMap<usize, usize> = Default::default();
    let mut queue: VecDeque<usize> = members
        .iter()
        .copied()
        .filter(|i| indegree[i] == 0)
        .collect();
    for &i in &queue {
        layer.insert(i, 0);
    }
    while let Some(i) = queue.pop_front() {
        let li = layer[&i];
        for &d in &g.dependents[i] {
            if !set.contains(&d) {
                continue;
            }
            let ld = layer.entry(d).or_insert(0);
            *ld = (*ld).max(li + 1);
            let deg = indegree.get_mut(&d).expect("member");
            *deg -= 1;
            if *deg == 0 {
                queue.push_back(d);
            }
        }
    }

    let mut columns: Vec<Vec<usize>> = Vec::new();
    let mut cycle = Vec::new();
    for &i in members {
        match (indegree[&i], layer.get(&i)) {
            (0, Some(&l)) => {
                if columns.len() <= l {
                    columns.resize_with(l + 1, Vec::new);
                }
                columns[l].push(i);
            }
            _ => cycle.push(i),
        }
    }
    for c in &mut columns {
        g.sort(c);
    }
    g.sort(&mut cycle);
    Layers { columns, cycle }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{bead, dep};

    fn ids(g: &Graph, v: &[usize]) -> Vec<String> {
        v.iter().map(|&i| g.beads[i].id.clone()).collect()
    }

    #[test]
    fn open_filter_dims_closed_parent_of_open_child_and_hides_closed_leaf() {
        let mut child = bead("c", "open");
        child.parent = Some("e".into());
        let mut epic = bead("e", "closed");
        epic.issue_type = "epic".into();
        let g = Graph::build(vec![epic, child, bead("leaf", "closed")]);
        let rows = flatten(&g, &Filter::default(), &HashSet::from(["e".to_string()]));
        assert_eq!(rows.len(), 2);
        assert_eq!(g.beads[rows[0].idx].id, "e");
        assert!(rows[0].context_only);
        assert!(rows[0].expanded);
        assert_eq!(g.beads[rows[1].idx].id, "c");
        assert!(!rows[1].context_only);
    }

    #[test]
    fn query_auto_expands_to_deep_match() {
        let mut mid = bead("mid", "open");
        mid.parent = Some("top".into());
        let mut deep = bead("deep", "open");
        deep.parent = Some("mid".into());
        deep.title = "Needle in haystack".into();
        let g = Graph::build(vec![bead("top", "open"), mid, deep, bead("other", "open")]);
        let filter = Filter {
            query: "needle".into(),
            ..Default::default()
        };
        let rows = flatten(&g, &filter, &HashSet::new());
        let shown: Vec<_> = rows
            .iter()
            .map(|r| (g.beads[r.idx].id.as_str(), r.depth))
            .collect();
        assert_eq!(shown, vec![("top", 0), ("mid", 1), ("deep", 2)]);
        assert!(rows[0].context_only && rows[1].context_only && !rows[2].context_only);
    }

    #[test]
    fn label_filter_keeps_only_labelled_beads() {
        let mut a = bead("a", "open");
        a.labels = vec!["api".into()];
        let g = Graph::build(vec![a, bead("b", "open")]);
        let filter = Filter {
            label: Some("api".into()),
            ..Default::default()
        };
        let rows = flatten(&g, &filter, &HashSet::new());
        assert_eq!(
            ids(&g, &rows.iter().map(|r| r.idx).collect::<Vec<_>>()),
            vec!["a"]
        );
    }

    #[test]
    fn chain_layers_left_to_right() {
        let a = bead("a", "open");
        let mut b = bead("b", "open");
        b.dependencies.push(dep("b", "a", "blocks"));
        let mut c = bead("c", "open");
        c.dependencies.push(dep("c", "b", "blocks"));
        let g = Graph::build(vec![c, b, a]);
        let l = layers(&g, &[0, 1, 2]);
        let cols: Vec<_> = l.columns.iter().map(|c| ids(&g, c)).collect();
        assert_eq!(cols, vec![vec!["a"], vec!["b"], vec!["c"]]);
        assert!(l.cycle.is_empty());
    }

    #[test]
    fn two_cycle_lands_in_cycle_column() {
        let mut a = bead("a", "open");
        a.dependencies.push(dep("a", "b", "blocks"));
        let mut b = bead("b", "open");
        b.dependencies.push(dep("b", "a", "blocks"));
        let g = Graph::build(vec![a, b, bead("free", "open")]);
        let l = layers(&g, &[0, 1, 2]);
        assert_eq!(ids(&g, &l.cycle), vec!["a", "b"]);
        assert_eq!(l.columns.len(), 1);
        assert_eq!(ids(&g, &l.columns[0]), vec!["free"]);
    }

    #[test]
    fn out_of_scope_blocker_does_not_shift_layers() {
        let mut a = bead("a", "open");
        a.dependencies.push(dep("a", "outside", "blocks"));
        let g = Graph::build(vec![a, bead("outside", "open")]);
        let l = layers(&g, &[0]);
        assert_eq!(l.columns.len(), 1);
        assert_eq!(ids(&g, &l.columns[0]), vec!["a"]);
    }

    #[test]
    fn local_scope_of_leaf_without_parent_is_its_blocks_component() {
        let mut a = bead("a", "open");
        a.dependencies.push(dep("a", "b", "blocks"));
        let mut c = bead("c", "open");
        c.dependencies.push(dep("c", "b", "blocks"));
        let g = Graph::build(vec![a, bead("b", "open"), c, bead("lonely", "open")]);
        let mut m = scope_members(&g, 0, GraphScope::Local, &Filter::default());
        m.sort();
        assert_eq!(ids(&g, &m), vec!["a", "b", "c"]);
    }
}
