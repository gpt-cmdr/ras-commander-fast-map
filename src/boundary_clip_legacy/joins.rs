//! Clipper 6's ring joining and splitting: `JoinCommonEdges` with
//! `JoinPoints`/`JoinHorz` (rings that share an edge are merged, or one ring is split in two),
//! the FirstLeft fix-ups that keep a PolyTree's nesting right afterwards, point-in-ring tests on
//! output rings, and `DoSimplePolygons` (`StrictlySimple`: split rings that touch at a vertex).

use super::clipper_base::slopes_equal3;
use super::engine::Clipper;
use super::types::{ClipperError, Direction, IntPoint};

/// The overlap of two x ranges, if any.
fn get_overlap(a1: i64, a2: i64, b1: i64, b2: i64) -> Option<(i64, i64)> {
    let (left, right) = if a1 < a2 {
        if b1 < b2 {
            (a1.max(b1), a2.min(b2))
        } else {
            (a1.max(b2), a2.min(b1))
        }
    } else if b1 < b2 {
        (a2.max(b1), a1.min(b2))
    } else {
        (a2.max(b2), a1.min(b1))
    };
    (left < right).then_some((left, right))
}

impl Clipper {
    /// One side of `JoinHorz`: walks `op` along the horizontal to `pt` and duplicates it there,
    /// returning the (possibly moved) point and its duplicate.
    fn join_horz_side(
        &mut self,
        mut op: usize,
        dir: Direction,
        pt: IntPoint,
        discard_left: bool,
    ) -> (usize, usize) {
        let mut opb;
        if dir == Direction::LeftToRight {
            loop {
                let next = self.out_pts[self.out_pts[op].next].pt;
                if next.x <= pt.x && next.x >= self.out_pts[op].pt.x && next.y == pt.y {
                    op = self.out_pts[op].next;
                } else {
                    break;
                }
            }
            if discard_left && self.out_pts[op].pt.x != pt.x {
                op = self.out_pts[op].next;
            }
            opb = self.dup_out_pt(op, !discard_left);
            if self.out_pts[opb].pt != pt {
                op = opb;
                self.out_pts[op].pt = pt;
                opb = self.dup_out_pt(op, !discard_left);
            }
        } else {
            loop {
                let next = self.out_pts[self.out_pts[op].next].pt;
                if next.x >= pt.x && next.x <= self.out_pts[op].pt.x && next.y == pt.y {
                    op = self.out_pts[op].next;
                } else {
                    break;
                }
            }
            if !discard_left && self.out_pts[op].pt.x != pt.x {
                op = self.out_pts[op].next;
            }
            opb = self.dup_out_pt(op, discard_left);
            if self.out_pts[opb].pt != pt {
                op = opb;
                self.out_pts[op].pt = pt;
                opb = self.dup_out_pt(op, discard_left);
            }
        }
        (op, opb)
    }

    fn join_horz(
        &mut self,
        op1: usize,
        op1b: usize,
        op2: usize,
        op2b: usize,
        pt: IntPoint,
        discard_left: bool,
    ) -> bool {
        let direction = |a: usize, b: usize| {
            if self.out_pts[a].pt.x > self.out_pts[b].pt.x {
                Direction::RightToLeft
            } else {
                Direction::LeftToRight
            }
        };
        let dir1 = direction(op1, op1b);
        let dir2 = direction(op2, op2b);
        if dir1 == dir2 {
            return false;
        }

        //When DiscardLeft, we want Op1b to be on the Left of Op1, otherwise we
        //want Op1b to be on the Right. (And likewise with Op2 and Op2b.)
        //So, to facilitate this while inserting Op1b and Op2b ...
        //when DiscardLeft, make sure we're AT or RIGHT of Pt before adding Op1b,
        //otherwise make sure we're AT or LEFT of Pt. (Likewise with Op2b.)
        let (op1, op1b) = self.join_horz_side(op1, dir1, pt, discard_left);
        let (op2, op2b) = self.join_horz_side(op2, dir2, pt, discard_left);

        if (dir1 == Direction::LeftToRight) == discard_left {
            self.out_pts[op1].prev = op2;
            self.out_pts[op2].next = op1;
            self.out_pts[op1b].next = op2b;
            self.out_pts[op2b].prev = op1b;
        } else {
            self.out_pts[op1].next = op2;
            self.out_pts[op2].prev = op1;
            self.out_pts[op1b].prev = op2b;
            self.out_pts[op2b].next = op1b;
        }
        true
    }

    /// The closing link-up shared by `JoinPoints`' strictly-simple and non-horizontal joins.
    fn link_join(&mut self, join: usize, op1: usize, op2: usize, reverse1: bool) {
        let op1b;
        if reverse1 {
            op1b = self.dup_out_pt(op1, false);
            let op2b = self.dup_out_pt(op2, true);
            self.out_pts[op1].prev = op2;
            self.out_pts[op2].next = op1;
            self.out_pts[op1b].next = op2b;
            self.out_pts[op2b].prev = op1b;
        } else {
            op1b = self.dup_out_pt(op1, true);
            let op2b = self.dup_out_pt(op2, false);
            self.out_pts[op1].next = op2;
            self.out_pts[op2].prev = op1;
            self.out_pts[op1b].prev = op2b;
            self.out_pts[op2b].next = op1b;
        }
        self.joins[join].out_pt1 = op1;
        self.joins[join].out_pt2 = op1b;
    }

    fn join_points(&mut self, join: usize, out_rec1: usize, out_rec2: usize) -> bool {
        let j = self.joins[join];
        let mut op1 = j.out_pt1;
        let mut op2 = j.out_pt2;

        //There are 3 kinds of joins for output polygons ...
        //1. Horizontal joins where Join.OutPt1 & Join.OutPt2 are vertices anywhere
        //along (horizontal) collinear edges (& Join.OffPt is on the same horizontal).
        //2. Non-horizontal joins where Join.OutPt1 & Join.OutPt2 are at the same
        //location at the Bottom of the overlapping segment (& Join.OffPt is above).
        //3. StrictlySimple joins where edges touch but are not collinear and where
        //Join.OutPt1, Join.OutPt2 & Join.OffPt all share the same point.
        let is_horizontal = self.out_pts[j.out_pt1].pt.y == j.off_pt.y;

        if is_horizontal
            && j.off_pt == self.out_pts[j.out_pt1].pt
            && j.off_pt == self.out_pts[j.out_pt2].pt
        {
            //Strictly Simple join ...
            if out_rec1 != out_rec2 {
                return false;
            }
            let mut op1b = self.out_pts[j.out_pt1].next;
            while op1b != op1 && self.out_pts[op1b].pt == j.off_pt {
                op1b = self.out_pts[op1b].next;
            }
            let reverse1 = self.out_pts[op1b].pt.y > j.off_pt.y;
            let mut op2b = self.out_pts[j.out_pt2].next;
            while op2b != op2 && self.out_pts[op2b].pt == j.off_pt {
                op2b = self.out_pts[op2b].next;
            }
            let reverse2 = self.out_pts[op2b].pt.y > j.off_pt.y;
            if reverse1 == reverse2 {
                return false;
            }
            self.link_join(join, op1, op2, reverse1);
            true
        } else if is_horizontal {
            //treat horizontal joins differently to non-horizontal joins since with
            //them we're not yet sure where the overlapping is. OutPt1.Pt & OutPt2.Pt
            //may be anywhere along the horizontal edge.
            let pts = &self.out_pts;
            let mut op1b = op1;
            while pts[pts[op1].prev].pt.y == pts[op1].pt.y
                && pts[op1].prev != op1b
                && pts[op1].prev != op2
            {
                op1 = pts[op1].prev;
            }
            while pts[pts[op1b].next].pt.y == pts[op1b].pt.y
                && pts[op1b].next != op1
                && pts[op1b].next != op2
            {
                op1b = pts[op1b].next;
            }
            if pts[op1b].next == op1 || pts[op1b].next == op2 {
                return false; //a flat 'polygon'
            }

            let mut op2b = op2;
            while pts[pts[op2].prev].pt.y == pts[op2].pt.y
                && pts[op2].prev != op2b
                && pts[op2].prev != op1b
            {
                op2 = pts[op2].prev;
            }
            while pts[pts[op2b].next].pt.y == pts[op2b].pt.y
                && pts[op2b].next != op2
                && pts[op2b].next != op1
            {
                op2b = pts[op2b].next;
            }
            if pts[op2b].next == op2 || pts[op2b].next == op1 {
                return false; //a flat 'polygon'
            }

            //Op1 -. Op1b & Op2 -. Op2b are the extremites of the horizontal edges
            let (p1, p1b, p2, p2b) = (pts[op1].pt, pts[op1b].pt, pts[op2].pt, pts[op2b].pt);
            let Some((left, right)) = get_overlap(p1.x, p1b.x, p2.x, p2b.x) else {
                return false;
            };

            //DiscardLeftSide: when overlapping edges are joined, a spike will created
            //which needs to be cleaned up. However, we don't want Op1 or Op2 caught up
            //on the discard Side as either may still be needed for other joins ...
            let (pt, discard_left_side) = if p1.x >= left && p1.x <= right {
                (p1, p1.x > p1b.x)
            } else if p2.x >= left && p2.x <= right {
                (p2, p2.x > p2b.x)
            } else if p1b.x >= left && p1b.x <= right {
                (p1b, p1b.x > p1.x)
            } else {
                (p2b, p2b.x > p2.x)
            };
            self.joins[join].out_pt1 = op1;
            self.joins[join].out_pt2 = op2;
            self.join_horz(op1, op1b, op2, op2b, pt, discard_left_side)
        } else {
            //nb: For non-horizontal joins ...
            //    1. Jr.OutPt1.Pt.Y == Jr.OutPt2.Pt.Y
            //    2. Jr.OutPt1.Pt > Jr.OffPt.Y

            //make sure the polygons are correctly oriented ...
            let Some((op1b, reverse1)) = self.oriented_partner(op1, j.off_pt) else {
                return false;
            };
            let Some((op2b, reverse2)) = self.oriented_partner(op2, j.off_pt) else {
                return false;
            };

            if op1b == op1
                || op2b == op2
                || op1b == op2b
                || (out_rec1 == out_rec2 && reverse1 == reverse2)
            {
                return false;
            }

            self.link_join(join, op1, op2, reverse1);
            true
        }
    }

    /// The next distinct point from `op` running up toward `off_pt`, forwards if that works,
    /// else backwards (reversed); `None` when neither direction does.
    fn oriented_partner(&self, op: usize, off_pt: IntPoint) -> Option<(usize, bool)> {
        let pts = &self.out_pts;
        let mut opb = pts[op].next;
        while pts[opb].pt == pts[op].pt && opb != op {
            opb = pts[opb].next;
        }
        let reverse = pts[opb].pt.y > pts[op].pt.y
            || !slopes_equal3(pts[op].pt, pts[opb].pt, off_pt, self.use_full_range);
        if reverse {
            opb = pts[op].prev;
            while pts[opb].pt == pts[op].pt && opb != op {
                opb = pts[opb].prev;
            }
            if pts[opb].pt.y > pts[op].pt.y
                || !slopes_equal3(pts[op].pt, pts[opb].pt, off_pt, self.use_full_range)
            {
                return None;
            }
        }
        Some((opb, reverse))
    }

    /// 0 outside, +1 inside, -1 on the boundary.
    fn point_in_out_polygon(&self, pt: IntPoint, mut op: usize) -> i32 {
        let pts = &self.out_pts;
        let mut result = 0;
        let start_op = op;
        let (ptx, pty) = (pt.x, pt.y);
        let (mut poly0x, mut poly0y) = (pts[op].pt.x, pts[op].pt.y);
        loop {
            op = pts[op].next;
            let (poly1x, poly1y) = (pts[op].pt.x, pts[op].pt.y);

            if poly1y == pty
                && (poly1x == ptx || (poly0y == pty && ((poly1x > ptx) == (poly0x < ptx))))
            {
                return -1;
            }
            if (poly0y < pty) != (poly1y < pty) {
                if poly0x >= ptx {
                    if poly1x > ptx {
                        result = 1 - result;
                    } else {
                        let d = (poly0x - ptx) as f64 * (poly1y - pty) as f64
                            - (poly1x - ptx) as f64 * (poly0y - pty) as f64;
                        if d == 0.0 {
                            return -1;
                        }
                        if (d > 0.0) == (poly1y > poly0y) {
                            result = 1 - result;
                        }
                    }
                } else if poly1x > ptx {
                    let d = (poly0x - ptx) as f64 * (poly1y - pty) as f64
                        - (poly1x - ptx) as f64 * (poly0y - pty) as f64;
                    if d == 0.0 {
                        return -1;
                    }
                    if (d > 0.0) == (poly1y > poly0y) {
                        result = 1 - result;
                    }
                }
            }
            poly0x = poly1x;
            poly0y = poly1y;
            if start_op == op {
                break;
            }
        }
        result
    }

    fn poly2_contains_poly1(&self, out_pt1: usize, out_pt2: usize) -> bool {
        let mut op = out_pt1;
        loop {
            //nb: PointInPolygon returns 0 if false, +1 if true, -1 if pt on polygon
            let res = self.point_in_out_polygon(self.out_pts[op].pt, out_pt2);
            if res >= 0 {
                return res > 0;
            }
            op = self.out_pts[op].next;
            if op == out_pt1 {
                break;
            }
        }
        true
    }

    fn fixup_first_lefts1(
        &mut self,
        old_out_rec: usize,
        new_out_rec: usize,
    ) -> Result<(), ClipperError> {
        for i in 0..self.poly_outs.len() {
            let first_left = self.parse_first_left(self.poly_outs[i].first_left);
            if let Some(pts) = self.poly_outs[i].pts {
                if first_left == Some(old_out_rec)
                    && self.poly2_contains_poly1(pts, self.ring_pts(new_out_rec)?)
                {
                    self.poly_outs[i].first_left = Some(new_out_rec);
                }
            }
        }
        Ok(())
    }

    /// A polygon has split into two such that one is now the inner of the other. It's possible
    /// that these polygons now wrap around other polygons, so check every polygon that's also
    /// contained by OuterOutRec's FirstLeft container (including nil) to see if they've become
    /// inner to the new inner polygon ...
    fn fixup_first_lefts2(
        &mut self,
        inner_out_rec: usize,
        outer_out_rec: usize,
    ) -> Result<(), ClipperError> {
        let orfl = self.poly_outs[outer_out_rec].first_left;
        for i in 0..self.poly_outs.len() {
            let Some(pts) = self.poly_outs[i].pts else {
                continue;
            };
            if i == outer_out_rec || i == inner_out_rec {
                continue;
            }
            let first_left = self.parse_first_left(self.poly_outs[i].first_left);
            if first_left != orfl
                && first_left != Some(inner_out_rec)
                && first_left != Some(outer_out_rec)
            {
                continue;
            }
            if self.poly2_contains_poly1(pts, self.ring_pts(inner_out_rec)?) {
                self.poly_outs[i].first_left = Some(inner_out_rec);
            } else if self.poly2_contains_poly1(pts, self.ring_pts(outer_out_rec)?) {
                self.poly_outs[i].first_left = Some(outer_out_rec);
            } else if self.poly_outs[i].first_left == Some(inner_out_rec)
                || self.poly_outs[i].first_left == Some(outer_out_rec)
            {
                self.poly_outs[i].first_left = orfl;
            }
        }
        Ok(())
    }

    /// Same as FixupFirstLefts1 but doesn't call Poly2ContainsPoly1().
    fn fixup_first_lefts3(&mut self, old_out_rec: usize, new_out_rec: usize) {
        for i in 0..self.poly_outs.len() {
            let first_left = self.parse_first_left(self.poly_outs[i].first_left);
            if self.poly_outs[i].pts.is_some() && first_left == Some(old_out_rec) {
                self.poly_outs[i].first_left = Some(new_out_rec);
            }
        }
    }

    fn parse_first_left(&self, mut first_left: Option<usize>) -> Option<usize> {
        while let Some(fl) = first_left {
            if self.poly_outs[fl].pts.is_some() {
                break;
            }
            first_left = self.poly_outs[fl].first_left;
        }
        first_left
    }

    /// After splitting `out_rec1` into it and the new `out_rec2`, decide which (if either)
    /// contains the other and fix hole states and FirstLeft links (shared by
    /// `JoinCommonEdges` and `DoSimplePolygons`). Returns which ring may need reorienting.
    fn settle_split(
        &mut self,
        out_rec1: usize,
        out_rec2: usize,
    ) -> Result<Option<usize>, ClipperError> {
        let (pts1, pts2) = (self.ring_pts(out_rec1)?, self.ring_pts(out_rec2)?);
        if self.poly2_contains_poly1(pts2, pts1) {
            //outRec1 contains outRec2 ...
            self.poly_outs[out_rec2].is_hole = !self.poly_outs[out_rec1].is_hole;
            self.poly_outs[out_rec2].first_left = Some(out_rec1);
            if self.using_poly_tree {
                self.fixup_first_lefts2(out_rec2, out_rec1)?;
            }
            Ok(Some(out_rec2))
        } else if self.poly2_contains_poly1(pts1, pts2) {
            //outRec2 contains outRec1 ...
            self.poly_outs[out_rec2].is_hole = self.poly_outs[out_rec1].is_hole;
            self.poly_outs[out_rec1].is_hole = !self.poly_outs[out_rec2].is_hole;
            self.poly_outs[out_rec2].first_left = self.poly_outs[out_rec1].first_left;
            self.poly_outs[out_rec1].first_left = Some(out_rec2);
            if self.using_poly_tree {
                self.fixup_first_lefts2(out_rec1, out_rec2)?;
            }
            Ok(Some(out_rec1))
        } else {
            //the 2 polygons are completely separate ...
            self.poly_outs[out_rec2].is_hole = self.poly_outs[out_rec1].is_hole;
            self.poly_outs[out_rec2].first_left = self.poly_outs[out_rec1].first_left;
            //fixup FirstLeft pointers that may need reassigning to OutRec2
            if self.using_poly_tree {
                self.fixup_first_lefts1(out_rec1, out_rec2)?;
            }
            Ok(None)
        }
    }

    pub(super) fn join_common_edges(&mut self) -> Result<(), ClipperError> {
        for i in 0..self.joins.len() {
            let join = self.joins[i];

            let out_rec1 = self.get_out_rec(self.out_pts[join.out_pt1].idx);
            let mut out_rec2 = self.get_out_rec(self.out_pts[join.out_pt2].idx);

            if self.poly_outs[out_rec1].pts.is_none() || self.poly_outs[out_rec2].pts.is_none() {
                continue;
            }
            if self.poly_outs[out_rec1].is_open || self.poly_outs[out_rec2].is_open {
                continue;
            }

            //get the polygon fragment with the correct hole state (FirstLeft)
            //before calling JoinPoints() ...
            let hole_state_rec = if out_rec1 == out_rec2 {
                out_rec1
            } else if self.out_rec1_right_of_out_rec2(out_rec1, out_rec2) {
                out_rec2
            } else if self.out_rec1_right_of_out_rec2(out_rec2, out_rec1) {
                out_rec1
            } else {
                self.get_lowermost_rec(out_rec1, out_rec2)?
            };

            if !self.join_points(i, out_rec1, out_rec2) {
                continue;
            }

            if out_rec1 == out_rec2 {
                //instead of joining two polygons, we've just created a new one by
                //splitting one polygon into two.
                self.poly_outs[out_rec1].pts = Some(self.joins[i].out_pt1);
                self.poly_outs[out_rec1].bottom_pt = None;
                out_rec2 = self.create_out_rec();
                self.poly_outs[out_rec2].pts = Some(self.joins[i].out_pt2);

                //update all OutRec2.Pts Idx's ...
                self.update_out_pt_idxs(out_rec2)?;

                if let Some(inner) = self.settle_split(out_rec1, out_rec2)? {
                    let pts = self.poly_outs[inner].pts;
                    if (self.poly_outs[inner].is_hole ^ self.reverse_solution)
                        == (self.area_out_pt(pts) > 0.0)
                    {
                        self.reverse_poly_pt_links(pts);
                    }
                }
            } else {
                //joined 2 polygons together ...

                self.poly_outs[out_rec2].pts = None;
                self.poly_outs[out_rec2].bottom_pt = None;
                self.poly_outs[out_rec2].idx = self.poly_outs[out_rec1].idx;

                self.poly_outs[out_rec1].is_hole = self.poly_outs[hole_state_rec].is_hole;
                if hole_state_rec == out_rec2 {
                    self.poly_outs[out_rec1].first_left = self.poly_outs[out_rec2].first_left;
                }
                self.poly_outs[out_rec2].first_left = Some(out_rec1);

                //fixup FirstLeft pointers that may need reassigning to OutRec1
                if self.using_poly_tree {
                    self.fixup_first_lefts3(out_rec2, out_rec1);
                }
            }
        }
        Ok(())
    }

    fn update_out_pt_idxs(&mut self, out_rec: usize) -> Result<(), ClipperError> {
        let idx = self.poly_outs[out_rec].idx;
        let start = self.ring_pts(out_rec)?;
        let mut op = start;
        loop {
            self.out_pts[op].idx = idx;
            op = self.out_pts[op].prev;
            if op == start {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn do_simple_polygons(&mut self) -> Result<(), ClipperError> {
        let mut i = 0;
        while i < self.poly_outs.len() {
            let out_rec = i;
            i += 1;
            let Some(mut op) = self.poly_outs[out_rec].pts else {
                continue;
            };
            if self.poly_outs[out_rec].is_open {
                continue;
            }
            loop {
                //for each Pt in Polygon until duplicate found do ...
                let mut op2 = self.out_pts[op].next;
                while Some(op2) != self.poly_outs[out_rec].pts {
                    if self.out_pts[op].pt == self.out_pts[op2].pt
                        && self.out_pts[op2].next != op
                        && self.out_pts[op2].prev != op
                    {
                        //split the polygon into two ...
                        let op3 = self.out_pts[op].prev;
                        let op4 = self.out_pts[op2].prev;
                        self.out_pts[op].prev = op4;
                        self.out_pts[op4].next = op;
                        self.out_pts[op2].prev = op3;
                        self.out_pts[op3].next = op2;

                        self.poly_outs[out_rec].pts = Some(op);
                        let out_rec2 = self.create_out_rec();
                        self.poly_outs[out_rec2].pts = Some(op2);
                        self.update_out_pt_idxs(out_rec2)?;
                        self.settle_split(out_rec, out_rec2)?;
                        op2 = op; //ie get ready for the next iteration
                    }
                    op2 = self.out_pts[op2].next;
                }
                op = self.out_pts[op].next;
                if Some(op) == self.poly_outs[out_rec].pts {
                    break;
                }
            }
        }
        Ok(())
    }
}
