use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

#[derive(Debug, Clone, Copy)]
enum Mode {
    Plain,
    AccountingRoot,
    AccountingBody,
}

#[derive(Debug, Clone, Copy)]
pub struct Tree<'a> {
    node: &'a Node,
    mode: Mode,
}
impl<'a> Tree<'a> {
    pub(super) fn raw_node(self) -> Option<&'a Node> {
        matches!(self.mode, Mode::Plain).then_some(self.node)
    }
    pub fn to_node(self, budget: &MemoryBudget) -> Result<Charged<Node>, BudgetError> {
        struct Frame<'a> {
            list: TreeList<'a>,
            next: usize,
            items: Charged<Vec<Node>>,
        }
        let mut stack = BudgetedVec::new(budget, Category::Metadata)?;
        let mut next = self;
        loop {
            let mut built = if let Some(list) = next.as_list() {
                let items = BudgetedVec::try_with_capacity(budget, Category::Metadata, list.len())?
                    .into_charged_vec();
                if let Some(first) = list.get(0) {
                    stack.try_push(Frame {
                        list,
                        next: 1,
                        items,
                    })?;
                    next = first;
                    continue;
                }
                items.map(Node::List)
            } else {
                let value =
                    super::index::text(budget, &[next.text().ok_or(BudgetError::Overflow)?])?
                        .into_string()?;
                if next.as_str().is_some() {
                    value.map(Node::Str)
                } else {
                    value.map(Node::Atom)
                }
            };
            loop {
                let Some(mut frame) = stack.pop() else {
                    return Ok(built);
                };
                frame.items = super::values::append(frame.items, built)?;
                if let Some(child) = frame.list.get(frame.next) {
                    frame.next = frame.next.checked_add(1).ok_or(BudgetError::Overflow)?;
                    stack.try_push(frame)?;
                    next = child;
                    break;
                }
                built = frame.items.map(Node::List);
            }
        }
    }
    pub fn new(kind: &str, node: &'a Node) -> Self {
        let mode = if kind == "AccountingRegister"
            && node.get(&[1, 0]).and_then(Node::as_i64) == Some(22)
        {
            Mode::AccountingRoot
        } else {
            Mode::Plain
        };
        Self { node, mode }
    }
    fn child(self, index: usize) -> Option<Self> {
        let list = self.node.as_list()?;
        let raw_index = if matches!(self.mode, Mode::AccountingBody) && list.len() > 1 && index >= 1
        {
            index.checked_add(1)?
        } else {
            index
        };
        let node = list.get(raw_index)?;
        let mode = if matches!(self.mode, Mode::AccountingRoot) && index == 1 {
            Mode::AccountingBody
        } else {
            Mode::Plain
        };
        Some(Self { node, mode })
    }
    pub fn get(self, path: &[usize]) -> Option<Self> {
        self.get_path(path.iter().copied())
    }
    pub fn get_path(self, path: impl IntoIterator<Item = usize>) -> Option<Self> {
        let mut tree = self;
        for index in path {
            tree = tree.child(index)?;
        }
        Some(tree)
    }
    pub fn as_list(self) -> Option<TreeList<'a>> {
        let list = self.node.as_list()?;
        let skipped = usize::from(matches!(self.mode, Mode::AccountingBody) && list.len() > 1);
        Some(TreeList {
            parent: self,
            start: 0,
            len: list.len() - skipped,
        })
    }
    pub fn text(self) -> Option<&'a str> {
        self.node.text()
    }
    pub fn as_str(self) -> Option<&'a str> {
        self.node.as_str()
    }
    pub fn as_atom(self) -> Option<&'a str> {
        self.node.as_atom()
    }
    pub fn as_guid(self) -> Option<Guid> {
        self.node.as_guid()
    }
    pub fn as_i64(self) -> Option<i64> {
        self.node.as_i64()
    }
    pub fn as_u32(self) -> Option<u32> {
        self.node.as_u32()
    }
    pub fn as_bool(self) -> Option<bool> {
        self.node.as_bool()
    }
    pub fn is_list(self) -> bool {
        self.node.is_list()
    }
    pub fn is_empty_slot(self) -> bool {
        self.node.is_empty_slot()
    }
    pub fn find_collection(self, class: &str) -> Option<TreeList<'a>> {
        for entry in self.as_list()?.iter() {
            let Some(list) = entry.as_list() else {
                continue;
            };
            let (Some(head), Some(count)) = (list.get(0), list.get(1)) else {
                continue;
            };
            if head.text() != Some(class) || count.is_list() {
                continue;
            }
            let Some(count) = count
                .text()
                .and_then(|text| text.trim().parse::<usize>().ok())
            else {
                continue;
            };
            return Some(TreeList {
                parent: entry,
                start: 2,
                len: count.min(list.len().saturating_sub(2)),
            });
        }
        None
    }
    fn is_header(self, uuid: &str) -> bool {
        self.as_list().is_some_and(|list| {
            list.len() >= 5
                && list.get(2).and_then(Self::as_str).is_some()
                && list.get(1).and_then(Self::as_list).is_some_and(|header| {
                    header.len() == 3 && header.get(2).and_then(Self::text) == Some(uuid)
                })
        })
    }
    pub fn header_path(
        self,
        uuid: &str,
        budget: &MemoryBudget,
        category: Category,
    ) -> Result<Option<Charged<Vec<usize>>>, BudgetError> {
        if self.is_header(uuid) {
            return Ok(Some(
                BudgetedVec::try_with_capacity(budget, category, 0)?.into_charged_vec(),
            ));
        }
        let Some(list) = self.as_list().filter(|list| !list.is_empty()) else {
            return Ok(None);
        };
        struct Frame<'a> {
            list: TreeList<'a>,
            child: usize,
        }
        let mut stack = BudgetedVec::new(budget, category)?;
        stack.try_push(Frame { list, child: 0 })?;
        while let Some(frame) = stack.last_mut() {
            let Some(child) = frame.list.get(frame.child) else {
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.child = parent.child.checked_add(1).ok_or(BudgetError::Overflow)?;
                }
                continue;
            };
            if child.is_header(uuid) {
                let mut output = BudgetedVec::try_with_capacity(budget, category, stack.len())?;
                for frame in stack.iter() {
                    output.try_push(frame.child)?;
                }
                return Ok(Some(output.into_charged_vec()));
            }
            if let Some(list) = child.as_list().filter(|list| !list.is_empty()) {
                stack.try_push(Frame { list, child: 0 })?;
            } else {
                frame.child = frame.child.checked_add(1).ok_or(BudgetError::Overflow)?;
            }
        }
        Ok(None)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TreeList<'a> {
    parent: Tree<'a>,
    start: usize,
    len: usize,
}
impl<'a> TreeList<'a> {
    pub fn len(self) -> usize {
        self.len
    }
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
    pub fn get(self, index: usize) -> Option<Tree<'a>> {
        if index >= self.len {
            return None;
        }
        self.parent.child(self.start.checked_add(index)?)
    }
    pub fn iter(self) -> impl DoubleEndedIterator<Item = Tree<'a>> {
        (0..self.len).filter_map(move |index| self.get(index))
    }
}
