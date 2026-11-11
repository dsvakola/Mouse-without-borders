//! How the (up to three) computers' screens are arranged, as the user drags
//! them in the settings dialog. Screens sit on one shared virtual desktop.

use crate::{AppError, Result};

pub const MAX_MACHINES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub id: String,
    /// Top-left corner on the shared virtual desktop.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Screen {
    pub fn new(id: &str, x: i32, y: i32, width: i32, height: i32) -> Screen {
        Screen {
            id: id.to_string(),
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains_global(&self, gx: i32, gy: i32) -> bool {
        gx >= self.x && gx < self.x + self.width && gy >= self.y && gy < self.y + self.height
    }

    fn overlaps(&self, o: &Screen) -> bool {
        self.x < o.x + o.width
            && o.x < self.x + self.width
            && self.y < o.y + o.height
            && o.y < self.y + self.height
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    screens: Vec<Screen>,
}

impl Layout {
    pub fn new(screens: Vec<Screen>) -> Result<Layout> {
        if screens.is_empty() {
            return Err(AppError::InvalidLayout("no computers".into()));
        }
        if screens.len() > MAX_MACHINES {
            return Err(AppError::InvalidLayout(format!(
                "at most {MAX_MACHINES} computers"
            )));
        }
        for (i, a) in screens.iter().enumerate() {
            if a.width <= 0 || a.height <= 0 {
                return Err(AppError::InvalidLayout(format!(
                    "{} has no screen size",
                    a.id
                )));
            }
            for b in &screens[i + 1..] {
                if a.id == b.id {
                    return Err(AppError::InvalidLayout(format!("{} appears twice", a.id)));
                }
                if a.overlaps(b) {
                    return Err(AppError::InvalidLayout(format!(
                        "{} overlaps {}",
                        a.id, b.id
                    )));
                }
            }
        }
        Ok(Layout { screens })
    }

    pub fn screen(&self, id: &str) -> Option<&Screen> {
        self.screens.iter().find(|s| s.id == id)
    }

    pub fn screens(&self) -> &[Screen] {
        &self.screens
    }

    pub fn at_global(&self, gx: i32, gy: i32) -> Option<&Screen> {
        self.screens.iter().find(|s| s.contains_global(gx, gy))
    }
}

pub const GRID_COLS: usize = 3;
pub const GRID_ROWS: usize = 2;

/// A computer with its screen size and the grid cell the user dragged it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub id: String,
    pub width: i32,
    pub height: i32,
    pub col: u8,
    pub row: u8,
}

/// Turns the draggable grid into real screen positions. Empty columns and
/// rows take no space, so neighbouring computers touch each other.
pub fn layout_from_cells(items: &[Placed]) -> Result<Layout> {
    let mut col_w = [0i32; GRID_COLS];
    let mut row_h = [0i32; GRID_ROWS];
    for it in items {
        let (c, r) = (it.col as usize, it.row as usize);
        if c >= GRID_COLS || r >= GRID_ROWS {
            return Err(AppError::InvalidLayout(format!(
                "{} is outside the grid",
                it.id
            )));
        }
        col_w[c] = col_w[c].max(it.width);
        row_h[r] = row_h[r].max(it.height);
    }
    let screens = items
        .iter()
        .map(|it| {
            let x: i32 = col_w[..it.col as usize].iter().sum();
            let y: i32 = row_h[..it.row as usize].iter().sum();
            Screen::new(&it.id, x, y, it.width, it.height)
        })
        .collect();
    // Two computers in one cell would overlap; Layout::new rejects that.
    Layout::new(screens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, w: i32, h: i32, col: u8, row: u8) -> Placed {
        Placed {
            id: id.into(),
            width: w,
            height: h,
            col,
            row,
        }
    }

    #[test]
    fn empty_columns_collapse_and_rows_stack() {
        let l = layout_from_cells(&[
            p("A", 1920, 1080, 0, 0),
            p("B", 1366, 768, 2, 0),
            p("C", 1600, 900, 1, 1),
        ])
        .unwrap();
        assert_eq!(l.screen("A").unwrap().x, 0);
        assert_eq!(l.screen("B").unwrap().x, 1920 + 1600); // column 1 is used by C
        assert_eq!(l.screen("C").unwrap().y, 1080);
        assert_eq!(l.screen("C").unwrap().x, 1920);
        let l = layout_from_cells(&[p("A", 1920, 1080, 0, 0), p("B", 1366, 768, 2, 0)]).unwrap();
        assert_eq!(l.screen("B").unwrap().x, 1920); // middle column empty: no gap
    }

    #[test]
    fn same_cell_or_outside_grid_is_rejected() {
        assert!(layout_from_cells(&[p("A", 100, 100, 0, 0), p("B", 100, 100, 0, 0)]).is_err());
        assert!(layout_from_cells(&[p("A", 100, 100, 3, 0)]).is_err());
    }
}
