//! Clipper 6's `PolyTree`/`PolyNode`: the solution as a tree of outlines and
//! holes. Nodes live in one arena; node 0 is the tree itself, and
//! every other node follows in the order the clipper adds them.

use super::types::Path;

#[derive(Clone, Debug, Default)]
pub struct PolyNode {
    pub parent: Option<usize>,
    pub polygon: Path,
    /// This node's position among its parent's children.
    pub index: usize,
    pub childs: Vec<usize>,
    pub is_open: bool,
}

#[derive(Clone, Debug)]
pub struct PolyTree {
    pub nodes: Vec<PolyNode>,
}

impl Default for PolyTree {
    fn default() -> Self {
        Self::new()
    }
}

impl PolyTree {
    /// The root node's index.
    pub const ROOT: usize = 0;

    pub fn new() -> Self {
        PolyTree {
            nodes: vec![PolyNode::default()],
        }
    }

    pub fn clear(&mut self) {
        self.nodes.truncate(1);
        self.nodes[0].childs.clear();
    }

    /// Adds a detached node and returns its index.
    pub(super) fn new_node(&mut self, polygon: Path) -> usize {
        self.nodes.push(PolyNode {
            polygon,
            ..PolyNode::default()
        });
        self.nodes.len() - 1
    }

    pub(super) fn add_child(&mut self, parent: usize, child: usize) {
        let cnt = self.nodes[parent].childs.len();
        self.nodes[parent].childs.push(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[child].index = cnt;
    }

    pub fn contour(&self, node: usize) -> &Path {
        &self.nodes[node].polygon
    }

    pub fn childs(&self, node: usize) -> &[usize] {
        &self.nodes[node].childs
    }

    pub fn child_count(&self, node: usize) -> usize {
        self.nodes[node].childs.len()
    }

    /// True for the root, then alternating with depth.
    pub fn is_hole(&self, node: usize) -> bool {
        let mut result = true;
        let mut parent = self.nodes[node].parent;
        while let Some(p) = parent {
            result = !result;
            parent = self.nodes[p].parent;
        }
        result
    }

    pub fn get_first(&self) -> Option<usize> {
        self.nodes[Self::ROOT].childs.first().copied()
    }

    /// Depth first, parents before children.
    pub fn get_next(&self, node: usize) -> Option<usize> {
        match self.nodes[node].childs.first() {
            Some(&child) => Some(child),
            None => self.get_next_sibling_up(node),
        }
    }

    fn get_next_sibling_up(&self, node: usize) -> Option<usize> {
        let parent = self.nodes[node].parent?;
        let index = self.nodes[node].index;
        if index == self.nodes[parent].childs.len() - 1 {
            self.get_next_sibling_up(parent)
        } else {
            Some(self.nodes[parent].childs[index + 1])
        }
    }

    /// Every node but the root, less the hidden outer polygon a negative
    /// offset leaves when the first child is not the first node built.
    pub fn total(&self) -> usize {
        let mut result = self.nodes.len() - 1;
        if result > 0 && self.nodes[Self::ROOT].childs.first() != Some(&1) {
            result -= 1;
        }
        result
    }
}
