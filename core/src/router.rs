//! Decides where the master's mouse is and when it moves to another computer.
//!
//! The master feeds raw mouse movement (dx, dy) in. The router keeps the
//! cursor position on whichever screen is active and returns what to do.
//! When the master itself is active the OS moves the real cursor; the router
//! only watches for the cursor leaving the screen. While a secondary is active
//! the master swallows local input and forwards the movement instead.
//! Call `sync_local` with the real cursor position whenever the master is active.

use crate::layout::Layout;

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
        let cur = match self.layout.screen(&self.active) {
            Some(s) => s.clone(),
            None => return vec![],
        };
        let (nx, ny) = (self.x + dx, self.y + dy);
        let (gx, gy) = (cur.x + nx, cur.y + ny);
        let remote = !self.is_local_active();

        if cur.contains_global(gx, gy) {
            self.x = nx;
            self.y = ny;
            return if remote {
                vec![Action::Forward { dx, dy }]
            } else {
                vec![]
            };
        }
        if let Some(next) = self.layout.at_global(gx, gy).cloned() {
            let (ex, ey) = (gx - next.x, gy - next.y);
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
        r.sync_local(1900, 500);
        assert_eq!(r.feed_delta(10, 0), vec![]);
        r.sync_local(1919, 500);
        assert_eq!(r.feed_delta(5, 0), vec![leave("A"), enter("B", 4, 500)]);
        assert_eq!(r.active(), "B");
        assert_eq!(
            r.feed_delta(30, -7),
            vec![Action::Forward { dx: 30, dy: -7 }]
        );
        // Move back left across the shared edge.
        r.jump_to("A");
        r.sync_local(1919, 100);
        r.feed_delta(1, 0);
        assert_eq!(r.active(), "B");
        assert_eq!(r.feed_delta(-3, 0), vec![leave("B"), enter("A", 1917, 100)]);
        assert!(r.is_local_active());
    }

    #[test]
    fn tall_neighbour_gap_clamps_instead_of_crossing() {
        // B is shorter than A: the part of A's right edge below B has no neighbour.
        let l = lay(vec![
            Screen::new("A", 0, 0, 1920, 1080),
            Screen::new("B", 1920, 0, 1366, 768),
        ]);
        let mut r = Router::new(l, "A").unwrap();
        r.sync_local(1919, 900);
        assert_eq!(r.feed_delta(50, 0), vec![]);
        assert_eq!(r.active(), "A");
        assert_eq!(r.position(), (1919, 900));
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
