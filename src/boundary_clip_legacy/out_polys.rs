//! Clipper 6's output rings: creating out-records and points (`AddOutPt`,
//! `AddLocalMinPoly`, `AddLocalMaxPoly`), hole state and the lowermost-record tie-breaks,
//! merging two rings (`AppendPolygon`), the clean-up passes (`FixupOutPolygon`,
//! `FixupOutPolyline`) and the results (`BuildResult` into paths, `BuildResult2` into a
//! [`PolyTree`]). The order the points are read back here is what fixes each ring's start.

use super::clipper_base::{
    is_horizontal, pt2_is_between_pt1_and_pt3, slopes_equal_edges, slopes_equal3,
};
use super::engine::{Clipper, top_x};
use super::poly_tree::PolyTree;
use super::types::{
    ClipperError, EdgeSide, HORIZONTAL, IntPoint, OutPt, OutRec, Paths, UNASSIGNED,
};

impl Clipper {
    pub(super) fn create_out_rec(&mut self) -> usize {
        let index = self.poly_outs.len();
        self.poly_outs.push(OutRec {
            idx: index as i32,
            is_hole: false,
            is_open: false,
            first_left: None,
            pts: None,
            bottom_pt: None,
            poly_node: None,
        });
        index
    }

    /// `new OutPt()` linked to itself.
    pub(super) fn new_out_pt(&mut self, idx: i32, pt: IntPoint) -> usize {
        let index = self.out_pts.len();
        self.out_pts.push(OutPt {
            idx,
            pt,
            next: index,
            prev: index,
        });
        index
    }

    /// The first point of `out_rec`'s ring, which the sweep only reads while the ring has
    /// points; [`ClipperError::OutRecHasNoPoints`] when it has none.
    pub(super) fn ring_pts(&self, out_rec: usize) -> Result<usize, ClipperError> {
        self.poly_outs[out_rec]
            .pts
            .ok_or(ClipperError::OutRecHasNoPoints)
    }

    pub(super) fn add_local_max_poly(
        &mut self,
        e1: usize,
        e2: usize,
        pt: IntPoint,
    ) -> Result<(), ClipperError> {
        self.add_out_pt(e1, pt)?;
        if self.edges[e2].wind_delta == 0 {
            self.add_out_pt(e2, pt)?;
        }
        if self.edges[e1].out_idx == self.edges[e2].out_idx {
            self.edges[e1].out_idx = UNASSIGNED;
            self.edges[e2].out_idx = UNASSIGNED;
        } else if self.edges[e1].out_idx < self.edges[e2].out_idx {
            self.append_polygon(e1, e2)?;
        } else {
            self.append_polygon(e2, e1)?;
        }
        Ok(())
    }

    pub(super) fn add_local_min_poly(
        &mut self,
        e1: usize,
        e2: usize,
        pt: IntPoint,
    ) -> Result<usize, ClipperError> {
        let (result, e, prev_e) =
            if is_horizontal(&self.edges[e2]) || self.edges[e1].dx > self.edges[e2].dx {
                let result = self.add_out_pt(e1, pt)?;
                self.edges[e2].out_idx = self.edges[e1].out_idx;
                self.edges[e1].side = EdgeSide::Left;
                self.edges[e2].side = EdgeSide::Right;
                let e = e1;
                let prev_e = if self.edges[e].prev_in_ael == Some(e2) {
                    self.edges[e2].prev_in_ael
                } else {
                    self.edges[e].prev_in_ael
                };
                (result, e, prev_e)
            } else {
                let result = self.add_out_pt(e2, pt)?;
                self.edges[e1].out_idx = self.edges[e2].out_idx;
                self.edges[e1].side = EdgeSide::Right;
                self.edges[e2].side = EdgeSide::Left;
                let e = e2;
                let prev_e = if self.edges[e].prev_in_ael == Some(e1) {
                    self.edges[e1].prev_in_ael
                } else {
                    self.edges[e].prev_in_ael
                };
                (result, e, prev_e)
            };

        if let Some(prev_e) = prev_e {
            let (p, c) = (&self.edges[prev_e], &self.edges[e]);
            if p.out_idx >= 0
                && top_x(p, pt.y) == top_x(c, pt.y)
                && slopes_equal_edges(c, p, self.use_full_range)
                && c.wind_delta != 0
                && p.wind_delta != 0
            {
                let e_top = c.top;
                let out_pt = self.add_out_pt(prev_e, pt)?;
                self.add_join(result, out_pt, e_top);
            }
        }
        Ok(result)
    }

    pub(super) fn add_out_pt(&mut self, e: usize, pt: IntPoint) -> Result<usize, ClipperError> {
        if self.edges[e].out_idx < 0 {
            let out_rec = self.create_out_rec();
            self.poly_outs[out_rec].is_open = self.edges[e].wind_delta == 0;
            let new_op = self.new_out_pt(self.poly_outs[out_rec].idx, pt);
            self.poly_outs[out_rec].pts = Some(new_op);
            if !self.poly_outs[out_rec].is_open {
                self.set_hole_state(e, out_rec);
            }
            self.edges[e].out_idx = self.poly_outs[out_rec].idx; //nb: do this after SetZ !
            Ok(new_op)
        } else {
            let out_rec = self.edges[e].out_idx as usize;
            //OutRec.Pts is the 'Left-most' point & OutRec.Pts.Prev is the 'Right-most'
            let op = self.ring_pts(out_rec)?;
            let to_front = self.edges[e].side == EdgeSide::Left;
            let op_prev = self.out_pts[op].prev;
            if to_front && pt == self.out_pts[op].pt {
                return Ok(op);
            } else if !to_front && pt == self.out_pts[op_prev].pt {
                return Ok(op_prev);
            }

            let new_op = self.new_out_pt(self.poly_outs[out_rec].idx, pt);
            self.out_pts[new_op].next = op;
            self.out_pts[new_op].prev = op_prev;
            self.out_pts[op_prev].next = new_op;
            self.out_pts[op].prev = new_op;
            if to_front {
                self.poly_outs[out_rec].pts = Some(new_op);
            }
            Ok(new_op)
        }
    }

    pub(super) fn get_last_out_pt(&self, e: usize) -> Result<usize, ClipperError> {
        let pts = self.ring_pts(self.edges[e].out_idx as usize)?;
        Ok(if self.edges[e].side == EdgeSide::Left {
            pts
        } else {
            self.out_pts[pts].prev
        })
    }

    fn set_hole_state(&mut self, e: usize, out_rec: usize) {
        let mut e2 = self.edges[e].prev_in_ael;
        let mut e_tmp: Option<usize> = None;
        while let Some(other) = e2 {
            if self.edges[other].out_idx >= 0 && self.edges[other].wind_delta != 0 {
                match e_tmp {
                    None => e_tmp = Some(other),
                    Some(t) if self.edges[t].out_idx == self.edges[other].out_idx => {
                        e_tmp = None; //paired
                    }
                    Some(_) => {}
                }
            }
            e2 = self.edges[other].prev_in_ael;
        }

        match e_tmp {
            None => {
                self.poly_outs[out_rec].first_left = None;
                self.poly_outs[out_rec].is_hole = false;
            }
            Some(t) => {
                let first_left = self.edges[t].out_idx as usize;
                self.poly_outs[out_rec].first_left = Some(first_left);
                self.poly_outs[out_rec].is_hole = !self.poly_outs[first_left].is_hole;
            }
        }
    }

    fn first_is_bottom_pt(&self, btm_pt1: usize, btm_pt2: usize) -> bool {
        let pts = &self.out_pts;
        let dx_around = |btm: usize| {
            let mut p = pts[btm].prev;
            while pts[p].pt == pts[btm].pt && p != btm {
                p = pts[p].prev;
            }
            let dxp = get_dx(pts[btm].pt, pts[p].pt).abs();
            p = pts[btm].next;
            while pts[p].pt == pts[btm].pt && p != btm {
                p = pts[p].next;
            }
            let dxn = get_dx(pts[btm].pt, pts[p].pt).abs();
            (dxp, dxn)
        };
        let (dx1p, dx1n) = dx_around(btm_pt1);
        let (dx2p, dx2n) = dx_around(btm_pt2);

        if dx1p.max(dx1n) == dx2p.max(dx2n) && dx1p.min(dx1n) == dx2p.min(dx2n) {
            self.area_out_pt(Some(btm_pt1)) > 0.0 //if otherwise identical use orientation
        } else {
            (dx1p >= dx2p && dx1p >= dx2n) || (dx1n >= dx2p && dx1n >= dx2n)
        }
    }

    fn get_bottom_pt(&self, mut pp: usize) -> usize {
        let pts = &self.out_pts;
        let mut dups: Option<usize> = None;
        let mut p = pts[pp].next;
        while p != pp {
            if pts[p].pt.y > pts[pp].pt.y {
                pp = p;
                dups = None;
            } else if pts[p].pt.y == pts[pp].pt.y && pts[p].pt.x <= pts[pp].pt.x {
                if pts[p].pt.x < pts[pp].pt.x {
                    dups = None;
                    pp = p;
                } else if pts[p].next != pp && pts[p].prev != pp {
                    dups = Some(p);
                }
            }
            p = pts[p].next;
        }
        if let Some(mut d) = dups {
            //there appears to be at least 2 vertices at bottomPt so ...
            while d != p {
                if !self.first_is_bottom_pt(p, d) {
                    pp = d;
                }
                d = pts[d].next;
                while pts[d].pt != pts[pp].pt {
                    d = pts[d].next;
                }
            }
        }
        pp
    }

    /// Work out which polygon fragment has the correct hole state ...
    pub(super) fn get_lowermost_rec(
        &mut self,
        out_rec1: usize,
        out_rec2: usize,
    ) -> Result<usize, ClipperError> {
        for rec in [out_rec1, out_rec2] {
            if self.poly_outs[rec].bottom_pt.is_none() {
                let pts = self.ring_pts(rec)?;
                self.poly_outs[rec].bottom_pt = Some(self.get_bottom_pt(pts));
            }
        }
        let b_pt1 = self.poly_outs[out_rec1].bottom_pt.unwrap_or_default();
        let b_pt2 = self.poly_outs[out_rec2].bottom_pt.unwrap_or_default();
        let (p1, p2) = (self.out_pts[b_pt1].pt, self.out_pts[b_pt2].pt);
        Ok(if p1.y > p2.y {
            out_rec1
        } else if p1.y < p2.y {
            out_rec2
        } else if p1.x < p2.x {
            out_rec1
        } else if p1.x > p2.x || self.out_pts[b_pt1].next == b_pt1 {
            out_rec2
        } else if self.out_pts[b_pt2].next == b_pt2 || self.first_is_bottom_pt(b_pt1, b_pt2) {
            out_rec1
        } else {
            out_rec2
        })
    }

    pub(super) fn out_rec1_right_of_out_rec2(&self, out_rec1: usize, out_rec2: usize) -> bool {
        let mut rec = Some(out_rec1);
        while let Some(r) = rec {
            rec = self.poly_outs[r].first_left;
            if rec == Some(out_rec2) {
                return true;
            }
        }
        false
    }

    /// Follows `Idx` to the record a merged one now lives in.
    pub(super) fn get_out_rec(&self, idx: i32) -> usize {
        let mut out_rec = idx as usize;
        while out_rec != self.poly_outs[out_rec].idx as usize {
            out_rec = self.poly_outs[out_rec].idx as usize;
        }
        out_rec
    }

    fn append_polygon(&mut self, e1: usize, e2: usize) -> Result<(), ClipperError> {
        let out_rec1 = self.edges[e1].out_idx as usize;
        let out_rec2 = self.edges[e2].out_idx as usize;

        let hole_state_rec = if self.out_rec1_right_of_out_rec2(out_rec1, out_rec2) {
            out_rec2
        } else if self.out_rec1_right_of_out_rec2(out_rec2, out_rec1) {
            out_rec1
        } else {
            self.get_lowermost_rec(out_rec1, out_rec2)?
        };

        //get the start and ends of both output polygons and
        //join E2 poly onto E1 poly and delete pointers to E2 ...
        let p1_lft = self.ring_pts(out_rec1)?;
        let p1_rt = self.out_pts[p1_lft].prev;
        let p2_lft = self.ring_pts(out_rec2)?;
        let p2_rt = self.out_pts[p2_lft].prev;

        //join e2 poly onto e1 poly and delete pointers to e2 ...
        if self.edges[e1].side == EdgeSide::Left {
            if self.edges[e2].side == EdgeSide::Left {
                //z y x a b c
                self.reverse_poly_pt_links(Some(p2_lft));
                self.out_pts[p2_lft].next = p1_lft;
                self.out_pts[p1_lft].prev = p2_lft;
                self.out_pts[p1_rt].next = p2_rt;
                self.out_pts[p2_rt].prev = p1_rt;
                self.poly_outs[out_rec1].pts = Some(p2_rt);
            } else {
                //x y z a b c
                self.out_pts[p2_rt].next = p1_lft;
                self.out_pts[p1_lft].prev = p2_rt;
                self.out_pts[p2_lft].prev = p1_rt;
                self.out_pts[p1_rt].next = p2_lft;
                self.poly_outs[out_rec1].pts = Some(p2_lft);
            }
        } else if self.edges[e2].side == EdgeSide::Right {
            //a b c z y x
            self.reverse_poly_pt_links(Some(p2_lft));
            self.out_pts[p1_rt].next = p2_rt;
            self.out_pts[p2_rt].prev = p1_rt;
            self.out_pts[p2_lft].next = p1_lft;
            self.out_pts[p1_lft].prev = p2_lft;
        } else {
            //a b c x y z
            self.out_pts[p1_rt].next = p2_lft;
            self.out_pts[p2_lft].prev = p1_rt;
            self.out_pts[p1_lft].prev = p2_rt;
            self.out_pts[p2_rt].next = p1_lft;
        }

        self.poly_outs[out_rec1].bottom_pt = None;
        if hole_state_rec == out_rec2 {
            if self.poly_outs[out_rec2].first_left != Some(out_rec1) {
                self.poly_outs[out_rec1].first_left = self.poly_outs[out_rec2].first_left;
            }
            self.poly_outs[out_rec1].is_hole = self.poly_outs[out_rec2].is_hole;
        }
        self.poly_outs[out_rec2].pts = None;
        self.poly_outs[out_rec2].bottom_pt = None;

        self.poly_outs[out_rec2].first_left = Some(out_rec1);

        let ok_idx = self.edges[e1].out_idx;
        let obsolete_idx = self.edges[e2].out_idx;

        self.edges[e1].out_idx = UNASSIGNED; //nb: safe because we only get here via AddLocalMaxPoly
        self.edges[e2].out_idx = UNASSIGNED;

        let mut e = self.active_edges;
        while let Some(edge) = e {
            if self.edges[edge].out_idx == obsolete_idx {
                self.edges[edge].out_idx = ok_idx;
                self.edges[edge].side = self.edges[e1].side;
                break;
            }
            e = self.edges[edge].next_in_ael;
        }
        self.poly_outs[out_rec2].idx = self.poly_outs[out_rec1].idx;
        Ok(())
    }

    pub(super) fn reverse_poly_pt_links(&mut self, pp: Option<usize>) {
        let Some(pp) = pp else { return };
        let mut pp1 = pp;
        loop {
            let pp2 = self.out_pts[pp1].next;
            self.out_pts[pp1].next = self.out_pts[pp1].prev;
            self.out_pts[pp1].prev = pp2;
            pp1 = pp2;
            if pp1 == pp {
                break;
            }
        }
    }

    fn point_count(&self, pts: Option<usize>) -> usize {
        let Some(pts) = pts else { return 0 };
        let mut result = 0;
        let mut p = pts;
        loop {
            result += 1;
            p = self.out_pts[p].next;
            if p == pts {
                break;
            }
        }
        result
    }

    /// Each ring read backwards from its right-most point.
    pub(super) fn build_result(&self, polyg: &mut Paths) {
        polyg.clear();
        for out_rec in &self.poly_outs {
            let Some(pts) = out_rec.pts else { continue };
            let mut p = self.out_pts[pts].prev;
            let cnt = self.point_count(Some(p));
            if cnt < 2 {
                continue;
            }
            let mut pg = Vec::with_capacity(cnt);
            for _ in 0..cnt {
                pg.push(self.out_pts[p].pt);
                p = self.out_pts[p].prev;
            }
            polyg.push(pg);
        }
    }

    /// Skip if an outermost polygon or already already points to the
    /// correct FirstLeft ...
    fn fix_hole_linkage(&mut self, out_rec: usize) {
        let Some(first_left) = self.poly_outs[out_rec].first_left else {
            return;
        };
        let is_hole = self.poly_outs[out_rec].is_hole;
        if is_hole != self.poly_outs[first_left].is_hole && self.poly_outs[first_left].pts.is_some()
        {
            return;
        }

        let mut orfl = Some(first_left);
        while let Some(r) = orfl {
            if self.poly_outs[r].is_hole == is_hole || self.poly_outs[r].pts.is_none() {
                orfl = self.poly_outs[r].first_left;
            } else {
                break;
            }
        }
        self.poly_outs[out_rec].first_left = orfl;
    }

    pub(super) fn build_result2(&mut self, polytree: &mut PolyTree) {
        polytree.clear();

        //add each output polygon/contour to polytree ...
        for i in 0..self.poly_outs.len() {
            let cnt = self.point_count(self.poly_outs[i].pts);
            let is_open = self.poly_outs[i].is_open;
            if (is_open && cnt < 2) || (!is_open && cnt < 3) {
                continue;
            }
            self.fix_hole_linkage(i);
            let Some(pts) = self.poly_outs[i].pts else {
                continue;
            };
            let mut polygon = Vec::with_capacity(cnt);
            let mut op = self.out_pts[pts].prev;
            for _ in 0..cnt {
                polygon.push(self.out_pts[op].pt);
                op = self.out_pts[op].prev;
            }
            let pn = polytree.new_node(polygon);
            self.poly_outs[i].poly_node = Some(pn);
        }

        //fixup PolyNode links etc ...
        for i in 0..self.poly_outs.len() {
            let out_rec = self.poly_outs[i];
            let Some(node) = out_rec.poly_node else {
                continue;
            };
            if out_rec.is_open {
                polytree.nodes[node].is_open = true;
                polytree.add_child(PolyTree::ROOT, node);
            } else if let Some(parent) = out_rec
                .first_left
                .and_then(|first_left| self.poly_outs[first_left].poly_node)
            {
                polytree.add_child(parent, node);
            } else {
                polytree.add_child(PolyTree::ROOT, node);
            }
        }
    }

    pub(super) fn fixup_out_polyline(&mut self, out_rec: usize) {
        let Some(mut pp) = self.poly_outs[out_rec].pts else {
            return;
        };
        let mut last_pp = self.out_pts[pp].prev;
        while pp != last_pp {
            pp = self.out_pts[pp].next;
            let prev = self.out_pts[pp].prev;
            if self.out_pts[pp].pt == self.out_pts[prev].pt {
                if pp == last_pp {
                    last_pp = prev;
                }
                let tmp_pp = prev;
                let next = self.out_pts[pp].next;
                self.out_pts[tmp_pp].next = next;
                self.out_pts[next].prev = tmp_pp;
                pp = tmp_pp;
            }
        }
        if pp == self.out_pts[pp].prev {
            self.poly_outs[out_rec].pts = None;
        }
    }

    /// Removes duplicate points and simplifies consecutive parallel
    /// edges by removing the middle vertex.
    pub(super) fn fixup_out_polygon(&mut self, out_rec: usize) {
        let mut last_ok: Option<usize> = None;
        self.poly_outs[out_rec].bottom_pt = None;
        let Some(mut pp) = self.poly_outs[out_rec].pts else {
            return;
        };
        let preserve_col = self.preserve_collinear || self.strictly_simple;
        loop {
            let (prev, next) = (self.out_pts[pp].prev, self.out_pts[pp].next);
            if prev == pp || prev == next {
                self.poly_outs[out_rec].pts = None;
                return;
            }
            //test for duplicate points and collinear edges ...
            let (pt_prev, pt, pt_next) = (
                self.out_pts[prev].pt,
                self.out_pts[pp].pt,
                self.out_pts[next].pt,
            );
            if pt == pt_next
                || pt == pt_prev
                || (slopes_equal3(pt_prev, pt, pt_next, self.use_full_range)
                    && (!preserve_col || !pt2_is_between_pt1_and_pt3(pt_prev, pt, pt_next)))
            {
                last_ok = None;
                self.out_pts[prev].next = next;
                self.out_pts[next].prev = prev;
                pp = prev;
            } else if Some(pp) == last_ok {
                break;
            } else {
                if last_ok.is_none() {
                    last_ok = Some(pp);
                }
                pp = next;
            }
        }
        self.poly_outs[out_rec].pts = Some(pp);
    }

    pub(super) fn dup_out_pt(&mut self, out_pt: usize, insert_after: bool) -> usize {
        let OutPt { idx, pt, .. } = self.out_pts[out_pt];
        let result = self.new_out_pt(idx, pt);
        if insert_after {
            let next = self.out_pts[out_pt].next;
            self.out_pts[result].next = next;
            self.out_pts[result].prev = out_pt;
            self.out_pts[next].prev = result;
            self.out_pts[out_pt].next = result;
        } else {
            let prev = self.out_pts[out_pt].prev;
            self.out_pts[result].prev = prev;
            self.out_pts[result].next = out_pt;
            self.out_pts[prev].next = result;
            self.out_pts[out_pt].prev = result;
        }
        result
    }

    /// The area of the ring starting at `op`. Unlike the static `Area`, it sums
    /// in 64-bit integers before converting, and is positive for clockwise (y up) rings.
    pub(super) fn area_out_pt(&self, op: Option<usize>) -> f64 {
        let Some(op_first) = op else { return 0.0 };
        let mut op = op_first;
        let mut a = 0.0;
        loop {
            let prev = self.out_pts[self.out_pts[op].prev].pt;
            let pt = self.out_pts[op].pt;
            a += prev.x.wrapping_add(pt.x) as f64 * prev.y.wrapping_sub(pt.y) as f64;
            op = self.out_pts[op].next;
            if op == op_first {
                break;
            }
        }
        a * 0.5
    }
}

fn get_dx(pt1: IntPoint, pt2: IntPoint) -> f64 {
    if pt1.y == pt2.y {
        HORIZONTAL
    } else {
        (pt2.x - pt1.x) as f64 / (pt2.y - pt1.y) as f64
    }
}

#[cfg(test)]
#[path = "join_fixtures.rs"]
mod join_fixtures;
