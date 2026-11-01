//! A file tab's rendered markdown view, one unit so a tab switch or a recovery moves it whole.

use crate::diff::{RenderedKind, Row};
use crate::marks::{DocMap, MarkMap, Unit, landing};
use std::collections::HashMap;

/// The open file's rendered view in one file tab.
#[derive(Debug, Default)]
pub(crate) struct RenderedView {
    /// The render's input; `None` for a non-markdown file, a notice, or an empty new side.
    pub content: Option<Content>,
    /// The old side's source map, width-independent, so a resize re-renders only the new side.
    pub old_map: Option<OldMap>,
    /// The reviewer's `<details>` choices by key; an unchosen one opens over a change or comment.
    pub details: HashMap<String, bool>,
    /// The render behind the rows: styled lines, links, `<details>`, heading anchors.
    pub doc: crate::markdown::Rendered,
    /// The change marks of the rows on screen.
    pub marks: MarkMap,
    /// The index over the rows on screen; empty while they are source rows.
    pub index: RenderedIndex,
    /// What the rows were last built from; `None` forces the next build.
    pub built: Option<Built>,
}

/// The open markdown file's text, its old side in `Changes`, and whether it renders nothing.
#[derive(Debug)]
pub(crate) struct Content {
    pub text: String,
    pub old: Option<String>,
    pub nothing: bool,
}

/// The old side's source map and the old text and open `<details>` it was read from.
#[derive(Debug)]
pub(crate) struct OldMap {
    pub text: String,
    pub open: Vec<String>,
    pub map: DocMap,
}

impl RenderedView {
    /// The current text, when the open file is markdown that renders.
    pub(crate) fn text(&self) -> Option<&str> {
        self.content.as_ref().map(|c| c.text.as_str())
    }

    /// Whether the content renders no rows, so its source shows and `m` is inert.
    pub(crate) fn renders_nothing(&self) -> bool {
        self.content.as_ref().is_some_and(|c| c.nothing)
    }

    /// Whether the rows on screen are rendered ones.
    pub(crate) fn on_screen(&self) -> bool {
        !self.index.units.is_empty()
    }

    /// Drop the render, marks and index behind rows no longer on screen.
    pub(crate) fn drop_rows(&mut self) {
        self.doc = crate::markdown::Rendered::default();
        self.marks = MarkMap::default();
        self.index = RenderedIndex::default();
    }

    /// Forget the open file: no content, nothing built, no rows behind.
    pub(crate) fn clear(&mut self) {
        self.content = None;
        self.built = None;
        self.drop_rows();
    }
}

/// One build of the rendered rows: its input, and whether the content rendered nothing.
#[derive(Debug)]
pub(crate) struct Built {
    pub input: RenderedInput,
    pub empty: bool,
}

/// Everything rendered rows build from: equal inputs build equal rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RenderedInput {
    pub text: String,
    pub details: Vec<String>,
    pub width: usize,
    pub theme: &'static str,
    /// A digest of the diff's changed lines in `Changes`, which a scope switch moves.
    pub changes: Option<u64>,
}

/// A rendered row's identity across rebuilds: its unit, source line, and wrap (`None` for a gap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowId {
    pub unit: Unit,
    pub line: u32,
    pub wrap: Option<u32>,
}

impl RowId {
    /// The identity of rendered row `row`; `None` for any other row.
    pub(crate) fn of(row: &Row) -> Option<Self> {
        let unit = unit_of(row)?;
        let (line, wrap) = match row {
            Row::Rendered { kind: RenderedKind::Block { source, wrap, .. }, .. } => {
                (source.0, *wrap)
            }
            _ => (unit.src(), Some(0)),
        };
        Some(RowId { unit, line, wrap })
    }

    /// The same identity with every source line carried through `map` — an edit's line map.
    pub(crate) fn map(self, map: impl Fn(u32) -> u32) -> Self {
        let unit = match self.unit {
            Unit::Block(src) => Unit::Block(map(src)),
            Unit::Marker(src, kind) => Unit::Marker(map(src), kind),
        };
        RowId { unit, line: map(self.line), wrap: self.wrap }
    }
}

/// The unit a rendered row belongs to: its block, or the marker it is.
pub(crate) fn unit_of(row: &Row) -> Option<Unit> {
    match row {
        Row::Rendered { src, kind: RenderedKind::Block { .. }, .. } => Some(Unit::Block(*src)),
        Row::Rendered { src, kind: RenderedKind::Marker { kind, .. }, .. } => {
            Some(Unit::Marker(*src, *kind))
        }
        _ => None,
    }
}

/// One unit's rows, source range, and lead row (its first non-gap row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnitRows {
    pub unit: Unit,
    pub start: usize,
    pub end: usize,
    pub src: u32,
    pub src_end: u32,
    pub lead: usize,
}

/// The index over a build's rendered rows: each unit's run, built once per rebuild.
#[derive(Clone, Debug, Default)]
pub(crate) struct RenderedIndex {
    units: Vec<UnitRows>,
    by_unit: HashMap<Unit, usize>,
    /// The blocks' positions in `units`, and their source ranges: where a source line lands.
    blocks: Vec<usize>,
    block_ranges: Vec<(u32, u32)>,
    /// Per row: the source lines its text comes from, and whether it is a gap.
    row_source: Vec<(u32, u32)>,
    gap: Vec<bool>,
    /// The units a new-side comment can show in, and their ranges.
    new_side: Vec<usize>,
    new_ranges: Vec<(u32, u32)>,
}

impl RenderedIndex {
    /// The index over `rows`.
    pub(crate) fn build(rows: &[Row]) -> Self {
        let row_source = rows
            .iter()
            .map(|r| match r {
                Row::Rendered { kind: RenderedKind::Block { source, .. }, .. } => *source,
                Row::Rendered { src, .. } => (*src, *src),
                _ => (0, 0),
            })
            .collect();
        let gap: Vec<bool> = rows
            .iter()
            .map(|r| {
                matches!(r, Row::Rendered { kind: RenderedKind::Block { wrap: None, .. }, .. })
            })
            .collect();
        let mut index = Self { row_source, gap: gap.clone(), ..Self::default() };
        let mut start = 0;
        while start < rows.len() {
            let (Some(unit), Row::Rendered { src, src_end, kind, .. }) =
                (unit_of(&rows[start]), &rows[start])
            else {
                start += 1;
                continue;
            };
            let end = start + rows[start..].iter().take_while(|r| unit_of(r) == Some(unit)).count();
            let lead = start + (start..end).position(|k| !gap[k]).unwrap_or(0);
            let k = index.units.len();
            match kind {
                RenderedKind::Block { .. } => {
                    index.blocks.push(k);
                    index.block_ranges.push((*src, *src_end));
                    index.new_side.push(k);
                    index.new_ranges.push((*src, *src_end));
                }
                RenderedKind::Marker { gone: false, .. } => {
                    index.new_side.push(k);
                    index.new_ranges.push((*src, *src_end));
                }
                RenderedKind::Marker { gone: true, .. } => {}
            }
            index.by_unit.insert(unit, k);
            index.units.push(UnitRows { unit, start, end, src: *src, src_end: *src_end, lead });
            start = end;
        }
        index
    }

    /// Every unit's run, in row order.
    pub(crate) fn units(&self) -> &[UnitRows] {
        &self.units
    }

    /// The position of `unit`'s run in [`Self::units`].
    pub(crate) fn position(&self, unit: Unit) -> Option<usize> {
        self.by_unit.get(&unit).copied()
    }

    /// The run of `unit`.
    pub(crate) fn get(&self, unit: Unit) -> Option<&UnitRows> {
        self.units.get(self.position(unit)?)
    }

    /// The run row `row` belongs to.
    pub(crate) fn unit_at(&self, row: usize) -> Option<&UnitRows> {
        let k = self.units.partition_point(|u| u.start <= row).checked_sub(1)?;
        self.units.get(k).filter(|u| row < u.end)
    }

    /// The block source line `line` lands on, by the landing rule ([`landing`]).
    fn land(&self, line: Option<u32>) -> Option<&UnitRows> {
        landing(&self.block_ranges, line).map(|k| &self.units[self.blocks[k]])
    }

    /// The row source line `line` lands on: the lead row of its landing block.
    pub(crate) fn row_at_line(&self, line: u32) -> Option<usize> {
        self.land(Some(line)).map(|u| u.lead)
    }

    /// The units a new-side range overlaps, else the one its first line lands on.
    pub(crate) fn new_side_cover(&self, start: u32, end: u32) -> Vec<usize> {
        let overlap: Vec<usize> = self
            .new_ranges
            .iter()
            .zip(&self.new_side)
            .filter(|&(&(s, e), _)| s <= end && start <= e)
            .map(|(_, &k)| k)
            .collect();
        if overlap.is_empty() {
            self.new_side_land(Some(start)).into_iter().collect()
        } else {
            overlap
        }
    }

    /// The unit a new-side line lands on; `None` (past the end) lands on the last.
    pub(crate) fn new_side_land(&self, line: Option<u32>) -> Option<usize> {
        landing(&self.new_ranges, line).map(|k| self.new_side[k])
    }

    /// The row `id` reconciles onto by source line and wrap, whatever rewrapped around it.
    pub(crate) fn row_of(&self, id: RowId) -> Option<usize> {
        if let Unit::Marker(..) = id.unit
            && let Some(u) = self.get(id.unit)
        {
            return Some(u.start);
        }
        let u = self.land(Some(id.line))?;
        // A gap row lands on the block's first row: its gap when it has one.
        let Some(wrap) = id.wrap else { return Some(u.start) };
        let rows = u.start..u.end;
        let starts: Vec<usize> =
            rows.clone().filter(|&k| !self.gap[k] && self.row_source[k].0 == id.line).collect();
        if let Some(&last) = starts.last() {
            return Some(starts.get(wrap as usize).copied().unwrap_or(last));
        }
        let shows = |k: &usize| (self.row_source[*k].0..=self.row_source[*k].1).contains(&id.line);
        Some(rows.clone().find(shows).unwrap_or(u.lead))
    }
}
