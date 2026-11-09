//! Clipper 6's `ClipperBase`: turning input paths into edge bounds and local
//! minima (`AddPath`, `ProcessBound`, `FindNextLocMin`), the local-minima and scanbeam lists,
//! `Reset`, and the active and sorted edge lists (AEL/SEL) the sweep in [`super::engine`] walks.

use super::engine::Clipper;
use super::types::{
    ClipperError, EdgeSide, HI_RANGE, HORIZONTAL, IntPoint, LO_RANGE, LocalMinima, PolyType, SKIP,
    Scanbeam, TEdge, UNASSIGNED, int128_mul,
};

pub(super) fn is_horizontal(e: &TEdge) -> bool {
    e.delta.y == 0
}

pub(super) fn slopes_equal_edges(e1: &TEdge, e2: &TEdge, use_full_range: bool) -> bool {
    if use_full_range {
        int128_mul(e1.delta.y, e2.delta.x) == int128_mul(e1.delta.x, e2.delta.y)
    } else {
        e1.delta.y.wrapping_mul(e2.delta.x) == e1.delta.x.wrapping_mul(e2.delta.y)
    }
}

pub(super) fn slopes_equal3(
    pt1: IntPoint,
    pt2: IntPoint,
    pt3: IntPoint,
    use_full_range: bool,
) -> bool {
    if use_full_range {
        int128_mul(pt1.y - pt2.y, pt2.x - pt3.x) == int128_mul(pt1.x - pt2.x, pt2.y - pt3.y)
    } else {
        (pt1.y - pt2.y)
            .wrapping_mul(pt2.x - pt3.x)
            .wrapping_sub((pt1.x - pt2.x).wrapping_mul(pt2.y - pt3.y))
            == 0
    }
}

pub(super) fn slopes_equal4(
    pt1: IntPoint,
    pt2: IntPoint,
    pt3: IntPoint,
    pt4: IntPoint,
    use_full_range: bool,
) -> bool {
    if use_full_range {
        int128_mul(pt1.y - pt2.y, pt3.x - pt4.x) == int128_mul(pt1.x - pt2.x, pt3.y - pt4.y)
    } else {
        (pt1.y - pt2.y)
            .wrapping_mul(pt3.x - pt4.x)
            .wrapping_sub((pt1.x - pt2.x).wrapping_mul(pt3.y - pt4.y))
            == 0
    }
}

pub(super) fn pt2_is_between_pt1_and_pt3(pt1: IntPoint, pt2: IntPoint, pt3: IntPoint) -> bool {
    if pt1 == pt3 || pt1 == pt2 || pt3 == pt2 {
        false
    } else if pt1.x != pt3.x {
        (pt2.x > pt1.x) == (pt2.x < pt3.x)
    } else {
        (pt2.y > pt1.y) == (pt2.y < pt3.y)
    }
}

fn range_test(pt: IntPoint, use_full_range: &mut bool) -> Result<(), ClipperError> {
    if *use_full_range {
        if pt.x > HI_RANGE || pt.y > HI_RANGE || -pt.x > HI_RANGE || -pt.y > HI_RANGE {
            return Err(ClipperError::CoordinateOutsideRange);
        }
    } else if pt.x > LO_RANGE || pt.y > LO_RANGE || -pt.x > LO_RANGE || -pt.y > LO_RANGE {
        *use_full_range = true;
        range_test(pt, use_full_range)?;
    }
    Ok(())
}

impl Clipper {
    /// False when the path has too few distinct points.
    pub fn add_path(
        &mut self,
        pg: &[IntPoint],
        poly_type: PolyType,
        closed: bool,
    ) -> Result<bool, ClipperError> {
        if !closed && poly_type == PolyType::Clip {
            return Err(ClipperError::OpenPathMustBeSubject);
        }

        let mut high_i = pg.len() as i32 - 1;
        if closed {
            while high_i > 0 && pg[high_i as usize] == pg[0] {
                high_i -= 1;
            }
        }
        while high_i > 0 && pg[high_i as usize] == pg[high_i as usize - 1] {
            high_i -= 1;
        }
        if (closed && high_i < 2) || (!closed && high_i < 1) {
            return Ok(false);
        }
        let high_i = high_i as usize;

        //create a new edge array ...
        let base = self.edges.len();
        for _ in 0..=high_i {
            self.edges.push(TEdge::new());
        }
        let edge = |i: usize| base + i;

        let mut is_flat = true;

        //1. Basic (first) edge initialization ...
        self.edges[edge(1)].curr = pg[1];
        range_test(pg[0], &mut self.use_full_range)?;
        range_test(pg[high_i], &mut self.use_full_range)?;
        self.init_edge(edge(0), edge(1), edge(high_i), pg[0]);
        self.init_edge(edge(high_i), edge(0), edge(high_i - 1), pg[high_i]);
        for i in (1..high_i).rev() {
            range_test(pg[i], &mut self.use_full_range)?;
            self.init_edge(edge(i), edge(i + 1), edge(i - 1), pg[i]);
        }
        let mut e_start = edge(0);

        //2. Remove duplicate vertices, and (when closed) collinear edges ...
        let mut e = e_start;
        let mut e_loop_stop = e_start;
        loop {
            //nb: allows matching start and end points when not Closed ...
            let next = self.edges[e].next;
            if self.edges[e].curr == self.edges[next].curr && (closed || next != e_start) {
                if e == next {
                    break;
                }
                if e == e_start {
                    e_start = next;
                }
                e = self.remove_edge(e);
                e_loop_stop = e;
                continue;
            }
            let prev = self.edges[e].prev;
            if prev == next {
                break; //only two vertices
            } else if closed
                && slopes_equal3(
                    self.edges[prev].curr,
                    self.edges[e].curr,
                    self.edges[next].curr,
                    self.use_full_range,
                )
                && (!self.preserve_collinear
                    || !pt2_is_between_pt1_and_pt3(
                        self.edges[prev].curr,
                        self.edges[e].curr,
                        self.edges[next].curr,
                    ))
            {
                //Collinear edges are allowed for open paths but in closed paths
                //the default is to merge adjacent collinear edges into a single edge.
                //However, if the PreserveCollinear property is enabled, only overlapping
                //collinear edges (ie spikes) will be removed from closed paths.
                if e == e_start {
                    e_start = next;
                }
                e = self.remove_edge(e);
                e = self.edges[e].prev;
                e_loop_stop = e;
                continue;
            }
            e = next;
            if e == e_loop_stop || (!closed && self.edges[e].next == e_start) {
                break;
            }
        }

        if (!closed && e == self.edges[e].next)
            || (closed && self.edges[e].prev == self.edges[e].next)
        {
            return Ok(false);
        }

        if !closed {
            self.has_open_paths = true;
            let prev = self.edges[e_start].prev;
            self.edges[prev].out_idx = SKIP;
        }

        //3. Do second stage of edge initialization ...
        e = e_start;
        loop {
            self.init_edge2(e, poly_type);
            e = self.edges[e].next;
            if is_flat && self.edges[e].curr.y != self.edges[e_start].curr.y {
                is_flat = false;
            }
            if e == e_start {
                break;
            }
        }

        //4. Finally, add edge bounds to LocalMinima list ...

        //Totally flat paths must be handled differently when adding them
        //to LocalMinima list to avoid endless loops etc ...
        if is_flat {
            if closed {
                return Ok(false);
            }
            let prev = self.edges[e].prev;
            self.edges[prev].out_idx = SKIP;
            let loc_min = LocalMinima {
                y: self.edges[e].bot.y,
                left_bound: None,
                right_bound: Some(e),
                next: None,
            };
            self.edges[e].side = EdgeSide::Right;
            self.edges[e].wind_delta = 0;
            loop {
                let prev = self.edges[e].prev;
                if self.edges[e].bot.x != self.edges[prev].top.x {
                    self.reverse_horizontal(e);
                }
                let next = self.edges[e].next;
                if self.edges[next].out_idx == SKIP {
                    break;
                }
                self.edges[e].next_in_lml = Some(next);
                e = next;
            }
            self.insert_local_minima(loc_min);
            return Ok(true);
        }

        let mut left_bound_is_forward;
        let mut e_min: Option<usize> = None;

        //workaround to avoid an endless loop in the while loop below when
        //open paths have matching start and end points ...
        let prev = self.edges[e].prev;
        if self.edges[prev].bot == self.edges[prev].top {
            e = self.edges[e].next;
        }

        loop {
            e = self.find_next_loc_min(e);
            if Some(e) == e_min {
                break;
            } else if e_min.is_none() {
                e_min = Some(e);
            }

            //E and E.Prev now share a local minima (left aligned if horizontal).
            //Compare their slopes to find which starts which bound ...
            let prev = self.edges[e].prev;
            let (left, right);
            if self.edges[e].dx < self.edges[prev].dx {
                left = prev;
                right = e;
                left_bound_is_forward = false; //Q.nextInLML = Q.prev
            } else {
                left = e;
                right = prev;
                left_bound_is_forward = true; //Q.nextInLML = Q.next
            }
            let mut loc_min = LocalMinima {
                y: self.edges[e].bot.y,
                left_bound: Some(left),
                right_bound: Some(right),
                next: None,
            };
            self.edges[left].side = EdgeSide::Left;
            self.edges[right].side = EdgeSide::Right;

            if !closed {
                self.edges[left].wind_delta = 0;
            } else if self.edges[left].next == right {
                self.edges[left].wind_delta = -1;
            } else {
                self.edges[left].wind_delta = 1;
            }
            self.edges[right].wind_delta = -self.edges[left].wind_delta;

            e = self.process_bound(left, left_bound_is_forward);
            if self.edges[e].out_idx == SKIP {
                e = self.process_bound(e, left_bound_is_forward);
            }

            let mut e2 = self.process_bound(right, !left_bound_is_forward);
            if self.edges[e2].out_idx == SKIP {
                e2 = self.process_bound(e2, !left_bound_is_forward);
            }

            if self.edges[left].out_idx == SKIP {
                loc_min.left_bound = None;
            } else if self.edges[right].out_idx == SKIP {
                loc_min.right_bound = None;
            }
            self.insert_local_minima(loc_min);
            if !left_bound_is_forward {
                e = e2;
            }
        }
        Ok(true)
    }

    /// True when any path was added.
    pub fn add_paths(
        &mut self,
        ppg: &[Vec<IntPoint>],
        poly_type: PolyType,
        closed: bool,
    ) -> Result<bool, ClipperError> {
        let mut result = false;
        for pg in ppg {
            if self.add_path(pg, poly_type, closed)? {
                result = true;
            }
        }
        Ok(result)
    }

    fn init_edge(&mut self, e: usize, e_next: usize, e_prev: usize, pt: IntPoint) {
        let edge = &mut self.edges[e];
        edge.next = e_next;
        edge.prev = e_prev;
        edge.curr = pt;
        edge.out_idx = UNASSIGNED;
    }

    fn init_edge2(&mut self, e: usize, poly_type: PolyType) {
        let next_curr = self.edges[self.edges[e].next].curr;
        let edge = &mut self.edges[e];
        if edge.curr.y >= next_curr.y {
            edge.bot = edge.curr;
            edge.top = next_curr;
        } else {
            edge.top = edge.curr;
            edge.bot = next_curr;
        }
        set_dx(edge);
        edge.poly_typ = poly_type;
    }

    /// Unlinks `e` from its ring (the arena keeps it) and returns its next.
    fn remove_edge(&mut self, e: usize) -> usize {
        let (prev, next) = (self.edges[e].prev, self.edges[e].next);
        self.edges[prev].next = next;
        self.edges[next].prev = prev;
        next
    }

    fn find_next_loc_min(&self, mut e: usize) -> usize {
        let edges = &self.edges;
        loop {
            while edges[e].bot != edges[edges[e].prev].bot || edges[e].curr == edges[e].top {
                e = edges[e].next;
            }
            if edges[e].dx != HORIZONTAL && edges[edges[e].prev].dx != HORIZONTAL {
                break;
            }
            while edges[edges[e].prev].dx == HORIZONTAL {
                e = edges[e].prev;
            }
            let e2 = e;
            while edges[e].dx == HORIZONTAL {
                e = edges[e].next;
            }
            if edges[e].top.y == edges[edges[e].prev].bot.y {
                continue; //ie just an intermediate horz.
            }
            if edges[edges[e2].prev].bot.x < edges[e].bot.x {
                e = e2;
            }
            break;
        }
        e
    }

    fn process_bound(&mut self, mut e: usize, left_bound_is_forward: bool) -> usize {
        let mut result = e;

        if self.edges[result].out_idx == SKIP {
            //check if there are edges beyond the skip edge in the bound and if so
            //create another LocMin and calling ProcessBound once more ...
            e = result;
            if left_bound_is_forward {
                while self.edges[e].top.y == self.edges[self.edges[e].next].bot.y {
                    e = self.edges[e].next;
                }
                while e != result && self.edges[e].dx == HORIZONTAL {
                    e = self.edges[e].prev;
                }
            } else {
                while self.edges[e].top.y == self.edges[self.edges[e].prev].bot.y {
                    e = self.edges[e].prev;
                }
                while e != result && self.edges[e].dx == HORIZONTAL {
                    e = self.edges[e].next;
                }
            }
            if e == result {
                result = if left_bound_is_forward {
                    self.edges[e].next
                } else {
                    self.edges[e].prev
                };
            } else {
                //there are more edges in the bound beyond result starting with E
                e = if left_bound_is_forward {
                    self.edges[result].next
                } else {
                    self.edges[result].prev
                };
                let loc_min = LocalMinima {
                    y: self.edges[e].bot.y,
                    left_bound: None,
                    right_bound: Some(e),
                    next: None,
                };
                self.edges[e].wind_delta = 0;
                result = self.process_bound(e, left_bound_is_forward);
                self.insert_local_minima(loc_min);
            }
            return result;
        }

        if self.edges[e].dx == HORIZONTAL {
            //We need to be careful with open paths because this may not be a
            //true local minima (ie E may be following a skip edge).
            //Also, consecutive horz. edges may start heading left before going right.
            let e_start = if left_bound_is_forward {
                self.edges[e].prev
            } else {
                self.edges[e].next
            };
            if self.edges[e_start].dx == HORIZONTAL {
                //ie an adjoining horizontal skip edge
                if self.edges[e_start].bot.x != self.edges[e].bot.x
                    && self.edges[e_start].top.x != self.edges[e].bot.x
                {
                    self.reverse_horizontal(e);
                }
            } else if self.edges[e_start].bot.x != self.edges[e].bot.x {
                self.reverse_horizontal(e);
            }
        }

        let e_start = e;
        if left_bound_is_forward {
            while self.edges[result].top.y == self.edges[self.edges[result].next].bot.y
                && self.edges[self.edges[result].next].out_idx != SKIP
            {
                result = self.edges[result].next;
            }
            if self.edges[result].dx == HORIZONTAL
                && self.edges[self.edges[result].next].out_idx != SKIP
            {
                //nb: at the top of a bound, horizontals are added to the bound
                //only when the preceding edge attaches to the horizontal's left vertex
                //unless a Skip edge is encountered when that becomes the top divide
                let mut horz = result;
                while self.edges[self.edges[horz].prev].dx == HORIZONTAL {
                    horz = self.edges[horz].prev;
                }
                let horz_prev = self.edges[horz].prev;
                if self.edges[horz_prev].top.x > self.edges[self.edges[result].next].top.x {
                    result = horz_prev;
                }
            }
            while e != result {
                self.edges[e].next_in_lml = Some(self.edges[e].next);
                if self.edges[e].dx == HORIZONTAL
                    && e != e_start
                    && self.edges[e].bot.x != self.edges[self.edges[e].prev].top.x
                {
                    self.reverse_horizontal(e);
                }
                e = self.edges[e].next;
            }
            if self.edges[e].dx == HORIZONTAL
                && e != e_start
                && self.edges[e].bot.x != self.edges[self.edges[e].prev].top.x
            {
                self.reverse_horizontal(e);
            }
            result = self.edges[result].next; //move to the edge just beyond current bound
        } else {
            while self.edges[result].top.y == self.edges[self.edges[result].prev].bot.y
                && self.edges[self.edges[result].prev].out_idx != SKIP
            {
                result = self.edges[result].prev;
            }
            if self.edges[result].dx == HORIZONTAL
                && self.edges[self.edges[result].prev].out_idx != SKIP
            {
                let mut horz = result;
                while self.edges[self.edges[horz].next].dx == HORIZONTAL {
                    horz = self.edges[horz].next;
                }
                let horz_next = self.edges[horz].next;
                let result_prev_top_x = self.edges[self.edges[result].prev].top.x;
                if self.edges[horz_next].top.x >= result_prev_top_x {
                    result = horz_next;
                }
            }

            while e != result {
                self.edges[e].next_in_lml = Some(self.edges[e].prev);
                if self.edges[e].dx == HORIZONTAL
                    && e != e_start
                    && self.edges[e].bot.x != self.edges[self.edges[e].next].top.x
                {
                    self.reverse_horizontal(e);
                }
                e = self.edges[e].prev;
            }
            if self.edges[e].dx == HORIZONTAL
                && e != e_start
                && self.edges[e].bot.x != self.edges[self.edges[e].next].top.x
            {
                self.reverse_horizontal(e);
            }
            result = self.edges[result].prev; //move to the edge just beyond current bound
        }
        result
    }

    fn insert_local_minima(&mut self, mut new_lm: LocalMinima) {
        let new_index = self.minima.len();
        // AddPath never consumes this chain. Preserve arena/insertion indices,
        // and construct the exact classic ordered chain once at reset instead
        // of repeatedly walking it for descending source/tile coordinates.
        new_lm.next = self.minima_list;
        self.minima.push(new_lm);
        self.minima_list = Some(new_index);
    }

    fn order_local_minima(&mut self) -> Result<(), ClipperError> {
        // At most one minimum per input edge: this temporary index buffer uses
        // at most8 bytes/input vertex, within the caller's conservative256-byte
        // contour allowance. Sorting indices leaves every edge/arena ID intact.
        let mut order = Vec::new();
        order
            .try_reserve_exact(self.minima.len())
            .map_err(|_| ClipperError::LocalMinimaAllocation)?;
        order.extend(0..self.minima.len());
        order.sort_unstable_by(|&left, &right| {
            self.minima[right]
                .y
                .cmp(&self.minima[left].y)
                .then_with(|| right.cmp(&left))
        });
        for (position, &index) in order.iter().enumerate() {
            self.minima[index].next = order.get(position + 1).copied();
        }
        self.minima_list = order.first().copied();
        Ok(())
    }

    pub(super) fn pop_local_minima(&mut self, y: i64) -> Option<LocalMinima> {
        let current = self.current_lm?;
        if self.minima[current].y == y {
            self.current_lm = self.minima[current].next;
            return Some(self.minima[current]);
        }
        None
    }

    /// Swap horizontal edges' top and bottom x's so they follow the
    /// natural progression of the bounds - ie so their xbots will align with the adjoining
    /// lower edge. [Helpful in the ProcessHorizontal() method.]
    fn reverse_horizontal(&mut self, e: usize) {
        let edge = &mut self.edges[e];
        std::mem::swap(&mut edge.top.x, &mut edge.bot.x);
    }

    pub(super) fn reset(&mut self) -> Result<(), ClipperError> {
        self.order_local_minima()?;
        self.current_lm = self.minima_list;
        if self.current_lm.is_none() {
            return Ok(()); //ie nothing to process
        }

        //reset all edges ...
        self.scanbeam_list = None;
        self.scanbeams.clear();
        let mut last_scanbeam = None;
        let mut lm = self.minima_list;
        while let Some(index) = lm {
            let minima = self.minima[index];
            // Minima are already descending. Appending distinctY values gives
            // exactly the old insert_scanbeam arena and links, without scanning
            // the growing prefix again. Sweep-time insertion is unchanged.
            if last_scanbeam.is_none_or(|last: usize| self.scanbeams[last].y != minima.y) {
                let next = self.scanbeams.len();
                self.scanbeams.push(Scanbeam {
                    y: minima.y,
                    next: None,
                });
                match last_scanbeam {
                    Some(last) => self.scanbeams[last].next = Some(next),
                    None => self.scanbeam_list = Some(next),
                }
                last_scanbeam = Some(next);
            }
            for bound in [minima.left_bound, minima.right_bound]
                .into_iter()
                .flatten()
            {
                self.edges[bound].curr = self.edges[bound].bot;
                self.edges[bound].out_idx = UNASSIGNED;
            }
            lm = minima.next;
        }
        self.active_edges = None;
        Ok(())
    }

    /// Single-linked list sorted descending, ignoring dups.
    pub(super) fn insert_scanbeam(&mut self, y: i64) {
        let new_index = self.scanbeams.len();
        match self.scanbeam_list {
            None => {
                self.scanbeams.push(Scanbeam { y, next: None });
                self.scanbeam_list = Some(new_index);
            }
            Some(head) if y > self.scanbeams[head].y => {
                self.scanbeams.push(Scanbeam {
                    y,
                    next: Some(head),
                });
                self.scanbeam_list = Some(new_index);
            }
            Some(head) => {
                let mut sb2 = head;
                while let Some(next) = self.scanbeams[sb2].next {
                    if y <= self.scanbeams[next].y {
                        sb2 = next;
                    } else {
                        break;
                    }
                }
                if y == self.scanbeams[sb2].y {
                    return; //ie ignores duplicates
                }
                self.scanbeams.push(Scanbeam {
                    y,
                    next: self.scanbeams[sb2].next,
                });
                self.scanbeams[sb2].next = Some(new_index);
            }
        }
    }

    pub(super) fn pop_scanbeam(&mut self) -> Option<i64> {
        let head = self.scanbeam_list?;
        self.scanbeam_list = self.scanbeams[head].next;
        Some(self.scanbeams[head].y)
    }

    pub(super) fn local_minima_pending(&self) -> bool {
        self.current_lm.is_some()
    }

    /// Replaces `e` in the AEL with the next edge of its bound and
    /// returns that edge.
    pub(super) fn update_edge_into_ael(&mut self, e: usize) -> Result<usize, ClipperError> {
        let Some(next_in_lml) = self.edges[e].next_in_lml else {
            return Err(ClipperError::UpdateEdgeIntoAelInvalidCall);
        };
        let ael_prev = self.edges[e].prev_in_ael;
        let ael_next = self.edges[e].next_in_ael;
        self.edges[next_in_lml].out_idx = self.edges[e].out_idx;
        match ael_prev {
            Some(prev) => self.edges[prev].next_in_ael = Some(next_in_lml),
            None => self.active_edges = Some(next_in_lml),
        }
        if let Some(next) = ael_next {
            self.edges[next].prev_in_ael = Some(next_in_lml);
        }
        let old = self.edges[e].clone();
        let n = &mut self.edges[next_in_lml];
        n.side = old.side;
        n.wind_delta = old.wind_delta;
        n.wind_cnt = old.wind_cnt;
        n.wind_cnt2 = old.wind_cnt2;
        n.curr = n.bot;
        n.prev_in_ael = ael_prev;
        n.next_in_ael = ael_next;
        if !is_horizontal(n) {
            let top_y = n.top.y;
            self.insert_scanbeam(top_y);
        }
        Ok(next_in_lml)
    }

    pub(super) fn swap_positions_in_ael(&mut self, edge1: usize, edge2: usize) {
        //check that one or other edge hasn't already been removed from AEL ...
        if self.edges[edge1].next_in_ael == self.edges[edge1].prev_in_ael
            || self.edges[edge2].next_in_ael == self.edges[edge2].prev_in_ael
        {
            return;
        }

        if self.edges[edge1].next_in_ael == Some(edge2) {
            let next = self.edges[edge2].next_in_ael;
            if let Some(next) = next {
                self.edges[next].prev_in_ael = Some(edge1);
            }
            let prev = self.edges[edge1].prev_in_ael;
            if let Some(prev) = prev {
                self.edges[prev].next_in_ael = Some(edge2);
            }
            self.edges[edge2].prev_in_ael = prev;
            self.edges[edge2].next_in_ael = Some(edge1);
            self.edges[edge1].prev_in_ael = Some(edge2);
            self.edges[edge1].next_in_ael = next;
        } else if self.edges[edge2].next_in_ael == Some(edge1) {
            let next = self.edges[edge1].next_in_ael;
            if let Some(next) = next {
                self.edges[next].prev_in_ael = Some(edge2);
            }
            let prev = self.edges[edge2].prev_in_ael;
            if let Some(prev) = prev {
                self.edges[prev].next_in_ael = Some(edge1);
            }
            self.edges[edge1].prev_in_ael = prev;
            self.edges[edge1].next_in_ael = Some(edge2);
            self.edges[edge2].prev_in_ael = Some(edge1);
            self.edges[edge2].next_in_ael = next;
        } else {
            let next = self.edges[edge1].next_in_ael;
            let prev = self.edges[edge1].prev_in_ael;
            self.edges[edge1].next_in_ael = self.edges[edge2].next_in_ael;
            if let Some(n) = self.edges[edge1].next_in_ael {
                self.edges[n].prev_in_ael = Some(edge1);
            }
            self.edges[edge1].prev_in_ael = self.edges[edge2].prev_in_ael;
            if let Some(p) = self.edges[edge1].prev_in_ael {
                self.edges[p].next_in_ael = Some(edge1);
            }
            self.edges[edge2].next_in_ael = next;
            if let Some(n) = next {
                self.edges[n].prev_in_ael = Some(edge2);
            }
            self.edges[edge2].prev_in_ael = prev;
            if let Some(p) = prev {
                self.edges[p].next_in_ael = Some(edge2);
            }
        }

        if self.edges[edge1].prev_in_ael.is_none() {
            self.active_edges = Some(edge1);
        } else if self.edges[edge2].prev_in_ael.is_none() {
            self.active_edges = Some(edge2);
        }
    }

    pub(super) fn delete_from_ael(&mut self, e: usize) {
        let ael_prev = self.edges[e].prev_in_ael;
        let ael_next = self.edges[e].next_in_ael;
        if ael_prev.is_none() && ael_next.is_none() && Some(e) != self.active_edges {
            return; //already deleted
        }
        match ael_prev {
            Some(prev) => self.edges[prev].next_in_ael = ael_next,
            None => self.active_edges = ael_next,
        }
        if let Some(next) = ael_next {
            self.edges[next].prev_in_ael = ael_prev;
        }
        self.edges[e].next_in_ael = None;
        self.edges[e].prev_in_ael = None;
    }
}

fn set_dx(e: &mut TEdge) {
    e.delta.x = e.top.x - e.bot.x;
    e.delta.y = e.top.y - e.bot.y;
    if e.delta.y == 0 {
        e.dx = HORIZONTAL;
    } else {
        e.dx = e.delta.x as f64 / e.delta.y as f64;
    }
}

#[cfg(test)]
mod minima_order_tests {
    use super::*;
    use crate::boundary_clip_legacy::{ClipType, PolyFillType};

    fn minimum(y: i64) -> LocalMinima {
        LocalMinima {
            y,
            left_bound: None,
            right_bound: None,
            next: None,
        }
    }

    // Immutable reference for the former classic incremental list insertion.
    fn old_insert(
        nodes: &mut Vec<LocalMinima>,
        head: &mut Option<usize>,
        mut node: LocalMinima,
    ) -> u64 {
        let index = nodes.len();
        let mut comparisons = 0;
        match *head {
            None => {
                nodes.push(node);
                *head = Some(index);
            }
            Some(first) if node.y >= nodes[first].y => {
                node.next = Some(first);
                nodes.push(node);
                *head = Some(index);
            }
            Some(first) => {
                let mut tail = first;
                while let Some(next) = nodes[tail].next {
                    comparisons += 1;
                    if node.y < nodes[next].y {
                        tail = next;
                    } else {
                        break;
                    }
                }
                node.next = nodes[tail].next;
                nodes.push(node);
                nodes[tail].next = Some(index);
            }
        }
        comparisons
    }

    fn links(clipper: &Clipper) -> Vec<(i64, Option<usize>, Option<usize>, Option<usize>)> {
        clipper
            .minima
            .iter()
            .map(|n| (n.y, n.left_bound, n.right_bound, n.next))
            .collect()
    }
    fn ordered_indices(clipper: &Clipper) -> Vec<usize> {
        let mut result = vec![];
        let mut next = clipper.minima_list;
        while let Some(index) = next {
            assert!(result.len() < clipper.minima.len());
            result.push(index);
            next = clipper.minima[index].next;
        }
        result
    }
    fn edge_reset_order(clipper: &Clipper) -> Vec<usize> {
        ordered_indices(clipper)
            .into_iter()
            .flat_map(|i| {
                [clipper.minima[i].left_bound, clipper.minima[i].right_bound]
                    .into_iter()
                    .flatten()
            })
            .collect()
    }
    fn old_reset(clipper: &mut Clipper) {
        clipper.current_lm = clipper.minima_list;
        if clipper.current_lm.is_none() {
            return;
        }
        clipper.scanbeam_list = None;
        clipper.scanbeams.clear();
        let mut next = clipper.minima_list;
        while let Some(index) = next {
            let node = clipper.minima[index];
            clipper.insert_scanbeam(node.y);
            for edge in [node.left_bound, node.right_bound].into_iter().flatten() {
                clipper.edges[edge].curr = clipper.edges[edge].bot;
                clipper.edges[edge].out_idx = UNASSIGNED;
            }
            next = node.next;
        }
        clipper.active_edges = None;
    }
    fn assert_reset_equal(actual: &mut Clipper, reference: &mut Clipper) {
        actual.reset().unwrap();
        old_reset(reference);
        assert_eq!(links(actual), links(reference));
        assert_eq!(actual.minima_list, reference.minima_list);
        assert_eq!(actual.current_lm, reference.current_lm);
        assert_eq!(actual.scanbeam_list, reference.scanbeam_list);
        assert_eq!(
            actual
                .scanbeams
                .iter()
                .map(|s| (s.y, s.next))
                .collect::<Vec<_>>(),
            reference
                .scanbeams
                .iter()
                .map(|s| (s.y, s.next))
                .collect::<Vec<_>>()
        );
        assert_eq!(edge_reset_order(actual), edge_reset_order(reference));
        assert_eq!(
            format!("{:?}", actual.edges),
            format!("{:?}", reference.edges)
        );
        assert_eq!(actual.active_edges, reference.active_edges);
    }
    fn compare_y_values(values: &[i64]) {
        let mut actual = Clipper::new();
        let mut reference = Clipper::new();
        for &y in values {
            actual.insert_local_minima(minimum(y));
            old_insert(
                &mut reference.minima,
                &mut reference.minima_list,
                minimum(y),
            );
        }
        assert_reset_equal(&mut actual, &mut reference);
        // A reused clipper must not reverse equal-Y ties on the next execute.
        assert_reset_equal(&mut actual, &mut reference);
    }

    #[test]
    fn deferred_order_matches_old_links_exhaustively_with_ties() {
        for length in 0..=7 {
            for mut code in 0..3usize.pow(length) {
                let values: Vec<_> = (0..length)
                    .map(|_| {
                        let y = (code % 3) as i64 - 1;
                        code /= 3;
                        y
                    })
                    .collect();
                compare_y_values(&values);
            }
        }
    }

    #[test]
    fn integer_extremes_and_newest_first_equal_y_are_exact() {
        compare_y_values(&[i64::MIN, i64::MAX, 0, i64::MAX, -1, i64::MIN, 0]);
        let mut clipper = Clipper::new();
        for y in [5, 8, 5, 3, 8, 5] {
            clipper.insert_local_minima(minimum(y));
        }
        clipper.order_local_minima().unwrap();
        assert_eq!(ordered_indices(&clipper), vec![4, 1, 5, 2, 0, 3]);
    }

    #[test]
    fn append_after_reset_and_clear_keep_reference_nodes_and_scanbeams() {
        let mut actual = Clipper::new();
        let mut reference = Clipper::new();
        for batch in [&[8, 4, 8, 0][..], &[4, 9, -1, 8][..]] {
            for &y in batch {
                actual.insert_local_minima(minimum(y));
                old_insert(
                    &mut reference.minima,
                    &mut reference.minima_list,
                    minimum(y),
                );
            }
            assert_reset_equal(&mut actual, &mut reference);
        }
        actual.clear();
        reference.clear();
        assert_reset_equal(&mut actual, &mut reference);
        for y in [2, 2, -2, 2] {
            actual.insert_local_minima(minimum(y));
            old_insert(
                &mut reference.minima,
                &mut reference.minima_list,
                minimum(y),
            );
        }
        assert_reset_equal(&mut actual, &mut reference);
    }

    fn rectangle(x: i64, y: i64) -> Vec<IntPoint> {
        vec![
            IntPoint::new(x, y),
            IntPoint::new(x + 10, y),
            IntPoint::new(x + 10, y + 10),
            IntPoint::new(x, y + 10),
        ]
    }
    fn synthetic_paths() -> Vec<Vec<IntPoint>> {
        vec![
            rectangle(0, 40),
            rectangle(4, 40),
            rectangle(0, 10),
            rectangle(2, 10),
            rectangle(30, -10),
            vec![
                IntPoint::new(5, 5),
                IntPoint::new(20, 5),
                IntPoint::new(8, 25),
            ],
        ]
    }
    fn clipper_for(paths: &[Vec<IntPoint>], old: bool) -> Clipper {
        let mut clipper = Clipper::new();
        for (i, path) in paths.iter().enumerate() {
            clipper
                .add_path(
                    path,
                    if i == 0 {
                        PolyType::Subject
                    } else {
                        PolyType::Clip
                    },
                    true,
                )
                .unwrap();
        }
        if old {
            let nodes = std::mem::take(&mut clipper.minima);
            clipper.minima_list = None;
            for node in nodes {
                old_insert(&mut clipper.minima, &mut clipper.minima_list, node);
            }
        }
        clipper
    }
    fn execute(clipper: &mut Clipper) -> Vec<Vec<IntPoint>> {
        let mut result = vec![];
        assert!(
            clipper
                .execute(
                    ClipType::Union,
                    &mut result,
                    PolyFillType::NonZero,
                    PolyFillType::NonZero
                )
                .unwrap()
        );
        result
    }

    #[test]
    fn actual_edges_reset_in_reference_order_without_other_field_changes() {
        let paths = synthetic_paths();
        let mut actual = clipper_for(&paths, false);
        let mut reference = clipper_for(&paths, true);
        assert!(actual.minima.len() <= actual.edges.len());
        for (a, b) in actual.edges.iter_mut().zip(&mut reference.edges) {
            a.out_idx = 71;
            b.out_idx = 71;
            a.curr.x = -17;
            b.curr.x = -17;
        }
        assert_reset_equal(&mut actual, &mut reference);
    }

    #[test]
    fn repeated_execute_new_add_path_and_clear_preserve_raw_output() {
        let mut paths = synthetic_paths();
        let mut actual = clipper_for(&paths, false);
        let mut reference = clipper_for(&paths, true);
        let first = execute(&mut actual);
        assert_eq!(first, execute(&mut reference));
        assert_eq!(first, execute(&mut actual));
        let appended = rectangle(-25, 40);
        actual.add_path(&appended, PolyType::Clip, true).unwrap();
        paths.push(appended);
        assert_eq!(
            execute(&mut actual),
            execute(&mut clipper_for(&paths, true))
        );
        actual.clear();
        paths.reverse();
        for (i, path) in paths.iter().enumerate() {
            actual
                .add_path(
                    path,
                    if i == 0 {
                        PolyType::Subject
                    } else {
                        PolyType::Clip
                    },
                    true,
                )
                .unwrap();
        }
        assert_eq!(
            execute(&mut actual),
            execute(&mut clipper_for(&paths, true))
        );
    }

    #[test]
    #[ignore = "explicit bounded release microbenchmark"]
    fn deferred_minima_microbenchmark() {
        let count = 20_000usize;
        let mut old = Clipper::new();
        let start = std::time::Instant::now();
        let mut traversals = 0;
        for index in 0..count {
            traversals += old_insert(
                &mut old.minima,
                &mut old.minima_list,
                minimum(-(index as i64)),
            );
        }
        let old_time = start.elapsed();
        let mut new = Clipper::new();
        let start = std::time::Instant::now();
        for index in 0..count {
            new.insert_local_minima(minimum(-(index as i64)));
        }
        new.order_local_minima().unwrap();
        let new_time = start.elapsed();
        assert_eq!(links(&new), links(&old));
        assert_eq!(traversals, (count as u64 - 1) * (count as u64 - 2) / 2);
        let start = std::time::Instant::now();
        old_reset(&mut old);
        let old_reset_time = start.elapsed();
        let start = std::time::Instant::now();
        new.reset().unwrap();
        let new_reset_time = start.elapsed();
        assert_eq!(new.scanbeam_list, old.scanbeam_list);
        assert_eq!(
            new.scanbeams
                .iter()
                .map(|s| (s.y, s.next))
                .collect::<Vec<_>>(),
            old.scanbeams
                .iter()
                .map(|s| (s.y, s.next))
                .collect::<Vec<_>>()
        );
        eprintln!(
            "MINIMA_BENCH nodes={count} old_traversals={traversals} old_seconds={:.9} new_seconds={:.9} temporary_index_bytes={}",
            old_time.as_secs_f64(),
            new_time.as_secs_f64(),
            count * std::mem::size_of::<usize>()
        );
        eprintln!(
            "RESET_BENCH nodes={count} old_seconds={:.9} new_seconds={:.9}",
            old_reset_time.as_secs_f64(),
            new_reset_time.as_secs_f64()
        );
    }
}
