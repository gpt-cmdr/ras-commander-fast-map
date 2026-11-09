//! Clipper 6's edge crossings: the sorted edge list (SEL), building, ordering and
//! processing each scanbeam's intersection list, `IntersectPoint`, and `IntersectEdges`, which
//! swaps winding counts where two edges cross and starts, extends or closes output rings.

use super::clipper_base::is_horizontal;
use super::engine::{Clipper, SortContext, top_x};
use super::types::{
    ClipType, ClipperError, IntPoint, IntersectNode, PolyFillType, PolyType, TEdge, UNASSIGNED, ip,
    round,
};
use std::cmp::Ordering;

impl Clipper {
    /// SEL pointers in PEdge are use to build transient lists of horizontal
    /// edges. However, since we don't need to worry about processing order, all additions are
    /// made to the front of the list ...
    pub(super) fn add_edge_to_sel(&mut self, edge: usize) {
        match self.sorted_edges {
            None => {
                self.sorted_edges = Some(edge);
                self.edges[edge].prev_in_sel = None;
                self.edges[edge].next_in_sel = None;
            }
            Some(head) => {
                self.edges[edge].next_in_sel = Some(head);
                self.edges[edge].prev_in_sel = None;
                self.edges[head].prev_in_sel = Some(edge);
                self.sorted_edges = Some(edge);
            }
        }
    }

    /// Pop edge from front of SEL (ie SEL is a FILO list).
    pub(super) fn pop_edge_from_sel(&mut self) -> Option<usize> {
        let e = self.sorted_edges?;
        self.sorted_edges = self.edges[e].next_in_sel;
        if let Some(head) = self.sorted_edges {
            self.edges[head].prev_in_sel = None;
        }
        self.edges[e].next_in_sel = None;
        self.edges[e].prev_in_sel = None;
        Some(e)
    }

    fn copy_ael_to_sel(&mut self) {
        let mut e = self.active_edges;
        self.sorted_edges = e;
        while let Some(edge) = e {
            self.edges[edge].prev_in_sel = self.edges[edge].prev_in_ael;
            self.edges[edge].next_in_sel = self.edges[edge].next_in_ael;
            e = self.edges[edge].next_in_ael;
        }
    }

    fn swap_positions_in_sel(&mut self, edge1: usize, edge2: usize) {
        if self.edges[edge1].next_in_sel.is_none() && self.edges[edge1].prev_in_sel.is_none() {
            return;
        }
        if self.edges[edge2].next_in_sel.is_none() && self.edges[edge2].prev_in_sel.is_none() {
            return;
        }

        if self.edges[edge1].next_in_sel == Some(edge2) {
            let next = self.edges[edge2].next_in_sel;
            if let Some(next) = next {
                self.edges[next].prev_in_sel = Some(edge1);
            }
            let prev = self.edges[edge1].prev_in_sel;
            if let Some(prev) = prev {
                self.edges[prev].next_in_sel = Some(edge2);
            }
            self.edges[edge2].prev_in_sel = prev;
            self.edges[edge2].next_in_sel = Some(edge1);
            self.edges[edge1].prev_in_sel = Some(edge2);
            self.edges[edge1].next_in_sel = next;
        } else if self.edges[edge2].next_in_sel == Some(edge1) {
            let next = self.edges[edge1].next_in_sel;
            if let Some(next) = next {
                self.edges[next].prev_in_sel = Some(edge2);
            }
            let prev = self.edges[edge2].prev_in_sel;
            if let Some(prev) = prev {
                self.edges[prev].next_in_sel = Some(edge1);
            }
            self.edges[edge1].prev_in_sel = prev;
            self.edges[edge1].next_in_sel = Some(edge2);
            self.edges[edge2].prev_in_sel = Some(edge1);
            self.edges[edge2].next_in_sel = next;
        } else {
            let next = self.edges[edge1].next_in_sel;
            let prev = self.edges[edge1].prev_in_sel;
            self.edges[edge1].next_in_sel = self.edges[edge2].next_in_sel;
            if let Some(n) = self.edges[edge1].next_in_sel {
                self.edges[n].prev_in_sel = Some(edge1);
            }
            self.edges[edge1].prev_in_sel = self.edges[edge2].prev_in_sel;
            if let Some(p) = self.edges[edge1].prev_in_sel {
                self.edges[p].next_in_sel = Some(edge1);
            }
            self.edges[edge2].next_in_sel = next;
            if let Some(n) = next {
                self.edges[n].prev_in_sel = Some(edge2);
            }
            self.edges[edge2].prev_in_sel = prev;
            if let Some(p) = prev {
                self.edges[p].next_in_sel = Some(edge2);
            }
        }

        if self.edges[edge1].prev_in_sel.is_none() {
            self.sorted_edges = Some(edge1);
        } else if self.edges[edge2].prev_in_sel.is_none() {
            self.sorted_edges = Some(edge2);
        }
    }

    /// False when the crossings cannot be ordered so that only
    /// adjacent edges swap.
    pub(super) fn process_intersections(&mut self, top_y: i64) -> Result<bool, ClipperError> {
        if self.active_edges.is_none() {
            return Ok(true);
        }
        self.build_intersect_list(top_y);
        if self.intersect_list.is_empty() {
            return Ok(true);
        }
        if self.intersect_list.len() == 1 || self.fixup_intersection_order() {
            self.process_intersect_list()?;
        } else {
            return Ok(false);
        }
        self.sorted_edges = None;
        Ok(true)
    }

    fn build_intersect_list(&mut self, top_y: i64) {
        let Some(active) = self.active_edges else {
            return;
        };

        //prepare for sorting ...
        let mut e = Some(active);
        self.sorted_edges = e;
        while let Some(edge) = e {
            self.edges[edge].prev_in_sel = self.edges[edge].prev_in_ael;
            self.edges[edge].next_in_sel = self.edges[edge].next_in_ael;
            self.edges[edge].curr.x = top_x(&self.edges[edge], top_y);
            e = self.edges[edge].next_in_ael;
        }

        //bubblesort ...
        let mut is_modified = true;
        while is_modified {
            let Some(head) = self.sorted_edges else { break };
            is_modified = false;
            let mut e = head;
            while let Some(e_next) = self.edges[e].next_in_sel {
                if self.edges[e].curr.x > self.edges[e_next].curr.x {
                    let mut pt = self.intersect_point(e, e_next);
                    if pt.y < top_y {
                        pt = ip(top_x(&self.edges[e], top_y), top_y);
                    }
                    self.intersect_list.push(IntersectNode {
                        edge1: e,
                        edge2: e_next,
                        pt,
                    });

                    self.swap_positions_in_sel(e, e_next);
                    is_modified = true;
                } else {
                    e = e_next;
                }
            }
            match self.edges[e].prev_in_sel {
                Some(prev) => self.edges[prev].next_in_sel = None,
                None => break,
            }
        }
        self.sorted_edges = None;
    }

    fn edges_adjacent(&self, inode: &IntersectNode) -> bool {
        self.edges[inode.edge1].next_in_sel == Some(inode.edge2)
            || self.edges[inode.edge1].prev_in_sel == Some(inode.edge2)
    }

    fn fixup_intersection_order(&mut self) -> bool {
        //pre-condition: intersections are sorted bottom-most first.
        //Now it's crucial that intersections are made only between adjacent edges,
        //so to ensure this the order of intersections may need adjusting ...
        // MyIntersectNodeSort: descending Y, through the unstable introspective sort, which
        // fixes the order equal-Y crossings come out in.
        let mut compare =
            |node1: &IntersectNode, node2: &IntersectNode| node2.pt.y.cmp(&node1.pt.y);
        match self.sort_context {
            SortContext::Legacy => legacy_sort(&mut self.intersect_list, &mut compare),
            SortContext::Modern => {
                let capacity = self.intersect_list.capacity();
                modern_sort(&mut self.intersect_list, capacity, &mut compare);
            }
        }

        self.copy_ael_to_sel();
        let cnt = self.intersect_list.len();
        for i in 0..cnt {
            if !self.edges_adjacent(&self.intersect_list[i]) {
                let mut j = i + 1;
                while j < cnt && !self.edges_adjacent(&self.intersect_list[j]) {
                    j += 1;
                }
                if j == cnt {
                    return false;
                }
                self.intersect_list.swap(i, j);
            }
            let node = self.intersect_list[i];
            self.swap_positions_in_sel(node.edge1, node.edge2);
        }
        true
    }

    fn process_intersect_list(&mut self) -> Result<(), ClipperError> {
        for i in 0..self.intersect_list.len() {
            let node = self.intersect_list[i];
            self.intersect_edges(node.edge1, node.edge2, node.pt)?;
            self.swap_positions_in_ael(node.edge1, node.edge2);
        }
        self.intersect_list.clear();
        Ok(())
    }

    fn intersect_point(&self, edge1: usize, edge2: usize) -> IntPoint {
        let (edge1, edge2) = (&self.edges[edge1], &self.edges[edge2]);
        let mut ip = ip(0, 0);
        //nb: with very large coordinate values, it's possible for SlopesEqual() to
        //return false but for the edge.Dx value be equal due to double precision rounding.
        if edge1.dx == edge2.dx {
            ip.y = edge1.curr.y;
            ip.x = top_x(edge1, ip.y);
            return ip;
        }

        if edge1.delta.x == 0 {
            ip.x = edge1.bot.x;
            if is_horizontal(edge2) {
                ip.y = edge2.bot.y;
            } else {
                let b2 = edge2.bot.y as f64 - (edge2.bot.x as f64 / edge2.dx);
                ip.y = round(ip.x as f64 / edge2.dx + b2);
            }
        } else if edge2.delta.x == 0 {
            ip.x = edge2.bot.x;
            if is_horizontal(edge1) {
                ip.y = edge1.bot.y;
            } else {
                let b1 = edge1.bot.y as f64 - (edge1.bot.x as f64 / edge1.dx);
                ip.y = round(ip.x as f64 / edge1.dx + b1);
            }
        } else {
            let b1 = edge1.bot.x as f64 - edge1.bot.y as f64 * edge1.dx;
            let b2 = edge2.bot.x as f64 - edge2.bot.y as f64 * edge2.dx;
            let q = (b2 - b1) / (edge1.dx - edge2.dx);
            ip.y = round(q);
            if edge1.dx.abs() < edge2.dx.abs() {
                ip.x = round(edge1.dx * q + b1);
            } else {
                ip.x = round(edge2.dx * q + b2);
            }
        }

        if ip.y < edge1.top.y || ip.y < edge2.top.y {
            if edge1.top.y > edge2.top.y {
                ip.y = edge1.top.y;
            } else {
                ip.y = edge2.top.y;
            }
            if edge1.dx.abs() < edge2.dx.abs() {
                ip.x = top_x(edge1, ip.y);
            } else {
                ip.x = top_x(edge2, ip.y);
            }
        }
        //finally, don't allow 'ip' to be BELOW curr.Y (ie bottom of scanbeam) ...
        if ip.y > edge1.curr.y {
            ip.y = edge1.curr.y;
            //better to use the more vertical edge to derive X ...
            if edge1.dx.abs() > edge2.dx.abs() {
                ip.x = top_x(edge2, ip.y);
            } else {
                ip.x = top_x(edge1, ip.y);
            }
        }
        ip
    }

    /// e1 will be to the left of e2 BELOW the intersection. Therefore e1
    /// is before e2 in AEL except when e1 is being inserted at the intersection point ...
    pub(super) fn intersect_edges(
        &mut self,
        e1: usize,
        e2: usize,
        pt: IntPoint,
    ) -> Result<(), ClipperError> {
        let e1_contributing = self.edges[e1].out_idx >= 0;
        let e2_contributing = self.edges[e2].out_idx >= 0;

        //if either edge is on an OPEN path ...
        if self.edges[e1].wind_delta == 0 || self.edges[e2].wind_delta == 0 {
            let (a, b) = (self.edges[e1].clone(), self.edges[e2].clone());
            //ignore subject-subject open path intersections UNLESS they
            //are both open paths, AND they are both 'contributing maximas' ...
            if a.wind_delta == 0 && b.wind_delta == 0 {
                return Ok(());
            }
            //if intersecting a subj line with a subj poly ...
            else if a.poly_typ == b.poly_typ
                && a.wind_delta != b.wind_delta
                && self.clip_type == ClipType::Union
            {
                if a.wind_delta == 0 {
                    if e2_contributing {
                        self.add_out_pt(e1, pt)?;
                        if e1_contributing {
                            self.edges[e1].out_idx = UNASSIGNED;
                        }
                    }
                } else if e1_contributing {
                    self.add_out_pt(e2, pt)?;
                    if e2_contributing {
                        self.edges[e2].out_idx = UNASSIGNED;
                    }
                }
            } else if a.poly_typ != b.poly_typ {
                if a.wind_delta == 0
                    && b.wind_cnt.abs() == 1
                    && (self.clip_type != ClipType::Union || b.wind_cnt2 == 0)
                {
                    self.add_out_pt(e1, pt)?;
                    if e1_contributing {
                        self.edges[e1].out_idx = UNASSIGNED;
                    }
                } else if b.wind_delta == 0
                    && a.wind_cnt.abs() == 1
                    && (self.clip_type != ClipType::Union || a.wind_cnt2 == 0)
                {
                    self.add_out_pt(e2, pt)?;
                    if e2_contributing {
                        self.edges[e2].out_idx = UNASSIGNED;
                    }
                }
            }
            return Ok(());
        }

        //update winding counts...
        //assumes that e1 will be to the Right of e2 ABOVE the intersection
        if self.edges[e1].poly_typ == self.edges[e2].poly_typ {
            if self.is_even_odd_fill_type(e1) {
                let old_e1_wind_cnt = self.edges[e1].wind_cnt;
                self.edges[e1].wind_cnt = self.edges[e2].wind_cnt;
                self.edges[e2].wind_cnt = old_e1_wind_cnt;
            } else {
                let (e1_wd, e2_wd) = (self.edges[e1].wind_delta, self.edges[e2].wind_delta);
                if self.edges[e1].wind_cnt + e2_wd == 0 {
                    self.edges[e1].wind_cnt = -self.edges[e1].wind_cnt;
                } else {
                    self.edges[e1].wind_cnt += e2_wd;
                }
                if self.edges[e2].wind_cnt - e1_wd == 0 {
                    self.edges[e2].wind_cnt = -self.edges[e2].wind_cnt;
                } else {
                    self.edges[e2].wind_cnt -= e1_wd;
                }
            }
        } else {
            if !self.is_even_odd_fill_type(e2) {
                self.edges[e1].wind_cnt2 += self.edges[e2].wind_delta;
            } else {
                let wc2 = self.edges[e1].wind_cnt2;
                self.edges[e1].wind_cnt2 = if wc2 == 0 { 1 } else { 0 };
            }
            if !self.is_even_odd_fill_type(e1) {
                self.edges[e2].wind_cnt2 -= self.edges[e1].wind_delta;
            } else {
                let wc2 = self.edges[e2].wind_cnt2;
                self.edges[e2].wind_cnt2 = if wc2 == 0 { 1 } else { 0 };
            }
        }

        let (e1_fill_type, e1_fill_type2) = self.fill_types(e1);
        let (e2_fill_type, e2_fill_type2) = self.fill_types(e2);
        let e1_wc = wind_count_for(e1_fill_type, self.edges[e1].wind_cnt);
        let e2_wc = wind_count_for(e2_fill_type, self.edges[e2].wind_cnt);

        if e1_contributing && e2_contributing {
            if (e1_wc != 0 && e1_wc != 1)
                || (e2_wc != 0 && e2_wc != 1)
                || (self.edges[e1].poly_typ != self.edges[e2].poly_typ
                    && self.clip_type != ClipType::Xor)
            {
                self.add_local_max_poly(e1, e2, pt)?;
            } else {
                self.add_out_pt(e1, pt)?;
                self.add_out_pt(e2, pt)?;
                swap_sides(&mut self.edges, e1, e2);
                swap_poly_indexes(&mut self.edges, e1, e2);
            }
        } else if e1_contributing {
            if e2_wc == 0 || e2_wc == 1 {
                self.add_out_pt(e1, pt)?;
                swap_sides(&mut self.edges, e1, e2);
                swap_poly_indexes(&mut self.edges, e1, e2);
            }
        } else if e2_contributing {
            if e1_wc == 0 || e1_wc == 1 {
                self.add_out_pt(e2, pt)?;
                swap_sides(&mut self.edges, e1, e2);
                swap_poly_indexes(&mut self.edges, e1, e2);
            }
        } else if (e1_wc == 0 || e1_wc == 1) && (e2_wc == 0 || e2_wc == 1) {
            //neither edge is currently contributing ...
            let e1_wc2 = wind_count_for(e1_fill_type2, self.edges[e1].wind_cnt2);
            let e2_wc2 = wind_count_for(e2_fill_type2, self.edges[e2].wind_cnt2);

            if self.edges[e1].poly_typ != self.edges[e2].poly_typ {
                self.add_local_min_poly(e1, e2, pt)?;
            } else if e1_wc == 1 && e2_wc == 1 {
                match self.clip_type {
                    ClipType::Intersection => {
                        if e1_wc2 > 0 && e2_wc2 > 0 {
                            self.add_local_min_poly(e1, e2, pt)?;
                        }
                    }
                    ClipType::Union => {
                        if e1_wc2 <= 0 && e2_wc2 <= 0 {
                            self.add_local_min_poly(e1, e2, pt)?;
                        }
                    }
                    ClipType::Difference => {
                        let e1_typ = self.edges[e1].poly_typ;
                        if (e1_typ == PolyType::Clip && e1_wc2 > 0 && e2_wc2 > 0)
                            || (e1_typ == PolyType::Subject && e1_wc2 <= 0 && e2_wc2 <= 0)
                        {
                            self.add_local_min_poly(e1, e2, pt)?;
                        }
                    }
                    ClipType::Xor => {
                        self.add_local_min_poly(e1, e2, pt)?;
                    }
                }
            } else {
                swap_sides(&mut self.edges, e1, e2);
            }
        }
        Ok(())
    }
}

/// The winding count as a fill rule reads it.
fn wind_count_for(fill_type: PolyFillType, wind_cnt: i32) -> i32 {
    match fill_type {
        PolyFillType::Positive => wind_cnt,
        PolyFillType::Negative => -wind_cnt,
        _ => wind_cnt.abs(),
    }
}

fn swap_sides(edges: &mut [TEdge], edge1: usize, edge2: usize) {
    let side = edges[edge1].side;
    edges[edge1].side = edges[edge2].side;
    edges[edge2].side = side;
}

fn swap_poly_indexes(edges: &mut [TEdge], edge1: usize, edge2: usize) {
    let out_idx = edges[edge1].out_idx;
    edges[edge1].out_idx = edges[edge2].out_idx;
    edges[edge2].out_idx = out_idx;
}

// Compatibility overlay: legacy .NET 4.0 depth-limited quicksort, as used
// when BinaryCompatibility.TargetsAtLeast_Desktop_V4_5 is false. Algorithm
// described by Microsoft referencesource ArraySortHelper (MIT); no vendor code.
fn legacy_sort<T: Copy>(keys: &mut [T], compare: &mut impl FnMut(&T, &T) -> Ordering) {
    legacy_sort_depth(keys, compare, 32);
}
fn legacy_sort_depth<T: Copy>(
    keys: &mut [T],
    compare: &mut impl FnMut(&T, &T) -> Ordering,
    depth: usize,
) {
    fn swap_greater<T>(a: &mut [T], c: &mut impl FnMut(&T, &T) -> Ordering, i: usize, j: usize) {
        if c(&a[i], &a[j]) == Ordering::Greater {
            a.swap(i, j)
        }
    }
    fn down<T: Copy>(
        a: &mut [T],
        mut i: usize,
        n: usize,
        lo: usize,
        c: &mut impl FnMut(&T, &T) -> Ordering,
    ) {
        let value = a[lo + i - 1];
        while i <= n / 2 {
            let mut child = 2 * i;
            if child < n && c(&a[lo + child - 1], &a[lo + child]) == Ordering::Less {
                child += 1
            }
            if c(&value, &a[lo + child - 1]) != Ordering::Less {
                break;
            }
            a[lo + i - 1] = a[lo + child - 1];
            i = child;
        }
        a[lo + i - 1] = value;
    }
    fn quick<T: Copy>(
        a: &mut [T],
        mut left: isize,
        mut right: isize,
        mut depth: usize,
        c: &mut impl FnMut(&T, &T) -> Ordering,
    ) {
        loop {
            if left >= right {
                return;
            }
            if depth == 0 {
                let lo = left as usize;
                let n = (right - left + 1) as usize;
                for i in (1..=n / 2).rev() {
                    down(a, i, n, lo, c)
                }
                for i in (2..=n).rev() {
                    a.swap(lo, lo + i - 1);
                    down(a, 1, i - 1, lo, c)
                }
                return;
            }
            let (mut i, mut j) = (left, right);
            let middle = i + ((j - i) >> 1);
            swap_greater(a, c, i as usize, middle as usize);
            swap_greater(a, c, i as usize, j as usize);
            swap_greater(a, c, middle as usize, j as usize);
            let pivot = a[middle as usize];
            loop {
                while c(&a[i as usize], &pivot) == Ordering::Less {
                    i += 1
                }
                while c(&pivot, &a[j as usize]) == Ordering::Less {
                    j -= 1
                }
                if i > j {
                    break;
                }
                if i < j {
                    a.swap(i as usize, j as usize)
                }
                i += 1;
                j -= 1;
                if i > j {
                    break;
                }
            }
            depth -= 1;
            if j - left <= right - i {
                if left < j {
                    quick(a, left, j, depth, c)
                }
                left = i;
            } else {
                if i < right {
                    quick(a, i, right, depth, c)
                }
                right = j;
            }
        }
    }
    if keys.len() > 1 {
        quick(keys, 0, keys.len() as isize - 1, depth, compare)
    }
}

// Independent Rust translation of Microsoft Reference Source ArraySortHelper<T>
// IntrospectiveSort/IntroSort/PickPivotAndPartition/Heapsort/InsertionSort (MIT).
// Microsoft Reference Source ec9fa9ae770d522a5b5f0607898044b7478574a3.
// Full MIT attribution is retained in licenses/vendored/boundary_clip_legacy/.
fn modern_sort<T: Copy, F: FnMut(&T, &T) -> Ordering>(
    keys: &mut [T],
    backing_capacity: usize,
    compare: &mut F,
) {
    assert!(backing_capacity >= keys.len());
    let mut n = backing_capacity;
    let mut bits = 0;
    while n >= 1 {
        bits += 1;
        n /= 2;
    }
    modern_sort_depth(keys, compare, 2 * bits);
}
fn modern_sort_depth<T: Copy, F: FnMut(&T, &T) -> Ordering>(
    keys: &mut [T],
    compare: &mut F,
    depth: usize,
) {
    fn swap_greater<T, F: FnMut(&T, &T) -> Ordering>(a: &mut [T], c: &mut F, i: usize, j: usize) {
        if i != j && c(&a[i], &a[j]) == Ordering::Greater {
            a.swap(i, j);
        }
    }
    fn insertion<T: Copy, F: FnMut(&T, &T) -> Ordering>(
        a: &mut [T],
        c: &mut F,
        lo: usize,
        hi: usize,
    ) {
        for i in lo..hi {
            let t = a[i + 1];
            let mut j = i as isize;
            while j >= lo as isize && c(&t, &a[j as usize]) == Ordering::Less {
                a[j as usize + 1] = a[j as usize];
                j -= 1;
            }
            a[(j + 1) as usize] = t;
        }
    }
    fn down_heap<T: Copy, F: FnMut(&T, &T) -> Ordering>(
        a: &mut [T],
        c: &mut F,
        mut i: usize,
        n: usize,
        lo: usize,
    ) {
        let d = a[lo + i - 1];
        while i <= n / 2 {
            let mut child = 2 * i;
            if child < n && c(&a[lo + child - 1], &a[lo + child]) == Ordering::Less {
                child += 1;
            }
            if c(&d, &a[lo + child - 1]) != Ordering::Less {
                break;
            }
            a[lo + i - 1] = a[lo + child - 1];
            i = child;
        }
        a[lo + i - 1] = d;
    }
    fn heap<T: Copy, F: FnMut(&T, &T) -> Ordering>(a: &mut [T], c: &mut F, lo: usize, hi: usize) {
        let n = hi - lo + 1;
        for i in (1..=n / 2).rev() {
            down_heap(a, c, i, n, lo);
        }
        for i in (2..=n).rev() {
            a.swap(lo, lo + i - 1);
            down_heap(a, c, 1, i - 1, lo);
        }
    }
    fn partition<T: Copy, F: FnMut(&T, &T) -> Ordering>(
        a: &mut [T],
        c: &mut F,
        lo: usize,
        hi: usize,
    ) -> usize {
        let mid = lo + (hi - lo) / 2;
        swap_greater(a, c, lo, mid);
        swap_greater(a, c, lo, hi);
        swap_greater(a, c, mid, hi);
        let pivot = a[mid];
        a.swap(mid, hi - 1);
        let (mut left, mut right) = (lo, hi - 1);
        while left < right {
            left += 1;
            while c(&a[left], &pivot) == Ordering::Less {
                left += 1;
            }
            right -= 1;
            while c(&pivot, &a[right]) == Ordering::Less {
                right -= 1;
            }
            if left >= right {
                break;
            }
            a.swap(left, right);
        }
        a.swap(left, hi - 1);
        left
    }
    fn intro<T: Copy, F: FnMut(&T, &T) -> Ordering>(
        a: &mut [T],
        c: &mut F,
        lo: usize,
        mut hi: usize,
        mut depth: usize,
    ) {
        while hi > lo {
            let count = hi - lo + 1;
            if count <= 16 {
                if count == 2 {
                    swap_greater(a, c, lo, hi);
                    return;
                }
                if count == 3 {
                    swap_greater(a, c, lo, hi - 1);
                    swap_greater(a, c, lo, hi);
                    swap_greater(a, c, hi - 1, hi);
                    return;
                }
                insertion(a, c, lo, hi);
                return;
            }
            if depth == 0 {
                heap(a, c, lo, hi);
                return;
            }
            depth -= 1;
            let pivot = partition(a, c, lo, hi);
            intro(a, c, pivot + 1, hi, depth);
            hi = pivot - 1;
        }
    }
    if keys.len() >= 2 {
        intro(keys, compare, 0, keys.len() - 1, depth);
    }
}

#[cfg(test)]
#[path = "sort_fixtures.rs"]
mod sort_fixtures;
