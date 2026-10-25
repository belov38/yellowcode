use super::moxel::MoxelRow;
use brace::Node;
use memory_budget::BudgetError;
#[derive(Debug)]
pub(crate) enum ViewError {
    Memory(BudgetError),
    Semantic(&'static str),
}
impl From<BudgetError> for ViewError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
pub(crate) fn integer(n: &Node) -> Option<i64> {
    n.as_i64()
        .or_else(|| n.as_str().and_then(|s| s.parse().ok()))
}
fn int(n: &Node) -> Result<i64, ViewError> {
    integer(n).ok_or(ViewError::Semantic("not an integer"))
}
fn count(n: &Node) -> Result<usize, ViewError> {
    usize::try_from(int(n)?).map_err(|_| ViewError::Semantic("negative count"))
}
#[derive(Clone, Copy)]
pub(crate) enum Pairs<'a> {
    Flat(&'a [Node]),
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 owned-model adapter exercised by native roundtrip tests"
        )
    )]
    Native(&'a [(Node, Node)]),
}
impl<'a> Pairs<'a> {
    pub(crate) fn len(self) -> usize {
        match self {
            Self::Flat(v) => v.len() / 2,
            Self::Native(v) => v.len(),
        }
    }
    pub(crate) fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub(crate) fn iter(self) -> impl Iterator<Item = (&'a Node, &'a Node)> {
        let (flat, native) = match self {
            Self::Flat(v) => (v, &[][..]),
            Self::Native(v) => (&[][..], v),
        };
        flat.as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| Some((pair.first()?, pair.get(1)?)))
            .chain(native.iter().map(|(a, b)| (a, b)))
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Cells<'a> {
    Flat(&'a [Node]),
    Native(&'a [(i64, Node)]),
}
impl<'a> Cells<'a> {
    pub(crate) fn iter(self) -> impl Iterator<Item = (i64, &'a Node)> {
        let (flat, native) = match self {
            Self::Flat(v) => (v, &[][..]),
            Self::Native(v) => (&[][..], v),
        };
        flat.as_chunks::<2>()
            .0
            .iter()
            .filter_map(|p| Some((integer(p.first()?)?, p.get(1)?)))
            .chain(native.iter().map(|(a, b)| (*a, b)))
    }
    pub(crate) fn len(self) -> usize {
        match self {
            Self::Flat(v) => v.len() / 2,
            Self::Native(v) => v.len(),
        }
    }
    pub(crate) fn is_empty(self) -> bool {
        self.len() == 0
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Rows<'a> {
    Flat {
        items: &'a [Node],
        count: usize,
    },
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 owned-model adapter exercised by native roundtrip tests"
        )
    )]
    Native(&'a [MoxelRow]),
}
pub(crate) struct RowIter<'a> {
    rows: Rows<'a>,
    at: usize,
    left: usize,
}
impl<'a> Rows<'a> {
    pub(crate) fn iter(self) -> RowIter<'a> {
        RowIter {
            rows: self,
            at: 0,
            left: match self {
                Self::Flat { count, .. } => count,
                Self::Native(v) => v.len(),
            },
        }
    }
    pub(crate) fn len(self) -> usize {
        match self {
            Self::Flat { count, .. } => count,
            Self::Native(v) => v.len(),
        }
    }
    pub(crate) fn is_empty(self) -> bool {
        self.len() == 0
    }
}
impl<'a> Iterator for RowIter<'a> {
    type Item = (i64, i64, Cells<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        match self.rows {
            Rows::Native(rows) => {
                let (index, format, cells) = rows.get(self.at)?;
                self.at = self.at.checked_add(1)?;
                Some((*index, *format, Cells::Native(cells)))
            }
            Rows::Flat { items, .. } => {
                let mut cursor = Cursor { items, at: self.at };
                let index = integer(cursor.take().ok()?)?;
                let format = integer(cursor.take().ok()?)?;
                let pairs = cursor.pairs().ok()?;
                self.at = cursor.at;
                let Pairs::Flat(cells) = pairs else {
                    return None;
                };
                Some((index, format, Cells::Flat(cells)))
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Columns<'a> {
    Flat {
        first: &'a Node,
        rest: &'a [Node],
    },
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 owned-model adapter exercised by native roundtrip tests"
        )
    )]
    Native(&'a [Node]),
}
impl<'a> Columns<'a> {
    pub(crate) fn len(self) -> usize {
        match self {
            Self::Flat { rest, .. } => rest.len().saturating_add(1),
            Self::Native(v) => v.len(),
        }
    }
    pub(crate) fn get(self, index: usize) -> Option<&'a Node> {
        match self {
            Self::Flat { first, rest } => {
                if index == 0 {
                    Some(first)
                } else {
                    rest.get(index - 1)
                }
            }
            Self::Native(v) => v.get(index),
        }
    }
    pub(crate) fn iter(self) -> impl Iterator<Item = &'a Node> {
        let (first, rest) = match self {
            Self::Flat { first, rest } => (Some(first), rest),
            Self::Native(v) => (None, v),
        };
        first.into_iter().chain(rest.iter())
    }
}
#[derive(Clone, Copy)]
pub(crate) enum IndexPairs<'a> {
    Flat(&'a [Node]),
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 owned-model adapter exercised by native roundtrip tests"
        )
    )]
    Native(&'a [(i64, i64)]),
}
impl<'a> IndexPairs<'a> {
    pub(crate) fn iter(self) -> impl Iterator<Item = (i64, i64)> {
        let (flat, native) = match self {
            Self::Flat(v) => (v, &[][..]),
            Self::Native(v) => (&[][..], v),
        };
        flat.as_chunks::<2>()
            .0
            .iter()
            .filter_map(|p| Some((integer(p.first()?)?, integer(p.get(1)?)?)))
            .chain(native.iter().copied())
    }
}
#[derive(Clone, Copy)]
pub(crate) struct MoxelView<'a> {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub head: [u8; 6],
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub version: &'a [Node],
    pub lang: &'a Node,
    pub default_format: &'a Node,
    pub lines: &'a Node,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub u6: &'a Node,
    pub headers: &'a [Node],
    pub u13: &'a [Node],
    pub rows: Rows<'a>,
    pub columns: Columns<'a>,
    pub height: i64,
    pub row_columns: IndexPairs<'a>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub drawing_next: &'a Node,
    pub drawings: &'a [Node],
    pub vg: Pairs<'a>,
    pub hg: Pairs<'a>,
    pub vbreaks: &'a [Node],
    pub hbreaks: Pairs<'a>,
    pub merges: &'a Node,
    pub row_merges: &'a Node,
    pub col_merges: &'a Node,
    pub named: &'a Node,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub print_settings_name: &'a Node,
    pub print_settings: &'a Node,
    pub print_area: &'a Node,
    pub u_after_pa: &'a [Node],
    pub formats: &'a [Node],
    pub fonts: &'a [Node],
    pub format_strings: &'a [Node],
    pub value_types: &'a [Node],
    pub uuids: &'a [Node],
    pub colors: &'a [Node],
    pub pictures: &'a [Node],
    pub save_view: i64,
    pub read_only: i64,
    pub view_settings: &'a Node,
    pub embedded: Pairs<'a>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub used_file_name: &'a Node,
    pub style_colors: &'a Node,
    pub masks: &'a [Node],
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "C9 source sections retained for owned-model roundtrip parity"
        )
    )]
    pub tail: &'a [Node],
}
struct Cursor<'a> {
    items: &'a [Node],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self) -> Result<&'a Node, ViewError> {
        let result = self
            .items
            .get(self.at)
            .ok_or(ViewError::Semantic("truncated"))?;
        self.at = self.at.checked_add(1).ok_or(BudgetError::Overflow)?;
        Ok(result)
    }
    fn list(&mut self, n: usize) -> Result<&'a [Node], ViewError> {
        let end = self.at.checked_add(n).ok_or(BudgetError::Overflow)?;
        let result = self
            .items
            .get(self.at..end)
            .ok_or(ViewError::Semantic("truncated"))?;
        self.at = end;
        Ok(result)
    }
    fn count(&mut self) -> Result<usize, ViewError> {
        count(self.take()?)
    }
    fn int(&mut self) -> Result<i64, ViewError> {
        int(self.take()?)
    }
    fn counted(&mut self) -> Result<&'a [Node], ViewError> {
        let n = self.count()?;
        self.list(n)
    }
    fn pairs(&mut self) -> Result<Pairs<'a>, ViewError> {
        let n = self.count()?.checked_mul(2).ok_or(BudgetError::Overflow)?;
        Ok(Pairs::Flat(self.list(n)?))
    }
}
impl<'a> MoxelView<'a> {
    pub(crate) fn read(items: &'a [Node], head: [u8; 6]) -> Result<Self, ViewError> {
        let mut c = Cursor { items, at: 0 };
        let version = c.list(3)?;
        let lang = c.take()?;
        let default_format = c.take()?;
        let lines = c.take()?;
        let u6 = c.take()?;
        let headers = c.list(6)?;
        let u13 = c.list(2)?;
        let rows_n = c.count()?;
        let row_start = c.at;
        for _ in 0..rows_n {
            c.int()?;
            c.int()?;
            for (a, _) in c.pairs()?.iter() {
                int(a)?;
            }
        }
        let rows = Rows::Flat {
            items: items
                .get(row_start..c.at)
                .ok_or(ViewError::Semantic("truncated"))?,
            count: rows_n,
        };
        let first = c.take()?;
        let height = c.int()?;
        let rest = c.counted()?;
        let columns = Columns::Flat { first, rest };
        let pairs = c.pairs()?;
        for (a, b) in pairs.iter() {
            int(a)?;
            int(b)?;
        }
        let Pairs::Flat(pairs) = pairs else {
            return Err(ViewError::Semantic("invalid source pairs"));
        };
        let row_columns = IndexPairs::Flat(pairs);
        let drawing_next = c.take()?;
        let drawings = c.counted()?;
        let vg = c.pairs()?;
        let hg = c.pairs()?;
        let vbreaks = c.counted()?;
        let hbreaks = c.pairs()?;
        let merges = c.take()?;
        let row_merges = c.take()?;
        let col_merges = c.take()?;
        let named = c.take()?;
        let print_settings_name = c.take()?;
        let print_settings = c.take()?;
        let print_area = c.take()?;
        let u_after_pa = c.list(10)?;
        let formats = c.counted()?;
        let fonts = c.counted()?;
        let format_strings = c.counted()?;
        let value_types = c.counted()?;
        let uuids = c.counted()?;
        let colors = c.counted()?;
        let pictures = c.counted()?;
        let save_view = c.int()?;
        let (read_only, view_settings, embedded, used_file_name);
        if save_view != 0 {
            read_only = c.int()?;
            view_settings = c.take()?;
            embedded = c.pairs()?;
            used_file_name = c.take()?;
        } else {
            embedded = c.pairs()?;
            used_file_name = c.take()?;
            read_only = c.int()?;
            view_settings = c.take()?;
        }
        let style_colors = c.take()?;
        let masks = c.counted()?;
        let tail = items.get(c.at..).unwrap_or_default();
        Ok(Self {
            head,
            version,
            lang,
            default_format,
            lines,
            u6,
            headers,
            u13,
            rows,
            columns,
            height,
            row_columns,
            drawing_next,
            drawings,
            vg,
            hg,
            vbreaks,
            hbreaks,
            merges,
            row_merges,
            col_merges,
            named,
            print_settings_name,
            print_settings,
            print_area,
            u_after_pa,
            formats,
            fonts,
            format_strings,
            value_types,
            uuids,
            colors,
            pictures,
            save_view,
            read_only,
            view_settings,
            embedded,
            used_file_name,
            style_colors,
            masks,
            tail,
        })
    }
}

impl MoxelView<'_> {}
