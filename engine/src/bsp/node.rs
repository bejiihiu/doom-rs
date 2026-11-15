//! BSP node helpers: the point-vs-partition side test and the tree
//! traversal order, ported from `r_main.c` (`R_PointOnSide`) and
//! `r_bsp.c` (`R_RenderBSPNode`).

use crate::fixed::Fixed;
use crate::map::{Node, NodeChild};
use crate::math::{self, Divline};

use super::clip::ClipSegList;
use super::view::{ViewState, ViewTables};
use super::walls::check_bbox;

/// Which side of a BSP partition line a point is on: 0 (front) or 1 (back),
/// as `R_PointOnSide` with the sign-bit quick path. `math::point_on_divline_side`
/// already implements that fast path, so this is a thin adapter over the
/// node's partition line rather than a second copy of the arithmetic.
pub fn point_on_node_side(x: Fixed, y: Fixed, node: &Node) -> i32 {
    math::point_on_divline_side(
        x,
        y,
        &Divline {
            x: node.x,
            y: node.y,
            dx: node.dx,
            dy: node.dy,
        },
    )
}

/// Depth-first traversal in the original's render order: the child on the
/// view's side first, then the other child only when [`check_bbox`] says
/// its bbox might be visible. `R_RenderBSPNode` adapted to the map node
/// tree — leaves are reported to `on_subsector` with the clip list handed
/// through so walls can be drawn against (and update) the current clip state.
///
/// Callers building the render with no nodes at all pass
/// `NodeChild::SubSector(0)` as `root`, mirroring the vanilla `bspnum == -1`
/// case. Indices into `nodes` come from a validated [`crate::map::Map`],
/// so no bounds checks are repeated here.
pub fn traverse_bsp(
    nodes: &[Node],
    root: NodeChild,
    view: &ViewState,
    tables: &ViewTables,
    clip: &mut ClipSegList,
    mut on_subsector: impl FnMut(&mut ClipSegList, usize),
) {
    fn recur(
        nodes: &[Node],
        child: NodeChild,
        view: &ViewState,
        tables: &ViewTables,
        clip: &mut ClipSegList,
        on_subsector: &mut dyn FnMut(&mut ClipSegList, usize),
    ) {
        match child {
            NodeChild::SubSector(index) => on_subsector(clip, index),
            NodeChild::Node(index) => {
                let node = &nodes[index];
                // Decide which side the view point is on.
                // Recursively divide front space.
                let side = point_on_node_side(view.x, view.y, node) as usize;
                recur(nodes, node.child(side), view, tables, clip, on_subsector);

                // Possibly divide back space.
                if check_bbox(view, tables, clip, &node.bbox[side ^ 1]) {
                    recur(
                        nodes,
                        node.child(side ^ 1),
                        view,
                        tables,
                        clip,
                        on_subsector,
                    );
                }
            }
        }
    }

    recur(nodes, root, view, tables, clip, &mut on_subsector);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::types::NF_SUBSECTOR;
    use crate::tables::Angle;

    fn node(x: i32, y: i32, dx: i32, dy: i32, children: [u16; 2]) -> Node {
        Node {
            x: Fixed::from_int(x),
            y: Fixed::from_int(y),
            dx: Fixed::from_int(dx),
            dy: Fixed::from_int(dy),
            bbox: [[Fixed::ZERO; 4]; 2],
            children,
        }
    }

    #[test]
    fn point_on_node_side_matches_divline_side() {
        let n = node(10, 20, 30, -10, [0, 0]);
        for x in [-5, 0, 10, 25, 100] {
            for y in [-3, 7, 20, 55] {
                let expected = math::point_on_divline_side(
                    Fixed::from_int(x),
                    Fixed::from_int(y),
                    &Divline {
                        x: n.x,
                        y: n.y,
                        dx: n.dx,
                        dy: n.dy,
                    },
                );
                assert_eq!(
                    point_on_node_side(Fixed::from_int(x), Fixed::from_int(y), &n),
                    expected
                );
            }
        }
    }

    #[test]
    fn traversal_visits_front_child_before_back_child() {
        let t = tables();
        let view = ViewState::new(Fixed::ZERO, Fixed::ZERO, Angle::ZERO);
        // vertical node at x=0 dividing east (front side for this view is
        // children[1]; a point on the line picks children[1] as front)
        let mut n = node(0, 0, 0, 1, [NF_SUBSECTOR, NF_SUBSECTOR | 1]);
        // back child's bbox: tall slab far ahead in view, so it is not culled
        n.bbox[0] = [
            Fixed::from_int(100),
            Fixed::from_int(-100),
            Fixed::from_int(500),
            Fixed::from_int(700),
        ];
        let mut visited = Vec::new();
        let mut clip = ClipSegList::cleared(64);
        traverse_bsp(
            &[n],
            NodeChild::Node(0),
            &view,
            &t,
            &mut clip,
            &mut |_clip: &mut ClipSegList, i: usize| visited.push(i),
        );
        assert_eq!(visited, vec![1, 0]);
    }

    #[test]
    fn traversal_skips_back_subtree_when_bbox_is_covered() {
        let t = tables();
        let view = ViewState::new(Fixed::ZERO, Fixed::ZERO, Angle::ZERO);
        // vertical node at x=0; front child = children[1] (side for x <= 0)
        let mut n = node(0, 0, 0, 1, [NF_SUBSECTOR, NF_SUBSECTOR | 1]);
        // back child's bbox: tall slab far ahead, in view, but fully covered
        n.bbox[0] = [
            Fixed::from_int(100),
            Fixed::from_int(-100),
            Fixed::from_int(500),
            Fixed::from_int(700),
        ];
        let mut clip = ClipSegList::cleared(64);
        clip.clip_solid_wall_segment(0, 63, |_, _| {});

        let mut visited = Vec::new();
        traverse_bsp(
            &[n],
            NodeChild::Node(0),
            &view,
            &t,
            &mut clip,
            &mut |_: &mut ClipSegList, i: usize| visited.push(i),
        );
        // front side drawn; the back subtree got culled by the covered clip list
        assert_eq!(visited, vec![1]);
    }

    fn tables() -> ViewTables {
        ViewTables::new(64, Fixed::from_int(32))
    }
}
