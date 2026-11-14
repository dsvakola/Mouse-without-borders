//! Decides where the master's mouse is and when it moves to another computer.
//!
//! The master feeds raw mouse movement (dx, dy) in. The router keeps the
//! cursor position on whichever screen is active and returns what to do.
//! When the master itself is active the OS moves the real cursor; the router
//! only watches for the cursor leaving the screen. While a secondary is active
//! the master swallows local input and forwards the movement instead.
//! Call `sync_local` with the real cursor position whenever the master is active.

use crate::layout::{Layout, Screen, Side};

/// Scale `v` (0..from) to the same fraction of 0..to.
fn scale(v: i32, from: i32, to: i32) -> i32 {
    if from <= 1 || to <= 1 {
        return 0;
    }
    let v = v.clamp(0, from - 1) as i64;
    ((v * (to - 1) as i64 + (from - 1) as i64 / 2) / (from - 1) as i64) as i32
}

/// Where the cursor appears on `next` after leaving `cur` through `side`,
/// with (nx, ny) the position it would have had on `cur` (just past the edge).
/// It enters at the facing edge of `next`, carrying on by however far it
/// went past the edge, and the position along the edge keeps the same
/// fraction of the screen (a third of the way down stays a third of the way
/// down, whatever the two screen sizes).
pub fn entry_point(cur: &Screen, next: &Screen, side: Side, nx: i32, ny: i32) -> (i32, i32) {
    let last_x = next.width - 1;
    let last_y = next.height - 1;
    match side {
        Side::Right => (
            (nx - cur.width).clamp(0, last_x),
            scale(ny, cur.height, next.height),
        ),
        Side::Left => (
            (last_x + 1 + nx).clamp(0, last_x),
            scale(ny, cur.height, next.height),
        ),
        Side::Bottom => (
            scale(nx, cur.width, next.width),
            (ny - cur.height).clamp(0, last_y),
        ),
        Side::Top => (
            scale(nx, cur.width, next.width),
            (last_y + 1 + ny).clamp(0, last_y),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Cursor leaves this computer. If it is the master, start swallowing local input.
    Leave { id: String },
    /// Cursor enters this computer at (x, y). If it is the master, release local input
    /// and place the real cursor there.
    Enter { id: String, x: i32, y: i32 },
    /// Send this movement to the active secondary.
    Forward { dx: i32, dy: i32 },
}

pub struct Router {
    layout: Layout,
    local: String,
    active: String,
    x: i32,
    y: i32,
}

impl Router {
    /// `local` is the master's own id; the cursor starts in its centre.
    pub fn new(layout: Layout, local: &str) -> Option<Router> {
        let s = layout.screen(local)?;
        let (x, y) = (s.width / 2, s.height / 2);
        Some(Router {
            layout,
            local: local.to_string(),
            active: local.to_string(),
            x,
            y,
        })
    }

    pub fn active(&self) -> &str {
        &self.active
    }

    pub fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    pub fn is_local_active(&self) -> bool {
        self.active == self.local
    }

    pub fn set_layout(&mut self, layout: Layout) {
        // Keep the cursor where it is if its computer still exists.
        if layout.screen(&self.active).is_none() {
            if let Some(s) = layout.screen(&self.local) {
                self.x = s.width / 2;
                self.y = s.height / 2;
            }
            self.active = self.local.clone();
        }
        // A screen may have changed size (new resolution): stay inside it.
        if let Some(s) = layout.screen(&self.active) {
            self.x = self.x.clamp(0, (s.width - 1).max(0));
            self.y = self.y.clamp(0, (s.height - 1).max(0));
        }
        self.layout = layout;
    }

    /// A secondary went away: if it was active, bring the cursor home.
    pub fn machine_lost(&mut self, id: &str) -> Vec<Action> {
        if self.active != id || id == self.local {
            return vec![];
        }
        let s = match self.layout.screen(&self.local) {
            Some(s) => s,
            None => return vec![],
        };
        self.active = self.local.clone();
        self.x = s.width / 2;
        self.y = s.height / 2;
        vec![Action::Enter {
            id: self.local.clone(),
            x: self.x,
            y: self.y,
        }]
    }

    /// Real cursor position reported by the OS while the master is active.
    pub fn sync_local(&mut self, x: i32, y: i32) {
        if self.is_local_active() {
            self.x = x;
            self.y = y;
        }
    }

    /// Hotkey: jump straight to another computer's screen centre.
    pub fn jump_to(&mut self, id: &str) -> Vec<Action> {
        let target = match self.layout.screen(id) {
            Some(s) if s.id != self.active => s.clone(),
            _ => return vec![],
        };
        let mut out = vec![Action::Leave {
            id: self.active.clone(),
        }];
        self.active = target.id.clone();
        self.x = target.width / 2;
        self.y = target.height / 2;
        out.push(Action::Enter {
            id: target.id,
            x: self.x,
            y: self.y,
        });
        out
    }

    pub fn feed_delta(&mut self, dx: i32, dy: i32) -> Vec<Action> {
        self.feed(dx, dy, true)
    }

    /// Like `feed_delta`; with `allow_switch` false the cursor stops at the
    /// edge instead of moving to another computer (used while a mouse button
    /// is held, so a drag is never cut in half).
    pub fn feed(&mut self, dx: i32, dy: i32, allow_switch: bool) -> Vec<Action> {
        let cur = match self.layout.screen(&self.active) {
            Some(s) => s.clone(),
            None => return vec![],
        };
        let (nx, ny) = (self.x + dx, self.y + dy);
        let remote = !self.is_local_active();

        if nx >= 0 && nx < cur.width && ny >= 0 && ny < cur.height {
            self.x = nx;
            self.y = ny;
            return if remote {
                vec![Action::Forward { dx, dy }]
            } else {
                vec![]
            };
        }
        if allow_switch {
            // Horizontal edge first, then vertical (a diagonal move into a corner).
            let mut sides = Vec::with_capacity(2);
            if nx >= cur.width {
                sides.push(Side::Right);
            } else if nx < 0 {
                sides.push(Side::Left);
            }
            if ny >= cur.height {
                sides.push(Side::Bottom);
            } else if ny < 0 {
                sides.push(Side::Top);
            }
            for side in sides {
                if let Some(next) = self.layout.neighbour(&cur, side).cloned() {
                    let (ex, ey) = entry_point(&cur, &next, side, nx, ny);
                    let out = vec![
                        Action::Leave { id: cur.id.clone() },
                        Action::Enter {
                            id: next.id.clone(),
                            x: ex,
                            y: ey,
                        },
                    ];
                    self.active = next.id;
                    self.x = ex;
                    self.y = ey;
                    return out;
                }
            }
        }
        // Nothing beyond this edge: stop at the edge.
        let cx = nx.clamp(0, cur.width - 1);
        let cy = ny.clamp(0, cur.height - 1);
        let (fdx, fdy) = (cx - self.x, cy - self.y);
        self.x = cx;
        self.y = cy;
        if remote && (fdx != 0 || fdy != 0) {
            vec![Action::Forward { dx: fdx, dy: fdy }]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Screen;

    fn lay(v: Vec<Screen>) -> Layout {
        Layout::new(v).unwrap()
    }
    fn leave(id: &str) -> Action {
        Action::Leave { id: id.into() }
    }
    fn enter(id: &str, x: i32, y: i32) -> Action {
        Action::Enter {
            id: id.into(),
            x,
            y,
        }
    }

    #[test]
    fn side_by_side_there_and_back() {
        // A (master) 1920x1080 on the left, B 1366x768 on the right, tops aligned.
        let l = lay(vec![
            Screen::new("A", 0, 0, 1920, 1080),
            Screen::new("B", 1920, 0, 1366, 768),
        ]);
        let mut r = Router::new(l, "A").unwrap();
        r.sync_local(1900, 540);
        assert_eq!(r.feed_delta(10, 0), vec![]);
        r.sync_local(1919, 540);
        // Enters at B's left edge, half-way down stays half-way down.
        assert_eq!(r.feed_delta(5, 0), vec![leave("A"), enter("B", 4, 384)]);
        assert_eq!(r.active(), "B");
        assert_eq!(
            r.feed_delta(30, -7),
            vec![Action::Forward { dx: 30, dy: -7 }]
        );
        // Back left across the shared edge: lands at A's right edge.
        assert_eq!(
            r.feed_delta(-40, 0),
            vec![leave("B"), enter("A", 1914, 530)]
        );
        assert!(r.is_local_active());
    }

    #[test]
    fn shorter_neighbour_is_reached_along_the_whole_edge() {
        // B is shorter than A: the bottom of A's right edge maps to B's bottom.
        let l = lay(vec![
            Screen::new("A", 0, 0, 1920, 1080),
            Screen::new("B", 1920, 0, 1366, 768),
        ]);
        let mut r = Router::new(l, "A").unwrap();
        r.sync_local(1919, 1079);
        assert_eq!(r.feed_delta(1, 0), vec![leave("A"), enter("B", 0, 767)]);
    }

    #[test]
    fn enters_at_facing_edge_in_every_direction() {
        // Master in the middle of a plus shape is impossible with three PCs,
        // so check each direction with its own two-PC layout built from the grid.
        use crate::layout::{layout_from_cells, Placed};
        let pl = |id: &str, w, h, col, row| Placed {
            id: id.into(),
            width: w,
            height: h,
            col,
            row,
        };
        // (B's cell, push dx, push dy, where B is entered).
        type Case = (Vec<Placed>, (i32, i32), (i32, i32), (i32, i32));
        let cases: Vec<Case> = vec![
            // B to the right: leave A's right edge a quarter down.
            (
                vec![pl("A", 1920, 1080, 0, 0), pl("B", 1366, 768, 1, 0)],
                (1919, 270),
                (1, 0),
                (0, 192),
            ),
            // B to the left.
            (
                vec![pl("B", 1366, 768, 0, 0), pl("A", 1920, 1080, 1, 0)],
                (0, 270),
                (-1, 0),
                (1365, 192),
            ),
            // B below.
            (
                vec![pl("A", 1920, 1080, 0, 0), pl("B", 1366, 768, 0, 1)],
                (480, 1079),
                (0, 1),
                (341, 0),
            ),
            // B above.
            (
                vec![pl("B", 1366, 768, 0, 0), pl("A", 1920, 1080, 0, 1)],
                (480, 0),
                (0, -1),
                (341, 767),
            ),
        ];
        for (cells, start, push, want) in cases {
            let l = layout_from_cells(&cells).unwrap();
            let mut r = Router::new(l, "A").unwrap();
            r.sync_local(start.0, start.1);
            assert_eq!(
                r.feed_delta(push.0, push.1),
                vec![leave("A"), enter("B", want.0, want.1)],
                "B at cell ({}, {})",
                cells.iter().find(|c| c.id == "B").unwrap().col,
                cells.iter().find(|c| c.id == "B").unwrap().row
            );
            // And straight back: the cursor returns to the edge it left from.
            let acts = r.feed_delta(-push.0 * 2, -push.1 * 2);
            assert_eq!(acts.len(), 2);
            assert!(r.is_local_active());
            let (x, y) = r.position();
            assert!((x - start.0).abs() <= 1 && (y - start.1).abs() <= 1);
        }
    }

    #[test]
    fn three_pcs_of_different_sizes_in_a_row_and_l_shape() {
        use crate::layout::{layout_from_cells, Placed};
        let pl = |id: &str, w, h, col, row| Placed {
            id: id.into(),
            width: w,
            height: h,
            col,
            row,
        };
        // A narrow PC above a wide one, a third PC to the right of the narrow one:
        // the grid column is as wide as the widest PC, so A and C do not touch,
        // but A's right edge still leads to C, entering at C's left edge.
        let l = layout_from_cells(&[
            pl("A", 1366, 768, 0, 0),
            pl("C", 1600, 900, 1, 0),
            pl("B", 2560, 1440, 0, 1),
        ])
        .unwrap();
        let mut r = Router::new(l, "A").unwrap();
        r.sync_local(1365, 384);
        assert_eq!(r.feed_delta(1, 0), vec![leave("A"), enter("C", 0, 450)]);
        // Back to A, then down into the wide PC below it.
        r.jump_to("A");
        r.sync_local(683, 767);
        assert_eq!(r.feed_delta(0, 1), vec![leave("A"), enter("B", 1280, 0)]);
        // B's top edge leads back up into A.
        assert_eq!(r.feed_delta(0, -2), vec![leave("B"), enter("A", 683, 766)]);
    }

    #[test]
    fn stacked_vertically_and_l_shape() {
        // A on top, B below it, C to the right of B.
        let l = lay(vec![
            Screen::new("A", 0, 0, 1000, 600),
            Screen::new("B", 0, 600, 1000, 600),
            Screen::new("C", 1000, 600, 800, 600),
        ]);
        let mut r = Router::new(l, "A").unwrap();
        r.sync_local(400, 599);
        assert_eq!(r.feed_delta(0, 3), vec![leave("A"), enter("B", 400, 2)]);
        r.feed_delta(700, 0); // 400+700 > 999 -> into C at x=100
        assert_eq!(r.active(), "C");
        assert_eq!(r.position(), (100, 2));
        // C's top edge has no neighbour above it: clamp, forward only the allowed part.
        assert_eq!(
            r.feed_delta(0, -10),
            vec![Action::Forward { dx: 0, dy: -2 }]
        );
        assert_eq!(r.feed_delta(0, -10), vec![]);
    }

    #[test]
    fn hotkey_jump_and_lost_machine() {
        let l = lay(vec![
            Screen::new("A", 0, 0, 800, 600),
            Screen::new("B", 800, 0, 800, 600),
        ]);
        let mut r = Router::new(l, "A").unwrap();
        assert_eq!(r.jump_to("B"), vec![leave("A"), enter("B", 400, 300)]);
        assert_eq!(r.jump_to("B"), vec![]);
        assert_eq!(r.machine_lost("B"), vec![enter("A", 400, 300)]);
        assert!(r.is_local_active());
        assert_eq!(r.machine_lost("B"), vec![]);
    }

    #[test]
    fn no_switch_while_dragging() {
        let l = lay(vec![
            Screen::new("A", 0, 0, 800, 600),
            Screen::new("B", 800, 0, 800, 600),
        ]);
        let mut r = Router::new(l, "A").unwrap();
        r.sync_local(799, 300);
        assert_eq!(r.feed(5, 0, false), vec![]);
        assert!(r.is_local_active());
        assert_eq!(r.feed(5, 0, true), vec![leave("A"), enter("B", 4, 300)]);
        // On the secondary, a drag stops at the edge too.
        assert_eq!(
            r.feed(-50, 0, false),
            vec![Action::Forward { dx: -4, dy: 0 }]
        );
        assert_eq!(r.active(), "B");
    }

    #[test]
    fn layout_rules() {
        assert!(Layout::new(vec![]).is_err());
        let four: Vec<_> = (0..4)
            .map(|i| Screen::new(&format!("p{i}"), i * 100, 0, 100, 100))
            .collect();
        assert!(Layout::new(four).is_err());
        let overlap = vec![
            Screen::new("a", 0, 0, 100, 100),
            Screen::new("b", 50, 50, 100, 100),
        ];
        assert!(Layout::new(overlap).is_err());
        let dup = vec![
            Screen::new("a", 0, 0, 100, 100),
            Screen::new("a", 100, 0, 100, 100),
        ];
        assert!(Layout::new(dup).is_err());
        assert!(Layout::new(vec![Screen::new("a", 0, 0, 0, 100)]).is_err());
    }
}
