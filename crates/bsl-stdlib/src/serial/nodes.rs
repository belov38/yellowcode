use brace::Node;
use std::ops::{Deref, DerefMut};

fn drop_nodes(mut nodes: Vec<Node>) {
    let mut parents = Node::empty();
    loop {
        if let Some(node) = nodes.pop() {
            if let Node::List(children) = node {
                nodes.push(parents);
                parents = Node::List(nodes);
                nodes = children;
            }
        } else {
            let Node::List(mut previous) = parents else {
                break;
            };
            let Some(link) = previous.pop() else {
                break;
            };
            parents = link;
            nodes = previous;
        }
    }
}

pub(super) struct OwnedNode(pub(super) Node);
impl Drop for OwnedNode {
    fn drop(&mut self) {
        if let Node::List(nodes) = std::mem::replace(&mut self.0, Node::empty()) {
            drop_nodes(nodes);
        }
    }
}

pub(super) struct NodeList(pub(super) Vec<Node>);
impl NodeList {
    pub(super) fn into_vec(mut self) -> Vec<Node> {
        std::mem::take(&mut self.0)
    }
    pub(super) fn into_node(self) -> Node {
        Node::List(self.into_vec())
    }
}
impl Deref for NodeList {
    type Target = Vec<Node>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for NodeList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for NodeList {
    fn drop(&mut self) {
        drop_nodes(std::mem::take(&mut self.0));
    }
}
