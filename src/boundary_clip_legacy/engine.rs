//! Clipper 6's `Clipper` class: the state of one clipper, `Execute` into paths
//! or a [`PolyTree`], the sweep loop (`ExecuteInternal`), inserting local minima into the
//! active edge list, and the fill-rule logic that decides which edges contribute
//! (`IsContributing`, `SetWindingCount`). The rest of the sweep is in [`super::horizontals`];
//! output rings are in [`super::out_polys`] and [`super::joins`].

use super::clipper_base::{is_horizontal, slopes_equal_edges};
use super::poly_tree::PolyTree;
use super::types::{
    ClipType, ClipperError, IntersectNode, Join, LocalMinima, Maxima, OutPt, OutRec, Paths,
    PolyFillType, PolyType, Scanbeam, TEdge,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SortContext {
    Legacy,
    Modern,
}

/// The clipper (Clipper 6's `Clipper` with its base `ClipperBase`). Every record it allocates
/// lives in one of the arenas here and is named by its index.
pub struct Clipper {
    pub(super) sort_context: SortContext,
    pub(super) edges: Vec<TEdge>,
    pub(super) minima: Vec<LocalMinima>,
    pub(super) minima_list: Option<usize>,
    pub(super) current_lm: Option<usize>,
    pub(super) scanbeams: Vec<Scanbeam>,
    pub(super) scanbeam_list: Option<usize>,
    pub(super) maxima: Vec<Maxima>,
    pub(super) maxima_list: Option<usize>,
    pub(super) poly_outs: Vec<OutRec>,
    pub(super) out_pts: Vec<OutPt>,
    pub(super) active_edges: Option<usize>,
    pub(super) sorted_edges: Option<usize>,
    pub(super) intersect_list: Vec<IntersectNode>,
    pub(super) joins: Vec<Join>,
    pub(super) ghost_joins: Vec<Join>,
    pub(super) use_full_range: bool,
    pub(super) has_open_paths: bool,
    pub(super) using_poly_tree: bool,
    pub(super) clip_type: ClipType,
    pub(super) subj_fill_type: PolyFillType,
    pub(super) clip_fill_type: PolyFillType,
    /// Default false: collinear vertices are dropped.
    pub preserve_collinear: bool,
    pub reverse_solution: bool,
    /// Also split rings that touch at a vertex.
    pub strictly_simple: bool,
}

impl Default for Clipper {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipper {
    /// No init options.
    pub fn new() -> Self {
        Clipper {
            sort_context: SortContext::Legacy,
            edges: Vec::new(),
            minima: Vec::new(),
            minima_list: None,
            current_lm: None,
            scanbeams: Vec::new(),
            scanbeam_list: None,
            maxima: Vec::new(),
            maxima_list: None,
            poly_outs: Vec::new(),
            out_pts: Vec::new(),
            active_edges: None,
            sorted_edges: None,
            intersect_list: Vec::new(),
            joins: Vec::new(),
            ghost_joins: Vec::new(),
            use_full_range: false,
            has_open_paths: false,
            using_poly_tree: false,
            clip_type: ClipType::Intersection,
            subj_fill_type: PolyFillType::EvenOdd,
            clip_fill_type: PolyFillType::EvenOdd,
            preserve_collinear: false,
            reverse_solution: false,
            strictly_simple: false,
        }
    }

    /// RASProcess global merge runs in the .NET 4.7.2 host; tile/TIN hosts use new().
    pub(crate) fn new_global_merge() -> Self {
        let mut clipper = Self::new();
        clipper.sort_context = SortContext::Modern;
        clipper
    }

    /// Forgets every added path.
    pub fn clear(&mut self) {
        self.minima.clear();
        self.minima_list = None;
        self.current_lm = None;
        self.edges.clear();
        self.use_full_range = false;
        self.has_open_paths = false;
    }

    /// Clips the added paths into `solution`. False (and an empty
    /// solution) when there was nothing to clip or the intersections could not be ordered.
    pub fn execute(
        &mut self,
        clip_type: ClipType,
        solution: &mut Paths,
        subj_fill_type: PolyFillType,
        clip_fill_type: PolyFillType,
    ) -> Result<bool, ClipperError> {
        if self.has_open_paths {
            return Err(ClipperError::PolyTreeNeededForOpenPaths);
        }
        solution.clear();
        self.subj_fill_type = subj_fill_type;
        self.clip_fill_type = clip_fill_type;
        self.clip_type = clip_type;
        self.using_poly_tree = false;
        let result = self.execute_internal();
        if let Ok(true) = result {
            self.build_result(solution);
        }
        self.dispose_all_poly_pts();
        result
    }

    pub fn execute_tree(
        &mut self,
        clip_type: ClipType,
        polytree: &mut PolyTree,
        subj_fill_type: PolyFillType,
        clip_fill_type: PolyFillType,
    ) -> Result<bool, ClipperError> {
        self.subj_fill_type = subj_fill_type;
        self.clip_fill_type = clip_fill_type;
        self.clip_type = clip_type;
        self.using_poly_tree = true;
        let result = self.execute_internal();
        if let Ok(true) = result {
            self.build_result2(polytree);
        }
        self.dispose_all_poly_pts();
        result
    }

    fn dispose_all_poly_pts(&mut self) {
        self.poly_outs.clear();
        self.out_pts.clear();
    }

    fn execute_internal(&mut self) -> Result<bool, ClipperError> {
        let result = self.execute_internal_body();
        self.joins.clear();
        self.ghost_joins.clear();
        result
    }

    fn execute_internal_body(&mut self) -> Result<bool, ClipperError> {
        self.reset()?;
        self.sorted_edges = None;
        self.maxima_list = None;
        self.maxima.clear();

        let Some(mut bot_y) = self.pop_scanbeam() else {
            return Ok(false);
        };
        loop {
            self.insert_local_minima_into_ael(bot_y)?;
            self.ghost_joins.clear();
            self.process_horizontals(false)?;
            let Some(top_y) = self.pop_scanbeam() else {
                break;
            };
            if !self.process_intersections(top_y)? {
                return Ok(false);
            }
            self.process_edges_at_top_of_scanbeam(top_y)?;
            bot_y = top_y;
            if self.scanbeam_list.is_none() && !self.local_minima_pending() {
                break;
            }
        }

        //fix orientations ...
        for i in 0..self.poly_outs.len() {
            let out_rec = self.poly_outs[i];
            let Some(pts) = out_rec.pts else { continue };
            if out_rec.is_open {
                continue;
            }
            if (out_rec.is_hole ^ self.reverse_solution) == (self.area_out_pt(Some(pts)) > 0.0) {
                self.reverse_poly_pt_links(Some(pts));
            }
        }

        self.join_common_edges()?;

        for i in 0..self.poly_outs.len() {
            if self.poly_outs[i].pts.is_none() {
                continue;
            } else if self.poly_outs[i].is_open {
                self.fixup_out_polyline(i);
            } else {
                self.fixup_out_polygon(i);
            }
        }

        if self.strictly_simple {
            self.do_simple_polygons()?;
        }
        Ok(true)
    }

    fn insert_local_minima_into_ael(&mut self, bot_y: i64) -> Result<(), ClipperError> {
        while let Some(lm) = self.pop_local_minima(bot_y) {
            let lb = lm.left_bound;
            let rb = lm.right_bound;

            let mut op1: Option<usize> = None;
            match (lb, rb) {
                (None, Some(rb)) => {
                    self.insert_edge_into_ael(rb, None);
                    self.set_winding_count(rb);
                    if self.is_contributing(rb) {
                        op1 = Some(self.add_out_pt(rb, self.edges[rb].bot)?);
                    }
                }
                (Some(lb), None) => {
                    self.insert_edge_into_ael(lb, None);
                    self.set_winding_count(lb);
                    if self.is_contributing(lb) {
                        op1 = Some(self.add_out_pt(lb, self.edges[lb].bot)?);
                    }
                    self.insert_scanbeam(self.edges[lb].top.y);
                }
                (Some(lb), Some(rb)) => {
                    self.insert_edge_into_ael(lb, None);
                    self.insert_edge_into_ael(rb, Some(lb));
                    self.set_winding_count(lb);
                    self.edges[rb].wind_cnt = self.edges[lb].wind_cnt;
                    self.edges[rb].wind_cnt2 = self.edges[lb].wind_cnt2;
                    if self.is_contributing(lb) {
                        op1 = Some(self.add_local_min_poly(lb, rb, self.edges[lb].bot)?);
                    }
                    self.insert_scanbeam(self.edges[lb].top.y);
                }
                (None, None) => {}
            }

            if let Some(rb) = rb {
                if is_horizontal(&self.edges[rb]) {
                    if let Some(next) = self.edges[rb].next_in_lml {
                        self.insert_scanbeam(self.edges[next].top.y);
                    }
                    self.add_edge_to_sel(rb);
                } else {
                    self.insert_scanbeam(self.edges[rb].top.y);
                }
            }

            let (Some(lb), Some(rb)) = (lb, rb) else {
                continue;
            };

            //if output polygons share an Edge with a horizontal rb, they'll need joining later ...
            if let Some(op1) = op1 {
                if is_horizontal(&self.edges[rb])
                    && !self.ghost_joins.is_empty()
                    && self.edges[rb].wind_delta != 0
                {
                    for i in 0..self.ghost_joins.len() {
                        //if the horizontal Rb and a 'ghost' horizontal overlap, then convert
                        //the 'ghost' join to a real join ready for later ...
                        let j = self.ghost_joins[i];
                        if horz_segments_overlap(
                            self.out_pts[j.out_pt1].pt.x,
                            j.off_pt.x,
                            self.edges[rb].bot.x,
                            self.edges[rb].top.x,
                        ) {
                            self.add_join(j.out_pt1, op1, j.off_pt);
                        }
                    }
                }
            }

            if let Some(lb_prev) = self.edges[lb].prev_in_ael {
                if self.edges[lb].out_idx >= 0
                    && self.edges[lb_prev].curr.x == self.edges[lb].bot.x
                    && self.edges[lb_prev].out_idx >= 0
                    && slopes_equal_edges(
                        &self.edges[lb_prev],
                        &self.edges[lb],
                        self.use_full_range,
                    )
                    && self.edges[lb].wind_delta != 0
                    && self.edges[lb_prev].wind_delta != 0
                {
                    let op2 = self.add_out_pt(lb_prev, self.edges[lb].bot)?;
                    // lb contributes here, so op1 was set when it was inserted.
                    if let Some(op1) = op1 {
                        self.add_join(op1, op2, self.edges[lb].top);
                    }
                }
            }

            if self.edges[lb].next_in_ael != Some(rb) {
                if let Some(rb_prev) = self.edges[rb].prev_in_ael {
                    if self.edges[rb].out_idx >= 0
                        && self.edges[rb_prev].out_idx >= 0
                        && slopes_equal_edges(
                            &self.edges[rb_prev],
                            &self.edges[rb],
                            self.use_full_range,
                        )
                        && self.edges[rb].wind_delta != 0
                        && self.edges[rb_prev].wind_delta != 0
                    {
                        let op2 = self.add_out_pt(rb_prev, self.edges[rb].bot)?;
                        if let Some(op1) = op1 {
                            self.add_join(op1, op2, self.edges[rb].top);
                        }
                    }
                }

                let mut e = self.edges[lb].next_in_ael;
                while let Some(edge) = e {
                    if edge == rb {
                        break;
                    }
                    //nb: For calculating winding counts etc, IntersectEdges() assumes
                    //that param1 will be to the right of param2 ABOVE the intersection ...
                    let pt = self.edges[lb].curr;
                    self.intersect_edges(rb, edge, pt)?; //order important here
                    e = self.edges[edge].next_in_ael;
                }
            }
        }
        Ok(())
    }

    fn insert_edge_into_ael(&mut self, edge: usize, start_edge: Option<usize>) {
        let Some(active) = self.active_edges else {
            self.edges[edge].prev_in_ael = None;
            self.edges[edge].next_in_ael = None;
            self.active_edges = Some(edge);
            return;
        };
        if start_edge.is_none() && self.e2_inserts_before_e1(active, edge) {
            self.edges[edge].prev_in_ael = None;
            self.edges[edge].next_in_ael = Some(active);
            self.edges[active].prev_in_ael = Some(edge);
            self.active_edges = Some(edge);
        } else {
            let mut start = start_edge.unwrap_or(active);
            while let Some(next) = self.edges[start].next_in_ael {
                if self.e2_inserts_before_e1(next, edge) {
                    break;
                }
                start = next;
            }
            let next = self.edges[start].next_in_ael;
            self.edges[edge].next_in_ael = next;
            if let Some(next) = next {
                self.edges[next].prev_in_ael = Some(edge);
            }
            self.edges[edge].prev_in_ael = Some(start);
            self.edges[start].next_in_ael = Some(edge);
        }
    }

    fn e2_inserts_before_e1(&self, e1: usize, e2: usize) -> bool {
        let (e1, e2) = (&self.edges[e1], &self.edges[e2]);
        if e2.curr.x == e1.curr.x {
            if e2.top.y > e1.top.y {
                e2.top.x < top_x(e1, e2.top.y)
            } else {
                e1.top.x > top_x(e2, e1.top.y)
            }
        } else {
            e2.curr.x < e1.curr.x
        }
    }

    pub(super) fn is_even_odd_fill_type(&self, edge: usize) -> bool {
        if self.edges[edge].poly_typ == PolyType::Subject {
            self.subj_fill_type == PolyFillType::EvenOdd
        } else {
            self.clip_fill_type == PolyFillType::EvenOdd
        }
    }

    fn is_even_odd_alt_fill_type(&self, edge: usize) -> bool {
        if self.edges[edge].poly_typ == PolyType::Subject {
            self.clip_fill_type == PolyFillType::EvenOdd
        } else {
            self.subj_fill_type == PolyFillType::EvenOdd
        }
    }

    /// The edge's own and the other poly type's fill rules.
    pub(super) fn fill_types(&self, edge: usize) -> (PolyFillType, PolyFillType) {
        if self.edges[edge].poly_typ == PolyType::Subject {
            (self.subj_fill_type, self.clip_fill_type)
        } else {
            (self.clip_fill_type, self.subj_fill_type)
        }
    }

    fn is_contributing(&self, edge: usize) -> bool {
        let (pft, pft2) = self.fill_types(edge);
        let e = &self.edges[edge];

        match pft {
            PolyFillType::EvenOdd => {
                //return false if a subj line has been flagged as inside a subj polygon
                if e.wind_delta == 0 && e.wind_cnt != 1 {
                    return false;
                }
            }
            PolyFillType::NonZero => {
                if e.wind_cnt.abs() != 1 {
                    return false;
                }
            }
            PolyFillType::Positive => {
                if e.wind_cnt != 1 {
                    return false;
                }
            }
            PolyFillType::Negative => {
                if e.wind_cnt != -1 {
                    return false;
                }
            }
        }

        let outside_other = || match pft2 {
            PolyFillType::EvenOdd | PolyFillType::NonZero => e.wind_cnt2 == 0,
            PolyFillType::Positive => e.wind_cnt2 <= 0,
            PolyFillType::Negative => e.wind_cnt2 >= 0,
        };
        let inside_other = || match pft2 {
            PolyFillType::EvenOdd | PolyFillType::NonZero => e.wind_cnt2 != 0,
            PolyFillType::Positive => e.wind_cnt2 > 0,
            PolyFillType::Negative => e.wind_cnt2 < 0,
        };

        match self.clip_type {
            ClipType::Intersection => inside_other(),
            ClipType::Union => outside_other(),
            ClipType::Difference => {
                if e.poly_typ == PolyType::Subject {
                    outside_other()
                } else {
                    inside_other()
                }
            }
            ClipType::Xor => {
                if e.wind_delta == 0 {
                    //XOr always contributing unless open
                    outside_other()
                } else {
                    true
                }
            }
        }
    }

    fn set_winding_count(&mut self, edge: usize) {
        let mut e = self.edges[edge].prev_in_ael;
        //find the edge of the same polytype that immediately preceeds 'edge' in AEL
        while let Some(prev) = e {
            if self.edges[prev].poly_typ != self.edges[edge].poly_typ
                || self.edges[prev].wind_delta == 0
            {
                e = self.edges[prev].prev_in_ael;
            } else {
                break;
            }
        }
        let edge_wind_delta = self.edges[edge].wind_delta;
        match e {
            None => {
                let pft = if self.edges[edge].poly_typ == PolyType::Subject {
                    self.subj_fill_type
                } else {
                    self.clip_fill_type
                };
                self.edges[edge].wind_cnt = if edge_wind_delta == 0 {
                    if pft == PolyFillType::Negative { -1 } else { 1 }
                } else {
                    edge_wind_delta
                };
                self.edges[edge].wind_cnt2 = 0;
                e = self.active_edges; //ie get ready to calc WindCnt2
            }
            Some(prev) if edge_wind_delta == 0 && self.clip_type != ClipType::Union => {
                self.edges[edge].wind_cnt = 1;
                self.edges[edge].wind_cnt2 = self.edges[prev].wind_cnt2;
                e = self.edges[prev].next_in_ael; //ie get ready to calc WindCnt2
            }
            Some(prev) if self.is_even_odd_fill_type(edge) => {
                //EvenOdd filling ...
                if edge_wind_delta == 0 {
                    //are we inside a subj polygon ...
                    let mut inside = true;
                    let mut e2 = self.edges[prev].prev_in_ael;
                    while let Some(other) = e2 {
                        if self.edges[other].poly_typ == self.edges[prev].poly_typ
                            && self.edges[other].wind_delta != 0
                        {
                            inside = !inside;
                        }
                        e2 = self.edges[other].prev_in_ael;
                    }
                    self.edges[edge].wind_cnt = if inside { 0 } else { 1 };
                } else {
                    self.edges[edge].wind_cnt = edge_wind_delta;
                }
                self.edges[edge].wind_cnt2 = self.edges[prev].wind_cnt2;
                e = self.edges[prev].next_in_ael; //ie get ready to calc WindCnt2
            }
            Some(prev) => {
                //nonZero, Positive or Negative filling ...
                let (prev_wind_cnt, prev_wind_delta) =
                    (self.edges[prev].wind_cnt, self.edges[prev].wind_delta);
                self.edges[edge].wind_cnt = if prev_wind_cnt * prev_wind_delta < 0 {
                    //prev edge is 'decreasing' WindCount (WC) toward zero
                    //so we're outside the previous polygon ...
                    if prev_wind_cnt.abs() > 1 {
                        //outside prev poly but still inside another.
                        //when reversing direction of prev poly use the same WC
                        if prev_wind_delta * edge_wind_delta < 0 {
                            prev_wind_cnt
                        } else {
                            //otherwise continue to 'decrease' WC ...
                            prev_wind_cnt + edge_wind_delta
                        }
                    } else if edge_wind_delta == 0 {
                        //now outside all polys of same polytype so set own WC ...
                        1
                    } else {
                        edge_wind_delta
                    }
                } else {
                    //prev edge is 'increasing' WindCount (WC) away from zero
                    //so we're inside the previous polygon ...
                    if edge_wind_delta == 0 {
                        if prev_wind_cnt < 0 {
                            prev_wind_cnt - 1
                        } else {
                            prev_wind_cnt + 1
                        }
                    } else if prev_wind_delta * edge_wind_delta < 0 {
                        //if wind direction is reversing prev then use same WC
                        prev_wind_cnt
                    } else {
                        //otherwise add to WC ...
                        prev_wind_cnt + edge_wind_delta
                    }
                };
                self.edges[edge].wind_cnt2 = self.edges[prev].wind_cnt2;
                e = self.edges[prev].next_in_ael; //ie get ready to calc WindCnt2
            }
        }

        //update WindCnt2 ...
        let even_odd_alt = self.is_even_odd_alt_fill_type(edge);
        while let Some(other) = e {
            if other == edge {
                break;
            }
            if even_odd_alt {
                //EvenOdd filling ...
                if self.edges[other].wind_delta != 0 {
                    let wc2 = self.edges[edge].wind_cnt2;
                    self.edges[edge].wind_cnt2 = if wc2 == 0 { 1 } else { 0 };
                }
            } else {
                //nonZero, Positive or Negative filling ...
                self.edges[edge].wind_cnt2 += self.edges[other].wind_delta;
            }
            e = self.edges[other].next_in_ael;
        }
    }

    pub(super) fn add_join(&mut self, op1: usize, op2: usize, off_pt: super::IntPoint) {
        self.joins.push(Join {
            out_pt1: op1,
            out_pt2: op2,
            off_pt,
        });
    }

    pub(super) fn add_ghost_join(&mut self, op: usize, off_pt: super::IntPoint) {
        self.ghost_joins.push(Join {
            out_pt1: op,
            out_pt2: usize::MAX,
            off_pt,
        });
    }
}

pub(super) fn horz_segments_overlap(
    mut seg1a: i64,
    mut seg1b: i64,
    mut seg2a: i64,
    mut seg2b: i64,
) -> bool {
    if seg1a > seg1b {
        std::mem::swap(&mut seg1a, &mut seg1b);
    }
    if seg2a > seg2b {
        std::mem::swap(&mut seg2a, &mut seg2b);
    }
    (seg1a < seg2b) && (seg2a < seg1b)
}

pub(super) fn top_x(edge: &TEdge, current_y: i64) -> i64 {
    if current_y == edge.top.y {
        return edge.top.x;
    }
    edge.bot.x + super::types::round(edge.dx * (current_y - edge.bot.y) as f64)
}
