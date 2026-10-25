use super::{index::text, values::append};
use brace::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

struct Frame<'a> {
    children: &'a [Node],
    next: usize,
    output: Charged<Vec<Node>>,
}

pub(super) fn copy_node(
    budget: &MemoryBudget,
    source: &Node,
) -> Result<Charged<Node>, BudgetError> {
    let mut stack = BudgetedVec::<Frame<'_>>::new(budget, Category::Metadata)?;
    let mut current = source;
    loop {
        let mut complete = match current {
            Node::Str(value) => text(budget, &[value])?.into_string()?.map(Node::Str),
            Node::Atom(value) => text(budget, &[value])?.into_string()?.map(Node::Atom),
            Node::List(children) => {
                if stack.len() >= brace::MAX_DEPTH {
                    return Err(BudgetError::Overflow);
                }
                let output = BudgetedVec::<Node>::try_with_capacity(
                    budget,
                    Category::Metadata,
                    children.len(),
                )?
                .into_charged_vec();
                if let Some(first) = children.first() {
                    stack.try_push(Frame {
                        children,
                        next: 1,
                        output,
                    })?;
                    current = first;
                    continue;
                }
                output.map(Node::List)
            }
        };
        loop {
            let Some(mut parent) = stack.pop() else {
                return Ok(complete);
            };
            parent.output = append(parent.output, complete)?;
            if let Some(next) = parent.children.get(parent.next) {
                parent.next += 1;
                stack.try_push(parent)?;
                current = next;
                break;
            }
            complete = parent.output.map(Node::List);
        }
    }
}
