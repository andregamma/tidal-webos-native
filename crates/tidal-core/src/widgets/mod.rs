//! Reusable components that are not pages: they own no route and no backend access, only their
//! own small state. `App` draws the mini bar on top of whichever page is showing; a page draws the
//! widgets it owns (e.g. `select`) itself.

pub mod minibar;
pub mod select;
